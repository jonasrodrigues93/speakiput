use speakiput_contract::{
    AudioDevice, AudioDevicesResponse, BackendChoice, BackendsResponse, DiagnosticCheck,
    DiagnosticsResponse, DictationState, Envelope, HistoryEntry, HistoryListResponse, MessageKind,
    RecordingLevelEvent, SessionFailedEvent, Settings, SettingsResponse, StateChangedEvent,
    StateSnapshot, TranscriptFinalEvent, TranscriptPartialEvent,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct GuiViewModel {
    pub instance_id: Option<Uuid>,
    pub last_sequence: Option<u64>,
    pub state: DictationState,
    pub active_session_id: Option<Uuid>,
    pub settings_revision: u64,
    pub settings: Settings,
    pub capabilities: Vec<String>,
    pub audio_devices: Vec<AudioDevice>,
    pub transcription_backends: Vec<BackendChoice>,
    pub post_processing_backends: Vec<BackendChoice>,
    pub history: Vec<HistoryEntry>,
    pub history_next_cursor: Option<String>,
    pub diagnostics: Vec<DiagnosticCheck>,
    pub diagnostics_log_path: Option<String>,
    pub audio_level: f32,
    pub partial_text: String,
    pub error: Option<String>,
    pub settings_error_field: Option<String>,
    pub settings_error_message: Option<String>,
    pub settings_stale: bool,
}

impl Default for GuiViewModel {
    fn default() -> Self {
        Self {
            instance_id: None,
            last_sequence: None,
            state: DictationState::Starting,
            active_session_id: None,
            settings_revision: 0,
            settings: Settings::default(),
            capabilities: Vec::new(),
            audio_devices: Vec::new(),
            transcription_backends: Vec::new(),
            post_processing_backends: Vec::new(),
            history: Vec::new(),
            history_next_cursor: None,
            diagnostics: Vec::new(),
            diagnostics_log_path: None,
            audio_level: 0.0,
            partial_text: String::new(),
            error: None,
            settings_error_field: None,
            settings_error_message: None,
            settings_stale: false,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ViewModelError {
    #[error("unexpected message kind or name: {0}")]
    UnexpectedMessage(String),
    #[error("event belongs to backend {received}, expected {expected}")]
    BackendRestarted { expected: Uuid, received: Uuid },
    #[error("event sequence gap: expected {expected}, received {received}")]
    SequenceGap { expected: u64, received: u64 },
    #[error("invalid event payload: {0}")]
    Payload(String),
}

impl GuiViewModel {
    pub fn apply_snapshot(&mut self, envelope: &Envelope) -> Result<(), ViewModelError> {
        if envelope.kind != MessageKind::Response || envelope.name != "state.get" {
            return Err(ViewModelError::UnexpectedMessage(envelope.name.clone()));
        }
        let snapshot: StateSnapshot = envelope
            .payload_as()
            .map_err(|error| ViewModelError::Payload(error.to_string()))?;
        if self.instance_id != envelope.instance_id {
            self.last_sequence = None;
        }
        self.instance_id = envelope.instance_id;
        self.state = snapshot.state;
        self.active_session_id = snapshot.active_session_id;
        self.settings_revision = snapshot.settings_revision;
        self.capabilities = snapshot.capabilities;
        self.audio_level = 0.0;
        self.error = None;
        Ok(())
    }

    pub fn apply_settings(&mut self, envelope: &Envelope) -> Result<(), ViewModelError> {
        if envelope.kind != MessageKind::Response
            || !matches!(envelope.name.as_str(), "settings.get" | "settings.replace")
        {
            return Err(ViewModelError::UnexpectedMessage(envelope.name.clone()));
        }
        let response: SettingsResponse = envelope
            .payload_as()
            .map_err(|error| ViewModelError::Payload(error.to_string()))?;
        self.settings_revision = response.revision;
        self.settings = response.settings;
        self.settings_stale = false;
        self.error = None;
        Ok(())
    }

    pub fn apply_audio_devices(&mut self, envelope: &Envelope) -> Result<(), ViewModelError> {
        Self::expect_response(envelope, "audio_devices.list")?;
        let response: AudioDevicesResponse = Self::payload(envelope)?;
        self.audio_devices = response.devices;
        self.settings.audio.input_device_id = response.selected_id;
        Ok(())
    }

    pub fn apply_backends(&mut self, envelope: &Envelope) -> Result<(), ViewModelError> {
        Self::expect_response(envelope, "backends.list")?;
        let response: BackendsResponse = Self::payload(envelope)?;
        self.transcription_backends = response.transcription;
        self.post_processing_backends = response.post_processing;
        Ok(())
    }

    pub fn apply_history(&mut self, envelope: &Envelope) -> Result<(), ViewModelError> {
        Self::expect_response(envelope, "history.list")?;
        let response: HistoryListResponse = Self::payload(envelope)?;
        self.history = response.entries;
        self.history_next_cursor = response.next_cursor;
        Ok(())
    }

    pub fn apply_diagnostics(&mut self, envelope: &Envelope) -> Result<(), ViewModelError> {
        Self::expect_response(envelope, "diagnostics.get")?;
        let response: DiagnosticsResponse = Self::payload(envelope)?;
        self.diagnostics = response.checks;
        self.diagnostics_log_path = response.log_path;
        Ok(())
    }

    pub fn apply_event(&mut self, envelope: &Envelope) -> Result<(), ViewModelError> {
        if envelope.kind != MessageKind::Event {
            return Err(ViewModelError::UnexpectedMessage(envelope.name.clone()));
        }
        let received_instance = envelope
            .instance_id
            .ok_or_else(|| ViewModelError::UnexpectedMessage(envelope.name.clone()))?;
        if let Some(expected) = self.instance_id {
            if expected != received_instance {
                return Err(ViewModelError::BackendRestarted {
                    expected,
                    received: received_instance,
                });
            }
        } else {
            self.instance_id = Some(received_instance);
        }
        let received_sequence = envelope
            .sequence
            .ok_or_else(|| ViewModelError::UnexpectedMessage(envelope.name.clone()))?;
        if let Some(previous) = self.last_sequence {
            let expected = previous.saturating_add(1);
            if received_sequence != expected {
                return Err(ViewModelError::SequenceGap {
                    expected,
                    received: received_sequence,
                });
            }
        }
        self.last_sequence = Some(received_sequence);

        match envelope.name.as_str() {
            "state.changed" => {
                let event: StateChangedEvent = Self::payload(envelope)?;
                self.state = event.current;
                self.active_session_id = if event.current == DictationState::Idle {
                    None
                } else {
                    event.session_id
                };
                if event.current != DictationState::Recording {
                    self.audio_level = 0.0;
                }
                if event.current == DictationState::Recording {
                    self.partial_text.clear();
                }
            }
            "recording.level" => {
                let event: RecordingLevelEvent = Self::payload(envelope)?;
                if Some(event.session_id) == self.active_session_id {
                    self.audio_level = event.level.clamp(0.0, 1.0);
                }
            }
            "transcript.partial" => {
                let event: TranscriptPartialEvent = Self::payload(envelope)?;
                if Some(event.session_id) == self.active_session_id {
                    self.partial_text = event.text;
                }
            }
            "transcript.final" => {
                let _: TranscriptFinalEvent = Self::payload(envelope)?;
                self.partial_text.clear();
                self.audio_level = 0.0;
            }
            "session.failed" => {
                let event: SessionFailedEvent = Self::payload(envelope)?;
                self.error = (event.code != "cancelled").then_some(event.message);
                self.audio_level = 0.0;
            }
            "settings.changed" => {
                self.settings_stale = envelope.payload["revision"]
                    .as_u64()
                    .is_some_and(|revision| revision != self.settings_revision);
            }
            "history.added" | "history.cleared" | "backend.health_changed" => {}
            other => return Err(ViewModelError::UnexpectedMessage(other.to_owned())),
        }
        Ok(())
    }

    fn expect_response(envelope: &Envelope, name: &str) -> Result<(), ViewModelError> {
        if envelope.kind != MessageKind::Response || envelope.name != name {
            return Err(ViewModelError::UnexpectedMessage(envelope.name.clone()));
        }
        Ok(())
    }

    fn payload<T: serde::de::DeserializeOwned>(envelope: &Envelope) -> Result<T, ViewModelError> {
        envelope
            .payload_as()
            .map_err(|error| ViewModelError::Payload(error.to_string()))
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use chrono::Utc;
    use serde_json::json;
    use speakiput_contract::{MessageKind, PROTOCOL_VERSION};

    use super::*;

    fn event(name: &str, instance_id: Uuid, sequence: u64, payload: serde_json::Value) -> Envelope {
        Envelope {
            protocol_version: PROTOCOL_VERSION.into(),
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

    #[test]
    fn events_drive_overlay_without_gui_owned_toggle_state() {
        let backend = Uuid::new_v4();
        let session = Uuid::new_v4();
        let mut model = GuiViewModel {
            instance_id: Some(backend),
            ..GuiViewModel::default()
        };
        model
            .apply_event(&event(
                "state.changed",
                backend,
                1,
                json!({ "previous": "idle", "current": "recording", "session_id": session }),
            ))
            .unwrap();
        model
            .apply_event(&event(
                "recording.level",
                backend,
                2,
                json!({ "session_id": session, "level": 1.5 }),
            ))
            .unwrap();
        model
            .apply_event(&event(
                "transcript.partial",
                backend,
                3,
                json!({ "session_id": session, "text": "falando agora" }),
            ))
            .unwrap();
        assert_eq!(model.state, DictationState::Recording);
        assert_eq!(model.audio_level, 1.0);
        assert_eq!(model.partial_text, "falando agora");
    }

    #[test]
    fn sequence_gap_requires_a_fresh_snapshot() {
        let backend = Uuid::new_v4();
        let mut model = GuiViewModel {
            instance_id: Some(backend),
            last_sequence: Some(2),
            ..GuiViewModel::default()
        };
        assert_eq!(
            model
                .apply_event(&event("history.cleared", backend, 4, json!({})))
                .unwrap_err(),
            ViewModelError::SequenceGap {
                expected: 3,
                received: 4
            }
        );
    }

    #[test]
    fn backend_restart_invalidates_sequence_assumptions() {
        let old = Uuid::new_v4();
        let new = Uuid::new_v4();
        let mut model = GuiViewModel {
            instance_id: Some(old),
            last_sequence: Some(10),
            ..GuiViewModel::default()
        };
        assert!(matches!(
            model.apply_event(&event("history.cleared", new, 1, json!({}))),
            Err(ViewModelError::BackendRestarted { .. })
        ));
    }
}
