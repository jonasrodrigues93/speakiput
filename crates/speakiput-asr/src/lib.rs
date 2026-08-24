//! Portable ASR interfaces and local Whisper implementation.

mod filler;
mod prompt_echo;
mod whisper;

use async_trait::async_trait;
pub use filler::remove_filler_words;
pub use prompt_echo::is_prompt_echo;
use thiserror::Error;
use tokio::sync::mpsc;
pub use whisper::{LocalWhisperBackend, SegmentationMode};

pub type AudioChunk = Vec<i16>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionConfig {
    pub language: String,
    pub model_id: String,
    pub prompt: Option<String>,
}

#[derive(Debug, Error)]
pub enum AsrError {
    #[error("ASR backend is unavailable: {0}")]
    Unavailable(String),
    #[error("ASR inference failed: {0}")]
    Inference(String),
    #[error("ASR worker failed: {0}")]
    Worker(String),
    #[error("audio encoding failed: {0}")]
    Audio(#[from] speakiput_audio::WavError),
}

#[async_trait]
pub trait TranscriptionBackend: Send + Sync {
    async fn transcribe_pcm(
        &self,
        audio: &[i16],
        config: &TranscriptionConfig,
    ) -> Result<String, AsrError>;

    async fn transcribe_stream(
        &self,
        mut audio_rx: mpsc::Receiver<AudioChunk>,
        text_tx: mpsc::Sender<String>,
        config: &TranscriptionConfig,
    ) -> Result<(), AsrError> {
        let mut samples = Vec::new();
        while let Some(chunk) = audio_rx.recv().await {
            samples.extend(chunk);
        }
        if samples.is_empty() {
            return Ok(());
        }
        let text = self.transcribe_pcm(&samples, config).await?;
        if !text.trim().is_empty() {
            let _ = text_tx.send(text).await;
        }
        Ok(())
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn available(&self) -> bool;
}
