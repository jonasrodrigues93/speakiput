// Adapted from whisrs src/transcription/local_whisper.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

use async_trait::async_trait;
#[cfg(feature = "local-whisper")]
use tokio::sync::mpsc;

use crate::{AsrError, TranscriptionBackend, TranscriptionConfig};
#[cfg(feature = "local-whisper")]
use crate::{AudioChunk, is_prompt_echo};

#[cfg(feature = "local-whisper")]
const SAMPLE_RATE: usize = 16_000;
#[cfg(feature = "local-whisper")]
const SILENCE_THRESHOLD: f64 = 0.003;
#[cfg(feature = "local-whisper")]
const MIN_DECODE_SAMPLES: usize = SAMPLE_RATE + SAMPLE_RATE / 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SegmentationMode {
    #[default]
    Silence,
}

impl SegmentationMode {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let _ = value;
        Self::Silence
    }
}

#[cfg(feature = "local-whisper")]
mod implementation {
    use std::{path::Path, sync::Arc};

    use speakiput_audio::{PhraseSplitter, SILENCE_RMS_THRESHOLD, audio_gate_reason};

    use super::{
        AsrError, AudioChunk, MIN_DECODE_SAMPLES, SAMPLE_RATE, SILENCE_THRESHOLD,
        TranscriptionBackend, TranscriptionConfig, async_trait, is_prompt_echo, mpsc,
    };

    pub struct LocalWhisperBackend {
        context: Option<Arc<whisper_rs::WhisperContext>>,
        model_path: String,
        phrase_silence_ms: u64,
        load_error: Option<String>,
    }

    impl LocalWhisperBackend {
        #[must_use]
        pub fn new(model_path: impl Into<String>) -> Self {
            let model_path = model_path.into();
            let loaded = Self::load_model(&model_path);
            let (context, load_error) = match loaded {
                Ok(context) => (Some(Arc::new(context)), None),
                Err(error) => (None, Some(error)),
            };
            Self {
                context,
                model_path,
                phrase_silence_ms: 700,
                load_error,
            }
        }

        #[must_use]
        pub fn with_segmentation(mut self, _mode: &str, phrase_silence_ms: u64) -> Self {
            self.phrase_silence_ms = phrase_silence_ms.max(1);
            self
        }

        fn load_model(path: &str) -> Result<whisper_rs::WhisperContext, String> {
            if !Path::new(path).is_file() {
                return Err(format!("model file not found: {path}"));
            }
            whisper_rs::WhisperContext::new_with_params(
                path,
                whisper_rs::WhisperContextParameters::default(),
            )
            .map_err(|error| error.to_string())
        }

        fn context(&self) -> Result<Arc<whisper_rs::WhisperContext>, AsrError> {
            self.context.clone().ok_or_else(|| {
                AsrError::Unavailable(
                    self.load_error.clone().unwrap_or_else(|| {
                        format!("model was not loaded from {}", self.model_path)
                    }),
                )
            })
        }
    }

    #[async_trait]
    impl TranscriptionBackend for LocalWhisperBackend {
        async fn transcribe_pcm(
            &self,
            audio: &[i16],
            config: &TranscriptionConfig,
        ) -> Result<String, AsrError> {
            let context = self.context()?;
            let samples = i16_to_f32(audio);
            let config = config.clone();
            tokio::task::spawn_blocking(move || {
                let mut state = create_state(&context)?;
                let text = run_inference(&mut state, &samples, &config)?;
                Ok(filter_prompt_echo(text, config.prompt.as_deref()))
            })
            .await
            .map_err(|error| AsrError::Worker(error.to_string()))?
        }

        async fn transcribe_stream(
            &self,
            mut audio_rx: mpsc::Receiver<AudioChunk>,
            text_tx: mpsc::Sender<String>,
            config: &TranscriptionConfig,
        ) -> Result<(), AsrError> {
            let context = self.context()?;
            let mut splitter =
                PhraseSplitter::new(SAMPLE_RATE, SILENCE_THRESHOLD, self.phrase_silence_ms);
            let mut state = create_state(&context)?;
            while let Some(chunk) = audio_rx.recv().await {
                for phrase in splitter.feed(&chunk) {
                    state = decode_phrase(state, phrase, config, &text_tx).await?;
                }
            }
            if let Some(phrase) = splitter.flush() {
                let _ = decode_phrase(state, phrase, config, &text_tx).await?;
            }
            Ok(())
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn available(&self) -> bool {
            self.context.is_some()
        }
    }

    async fn decode_phrase(
        mut state: whisper_rs::WhisperState,
        mut phrase: Vec<i16>,
        config: &TranscriptionConfig,
        text_tx: &mpsc::Sender<String>,
    ) -> Result<whisper_rs::WhisperState, AsrError> {
        // Reject accidental taps and silent buffers before padding. Whisper
        // can otherwise turn a tiny/silent phrase into prompt-shaped text.
        if audio_gate_reason(&phrase, 16_000, 300, SILENCE_RMS_THRESHOLD).is_some() {
            return Ok(state);
        }
        phrase.resize(phrase.len().max(MIN_DECODE_SAMPLES), 0);
        let samples = i16_to_f32(&phrase);
        let config = config.clone();
        let (state, result) = tokio::task::spawn_blocking(move || {
            let result = run_inference(&mut state, &samples, &config)
                .map(|text| filter_prompt_echo(text, config.prompt.as_deref()));
            (state, result)
        })
        .await
        .map_err(|error| AsrError::Worker(error.to_string()))?;
        let text = result?;
        if !text.trim().is_empty() {
            let _ = text_tx.send(text).await;
        }
        Ok(state)
    }

    fn create_state(
        context: &whisper_rs::WhisperContext,
    ) -> Result<whisper_rs::WhisperState, AsrError> {
        context
            .create_state()
            .map_err(|error| AsrError::Inference(error.to_string()))
    }

    fn run_inference(
        state: &mut whisper_rs::WhisperState,
        audio: &[f32],
        config: &TranscriptionConfig,
    ) -> Result<String, AsrError> {
        use whisper_rs::{FullParams, SamplingStrategy};

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        if config.language != "auto" {
            params.set_language(Some(&config.language));
        }
        if let Some(prompt) = config.prompt.as_deref().filter(|prompt| !prompt.is_empty()) {
            params.set_initial_prompt(prompt);
        }
        params.set_no_context(true);
        params.set_entropy_thold(2.6);
        params.set_suppress_nst(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        let threads = std::thread::available_parallelism()
            .map_or(4, std::num::NonZero::get)
            .min(8);
        params.set_n_threads(i32::try_from(threads).unwrap_or(4));
        state
            .full(params, audio)
            .map_err(|error| AsrError::Inference(error.to_string()))?;
        let mut text = String::new();
        for segment in state.as_iter() {
            text.push_str(&segment.to_string());
        }
        Ok(text.trim().to_owned())
    }

    fn i16_to_f32(samples: &[i16]) -> Vec<f32> {
        samples
            .iter()
            .map(|&sample| f32::from(sample) / f32::from(i16::MAX))
            .collect()
    }

    fn filter_prompt_echo(text: String, prompt: Option<&str>) -> String {
        if prompt.is_some_and(|prompt| is_prompt_echo(&text, prompt)) {
            String::new()
        } else {
            text
        }
    }
}

#[cfg(not(feature = "local-whisper"))]
mod implementation {
    use super::{AsrError, TranscriptionBackend, TranscriptionConfig, async_trait};

    pub struct LocalWhisperBackend {
        model_path: String,
    }

    impl LocalWhisperBackend {
        #[must_use]
        pub fn new(model_path: impl Into<String>) -> Self {
            Self {
                model_path: model_path.into(),
            }
        }

        #[must_use]
        pub const fn with_segmentation(self, _mode: &str, _phrase_silence_ms: u64) -> Self {
            self
        }
    }

    #[async_trait]
    impl TranscriptionBackend for LocalWhisperBackend {
        async fn transcribe_pcm(
            &self,
            _audio: &[i16],
            _config: &TranscriptionConfig,
        ) -> Result<String, AsrError> {
            Err(AsrError::Unavailable(format!(
                "local-whisper feature is disabled; model {} was not loaded",
                self.model_path
            )))
        }

        fn available(&self) -> bool {
            false
        }
    }
}

pub use implementation::LocalWhisperBackend;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segmentation_falls_back_to_silence() {
        assert_eq!(
            SegmentationMode::parse("silence"),
            SegmentationMode::Silence
        );
        assert_eq!(
            SegmentationMode::parse("unknown"),
            SegmentationMode::Silence
        );
    }

    #[tokio::test]
    async fn missing_or_disabled_model_is_unavailable() {
        let backend = LocalWhisperBackend::new("/definitely/missing/model.bin");
        let error = backend
            .transcribe_pcm(
                &[1, 2, 3],
                &TranscriptionConfig {
                    language: "pt".into(),
                    model_id: "test".into(),
                    prompt: None,
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AsrError::Unavailable(_)));
        assert!(!backend.available());
    }
}
