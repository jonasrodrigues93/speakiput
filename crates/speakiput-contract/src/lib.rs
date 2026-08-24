//! Versioned, transport-independent GUI/backend contract.
//!
//! This crate deliberately has no GUI, audio, networking, or OS dependencies.

mod codec;
mod dto;
mod envelope;

pub use codec::{FrameError, MAX_FRAME_BYTES, decode_frame, encode_frame, read_frame, write_frame};
pub use dto::*;
pub use envelope::{Envelope, EnvelopeError, MessageKind, ProtocolError, StableErrorCode};

pub const PROTOCOL_VERSION: &str = "1.0";
