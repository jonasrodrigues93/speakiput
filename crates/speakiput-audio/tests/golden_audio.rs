use std::path::PathBuf;

use speakiput_audio::{GateReason, PhraseSplitter, SILENCE_RMS_THRESHOLD, audio_gate_reason};

fn samples(name: &str) -> Vec<i16> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let reader = hound::WavReader::open(path).unwrap();
    assert_eq!(reader.spec().sample_rate, 16_000);
    assert_eq!(reader.spec().channels, 1);
    reader.into_samples::<i16>().map(Result::unwrap).collect()
}

#[test]
fn silence_and_short_tap_are_rejected_before_asr() {
    assert_eq!(
        audio_gate_reason(&samples("silence.wav"), 16_000, 300, SILENCE_RMS_THRESHOLD),
        Some(GateReason::Silent)
    );
    assert_eq!(
        audio_gate_reason(
            &samples("short-tap.wav"),
            16_000,
            300,
            SILENCE_RMS_THRESHOLD
        ),
        Some(GateReason::TooShort)
    );
}

#[test]
fn pause_fixture_splits_into_two_phrases() {
    let mut splitter = PhraseSplitter::new(16_000, SILENCE_RMS_THRESHOLD, 700);
    let mut phrases = splitter.feed(&samples("speech-pause-speech.wav"));
    if let Some(phrase) = splitter.flush() {
        phrases.push(phrase);
    }
    assert_eq!(phrases.len(), 2);
    assert!(phrases.iter().all(|phrase| !phrase.is_empty()));
}

#[test]
fn trailing_silence_fixture_triggers_auto_stop_behavior() {
    let audio = samples("speech-then-silence.wav");
    let mut detector = speakiput_audio::AutoStopDetector::new(SILENCE_RMS_THRESHOLD, 1_000, 16_000);
    assert!(audio.chunks(160).any(|chunk| detector.feed(chunk)));
}
