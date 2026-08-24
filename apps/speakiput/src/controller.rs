use std::sync::Arc;

use serde::Serialize;
use serde_json::json;
use speakiput_client::{BackendClient, ClientError, EventSubscription};
use speakiput_contract::{
    ClientHelloRequest, ClientHelloResponse, ClientIdentity, CredentialPutRequest, DictationState,
    Envelope, HistoryListRequest, PROTOCOL_VERSION, RecordingStartRequest, RecordingStartResponse,
    RecordingStopRequest, Settings, SettingsReplaceRequest, StableErrorCode,
};
use thiserror::Error;

use crate::view_model::{GuiViewModel, ViewModelError};

#[derive(Debug, Error)]
pub enum ControllerError {
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    ViewModel(#[from] ViewModelError),
    #[error("failed to encode request payload: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("recording state changed before the command could be sent")]
    StateChanged,
    #[error("settings are invalid at {field}: {message}")]
    InvalidSettings {
        field: &'static str,
        message: &'static str,
    },
}

pub struct GuiController {
    client: Arc<dyn BackendClient>,
    events: EventSubscription,
    pub model: GuiViewModel,
}

impl GuiController {
    #[must_use]
    pub fn new(client: Arc<dyn BackendClient>) -> Self {
        let events = client.subscribe();
        Self {
            client,
            events,
            model: GuiViewModel::default(),
        }
    }

    pub async fn bootstrap(&mut self) -> Result<(), ControllerError> {
        let hello: ClientHelloResponse = self
            .request(
                "client.hello",
                &ClientHelloRequest {
                    supported_versions: vec![PROTOCOL_VERSION.into()],
                    client: ClientIdentity {
                        name: "speakiput-gui".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                    subscriptions: vec!["*".into()],
                },
            )
            .await?
            .payload_as()?;
        self.model.capabilities = hello.capabilities;

        let state = self.request("state.get", &json!({})).await?;
        self.model.apply_snapshot(&state)?;
        let settings = self.request("settings.get", &json!({})).await?;
        self.model.apply_settings(&settings)?;
        self.refresh_supporting_data().await
    }

    pub async fn refresh_supporting_data(&mut self) -> Result<(), ControllerError> {
        let devices = self.request("audio_devices.list", &json!({})).await?;
        self.model.apply_audio_devices(&devices)?;
        let backends = self.request("backends.list", &json!({})).await?;
        self.model.apply_backends(&backends)?;
        self.refresh_history().await?;
        self.refresh_diagnostics().await
    }

    pub async fn reload_settings(&mut self) -> Result<(), ControllerError> {
        let settings = self.request("settings.get", &json!({})).await?;
        self.model.apply_settings(&settings)?;
        self.model.settings_error_field = None;
        self.model.settings_error_message = None;
        Ok(())
    }

    pub async fn refresh_history(&mut self) -> Result<(), ControllerError> {
        let history = self
            .request(
                "history.list",
                &HistoryListRequest {
                    limit: 100,
                    cursor: None,
                },
            )
            .await?;
        self.model.apply_history(&history)?;
        Ok(())
    }

    pub async fn refresh_diagnostics(&mut self) -> Result<(), ControllerError> {
        let diagnostics = self.request("diagnostics.get", &json!({})).await?;
        self.model.apply_diagnostics(&diagnostics)?;
        Ok(())
    }

    pub async fn start_or_stop(&mut self) -> Result<(), ControllerError> {
        match (self.model.state, self.model.active_session_id) {
            (DictationState::Idle, _) => {
                let response = self
                    .request(
                        "recording.start",
                        &RecordingStartRequest {
                            language: Some(self.model.settings.general.language.clone()),
                        },
                    )
                    .await?;
                let started: RecordingStartResponse = response.payload_as()?;
                self.model.active_session_id = Some(started.session_id);
                self.model.state = started.state;
                Ok(())
            }
            (DictationState::Recording, Some(session_id)) => {
                self.request("recording.stop", &RecordingStopRequest { session_id })
                    .await?;
                Ok(())
            }
            _ => Err(ControllerError::StateChanged),
        }
    }

    pub async fn save_settings(&mut self, settings: Settings) -> Result<(), ControllerError> {
        if let Err(error) = settings.validate() {
            self.model.settings_error_field = Some(error.field.into());
            self.model.settings_error_message = Some(error.message.into());
            return Err(ControllerError::InvalidSettings {
                field: error.field,
                message: error.message,
            });
        }
        let response = match self
            .request(
                "settings.replace",
                &SettingsReplaceRequest {
                    expected_revision: self.model.settings_revision,
                    settings,
                },
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                if matches!(
                    &error,
                    ControllerError::Client(ClientError::Protocol(protocol))
                        if protocol.code == StableErrorCode::Conflict
                ) {
                    self.model.settings_error_field = Some("settings".into());
                    self.model.settings_error_message = Some(
                        "Settings changed in another process. Reopen Settings to reload before saving."
                            .into(),
                    );
                }
                return Err(error);
            }
        };
        self.model.apply_settings(&response)?;
        self.model.settings_error_field = None;
        self.model.settings_error_message = None;
        Ok(())
    }

    pub async fn save_settings_with_credential(
        &mut self,
        settings: Settings,
        secret: Option<String>,
    ) -> Result<(), ControllerError> {
        if let Some(secret) = secret {
            let Some(credential_id) = settings
                .post_processing
                .credential_id
                .clone()
                .filter(|value| !value.trim().is_empty())
            else {
                self.model.settings_error_field = Some("post_processing.credential_id".into());
                self.model.settings_error_message =
                    Some("A credential ID is required when an API key is entered.".into());
                return Err(ControllerError::InvalidSettings {
                    field: "post_processing.credential_id",
                    message: "is required when an API key is entered",
                });
            };
            self.request(
                "credentials.put",
                &CredentialPutRequest {
                    credential_id,
                    secret,
                },
            )
            .await?;
        }
        self.save_settings(settings).await
    }

    pub async fn clear_history(&mut self) -> Result<(), ControllerError> {
        self.request("history.clear", &json!({})).await?;
        self.model.history.clear();
        self.model.history_next_cursor = None;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<(), ControllerError> {
        let event = self.events.recv().await?;
        match self.model.apply_event(&event) {
            Ok(()) => Ok(()),
            Err(ViewModelError::BackendRestarted { .. } | ViewModelError::SequenceGap { .. }) => {
                let snapshot = self.request("state.get", &json!({})).await?;
                self.model.apply_snapshot(&snapshot)?;
                let settings = self.request("settings.get", &json!({})).await?;
                self.model.apply_settings(&settings)?;
                Ok(())
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn request(
        &self,
        name: &str,
        payload: &impl Serialize,
    ) -> Result<Envelope, ControllerError> {
        let payload = serde_json::to_value(payload)?;
        Ok(self
            .client
            .request(Envelope::request(name, payload))
            .await?)
    }
}
