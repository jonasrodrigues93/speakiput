use std::{hint::black_box, time::Instant};

use speakiput_audio::{SILENCE_RMS_THRESHOLD, audio_gate_reason, encode_wav};

fn main() {
    let samples = synthetic_speech(60);

    let started = Instant::now();
    for _ in 0..20 {
        black_box(audio_gate_reason(
            black_box(&samples),
            16_000,
            300,
            SILENCE_RMS_THRESHOLD,
        ));
    }
    report(
        "silence_gate_60s_avg_ms",
        started.elapsed().as_secs_f64() * 50.0,
    );

    let started = Instant::now();
    for _ in 0..10 {
        black_box(encode_wav(black_box(&samples)).expect("WAV encoding"));
    }
    report(
        "wav_encode_60s_avg_ms",
        started.elapsed().as_secs_f64() * 100.0,
    );

    if let Some(rss_kib) = resident_memory_kib() {
        println!("resident_memory_kib={rss_kib}");
    }

    #[cfg(feature = "native")]
    if let Some(path) = std::env::var_os("SPEAKIPUT_MODEL_PATH") {
        use speakiput_asr::{LocalWhisperBackend, TranscriptionBackend};

        let started = Instant::now();
        let backend = LocalWhisperBackend::new(path.to_string_lossy());
        report(
            "whisper_model_load_ms",
            started.elapsed().as_secs_f64() * 1_000.0,
        );
        println!("whisper_model_available={}", backend.available());
    }
}

fn synthetic_speech(seconds: usize) -> Vec<i16> {
    (0..16_000 * seconds)
        .map(|index| if index % 40 < 20 { 8_000 } else { -8_000 })
        .collect()
}

fn report(name: &str, value: f64) {
    println!("{name}={value:.3}");
}

#[cfg(target_os = "linux")]
fn resident_memory_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn resident_memory_kib() -> Option<u64> {
    None
}
