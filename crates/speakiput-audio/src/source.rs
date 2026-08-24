// Adapted from whisrs src/audio/capture.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::mpsc;

pub type AudioChunk = Vec<i16>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Error)]
pub enum AudioCaptureError {
    #[error("no audio input device is available")]
    NoDevice,
    #[error("audio device {0} was not found")]
    DeviceNotFound(String),
    #[error("audio capture failed: {0}")]
    Capture(String),
    #[error("audio capture worker failed")]
    Worker,
}

#[async_trait]
pub trait CaptureController: Send + Sync {
    async fn stop_and_wait(&self) -> Result<(), AudioCaptureError>;
}

pub struct CaptureSession {
    receiver: mpsc::UnboundedReceiver<AudioChunk>,
    controller: Arc<dyn CaptureController>,
}

impl CaptureSession {
    #[must_use]
    pub fn new(
        receiver: mpsc::UnboundedReceiver<AudioChunk>,
        controller: Arc<dyn CaptureController>,
    ) -> Self {
        Self {
            receiver,
            controller,
        }
    }

    #[must_use]
    pub fn into_parts(self) -> (mpsc::UnboundedReceiver<AudioChunk>, CaptureStopHandle) {
        (
            self.receiver,
            CaptureStopHandle {
                controller: self.controller,
            },
        )
    }
}

#[derive(Clone)]
pub struct CaptureStopHandle {
    controller: Arc<dyn CaptureController>,
}

impl CaptureStopHandle {
    pub async fn stop_and_wait(self) -> Result<(), AudioCaptureError> {
        self.controller.stop_and_wait().await
    }
}

#[async_trait]
pub trait AudioSource: Send + Sync {
    async fn devices(&self) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError>;

    async fn start(
        &self,
        device_id: &str,
        level_tx: mpsc::UnboundedSender<f32>,
    ) -> Result<CaptureSession, AudioCaptureError>;
}

#[must_use]
pub fn compressed_audio_level(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_squares = samples
        .iter()
        .map(|&sample| {
            let normalized = f32::from(sample) / f32::from(i16::MAX);
            normalized * normalized
        })
        .sum::<f32>();
    let count = u16::try_from(samples.len()).map_or(f32::from(u16::MAX), f32::from);
    let rms = (sum_squares / count).sqrt();
    (1.0 - (-rms * 18.0).exp()).clamp(0.0, 1.0)
}

#[cfg(feature = "native-capture")]
mod cpal_impl {
    use std::sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    };

    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::{SampleRate, StreamConfig};

    use super::{
        Arc, AudioCaptureError, AudioChunk, AudioDeviceInfo, AudioSource, CaptureController,
        CaptureSession, async_trait, compressed_audio_level, mpsc,
    };
    use crate::{CHANNELS, SAMPLE_RATE};

    #[derive(Debug, Default)]
    pub struct CpalAudioSource;

    struct CpalController {
        stop: Arc<AtomicBool>,
        thread: Mutex<Option<std::thread::JoinHandle<()>>>,
    }

    #[async_trait]
    impl CaptureController for CpalController {
        async fn stop_and_wait(&self) -> Result<(), AudioCaptureError> {
            self.stop.store(true, Ordering::Release);
            let thread = self
                .thread
                .lock()
                .map_err(|_| AudioCaptureError::Worker)?
                .take();
            if let Some(thread) = thread {
                tokio::task::spawn_blocking(move || thread.join())
                    .await
                    .map_err(|_| AudioCaptureError::Worker)?
                    .map_err(|_| AudioCaptureError::Worker)?;
            }
            Ok(())
        }
    }

    #[async_trait]
    impl AudioSource for CpalAudioSource {
        async fn devices(&self) -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
            tokio::task::spawn_blocking(list_devices)
                .await
                .map_err(|_| AudioCaptureError::Worker)?
        }

        async fn start(
            &self,
            device_id: &str,
            level_tx: mpsc::UnboundedSender<f32>,
        ) -> Result<CaptureSession, AudioCaptureError> {
            let device_id = device_id.to_owned();
            let (audio_tx, audio_rx) = mpsc::unbounded_channel();
            let (init_tx, init_rx) = std::sync::mpsc::channel();
            let stop = Arc::new(AtomicBool::new(false));
            let thread_stop = Arc::clone(&stop);
            let thread = std::thread::Builder::new()
                .name("speakiput-audio".into())
                .spawn(move || {
                    if let Err(error) =
                        run_capture(&device_id, audio_tx, level_tx, &thread_stop, &init_tx)
                    {
                        let _ = init_tx.send(Err(error));
                    }
                })
                .map_err(|error| AudioCaptureError::Capture(error.to_string()))?;
            match init_rx.recv().map_err(|_| AudioCaptureError::Worker)? {
                Ok(()) => Ok(CaptureSession::new(
                    audio_rx,
                    Arc::new(CpalController {
                        stop,
                        thread: Mutex::new(Some(thread)),
                    }),
                )),
                Err(error) => {
                    let _ = thread.join();
                    Err(error)
                }
            }
        }
    }

    fn list_devices() -> Result<Vec<AudioDeviceInfo>, AudioCaptureError> {
        let host = cpal::default_host();
        let default_name = host
            .default_input_device()
            .and_then(|device| device.name().ok());
        let devices = host
            .input_devices()
            .map_err(|error| AudioCaptureError::Capture(error.to_string()))?
            .filter_map(|device| device.name().ok())
            .map(|name| AudioDeviceInfo {
                id: name.clone(),
                is_default: default_name.as_deref() == Some(&name),
                name,
            })
            .collect();
        Ok(devices)
    }

    fn run_capture(
        device_id: &str,
        audio_tx: mpsc::UnboundedSender<AudioChunk>,
        level_tx: mpsc::UnboundedSender<f32>,
        stop: &Arc<AtomicBool>,
        init_tx: &std::sync::mpsc::Sender<Result<(), AudioCaptureError>>,
    ) -> Result<(), AudioCaptureError> {
        let host = cpal::default_host();
        let device = if device_id == "default" {
            host.default_input_device()
                .ok_or(AudioCaptureError::NoDevice)?
        } else {
            host.input_devices()
                .map_err(|error| AudioCaptureError::Capture(error.to_string()))?
                .find(|device| device.name().ok().as_deref() == Some(device_id))
                .ok_or_else(|| AudioCaptureError::DeviceNotFound(device_id.to_owned()))?
        };
        let config = StreamConfig {
            channels: CHANNELS,
            sample_rate: SampleRate(SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        };
        let stream = device
            .build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let _ = level_tx.send(compressed_audio_level(data));
                    let _ = audio_tx.send(data.to_vec());
                },
                |_error| {},
                None,
            )
            .map_err(|error| AudioCaptureError::Capture(error.to_string()))?;
        stream
            .play()
            .map_err(|error| AudioCaptureError::Capture(error.to_string()))?;
        let _ = init_tx.send(Ok(()));
        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        drop(stream);
        Ok(())
    }
}

#[cfg(feature = "native-capture")]
pub use cpal_impl::CpalAudioSource;

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn compressed_level_uses_visualizer_range() {
        assert_eq!(compressed_audio_level(&[]), 0.0);
        assert_eq!(compressed_audio_level(&[0; 100]), 0.0);
        assert!(compressed_audio_level(&[3000; 100]) > 0.7);
        assert!(compressed_audio_level(&[i16::MAX; 100]) <= 1.0);
    }
}
