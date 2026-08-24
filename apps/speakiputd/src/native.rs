use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use speakiput_asr::{
    AsrError, AudioChunk, LocalWhisperBackend, TranscriptionBackend, TranscriptionConfig,
};
use speakiput_llm::{LlmError, OpenAiCompatibleProvider, PostProcessor, ProviderConfig};
use speakiput_storage::{CredentialRepository, SettingsRepository};
use tokio::sync::mpsc;

/// Resolves the configured model for every session and caches the loaded
/// Whisper context until the model path or segmentation setting changes.
pub struct ConfiguredLocalWhisper {
    settings: Arc<dyn SettingsRepository>,
    cached: Mutex<Option<CachedWhisper>>,
}

struct CachedWhisper {
    key: (String, u64),
    backend: Arc<LocalWhisperBackend>,
}

impl ConfiguredLocalWhisper {
    pub fn new(settings: Arc<dyn SettingsRepository>) -> Self {
        Self {
            settings,
            cached: Mutex::new(None),
        }
    }

    fn configured_backend(&self) -> Result<Arc<LocalWhisperBackend>, AsrError> {
        let settings = self
            .settings
            .get()
            .map_err(|error| AsrError::Unavailable(error.to_string()))?
            .settings;
        let path = settings.transcription.model_path.unwrap_or_default();
        let key = (path.clone(), settings.audio.phrase_silence_ms);
        let mut cached = self
            .cached
            .lock()
            .map_err(|_| AsrError::Worker("Whisper model cache lock is poisoned".into()))?;
        if let Some(cached) = cached.as_ref().filter(|cached| cached.key == key) {
            return Ok(Arc::clone(&cached.backend));
        }
        let backend = Arc::new(
            LocalWhisperBackend::new(path)
                .with_segmentation("silence", settings.audio.phrase_silence_ms),
        );
        *cached = Some(CachedWhisper {
            key,
            backend: Arc::clone(&backend),
        });
        Ok(backend)
    }
}

#[async_trait]
impl TranscriptionBackend for ConfiguredLocalWhisper {
    async fn transcribe_pcm(
        &self,
        audio: &[i16],
        config: &TranscriptionConfig,
    ) -> Result<String, AsrError> {
        self.configured_backend()?
            .transcribe_pcm(audio, config)
            .await
    }

    async fn transcribe_stream(
        &self,
        audio_rx: mpsc::Receiver<AudioChunk>,
        text_tx: mpsc::Sender<String>,
        config: &TranscriptionConfig,
    ) -> Result<(), AsrError> {
        self.configured_backend()?
            .transcribe_stream(audio_rx, text_tx, config)
            .await
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn available(&self) -> bool {
        self.settings
            .get()
            .ok()
            .and_then(|stored| stored.settings.transcription.model_path)
            .is_some_and(|path| Path::new(&path).is_file())
    }
}

/// Reads endpoint/model settings at call time so enabling or editing cleanup
/// does not require restarting the daemon.
pub struct ConfiguredPostProcessor {
    settings: Arc<dyn SettingsRepository>,
    credentials: Arc<dyn CredentialRepository>,
}

impl ConfiguredPostProcessor {
    pub fn new(
        settings: Arc<dyn SettingsRepository>,
        credentials: Arc<dyn CredentialRepository>,
    ) -> Self {
        Self {
            settings,
            credentials,
        }
    }
}

#[async_trait]
impl PostProcessor for ConfiguredPostProcessor {
    async fn process(&self, transcript: &str, instruction: &str) -> Result<String, LlmError> {
        let settings = self
            .settings
            .get()
            .map_err(|error| LlmError::InvalidConfiguration(error.to_string()))?
            .settings;
        let api_key = if let Some(credential_id) = settings.post_processing.credential_id.clone() {
            let credentials = Arc::clone(&self.credentials);
            tokio::task::spawn_blocking(move || credentials.get(&credential_id))
                .await
                .map_err(|error| LlmError::Request(error.to_string()))?
                .map_err(|error| LlmError::InvalidConfiguration(error.to_string()))?
        } else {
            None
        };
        let provider = OpenAiCompatibleProvider::new(ProviderConfig {
            endpoint: settings.post_processing.endpoint,
            model_id: settings.post_processing.model_id,
            api_key,
        })?;
        provider.process(transcript, instruction).await
    }
}
