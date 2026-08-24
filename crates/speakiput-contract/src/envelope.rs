use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::PROTOCOL_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Request,
    Response,
    Event,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StableErrorCode {
    InvalidArgument,
    InvalidState,
    NotFound,
    Conflict,
    Unavailable,
    PermissionDenied,
    Timeout,
    Unsupported,
    ProtocolMismatch,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: StableErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub details: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub protocol_version: String,
    pub message_id: Uuid,
    pub kind: MessageKind,
    pub name: String,
    pub sent_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EnvelopeError {
    #[error("unsupported protocol version {0}")]
    ProtocolVersion(String),
    #[error("message name must contain at least one dot and use lowercase identifiers")]
    InvalidName,
    #[error("payload must be a JSON object")]
    PayloadNotObject,
    #[error("response requires correlation_id and instance_id")]
    IncompleteResponse,
    #[error("event requires instance_id and a positive sequence")]
    IncompleteEvent,
    #[error("request must not have correlation_id or sequence")]
    InvalidRequestMetadata,
    #[error("error responses must use an empty payload")]
    NonEmptyErrorPayload,
}

impl Envelope {
    #[must_use]
    pub fn request(name: impl Into<String>, payload: Value) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            message_id: Uuid::new_v4(),
            kind: MessageKind::Request,
            name: name.into(),
            sent_at: Utc::now(),
            correlation_id: None,
            instance_id: None,
            sequence: None,
            payload,
            error: None,
        }
    }

    #[must_use]
    pub fn response(request: &Self, instance_id: Uuid, payload: Value) -> Self {
        Self {
            protocol_version: request.protocol_version.clone(),
            message_id: Uuid::new_v4(),
            kind: MessageKind::Response,
            name: request.name.clone(),
            sent_at: Utc::now(),
            correlation_id: Some(request.message_id),
            instance_id: Some(instance_id),
            sequence: None,
            payload,
            error: None,
        }
    }

    #[must_use]
    pub fn error_response(request: &Self, instance_id: Uuid, error: ProtocolError) -> Self {
        let mut response = Self::response(request, instance_id, serde_json::json!({}));
        response.error = Some(error);
        response
    }

    #[must_use]
    pub fn event(
        name: impl Into<String>,
        instance_id: Uuid,
        sequence: u64,
        payload: Value,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION.to_owned(),
            message_id: Uuid::new_v4(),
            kind: MessageKind::Event,
            name: name.into(),
            sent_at: Utc::now(),
            correlation_id: None,
            instance_id: Some(instance_id),
            sequence: Some(sequence),
            payload,
            error: None,
        }
    }

    pub fn payload_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_value(self.payload.clone())
    }

    pub fn validate(&self) -> Result<(), EnvelopeError> {
        if self.protocol_version != PROTOCOL_VERSION {
            return Err(EnvelopeError::ProtocolVersion(
                self.protocol_version.clone(),
            ));
        }
        if !valid_message_name(&self.name) {
            return Err(EnvelopeError::InvalidName);
        }
        let Some(payload) = self.payload.as_object() else {
            return Err(EnvelopeError::PayloadNotObject);
        };
        match self.kind {
            MessageKind::Request => {
                if self.correlation_id.is_some() || self.sequence.is_some() {
                    return Err(EnvelopeError::InvalidRequestMetadata);
                }
            }
            MessageKind::Response => {
                if self.correlation_id.is_none() || self.instance_id.is_none() {
                    return Err(EnvelopeError::IncompleteResponse);
                }
            }
            MessageKind::Event => {
                if self.instance_id.is_none() || self.sequence.is_none_or(|sequence| sequence == 0)
                {
                    return Err(EnvelopeError::IncompleteEvent);
                }
            }
        }
        if self.error.is_some() && !payload.is_empty() {
            return Err(EnvelopeError::NonEmptyErrorPayload);
        }
        Ok(())
    }
}

fn valid_message_name(name: &str) -> bool {
    let mut parts = name.split('.');
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(second) = parts.next() else {
        return false;
    };
    valid_identifier(first) && valid_identifier(second) && parts.all(valid_identifier)
}

fn valid_identifier(part: &str) -> bool {
    let mut chars = part.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|character| {
            character.is_ascii_lowercase() || character.is_ascii_digit() || character == '_'
        })
}
