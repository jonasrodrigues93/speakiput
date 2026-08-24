//! Portable audio primitives for speakiput.

mod phrase_split;
mod silence;
mod source;
mod wav;

pub use phrase_split::PhraseSplitter;
pub use silence::{
    AutoStopDetector, GateReason, SILENCE_RMS_THRESHOLD, audio_gate_reason, is_silent, rms_energy,
};
#[cfg(feature = "native-capture")]
pub use source::CpalAudioSource;
pub use source::{
    AudioCaptureError, AudioDeviceInfo, AudioSource, CaptureController, CaptureSession,
    CaptureStopHandle, compressed_audio_level,
};
pub use wav::{CHANNELS, SAMPLE_RATE, WavError, encode_wav};
