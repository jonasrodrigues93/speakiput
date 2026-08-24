use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use chrono::Utc;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use speakiput_asr::{AsrError, TranscriptionBackend, TranscriptionConfig};
use speakiput_audio::{AudioSource, AutoStopDetector, CaptureStopHandle, SILENCE_RMS_THRESHOLD};
use speakiput_client::{BackendService, ServiceOutput};
use speakiput_contract::{
    AudioDevice, AudioDevicesResponse, BackendChoice, BackendHealth, BackendsResponse,
    ClientHelloRequest, ClientHelloResponse, ClientIdentity, CredentialIdRequest,
    CredentialPutRequest, CredentialStatusResponse, DiagnosticsResponse, DictationState, Envelope,
    HistoryEntry, HistoryListRequest, HistoryListResponse, InsertionOutcome, InsertionStatus,
    MAX_FRAME_BYTES, OperationCancelRequest, OutputMode, PROTOCOL_VERSION, ProtocolError,
    RecordingStartRequest, RecordingStartResponse, RecordingStopRequest, RecordingStopResponse,
    ServerLimits, Settings, SettingsReplaceRequest, SettingsResponse, StableErrorCode,
    StateSnapshot, TranscriptFinalEvent,
};
use speakiput_core::{Action, LlmInsertion, StateMachine, Transition, prepare_llm_insertion};
use speakiput_llm::PostProcessor;
use speakiput_platform::{FocusService, FocusedTarget, InsertionMethod, TextOutput};
use speakiput_storage::{
    CredentialRepository, HistoryRepository, SettingsRepository, StorageError,
};
use tokio::{
    sync::{Mutex, broadcast, mpsc},
    task::JoinHandle,
};
use uuid::Uuid;

struct BackendState {
    machine: StateMachine,
    sequence: u64,
    active_recording: Option<ActiveRecording>,
}

struct ActiveRecording {
    session_id: Uuid,
    stop: CaptureStopHandle,
    forward_task: JoinHandle<()>,
    asr_task: JoinHandle<Result<(), AsrError>>,
    text_task: JoinHandle<String>,
    level_task: JoinHandle<()>,
    started_at: Instant,
    language: String,
    focused_target: Option<FocusedTarget>,
}

#[derive(Clone)]
pub struct RuntimeComponents {
    pub audio: Arc<dyn AudioSource>,
    pub asr: Arc<dyn TranscriptionBackend>,
    pub post_processor: Option<Arc<dyn PostProcessor>>,
    pub focus: Arc<dyn FocusService>,
    pub output: Arc<dyn TextOutput>,
}

pub struct SpeakiputService {
    instance_id: Uuid,
    state: Arc<Mutex<BackendState>>,
    settings: Arc<dyn SettingsRepository>,
    history: Arc<dyn HistoryRepository>,
    credentials: Option<Arc<dyn CredentialRepository>>,
    capabilities: Vec<String>,
    audio_devices: Vec<AudioDevice>,
    events: broadcast::Sender<Envelope>,
    runtime: Option<RuntimeComponents>,
}

impl SpeakiputService {
    #[must_use]
    pub fn new(
        settings: Arc<dyn SettingsRepository>,
        history: Arc<dyn HistoryRepository>,
        capabilities: Vec<String>,
        audio_devices: Vec<AudioDevice>,
    ) -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            state: Arc::new(Mutex::new(BackendState {
                machine: StateMachine::new(),
                sequence: 0,
                active_recording: None,
            })),
            settings,
            history,
            credentials: None,
            capabilities,
            audio_devices,
            events: broadcast::channel(256).0,
            runtime: None,
        }
    }

    #[must_use]
    pub fn with_runtime(mut self, runtime: RuntimeComponents) -> Self {
        self.runtime = Some(runtime);
        self
    }

    #[must_use]
    pub fn with_credentials(mut self, credentials: Arc<dyn CredentialRepository>) -> Self {
        self.credentials = Some(credentials);
        self
    }

    #[must_use]
    pub fn instance_id(&self) -> Uuid {
        self.instance_id
    }

    /// Routes a global-shortcut activation through the same explicit protocol
    /// commands used by GUI clients. Intermediate states intentionally ignore
    /// activations instead of inventing a second toggle state machine.
    pub async fn activate_record_shortcut(&self) -> Result<(), String> {
        let request = {
            let state = self.state.lock().await;
            match state.machine.state() {
                DictationState::Idle => Envelope::request(
                    "recording.start",
                    serde_json::to_value(RecordingStartRequest { language: None })
                        .map_err(|error| error.to_string())?,
                ),
                DictationState::Recording => Envelope::request(
                    "recording.stop",
                    serde_json::to_value(RecordingStopRequest {
                        session_id: state
                            .machine
                            .active_session_id()
                            .ok_or_else(|| "recording has no active session".to_owned())?,
                    })
                    .map_err(|error| error.to_string())?,
                ),
                _ => return Ok(()),
            }
        };
        let output = self.handle(request).await?;
        if let Some(error) = output.response.error {
            return Err(error.message);
        }
        Ok(())
    }

    async fn event(&self, name: &str, payload: Value) -> Envelope {
        emit_event(self.instance_id, &self.state, &self.events, name, payload).await
    }

    async fn transition_event(&self, transition: Transition) -> Envelope {
        self.event(
            "state.changed",
            json!({
                "previous": transition.previous,
                "current": transition.current,
                "session_id": transition.session_id,
            }),
        )
        .await
    }

    fn error(&self, request: &Envelope, error: ProtocolError) -> ServiceOutput {
        ServiceOutput::response(Envelope::error_response(request, self.instance_id, error))
    }

    fn invalid_payload(&self, request: &Envelope, error: impl std::fmt::Display) -> ServiceOutput {
        self.error(
            request,
            protocol_error(
                StableErrorCode::InvalidArgument,
                format!("invalid {} payload: {error}", request.name),
                false,
            ),
        )
    }

    #[allow(clippy::result_large_err)]
    fn parse<T: DeserializeOwned>(&self, request: &Envelope) -> Result<T, ServiceOutput> {
        request
            .payload_as()
            .map_err(|error| self.invalid_payload(request, error))
    }

    #[allow(clippy::too_many_lines)]
    async fn prepare_recording(
        &self,
        session_id: Uuid,
        settings: &Settings,
        language_override: Option<String>,
    ) -> Result<
        (
            Option<ActiveRecording>,
            Option<tokio::sync::oneshot::Receiver<()>>,
        ),
        String,
    > {
        let Some(runtime) = &self.runtime else {
            return Ok((None, None));
        };
        let language = language_override.unwrap_or_else(|| settings.general.language.clone());
        let focused_target = runtime.focus.focused_target().await.ok();
        let (level_tx, mut level_rx) = mpsc::unbounded_channel();
        let capture = runtime
            .audio
            .start(&settings.audio.input_device_id, level_tx)
            .await
            .map_err(|error| error.to_string())?;
        let (mut audio_rx, stop) = capture.into_parts();
        let (asr_audio_tx, asr_audio_rx) = mpsc::channel(32);
        let (text_tx, mut text_rx) = mpsc::channel(16);
        let (auto_stop_tx, auto_stop_rx) = tokio::sync::oneshot::channel();
        let auto_stop_ms = settings.general.auto_stop_ms;
        let forward_task = tokio::spawn(async move {
            let mut detector = AutoStopDetector::new(
                SILENCE_RMS_THRESHOLD,
                auto_stop_ms,
                speakiput_audio::SAMPLE_RATE,
            );
            let mut auto_stop_tx = Some(auto_stop_tx);
            while let Some(chunk) = audio_rx.recv().await {
                let should_stop = detector.feed(&chunk);
                if asr_audio_tx.send(chunk).await.is_err() {
                    return;
                }
                if should_stop {
                    if let Some(sender) = auto_stop_tx.take() {
                        let _ = sender.send(());
                    }
                    return;
                }
            }
        });
        let asr = Arc::clone(&runtime.asr);
        let transcription = TranscriptionConfig {
            language: language.clone(),
            model_id: settings.transcription.model_id.clone(),
            prompt: settings.transcription.prompt.clone(),
        };
        let asr_task = tokio::spawn(async move {
            asr.transcribe_stream(asr_audio_rx, text_tx, &transcription)
                .await
        });
        let instance_id = self.instance_id;
        let state = Arc::clone(&self.state);
        let events = self.events.clone();
        let text_task = tokio::spawn(async move {
            let mut accumulated = String::new();
            while let Some(partial) = text_rx.recv().await {
                let partial = partial.trim();
                if partial.is_empty() {
                    continue;
                }
                if !accumulated.is_empty() {
                    accumulated.push(' ');
                }
                accumulated.push_str(partial);
                emit_event(
                    instance_id,
                    &state,
                    &events,
                    "transcript.partial",
                    json!({ "session_id": session_id, "text": accumulated }),
                )
                .await;
            }
            accumulated
        });
        let state = Arc::clone(&self.state);
        let events = self.events.clone();
        let level_task = tokio::spawn(async move {
            while let Some(level) = level_rx.recv().await {
                emit_event(
                    instance_id,
                    &state,
                    &events,
                    "recording.level",
                    json!({ "session_id": session_id, "level": level.clamp(0.0, 1.0) }),
                )
                .await;
            }
        });
        Ok((
            Some(ActiveRecording {
                session_id,
                stop,
                forward_task,
                asr_task,
                text_task,
                level_task,
                started_at: Instant::now(),
                language,
                focused_target,
            }),
            Some(auto_stop_rx),
        ))
    }
}

#[async_trait]
impl BackendService for SpeakiputService {
    fn subscribe(&self) -> Option<broadcast::Receiver<Envelope>> {
        Some(self.events.subscribe())
    }

    #[allow(clippy::too_many_lines)]
    async fn handle(&self, request: Envelope) -> Result<ServiceOutput, String> {
        let output = match request.name.as_str() {
            "client.hello" => {
                let hello: ClientHelloRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                if hello
                    .supported_versions
                    .iter()
                    .any(|item| item == PROTOCOL_VERSION)
                {
                    success(
                        &request,
                        self.instance_id,
                        &ClientHelloResponse {
                            selected_version: PROTOCOL_VERSION.into(),
                            server: ClientIdentity {
                                name: "speakiputd".into(),
                                version: env!("CARGO_PKG_VERSION").into(),
                            },
                            capabilities: self.capabilities.clone(),
                            limits: ServerLimits {
                                max_frame_bytes: MAX_FRAME_BYTES,
                            },
                        },
                    )?
                } else {
                    self.error(
                        &request,
                        protocol_error(
                            StableErrorCode::ProtocolMismatch,
                            "client does not support protocol 1.0",
                            false,
                        ),
                    )
                }
            }
            "state.get" => {
                let stored = self.settings.get().map_err(|error| error.to_string())?;
                let state = self.state.lock().await;
                success(
                    &request,
                    self.instance_id,
                    &StateSnapshot {
                        state: state.machine.state(),
                        active_session_id: state.machine.active_session_id(),
                        settings_revision: stored.revision,
                        capabilities: self.capabilities.clone(),
                        health: BackendHealth::Ready,
                    },
                )?
            }
            "settings.get" => {
                let stored = self.settings.get().map_err(|error| error.to_string())?;
                success(
                    &request,
                    self.instance_id,
                    &SettingsResponse {
                        revision: stored.revision,
                        settings: stored.settings,
                    },
                )?
            }
            "settings.replace" => {
                let replace: SettingsReplaceRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                match self
                    .settings
                    .replace(replace.expected_revision, replace.settings)
                {
                    Ok(stored) => {
                        self.event("settings.changed", json!({ "revision": stored.revision }))
                            .await;
                        success(
                            &request,
                            self.instance_id,
                            &SettingsResponse {
                                revision: stored.revision,
                                settings: stored.settings,
                            },
                        )?
                    }
                    Err(StorageError::Conflict { expected, current }) => self.error(
                        &request,
                        ProtocolError {
                            code: StableErrorCode::Conflict,
                            message: "settings revision is stale".into(),
                            retryable: true,
                            details: serde_json::Map::from_iter([
                                ("expected".into(), json!(expected)),
                                ("current".into(), json!(current)),
                            ]),
                        },
                    ),
                    Err(error) => self.error(&request, storage_protocol_error(&error)),
                }
            }
            "credentials.put" => {
                let put: CredentialPutRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                let Some(credentials) = self.credentials.clone() else {
                    return Ok(self.error(
                        &request,
                        protocol_error(
                            StableErrorCode::Unsupported,
                            "secure credential storage is unavailable",
                            false,
                        ),
                    ));
                };
                let credential_id = put.credential_id.clone();
                let stored_id = credential_id.clone();
                let secret = put.secret;
                match tokio::task::spawn_blocking(move || credentials.put(&credential_id, &secret))
                    .await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        return Ok(self.error(&request, storage_protocol_error(&error)));
                    }
                    Err(error) => {
                        return Ok(self.error(
                            &request,
                            protocol_error(StableErrorCode::Internal, error.to_string(), true),
                        ));
                    }
                }
                success(
                    &request,
                    self.instance_id,
                    &CredentialStatusResponse {
                        credential_id: stored_id,
                        stored: true,
                    },
                )?
            }
            "credentials.status" => {
                let lookup: CredentialIdRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                let Some(credentials) = self.credentials.clone() else {
                    return Ok(self.error(
                        &request,
                        protocol_error(
                            StableErrorCode::Unsupported,
                            "secure credential storage is unavailable",
                            false,
                        ),
                    ));
                };
                let credential_id = lookup.credential_id.clone();
                let stored = match tokio::task::spawn_blocking(move || {
                    credentials.get(&credential_id)
                })
                .await
                {
                    Ok(Ok(secret)) => secret.is_some(),
                    Ok(Err(error)) => {
                        return Ok(self.error(&request, storage_protocol_error(&error)));
                    }
                    Err(error) => {
                        return Ok(self.error(
                            &request,
                            protocol_error(StableErrorCode::Internal, error.to_string(), true),
                        ));
                    }
                };
                success(
                    &request,
                    self.instance_id,
                    &CredentialStatusResponse {
                        credential_id: lookup.credential_id,
                        stored,
                    },
                )?
            }
            "credentials.delete" => {
                let lookup: CredentialIdRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                let Some(credentials) = self.credentials.clone() else {
                    return Ok(self.error(
                        &request,
                        protocol_error(
                            StableErrorCode::Unsupported,
                            "secure credential storage is unavailable",
                            false,
                        ),
                    ));
                };
                let credential_id = lookup.credential_id.clone();
                match tokio::task::spawn_blocking(move || credentials.delete(&credential_id)).await
                {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        return Ok(self.error(&request, storage_protocol_error(&error)));
                    }
                    Err(error) => {
                        return Ok(self.error(
                            &request,
                            protocol_error(StableErrorCode::Internal, error.to_string(), true),
                        ));
                    }
                }
                success(
                    &request,
                    self.instance_id,
                    &CredentialStatusResponse {
                        credential_id: lookup.credential_id,
                        stored: false,
                    },
                )?
            }
            "audio_devices.list" => {
                let selected_id = self
                    .settings
                    .get()
                    .map_err(|error| error.to_string())?
                    .settings
                    .audio
                    .input_device_id;
                let devices = if let Some(runtime) = &self.runtime {
                    runtime.audio.devices().await.map_or_else(
                        |_| self.audio_devices.clone(),
                        |devices| {
                            devices
                                .into_iter()
                                .map(|device| AudioDevice {
                                    id: device.id,
                                    name: device.name,
                                    is_default: device.is_default,
                                })
                                .collect()
                        },
                    )
                } else {
                    self.audio_devices.clone()
                };
                success(
                    &request,
                    self.instance_id,
                    &AudioDevicesResponse {
                        devices,
                        selected_id,
                    },
                )?
            }
            "backends.list" => success(
                &request,
                self.instance_id,
                &BackendsResponse {
                    transcription: vec![BackendChoice {
                        id: "local-whisper".into(),
                        name: "Local Whisper".into(),
                        available: self
                            .runtime
                            .as_ref()
                            .is_some_and(|runtime| runtime.asr.available()),
                        unavailable_reason: (!self
                            .runtime
                            .as_ref()
                            .is_some_and(|runtime| runtime.asr.available()))
                        .then(|| "local Whisper feature is not enabled".into()),
                    }],
                    post_processing: vec![BackendChoice {
                        id: "openai-compatible".into(),
                        name: "OpenAI compatible".into(),
                        available: true,
                        unavailable_reason: None,
                    }],
                },
            )?,
            "history.list" => {
                let list: HistoryListRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                let (entries, next_cursor) = self
                    .history
                    .list(list.limit.clamp(1, 500) as usize, list.cursor.as_deref())
                    .map_err(|error| error.to_string())?;
                success(
                    &request,
                    self.instance_id,
                    &HistoryListResponse {
                        entries,
                        next_cursor,
                    },
                )?
            }
            "history.clear" => {
                self.history.clear().map_err(|error| error.to_string())?;
                self.event("history.cleared", json!({})).await;
                success(&request, self.instance_id, &json!({ "cleared": true }))?
            }
            "diagnostics.get" => {
                let mut checks = vec![speakiput_contract::DiagnosticCheck {
                    id: "backend".into(),
                    status: "ok".into(),
                    message: "Backend is ready".into(),
                }];
                if let Some(runtime) = &self.runtime {
                    checks.push(match runtime.audio.devices().await {
                        Ok(devices) if !devices.is_empty() => speakiput_contract::DiagnosticCheck {
                            id: "microphone".into(),
                            status: "ok".into(),
                            message: format!("{} input device(s) available", devices.len()),
                        },
                        Ok(_) => speakiput_contract::DiagnosticCheck {
                            id: "microphone".into(),
                            status: "error".into(),
                            message: "No input device is available".into(),
                        },
                        Err(error) => speakiput_contract::DiagnosticCheck {
                            id: "microphone".into(),
                            status: "error".into(),
                            message: error.to_string(),
                        },
                    });
                    checks.push(speakiput_contract::DiagnosticCheck {
                        id: "transcription_model".into(),
                        status: if runtime.asr.available() {
                            "ok"
                        } else {
                            "error"
                        }
                        .into(),
                        message: if runtime.asr.available() {
                            "Local Whisper model is loaded".into()
                        } else {
                            "Local Whisper model is unavailable; select a valid model file".into()
                        },
                    });
                } else {
                    checks.push(speakiput_contract::DiagnosticCheck {
                        id: "native_runtime".into(),
                        status: "unavailable".into(),
                        message: "Daemon was built without native capture/Whisper support".into(),
                    });
                }
                for (id, capability) in [
                    ("keyboard_insertion", "keyboard_insertion"),
                    ("clipboard", "clipboard"),
                    ("global_shortcut", "global_shortcut"),
                    ("focus_safe_overlay", "focus_safe_overlay"),
                    ("vulkan_acceleration", "vulkan_acceleration"),
                    ("credential_store", "credential_store"),
                ] {
                    let available = self.capabilities.iter().any(|item| item == capability);
                    checks.push(speakiput_contract::DiagnosticCheck {
                        id: id.into(),
                        status: if available { "ok" } else { "unavailable" }.into(),
                        message: if available {
                            "Available".into()
                        } else {
                            "Unavailable in the current desktop session".into()
                        },
                    });
                }
                success(
                    &request,
                    self.instance_id,
                    &DiagnosticsResponse {
                        checks,
                        log_path: None,
                    },
                )?
            }
            "recording.start" => {
                if self.runtime.is_none() {
                    return Ok(self.error(
                        &request,
                        protocol_error(
                            StableErrorCode::Unavailable,
                            "native recording runtime is unavailable",
                            false,
                        ),
                    ));
                }
                let start: RecordingStartRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                let settings = self
                    .settings
                    .get()
                    .map_err(|error| error.to_string())?
                    .settings;
                let session_id = Uuid::new_v4();
                let (mut recording, auto_stop_rx) = match self
                    .prepare_recording(session_id, &settings, start.language)
                    .await
                {
                    Ok(recording) => recording,
                    Err(error) => {
                        return Ok(self.error(
                            &request,
                            protocol_error(StableErrorCode::Unavailable, error, true),
                        ));
                    }
                };
                let focus_restore_target = recording
                    .as_ref()
                    .and_then(|recording| recording.focused_target.clone());
                let transition = {
                    let mut state = self.state.lock().await;
                    let transition = state.machine.transition(Action::Start { session_id });
                    if transition.is_ok() {
                        state.active_recording = recording.take();
                    }
                    transition
                };
                match transition {
                    Ok(transition) => {
                        self.transition_event(transition).await;
                        if let (Some(target), Some(runtime)) =
                            (focus_restore_target, self.runtime.clone())
                        {
                            // Showing a regular Wayland/X11 overlay may briefly
                            // activate it on compositors without no-focus window
                            // support. Restore the target after the GUI has had
                            // time to project the recording event.
                            tokio::spawn(async move {
                                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                                let _ = runtime.focus.refocus(&target).await;
                            });
                        }
                        if let (Some(auto_stop_rx), Some(runtime)) =
                            (auto_stop_rx, self.runtime.clone())
                        {
                            let state = Arc::clone(&self.state);
                            let events = self.events.clone();
                            let history = Arc::clone(&self.history);
                            let settings = settings.clone();
                            let instance_id = self.instance_id;
                            tokio::spawn(async move {
                                watch_auto_stop(
                                    auto_stop_rx,
                                    instance_id,
                                    state,
                                    events,
                                    history,
                                    runtime,
                                    settings,
                                    session_id,
                                )
                                .await;
                            });
                        }
                        success(
                            &request,
                            self.instance_id,
                            &RecordingStartResponse {
                                session_id,
                                state: DictationState::Recording,
                            },
                        )?
                    }
                    Err(error) => {
                        // A concurrent start may have won while capture was being prepared.
                        // Any unclaimed recording is stopped by dropping its senders/tasks.
                        if let Some(recording) = recording {
                            stop_recording(recording).await;
                        }
                        self.error(
                            &request,
                            protocol_error(StableErrorCode::InvalidState, error.to_string(), false),
                        )
                    }
                }
            }
            "recording.stop" => {
                let stop: RecordingStopRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                let (transition, active) = {
                    let mut state = self.state.lock().await;
                    let transition = state.machine.transition(Action::Stop {
                        session_id: stop.session_id,
                    });
                    let active = transition
                        .as_ref()
                        .ok()
                        .and_then(|_| state.active_recording.take());
                    (transition, active)
                };
                match transition {
                    Ok(transition) => {
                        self.transition_event(transition).await;
                        if let (Some(active), Some(runtime)) = (active, self.runtime.clone()) {
                            let state = Arc::clone(&self.state);
                            let events = self.events.clone();
                            let history = Arc::clone(&self.history);
                            let settings = self
                                .settings
                                .get()
                                .map_err(|error| error.to_string())?
                                .settings;
                            let instance_id = self.instance_id;
                            tokio::spawn(async move {
                                run_recording_pipeline(
                                    instance_id,
                                    state,
                                    events,
                                    history,
                                    runtime,
                                    settings,
                                    active,
                                )
                                .await;
                            });
                        }
                        success(
                            &request,
                            self.instance_id,
                            &RecordingStopResponse {
                                session_id: stop.session_id,
                                accepted: true,
                            },
                        )?
                    }
                    Err(error) => self.error(
                        &request,
                        protocol_error(StableErrorCode::InvalidState, error.to_string(), false),
                    ),
                }
            }
            "operation.cancel" => {
                let cancel: OperationCancelRequest = match self.parse(&request) {
                    Ok(value) => value,
                    Err(output) => return Ok(output),
                };
                let (transition, active) = {
                    let mut state = self.state.lock().await;
                    let session_id = cancel
                        .session_id
                        .or_else(|| state.machine.active_session_id());
                    let transition = session_id
                        .ok_or_else(|| "there is no active operation".to_owned())
                        .and_then(|session_id| {
                            state
                                .machine
                                .transition(Action::Cancel { session_id })
                                .map_err(|error| error.to_string())
                        });
                    let active = transition
                        .as_ref()
                        .ok()
                        .and_then(|_| state.active_recording.take());
                    (transition, active)
                };
                match transition {
                    Ok(transition) => {
                        let session_id = transition.session_id;
                        if let Some(active) = active {
                            stop_recording(active).await;
                        }
                        self.transition_event(transition).await;
                        if let Some(session_id) = session_id {
                            self.event(
                                "session.failed",
                                json!({
                                    "session_id": session_id,
                                    "code": "cancelled",
                                    "message": "Recording was cancelled",
                                    "retryable": false,
                                }),
                            )
                            .await;
                        }
                        success(&request, self.instance_id, &json!({ "accepted": true }))?
                    }
                    Err(error) => self.error(
                        &request,
                        protocol_error(StableErrorCode::InvalidState, error, false),
                    ),
                }
            }
            _ => self.error(
                &request,
                protocol_error(
                    StableErrorCode::Unsupported,
                    format!("unsupported request {}", request.name),
                    false,
                ),
            ),
        };
        Ok(output)
    }
}

fn success(
    request: &Envelope,
    instance_id: Uuid,
    payload: &impl serde::Serialize,
) -> Result<ServiceOutput, String> {
    let payload = serde_json::to_value(payload).map_err(|error| error.to_string())?;
    Ok(ServiceOutput::response(Envelope::response(
        request,
        instance_id,
        payload,
    )))
}

fn protocol_error(
    code: StableErrorCode,
    message: impl Into<String>,
    retryable: bool,
) -> ProtocolError {
    ProtocolError {
        code,
        message: message.into(),
        retryable,
        details: serde_json::Map::new(),
    }
}

fn storage_protocol_error(error: &StorageError) -> ProtocolError {
    match error {
        StorageError::Validation { field, message }
        | StorageError::CredentialValidation { field, message } => ProtocolError {
            code: StableErrorCode::InvalidArgument,
            message: error.to_string(),
            retryable: false,
            details: serde_json::Map::from_iter([
                ("field".into(), json!(field)),
                ("message".into(), json!(message)),
            ]),
        },
        StorageError::Conflict { expected, current } => ProtocolError {
            code: StableErrorCode::Conflict,
            message: error.to_string(),
            retryable: true,
            details: serde_json::Map::from_iter([
                ("expected".into(), json!(expected)),
                ("current".into(), json!(current)),
            ]),
        },
        StorageError::Credential(_) => {
            protocol_error(StableErrorCode::Unavailable, error.to_string(), true)
        }
        StorageError::Io(_) | StorageError::Json(_) | StorageError::Poisoned => {
            protocol_error(StableErrorCode::Internal, error.to_string(), true)
        }
    }
}

async fn emit_event(
    instance_id: Uuid,
    state: &Arc<Mutex<BackendState>>,
    events: &broadcast::Sender<Envelope>,
    name: &str,
    payload: Value,
) -> Envelope {
    let mut state = state.lock().await;
    state.sequence = state.sequence.saturating_add(1);
    let event = Envelope::event(name, instance_id, state.sequence, payload);
    let _ = events.send(event.clone());
    event
}

async fn emit_transition(
    instance_id: Uuid,
    state: &Arc<Mutex<BackendState>>,
    events: &broadcast::Sender<Envelope>,
    transition: Transition,
) {
    emit_event(
        instance_id,
        state,
        events,
        "state.changed",
        json!({
            "previous": transition.previous,
            "current": transition.current,
            "session_id": transition.session_id,
        }),
    )
    .await;
}

async fn apply_transition(
    state: &Arc<Mutex<BackendState>>,
    action: Action,
) -> Result<Transition, String> {
    state
        .lock()
        .await
        .machine
        .transition(action)
        .map_err(|error| error.to_string())
}

async fn stop_recording(active: ActiveRecording) {
    let ActiveRecording {
        stop,
        forward_task,
        asr_task,
        text_task,
        level_task,
        ..
    } = active;
    let _ = stop.stop_and_wait().await;
    forward_task.abort();
    asr_task.abort();
    text_task.abort();
    level_task.abort();
}

#[allow(clippy::too_many_arguments)]
async fn watch_auto_stop(
    auto_stop: tokio::sync::oneshot::Receiver<()>,
    instance_id: Uuid,
    state: Arc<Mutex<BackendState>>,
    events: broadcast::Sender<Envelope>,
    history: Arc<dyn HistoryRepository>,
    runtime: RuntimeComponents,
    settings: Settings,
    session_id: Uuid,
) {
    if auto_stop.await.is_err() {
        return;
    }
    let (transition, active) = {
        let mut backend = state.lock().await;
        if backend.machine.state() != DictationState::Recording
            || backend.machine.active_session_id() != Some(session_id)
        {
            return;
        }
        let transition = backend.machine.transition(Action::Stop { session_id });
        let active = transition
            .as_ref()
            .ok()
            .and_then(|_| backend.active_recording.take());
        (transition, active)
    };
    let (Ok(transition), Some(active)) = (transition, active) else {
        return;
    };
    emit_transition(instance_id, &state, &events, transition).await;
    run_recording_pipeline(
        instance_id,
        state,
        events,
        history,
        runtime,
        settings,
        active,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn run_recording_pipeline(
    instance_id: Uuid,
    state: Arc<Mutex<BackendState>>,
    events: broadcast::Sender<Envelope>,
    history: Arc<dyn HistoryRepository>,
    runtime: RuntimeComponents,
    settings: Settings,
    active: ActiveRecording,
) {
    let session_id = active.session_id;
    if let Err(error) = run_recording_pipeline_inner(
        instance_id,
        &state,
        &events,
        &history,
        &runtime,
        &settings,
        active,
    )
    .await
    {
        fail_session(instance_id, &state, &events, session_id, &error).await;
    }
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)]
async fn run_recording_pipeline_inner(
    instance_id: Uuid,
    state: &Arc<Mutex<BackendState>>,
    events: &broadcast::Sender<Envelope>,
    history: &Arc<dyn HistoryRepository>,
    runtime: &RuntimeComponents,
    settings: &Settings,
    active: ActiveRecording,
) -> Result<(), String> {
    let ActiveRecording {
        session_id,
        stop,
        forward_task,
        asr_task,
        text_task,
        level_task,
        started_at,
        language,
        focused_target,
    } = active;
    stop.stop_and_wait()
        .await
        .map_err(|error| error.to_string())?;
    forward_task.await.map_err(|error| error.to_string())?;
    asr_task
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let raw_text = text_task.await.map_err(|error| error.to_string())?;
    level_task.abort();

    let cleaned_text = if settings.transcription.remove_filler_words {
        speakiput_asr::remove_filler_words(&raw_text, &settings.transcription.filler_words)
    } else {
        raw_text.clone()
    };
    let wants_post_processing = settings.post_processing.enabled
        && runtime.post_processor.is_some()
        && !cleaned_text.trim().is_empty();
    let wants_output = settings.output.mode != OutputMode::None && !cleaned_text.trim().is_empty();
    let transition = apply_transition(
        state,
        Action::TranscriptionComplete {
            session_id,
            post_process: wants_post_processing,
            inject: wants_output,
        },
    )
    .await?;
    let current = transition.current;
    emit_transition(instance_id, state, events, transition).await;

    let mut processed_text = cleaned_text;
    let mut post_processed = false;
    if current == DictationState::PostProcessing
        && let Some(processor) = &runtime.post_processor
        && let Ok(Ok(processed)) = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            processor.process(&processed_text, &settings.post_processing.instruction),
        )
        .await
        && !processed.trim().is_empty()
    {
        processed_text = processed;
        post_processed = true;
    }

    let is_terminal = focused_target
        .as_ref()
        .is_some_and(|target| target.is_terminal);
    let (output_text, safety_refused) = if post_processed {
        match prepare_llm_insertion(&processed_text, is_terminal) {
            LlmInsertion::Insert(text) => (text, false),
            LlmInsertion::Empty => (processed_text.clone(), false),
            LlmInsertion::RefusedMultilineTerminal(text) => (text, true),
        }
    } else {
        (processed_text.clone(), false)
    };
    let should_inject = wants_output && !safety_refused && !output_text.is_empty();

    if current == DictationState::PostProcessing {
        let transition = apply_transition(
            state,
            Action::PostProcessingComplete {
                session_id,
                inject: should_inject,
            },
        )
        .await?;
        emit_transition(instance_id, state, events, transition).await;
    }

    let mut insertion = if safety_refused {
        InsertionOutcome {
            status: InsertionStatus::Skipped,
            method: "terminal_multiline_safety".into(),
        }
    } else if !should_inject {
        InsertionOutcome {
            status: InsertionStatus::Skipped,
            method: "none".into(),
        }
    } else {
        if let Some(target) = &focused_target
            && let Err(error) = runtime.focus.refocus(target).await
        {
            tracing::warn!(%session_id, %error, "failed to restore focus before text insertion");
        }
        let method = match settings.output.mode {
            OutputMode::Keyboard => InsertionMethod::Keyboard,
            OutputMode::Clipboard => InsertionMethod::ClipboardOnly,
            OutputMode::None => unreachable!("should_inject excludes output mode none"),
        };
        match runtime
            .output
            .insert(&output_text, method, settings.output.key_delay_ms)
            .await
        {
            Ok(result) => {
                tracing::info!(
                    %session_id,
                    method = insertion_method_name(result.method),
                    "text insertion completed"
                );
                InsertionOutcome {
                    status: match result.method {
                        InsertionMethod::ClipboardOnly => InsertionStatus::Clipboard,
                        InsertionMethod::Keyboard | InsertionMethod::ClipboardPaste => {
                            InsertionStatus::Inserted
                        }
                    },
                    method: insertion_method_name(result.method).into(),
                }
            }
            Err(error) => {
                tracing::warn!(%session_id, %error, "text insertion failed");
                InsertionOutcome {
                    status: InsertionStatus::Failed,
                    method: error.to_string(),
                }
            }
        }
    };

    if should_inject {
        let transition = apply_transition(state, Action::InjectionComplete { session_id }).await?;
        emit_transition(instance_id, state, events, transition).await;
    }

    let duration_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    if settings.general.history_enabled
        && !output_text.is_empty()
        && history
            .append(&HistoryEntry {
                session_id,
                raw_text: raw_text.clone(),
                processed_text: processed_text.clone(),
                output_text: output_text.clone(),
                created_at: Utc::now().to_rfc3339(),
            })
            .is_ok()
    {
        emit_event(
            instance_id,
            state,
            events,
            "history.added",
            json!({ "session_id": session_id }),
        )
        .await;
    }
    if safety_refused {
        insertion.status = InsertionStatus::Skipped;
    }
    let final_event = TranscriptFinalEvent {
        session_id,
        raw_text,
        processed_text,
        output_text,
        post_processed,
        insertion,
        transcription_backend: settings.transcription.backend_id.clone(),
        language,
        duration_ms,
    };
    emit_event(
        instance_id,
        state,
        events,
        "transcript.final",
        serde_json::to_value(final_event).map_err(|error| error.to_string())?,
    )
    .await;
    Ok(())
}

async fn fail_session(
    instance_id: Uuid,
    state: &Arc<Mutex<BackendState>>,
    events: &broadcast::Sender<Envelope>,
    session_id: Uuid,
    message: &str,
) {
    if let Ok(transition) = apply_transition(state, Action::Fail { session_id }).await {
        emit_transition(instance_id, state, events, transition).await;
    }
    emit_event(
        instance_id,
        state,
        events,
        "session.failed",
        json!({
            "session_id": session_id,
            "code": "pipeline_failed",
            "message": message,
            "retryable": true,
        }),
    )
    .await;
    if let Ok(transition) = apply_transition(state, Action::Recover).await {
        emit_transition(instance_id, state, events, transition).await;
    }
}

const fn insertion_method_name(method: InsertionMethod) -> &'static str {
    match method {
        InsertionMethod::Keyboard => "keyboard",
        InsertionMethod::ClipboardPaste => "clipboard_paste",
        InsertionMethod::ClipboardOnly => "clipboard",
    }
}
