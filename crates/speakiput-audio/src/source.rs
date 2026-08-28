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
    use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};

    use super::{
        Arc, AudioCaptureError, AudioChunk, AudioDeviceInfo, AudioSource, CaptureController,
        CaptureSession, async_trait, compressed_audio_level, mpsc,
    };
    use crate::SAMPLE_RATE;

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
        let supported = device
            .default_input_config()
            .map_err(|error| AudioCaptureError::Capture(error.to_string()))?;
        let config: StreamConfig = supported.config();
        let sample_rate = config.sample_rate.0;
        let channels = config.channels;
        let stream = match supported.sample_format() {
            SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, channels, sample_rate, audio_tx, level_tx)
            }
            SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, channels, sample_rate, audio_tx, level_tx)
            }
            SampleFormat::U16 => {
                build_stream::<u16>(&device, &config, channels, sample_rate, audio_tx, level_tx)
            }
            format => Err(AudioCaptureError::Capture(format!(
                "unsupported input sample format: {format}"
            ))),
        }?;
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

    fn build_stream<T>(
        device: &cpal::Device,
        config: &StreamConfig,
        channels: u16,
        sample_rate: u32,
        audio_tx: mpsc::UnboundedSender<AudioChunk>,
        level_tx: mpsc::UnboundedSender<f32>,
    ) -> Result<cpal::Stream, AudioCaptureError>
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        device
            .build_input_stream(
                config,
                move |data: &[T], _| {
                    let samples = convert_samples(data, channels, sample_rate);
                    let _ = level_tx.send(compressed_audio_level(&samples));
                    let _ = audio_tx.send(samples);
                },
                |_error| {},
                None,
            )
            .map_err(|error| AudioCaptureError::Capture(error.to_string()))
    }

    fn convert_samples<T>(data: &[T], channels: u16, sample_rate: u32) -> AudioChunk
    where
        T: Sample,
        f32: FromSample<T>,
    {
        let channels = usize::from(channels.max(1));
        let mono = data
            .chunks_exact(channels)
            .map(|frame| {
                let sum = frame
                    .iter()
                    .map(|sample| f32::from_sample(*sample))
                    .sum::<f32>();
                sum / channels as f32
            })
            .collect::<Vec<_>>();
        if sample_rate == SAMPLE_RATE {
            return mono.into_iter().map(sample_to_i16).collect();
        }
        if mono.is_empty() {
            return Vec::new();
        }
        let output_len = mono.len().saturating_mul(SAMPLE_RATE as usize) / sample_rate as usize;
        (0..output_len)
            .map(|index| {
                let source_index =
                    index.saturating_mul(sample_rate as usize) / SAMPLE_RATE as usize;
                sample_to_i16(mono[source_index.min(mono.len() - 1)])
            })
            .collect()
    }

    fn sample_to_i16(sample: f32) -> i16 {
        (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn converts_stereo_48khz_input_to_mono_16khz() {
            let input = (0..480_u32)
                .flat_map(|index| {
                    let sample = if index % 2 == 0 { 0.5 } else { -0.5 };
                    [sample, sample]
                })
                .collect::<Vec<_>>();
            let output = convert_samples(&input, 2, 48_000);
            assert_eq!(output.len(), 160);
            assert_eq!(output[0], 16_383);
        }

        #[test]
        fn converts_unsigned_input_around_zero() {
            let output = convert_samples(&[u16::MIN, u16::MAX / 2, u16::MAX], 1, SAMPLE_RATE);
            assert_eq!(output, vec![-32_767, 0, 32_766]);
        }
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
