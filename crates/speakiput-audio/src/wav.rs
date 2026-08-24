// Adapted from whisrs src/audio/wav.rs at commit
// 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c) 2025-present
// Yosif Kitaneh, used under the MIT License; see THIRD_PARTY_LICENSES.md.

use std::io::Cursor;

use thiserror::Error;

pub const SAMPLE_RATE: u32 = 16_000;
pub const CHANNELS: u16 = 1;

#[derive(Debug, Error)]
pub enum WavError {
    #[error("WAV encoding failed: {0}")]
    Hound(#[from] hound::Error),
}

pub fn encode_wav(samples: &[i16]) -> Result<Vec<u8>, WavError> {
    let spec = hound::WavSpec {
        channels: CHANNELS,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec)?;
        for &sample in samples {
            writer.write_sample(sample)?;
        }
        writer.finalize()?;
    }
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_wav_round_trips_samples() {
        let samples = (0..1600)
            .map(|index| i16::try_from(index % 256).unwrap())
            .collect::<Vec<_>>();
        let wav = encode_wav(&samples).unwrap();
        assert_eq!(&wav[..4], b"RIFF");
        let reader = hound::WavReader::new(Cursor::new(wav)).unwrap();
        assert_eq!(reader.spec().channels, CHANNELS);
        assert_eq!(reader.spec().sample_rate, SAMPLE_RATE);
        assert_eq!(reader.spec().bits_per_sample, 16);
        assert_eq!(
            reader
                .into_samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            samples
        );
    }

    #[test]
    fn encode_wav_accepts_empty_audio() {
        assert_eq!(&encode_wav(&[]).unwrap()[..4], b"RIFF");
    }
}
