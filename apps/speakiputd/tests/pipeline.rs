use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use speakiput_asr::{AsrError, TranscriptionBackend, TranscriptionConfig};
use speakiput_audio::{
    AudioCaptureError, AudioDeviceInfo, AudioSource, CaptureController, CaptureSession,
};
use speakiput_client::{BackendClient, BackendService, ClientError, InMemoryBackendClient};
use speakiput_contract::{
    AudioDevice, Envelope, HistoryListResponse, Settings, StableErrorCode, TranscriptFinalEvent,
};
use speakiput_llm::{LlmError, PostProcessor, PromptRewriter};
use speakiput_platform::{
    FocusService, FocusedTarget, InsertionMethod, InsertionResult, PlatformError, TextOutput,
};
use speakiput_storage::{JsonSettingsRepository, JsonlHistoryRepository, SettingsRepository};
use speakiputd::service::{RuntimeComponents, SpeakiputService};

struct NoopStop;

#[async_trait]
impl CaptureController for NoopStop {
    async fn stop_and_wait(&self) -> Result<(), AudioCaptureError> {
        Ok(())
    }
}

struct FakeAudio;

struct DeniedAudio;

#[async_trait]
impl AudioSource for DeniedAudio {
    async fn devices(&self) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        Err(AudioCaptureError::Capture(
            "microphone permission denied".into(),
        ))
    }

    async fn start(
        &self,
        _device_id: &str,
        _level_tx: tokio::sync::mpsc::UnboundedSender<f32>,
    ) -> Result<CaptureSession, AudioCaptureError> {
        Err(AudioCaptureError::Capture(
            "microphone permission denied".into(),
        ))
    }
}

#[async_trait]
impl AudioSource for FakeAudio {
    async fn devices(&self) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        Ok(vec![])
    }

    async fn start(
        &self,
        _device_id: &str,
        level_tx: tokio::sync::mpsc::UnboundedSender<f32>,
    ) -> Result<CaptureSession, AudioCaptureError> {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
        level_tx.send(0.68).unwrap();
        audio_tx.send(vec![1200; 16_000]).unwrap();
        drop(audio_tx);
        Ok(CaptureSession::new(audio_rx, Arc::new(NoopStop)))
    }
}

struct FakeAsr;

#[async_trait]
impl TranscriptionBackend for FakeAsr {
    async fn transcribe_pcm(
        &self,
        audio: &[i16],
        _config: &TranscriptionConfig,
    ) -> Result<String, AsrError> {
        assert!(!audio.is_empty());
        Ok("um raw raw words".into())
    }

    fn available(&self) -> bool {
        true
    }
}

struct AutoStopAudio;

#[async_trait]
impl AudioSource for AutoStopAudio {
    async fn devices(&self) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        Ok(vec![])
    }

    async fn start(
        &self,
        _device_id: &str,
        _level_tx: tokio::sync::mpsc::UnboundedSender<f32>,
    ) -> Result<CaptureSession, AudioCaptureError> {
        let (audio_tx, audio_rx) = tokio::sync::mpsc::unbounded_channel();
        audio_tx.send(vec![12_000; 16_000]).unwrap();
        audio_tx.send(vec![0; 16_000]).unwrap();
        drop(audio_tx);
        Ok(CaptureSession::new(audio_rx, Arc::new(NoopStop)))
    }
}

struct FakePostProcessor;

struct FailingAsr;

#[async_trait]
impl TranscriptionBackend for FailingAsr {
    async fn transcribe_pcm(
        &self,
        _audio: &[i16],
        _config: &TranscriptionConfig,
    ) -> Result<String, AsrError> {
        Err(AsrError::Unavailable("model missing".into()))
    }

    fn available(&self) -> bool {
        false
    }
}

#[async_trait]
impl PostProcessor for FakePostProcessor {
    async fn process(&self, text: &str, _instruction: &str) -> Result<String, LlmError> {
        assert_eq!(text, "raw words");
        Ok("Processed words.".into())
    }
}

struct FakePromptRewriter;

#[async_trait]
impl PromptRewriter for FakePromptRewriter {
    async fn rewrite(&self, text: &str, _instruction: &str) -> Result<String, LlmError> {
        assert_eq!(text, "um raw raw words");
        Ok("Write a structured prompt from the raw words.".into())
    }
}

struct FakeFocus;

#[async_trait]
impl FocusService for FakeFocus {
    async fn focused_target(&self) -> Result<FocusedTarget, PlatformError> {
        Ok(FocusedTarget {
            window_id: Some("fake-window".into()),
            application_id: Some("editor".into()),
            title: Some("Document".into()),
            is_terminal: false,
        })
    }

    async fn refocus(&self, _target: &FocusedTarget) -> Result<(), PlatformError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeOutput(Mutex<Vec<String>>);

#[async_trait]
impl TextOutput for FakeOutput {
    async fn insert(
        &self,
        text: &str,
        method: InsertionMethod,
        _key_delay_ms: u64,
    ) -> Result<InsertionResult, PlatformError> {
        self.0.lock().unwrap().push(text.into());
        Ok(InsertionResult { method })
    }
}

async fn request(client: &impl BackendClient, name: &str, payload: serde_json::Value) -> Envelope {
    client
        .request(Envelope::request(name, payload))
        .await
        .unwrap()
}

#[tokio::test]
async fn accepted_recording_reaches_final_event_history_and_output() {
    let directory = tempfile::tempdir().unwrap();
    let settings = Arc::new(JsonSettingsRepository::new(
        directory.path().join("settings.json"),
    ));
    let mut configured = Settings::default();
    configured.transcription.remove_filler_words = true;
    settings.replace(0, configured).unwrap();
    let history = Arc::new(JsonlHistoryRepository::new(
        directory.path().join("history.jsonl"),
    ));
    let output = Arc::new(FakeOutput::default());
    let service = Arc::new(
        SpeakiputService::new(
            settings,
            history,
            vec!["local_whisper".into(), "keyboard_insertion".into()],
            vec![AudioDevice {
                id: "default".into(),
                name: "Default".into(),
                is_default: true,
            }],
        )
        .with_runtime(RuntimeComponents {
            audio: Arc::new(FakeAudio),
            asr: Arc::new(FakeAsr),
            post_processor: Some(Arc::new(FakePostProcessor)),
            prompt_rewriter: None,
            focus: Arc::new(FakeFocus),
            output: output.clone(),
        }),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    let mut events = client.subscribe();
    request(
        &client,
        "client.hello",
        json!({
            "supported_versions": ["1.0"],
            "client": { "name": "test", "version": "0.1.0" },
            "subscriptions": ["*"]
        }),
    )
    .await;
    let started = request(&client, "recording.start", json!({ "language": "pt" })).await;
    let session_id = started.payload["session_id"].as_str().unwrap().to_owned();
    request(
        &client,
        "recording.stop",
        json!({ "session_id": session_id }),
    )
    .await;

    let final_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.name == "transcript.final" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    let final_transcript: TranscriptFinalEvent = final_event.payload_as().unwrap();
    assert_eq!(final_transcript.raw_text, "um raw raw words");
    assert_eq!(final_transcript.processed_text, "Processed words.");
    assert!(final_transcript.post_processed);
    assert_eq!(output.0.lock().unwrap().as_slice(), ["Processed words."]);

    let history: HistoryListResponse = request(
        &client,
        "history.list",
        json!({ "limit": 10, "cursor": null }),
    )
    .await
    .payload_as()
    .unwrap();
    assert_eq!(history.entries.len(), 1);
    assert_eq!(history.entries[0].raw_text, "um raw raw words");
    assert_eq!(history.entries[0].processed_text, "Processed words.");
}

#[tokio::test]
async fn prompt_rewrite_is_a_separate_optional_stage_and_is_recorded() {
    let directory = tempfile::tempdir().unwrap();
    let settings = Arc::new(JsonSettingsRepository::new(
        directory.path().join("settings.json"),
    ));
    let mut configured = Settings::default();
    configured.post_processing.enabled = false;
    configured.post_processing.prompt_rewrite_enabled = true;
    settings.replace(0, configured).unwrap();
    let output = Arc::new(FakeOutput::default());
    let service = Arc::new(
        SpeakiputService::new(
            settings,
            Arc::new(JsonlHistoryRepository::new(
                directory.path().join("history.jsonl"),
            )),
            vec!["prompt_rewrite".into(), "keyboard_insertion".into()],
            vec![],
        )
        .with_runtime(RuntimeComponents {
            audio: Arc::new(FakeAudio),
            asr: Arc::new(FakeAsr),
            post_processor: None,
            prompt_rewriter: Some(Arc::new(FakePromptRewriter)),
            focus: Arc::new(FakeFocus),
            output: output.clone(),
        }),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    let mut events = client.subscribe();
    let started = request(&client, "recording.start", json!({})).await;
    let session_id = started.payload["session_id"].as_str().unwrap();
    request(
        &client,
        "recording.stop",
        json!({ "session_id": session_id }),
    )
    .await;
    let final_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.name == "transcript.final" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    let transcript: TranscriptFinalEvent = final_event.payload_as().unwrap();
    assert_eq!(transcript.processed_text, "um raw raw words");
    assert_eq!(
        transcript.rewritten_text.as_deref(),
        Some("Write a structured prompt from the raw words.")
    );
    assert!(!transcript.post_processed);
    assert!(transcript.prompt_rewritten);
    assert_eq!(
        output.0.lock().unwrap().as_slice(),
        ["Write a structured prompt from the raw words."]
    );
}

#[tokio::test]
async fn cancellation_emits_one_terminal_event_and_never_inserts() {
    let directory = tempfile::tempdir().unwrap();
    let settings = Arc::new(JsonSettingsRepository::new(
        directory.path().join("settings.json"),
    ));
    let output = Arc::new(FakeOutput::default());
    let service = Arc::new(
        SpeakiputService::new(
            settings,
            Arc::new(JsonlHistoryRepository::new(
                directory.path().join("history.jsonl"),
            )),
            vec!["local_whisper".into(), "keyboard_insertion".into()],
            vec![],
        )
        .with_runtime(RuntimeComponents {
            audio: Arc::new(FakeAudio),
            asr: Arc::new(FakeAsr),
            post_processor: None,
            prompt_rewriter: None,
            focus: Arc::new(FakeFocus),
            output: output.clone(),
        }),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    let mut events = client.subscribe();
    let started = request(&client, "recording.start", json!({})).await;
    let session_id = started.payload["session_id"].as_str().unwrap();
    request(
        &client,
        "operation.cancel",
        json!({ "session_id": session_id }),
    )
    .await;

    let mut terminal_names = Vec::new();
    while let Ok(Ok(event)) =
        tokio::time::timeout(std::time::Duration::from_millis(50), events.recv()).await
    {
        if matches!(event.name.as_str(), "transcript.final" | "session.failed") {
            terminal_names.push(event.name);
        }
    }
    assert_eq!(terminal_names, ["session.failed"]);
    assert!(output.0.lock().unwrap().is_empty());
}

#[tokio::test]
async fn silence_auto_stop_finishes_the_accepted_session() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SpeakiputService::new(
            Arc::new(JsonSettingsRepository::new(
                directory.path().join("settings.json"),
            )),
            Arc::new(JsonlHistoryRepository::new(
                directory.path().join("history.jsonl"),
            )),
            vec!["local_whisper".into()],
            vec![],
        )
        .with_runtime(RuntimeComponents {
            audio: Arc::new(AutoStopAudio),
            asr: Arc::new(FakeAsr),
            post_processor: None,
            prompt_rewriter: None,
            focus: Arc::new(FakeFocus),
            output: Arc::new(FakeOutput::default()),
        }),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    let mut events = client.subscribe();
    let started = request(&client, "recording.start", json!({})).await;
    let session_id = started.payload["session_id"].as_str().unwrap();

    let final_event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.name == "transcript.final" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(final_event.payload["session_id"], session_id);
}

#[tokio::test]
async fn transcription_failure_is_terminal_and_recovers_to_idle() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SpeakiputService::new(
            Arc::new(JsonSettingsRepository::new(
                directory.path().join("settings.json"),
            )),
            Arc::new(JsonlHistoryRepository::new(
                directory.path().join("history.jsonl"),
            )),
            vec![],
            vec![],
        )
        .with_runtime(RuntimeComponents {
            audio: Arc::new(FakeAudio),
            asr: Arc::new(FailingAsr),
            post_processor: None,
            prompt_rewriter: None,
            focus: Arc::new(FakeFocus),
            output: Arc::new(FakeOutput::default()),
        }),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    let mut events = client.subscribe();
    let started = request(&client, "recording.start", json!({})).await;
    let session_id = started.payload["session_id"].as_str().unwrap();
    request(
        &client,
        "recording.stop",
        json!({ "session_id": session_id }),
    )
    .await;

    let failure = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let event = events.recv().await.unwrap();
            if event.name == "session.failed" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(failure.payload["session_id"], session_id);

    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    let snapshot = request(&client, "state.get", json!({})).await;
    assert_eq!(snapshot.payload["state"], "idle");
}

#[tokio::test]
async fn microphone_failure_does_not_accept_or_strand_a_session() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SpeakiputService::new(
            Arc::new(JsonSettingsRepository::new(
                directory.path().join("settings.json"),
            )),
            Arc::new(JsonlHistoryRepository::new(
                directory.path().join("history.jsonl"),
            )),
            vec![],
            vec![],
        )
        .with_runtime(RuntimeComponents {
            audio: Arc::new(DeniedAudio),
            asr: Arc::new(FakeAsr),
            post_processor: None,
            prompt_rewriter: None,
            focus: Arc::new(FakeFocus),
            output: Arc::new(FakeOutput::default()),
        }),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    let result = client
        .request(Envelope::request("recording.start", json!({})))
        .await;
    assert!(matches!(
        result,
        Err(ClientError::Protocol(error))
            if error.code == StableErrorCode::Unavailable && error.retryable
    ));
    let snapshot = request(&client, "state.get", json!({})).await;
    assert_eq!(snapshot.payload["state"], "idle");
    assert_eq!(
        snapshot.payload["active_session_id"],
        serde_json::Value::Null
    );
}
