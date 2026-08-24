use std::path::Path;

use hound::{SampleFormat, WavSpec, WavWriter};

const RATE: u32 = 16_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&root)?;
    write(&root.join("silence.wav"), &vec![0; RATE as usize])?;
    write(&root.join("short-tap.wav"), &tone(100, 8_000))?;

    let mut pauses = tone(500, 9_000);
    pauses.extend(vec![0; samples_for_ms(800)]);
    pauses.extend(tone(500, 9_000));
    write(&root.join("speech-pause-speech.wav"), &pauses)?;

    let mut trailing_silence = tone(450, 10_000);
    trailing_silence.extend(vec![0; samples_for_ms(1_200)]);
    write(&root.join("speech-then-silence.wav"), &trailing_silence)?;
    Ok(())
}

fn tone(duration_ms: u64, amplitude: i16) -> Vec<i16> {
    (0..samples_for_ms(duration_ms))
        .map(|index| {
            if index % 32 < 16 {
                amplitude
            } else {
                -amplitude
            }
        })
        .collect()
}

fn samples_for_ms(duration_ms: u64) -> usize {
    usize::try_from(u64::from(RATE) * duration_ms / 1_000).unwrap()
}

fn write(path: &Path, samples: &[i16]) -> Result<(), hound::Error> {
    let mut writer = WavWriter::create(
        path,
        WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: SampleFormat::Int,
        },
    )?;
    for sample in samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()
}
