// Substantially derived from whisrs crates/audio-silence-gate at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

//! Lightweight RMS-based silence detection and auto-stop for audio capture.

/// Normalized RMS threshold below which audio is considered silence.
pub const SILENCE_RMS_THRESHOLD: f64 = 0.003;

/// Calculate normalized root mean square energy for i16 PCM samples.
#[must_use]
pub fn rms_energy(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_squares: f64 = samples
        .iter()
        .map(|&sample| f64::from(sample) * f64::from(sample))
        .sum();
    let sample_count = u32::try_from(samples.len()).map_or(f64::from(u32::MAX), f64::from);
    (sum_squares / sample_count).sqrt() / f64::from(i16::MAX)
}

/// Return whether a chunk's normalized RMS is below `threshold`.
#[must_use]
pub fn is_silent(samples: &[i16], threshold: f64) -> bool {
    rms_energy(samples) < threshold
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GateReason {
    Empty,
    TooShort,
    Silent,
    Invalid,
}

impl GateReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::TooShort => "too short",
            Self::Silent => "silent",
            Self::Invalid => "invalid",
        }
    }
}

#[must_use]
pub fn audio_gate_reason(
    samples: &[i16],
    sample_rate: u32,
    min_duration_ms: u64,
    threshold: f64,
) -> Option<GateReason> {
    if samples.is_empty() {
        return Some(GateReason::Empty);
    }
    if sample_rate == 0 {
        return Some(GateReason::Invalid);
    }
    let duration_ms = u64::try_from(samples.len())
        .unwrap_or(u64::MAX)
        .saturating_mul(1000)
        / u64::from(sample_rate);
    if duration_ms < min_duration_ms {
        return Some(GateReason::TooShort);
    }
    if is_silent(samples, threshold) {
        return Some(GateReason::Silent);
    }
    None
}

/// Tracks consecutive silent frames after speech and signals auto-stop.
#[derive(Debug)]
pub struct AutoStopDetector {
    threshold: f64,
    silent_samples: u64,
    timeout_samples: u64,
    confirmation_samples: u64,
    voiced_candidate_samples: u64,
    speech_detected: bool,
}

impl AutoStopDetector {
    #[must_use]
    pub fn new(threshold: f64, timeout_ms: u64, sample_rate: u32) -> Self {
        Self::with_confirmation(threshold, timeout_ms, 0, sample_rate)
    }

    /// Creates a detector with a temporal voice confirmation window. Short
    /// transients can cross the RMS threshold without cancelling the stop
    /// timer, which prevents taps and knocks from keeping a session alive.
    #[must_use]
    pub fn with_confirmation(
        threshold: f64,
        timeout_ms: u64,
        confirmation_ms: u64,
        sample_rate: u32,
    ) -> Self {
        let timeout_samples = (timeout_ms.saturating_mul(u64::from(sample_rate)) / 1000).max(1);
        let confirmation_samples = confirmation_ms.saturating_mul(u64::from(sample_rate)) / 1000;
        Self {
            threshold,
            silent_samples: 0,
            timeout_samples,
            confirmation_samples,
            voiced_candidate_samples: 0,
            speech_detected: false,
        }
    }

    pub fn feed(&mut self, samples: &[i16]) -> bool {
        let sample_count = u64::try_from(samples.len()).unwrap_or(u64::MAX);
        if rms_energy(samples) >= self.threshold {
            self.voiced_candidate_samples =
                self.voiced_candidate_samples.saturating_add(sample_count);
            if !self.speech_detected && self.voiced_candidate_samples >= self.confirmation_samples {
                self.speech_detected = true;
                self.silent_samples = 0;
            } else if self.speech_detected
                && self.voiced_candidate_samples >= self.confirmation_samples
            {
                self.silent_samples = 0;
            } else if self.speech_detected {
                self.silent_samples = self.silent_samples.saturating_add(sample_count);
            }
        } else {
            self.voiced_candidate_samples = 0;
            if self.speech_detected {
                self.silent_samples = self.silent_samples.saturating_add(sample_count);
            }
        }
        self.speech_detected && self.silent_samples >= self.timeout_samples
    }

    pub const fn reset(&mut self) {
        self.silent_samples = 0;
        self.voiced_candidate_samples = 0;
        self.speech_detected = false;
    }

    #[must_use]
    pub const fn has_speech(&self) -> bool {
        self.speech_detected
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_zero_rms() {
        let silence = vec![0_i16; 1600];
        assert_eq!(rms_energy(&silence), 0.0);
        assert!(is_silent(&silence, 0.01));
    }

    #[test]
    fn empty_slice_is_zero_rms() {
        assert_eq!(rms_energy(&[]), 0.0);
    }

    #[test]
    fn loud_signal_has_high_rms() {
        let loud = vec![i16::MAX; 1600];
        let rms = rms_energy(&loud);
        assert!(rms > 0.9, "loud signal RMS should be near 1.0, got {rms}");
        assert!(!is_silent(&loud, 0.01));
    }

    #[test]
    fn quiet_signal_is_detected() {
        let quiet = (0..1600)
            .map(|index| i16::try_from(index % 100).unwrap() - 50)
            .collect::<Vec<_>>();
        assert!(rms_energy(&quiet) < 0.01);
        assert!(is_silent(&quiet, 0.01));
    }

    #[test]
    fn auto_stop_not_triggered_without_speech() {
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);
        let silence = vec![0_i16; 16_000];
        assert!(!detector.feed(&silence));
        assert!(!detector.feed(&silence));
        assert!(!detector.feed(&silence));
        assert!(!detector.has_speech());
    }

    #[test]
    fn auto_stop_triggered_after_speech_then_silence() {
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);
        assert!(!detector.feed(&vec![10_000; 16_000]));
        assert!(!detector.feed(&vec![0; 16_000]));
        assert!(detector.feed(&vec![0; 16_000]));
    }

    #[test]
    fn auto_stop_resets_on_speech() {
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);
        let speech = vec![10_000; 16_000];
        let silence = vec![0; 16_000];
        detector.feed(&speech);
        detector.feed(&silence);
        assert!(!detector.feed(&vec![0; 8_000]));
        assert!(!detector.feed(&speech));
        detector.feed(&silence);
        assert!(!detector.feed(&vec![0; 8_000]));
        assert!(detector.feed(&vec![0; 8_000]));
    }

    #[test]
    fn auto_stop_reset() {
        let mut detector = AutoStopDetector::new(0.01, 2000, 16_000);
        detector.feed(&vec![10_000; 16_000]);
        detector.reset();
        assert!(!detector.has_speech());
    }

    #[test]
    fn gate_rejects_empty_short_and_silent_audio() {
        assert_eq!(
            audio_gate_reason(&[], 16_000, 300, 0.005),
            Some(GateReason::Empty)
        );
        assert_eq!(
            audio_gate_reason(&vec![10_000; 1_600], 16_000, 300, 0.005),
            Some(GateReason::TooShort)
        );
        assert_eq!(
            audio_gate_reason(&vec![0; 16_000], 16_000, 300, 0.005),
            Some(GateReason::Silent)
        );
    }

    #[test]
    fn gate_accepts_speech() {
        let samples = (0..16_000)
            .map(|index| ((f64::from(index) * 0.1).sin() * 16_000.0) as i16)
            .collect::<Vec<_>>();
        assert_eq!(audio_gate_reason(&samples, 16_000, 300, 0.005), None);
    }

    #[test]
    fn zero_configuration_is_safe() {
        let samples = vec![10_000; 1_600];
        assert_eq!(
            audio_gate_reason(&samples, 0, 300, 0.005),
            Some(GateReason::Invalid)
        );
        let mut detector = AutoStopDetector::new(0.01, 0, 0);
        assert!(!detector.feed(&samples));
        assert!(detector.feed(&[0]));
    }

    #[test]
    fn auto_stop_exact_threshold() {
        let mut detector = AutoStopDetector::new(0.01, 100, 16_000);
        detector.feed(&vec![10_000; 1_600]);
        assert!(detector.feed(&vec![0; 1_600]));
    }

    #[test]
    fn short_transient_does_not_restart_silence_timer() {
        let mut detector = AutoStopDetector::with_confirmation(0.01, 500, 200, 16_000);
        assert!(!detector.feed(&vec![10_000; 4_000]));
        assert!(!detector.feed(&vec![0; 4_000]));
        assert!(!detector.feed(&vec![10_000; 1_600]));
        assert!(detector.feed(&vec![0; 4_000]));
    }
}
