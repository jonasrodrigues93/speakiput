use std::io::{Read, Write};

use thiserror::Error;

use crate::Envelope;

pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame payload is {actual} bytes; maximum is {maximum}")]
    TooLarge { actual: usize, maximum: usize },
    #[error("truncated frame header")]
    TruncatedHeader,
    #[error("truncated frame payload: expected {expected} bytes, got {actual}")]
    TruncatedPayload { expected: usize, actual: usize },
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid protocol envelope: {0}")]
    Envelope(#[from] crate::EnvelopeError),
}

pub fn encode_frame(envelope: &Envelope) -> Result<Vec<u8>, FrameError> {
    envelope.validate()?;
    let payload = serde_json::to_vec(envelope)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: payload.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }

    let length = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge {
        actual: payload.len(),
        maximum: MAX_FRAME_BYTES,
    })?;
    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn decode_frame(frame: &[u8]) -> Result<Envelope, FrameError> {
    if frame.len() < 4 {
        return Err(FrameError::TruncatedHeader);
    }
    let length = u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: length,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let actual = frame.len() - 4;
    if actual != length {
        return Err(FrameError::TruncatedPayload {
            expected: length,
            actual,
        });
    }
    let envelope: Envelope = serde_json::from_slice(&frame[4..])?;
    envelope.validate()?;
    Ok(envelope)
}

pub fn write_frame(writer: &mut impl Write, envelope: &Envelope) -> Result<(), FrameError> {
    writer.write_all(&encode_frame(envelope)?)?;
    Ok(())
}

pub fn read_frame(reader: &mut impl Read) -> Result<Envelope, FrameError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::TruncatedHeader
        } else {
            FrameError::Io(error)
        }
    })?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge {
            actual: length,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).map_err(|error| {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            FrameError::TruncatedPayload {
                expected: length,
                actual: 0,
            }
        } else {
            FrameError::Io(error)
        }
    })?;
    let envelope: Envelope = serde_json::from_slice(&payload)?;
    envelope.validate()?;
    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Envelope;

    #[test]
    fn round_trips_a_frame() {
        let envelope = Envelope::request("state.get", json!({}));
        let decoded = decode_frame(&encode_frame(&envelope).unwrap()).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn rejects_oversized_header_before_allocating() {
        let frame = 1_048_577_u32.to_be_bytes();
        assert!(matches!(
            decode_frame(&frame),
            Err(FrameError::TooLarge { .. })
        ));
    }
}
