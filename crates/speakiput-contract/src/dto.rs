use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentity {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHelloRequest {
    pub supported_versions: Vec<String>,
    pub client: ClientIdentity,
    pub subscriptions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerLimits {
    pub max_frame_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHelloResponse {
    pub selected_version: String,
    pub server: ClientIdentity,
    pub capabilities: Vec<String>,
    pub limits: ServerLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DictationState {
    Starting,
    Idle,
    Recording,
    Transcribing,
    PostProcessing,
    Injecting,
    Error,
    ShuttingDown,
}

impl DictationState {
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Starting
                    | Self::Transcribing
                    | Self::PostProcessing
                    | Self::Injecting
                    | Self::Error
                    | Self::Recording,
                Self::Idle
            ) | (
                Self::Starting
                    | Self::Recording
                    | Self::Transcribing
                    | Self::PostProcessing
                    | Self::Injecting,
                Self::Error
            ) | (Self::Idle, Self::Recording | Self::ShuttingDown)
                | (Self::Recording, Self::Transcribing)
                | (Self::Transcribing, Self::PostProcessing | Self::Injecting)
                | (Self::PostProcessing, Self::Injecting)
                | (Self::Error, Self::ShuttingDown)
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendHealth {
    Starting,
    Ready,
    Degraded,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub state: DictationState,
    pub active_session_id: Option<Uuid>,
    pub settings_revision: u64,
    pub capabilities: Vec<String>,
    pub health: BackendHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct StateGetRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingStartRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingStartResponse {
    pub session_id: Uuid,
    pub state: DictationState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingStopRequest {
    pub session_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordingStopResponse {
    pub session_id: Uuid,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationCancelRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptedResponse {
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateChangedEvent {
    pub previous: DictationState,
    pub current: DictationState,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingLevelEvent {
    pub session_id: Uuid,
    pub level: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptPartialEvent {
    pub session_id: Uuid,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFailedEvent {
    pub session_id: Uuid,
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertionStatus {
    Inserted,
    Clipboard,
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InsertionOutcome {
    pub status: InsertionStatus,
    pub method: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptFinalEvent {
    pub session_id: Uuid,
    pub raw_text: String,
    pub processed_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewritten_text: Option<String>,
    pub output_text: String,
    pub post_processed: bool,
    #[serde(default)]
    pub prompt_rewritten: bool,
    pub insertion: InsertionOutcome,
    pub transcription_backend: String,
    pub language: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    pub schema_version: u32,
    pub general: GeneralSettings,
    pub audio: AudioSettings,
    pub transcription: TranscriptionSettings,
    pub post_processing: PostProcessingSettings,
    pub output: OutputSettings,
    pub shortcut: ShortcutSettings,
    pub overlay: OverlaySettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            general: GeneralSettings::default(),
            audio: AudioSettings::default(),
            transcription: TranscriptionSettings::default(),
            post_processing: PostProcessingSettings::default(),
            output: OutputSettings::default(),
            shortcut: ShortcutSettings::default(),
            overlay: OverlaySettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsValidationError {
    pub field: &'static str,
    pub message: &'static str,
}

impl Settings {
    pub fn validate(&self) -> Result<(), SettingsValidationError> {
        if self.schema_version != 1 {
            return Err(SettingsValidationError {
                field: "schema_version",
                message: "must be 1",
            });
        }
        if self.general.language.trim().is_empty() {
            return Err(SettingsValidationError {
                field: "general.language",
                message: "must not be empty",
            });
        }
        if !(250..=10_000).contains(&self.general.auto_stop_ms) {
            return Err(SettingsValidationError {
                field: "general.auto_stop_ms",
                message: "must be between 250 and 10000",
            });
        }
        if !(100..=5_000).contains(&self.audio.phrase_silence_ms) {
            return Err(SettingsValidationError {
                field: "audio.phrase_silence_ms",
                message: "must be between 100 and 5000",
            });
        }
        if self.audio.input_device_id.trim().is_empty() {
            return Err(SettingsValidationError {
                field: "audio.input_device_id",
                message: "must not be empty",
            });
        }
        if self.transcription.backend_id.trim().is_empty()
            || self.transcription.model_id.trim().is_empty()
        {
            return Err(SettingsValidationError {
                field: "transcription",
                message: "backend and model must not be empty",
            });
        }
        if (self.post_processing.enabled || self.post_processing.prompt_rewrite_enabled)
            && (self.post_processing.backend_id.trim().is_empty()
                || self.post_processing.model_id.trim().is_empty()
                || self.post_processing.endpoint.trim().is_empty()
                || self.post_processing.instruction.trim().is_empty())
        {
            return Err(SettingsValidationError {
                field: "post_processing",
                message: "enabled text processing requires backend, model and instruction",
            });
        }
        if self.output.key_delay_ms > 1_000 {
            return Err(SettingsValidationError {
                field: "output.key_delay_ms",
                message: "must not exceed 1000",
            });
        }
        if !valid_shortcut(&self.shortcut.record) {
            return Err(SettingsValidationError {
                field: "shortcut.record",
                message: "must contain a modifier and exactly one key",
            });
        }
        Ok(())
    }
}

fn valid_shortcut(shortcut: &str) -> bool {
    let mut has_modifier = false;
    let mut keys = 0;
    for part in shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.to_ascii_lowercase().as_str() {
            "super" | "meta" | "win" | "shift" | "ctrl" | "control" | "alt" => {
                has_modifier = true;
            }
            _ => keys += 1,
        }
    }
    has_modifier && keys == 1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneralSettings {
    pub language: String,
    pub auto_stop_ms: u64,
    pub history_enabled: bool,
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            language: "pt".into(),
            auto_stop_ms: 1000,
            history_enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioSettings {
    pub input_device_id: String,
    pub phrase_silence_ms: u64,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            input_device_id: "default".into(),
            phrase_silence_ms: 700,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptionSettings {
    pub backend_id: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_path: Option<String>,
    pub prompt: Option<String>,
    pub vocabulary: Vec<String>,
    #[serde(default)]
    pub remove_filler_words: bool,
    #[serde(default)]
    pub filler_words: Vec<String>,
}

impl Default for TranscriptionSettings {
    fn default() -> Self {
        Self {
            backend_id: "local-whisper".into(),
            model_id: "large-v3-turbo-q5_0".into(),
            model_path: None,
            prompt: None,
            vocabulary: Vec::new(),
            remove_filler_words: false,
            filler_words: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostProcessingSettings {
    pub enabled: bool,
    #[serde(default)]
    pub prompt_rewrite_enabled: bool,
    pub backend_id: String,
    pub model_id: String,
    pub endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<String>,
    pub instruction: String,
}

impl Default for PostProcessingSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            prompt_rewrite_enabled: false,
            backend_id: "openai-compatible".into(),
            model_id: "gemma-4-26b-no-reasoning".into(),
            endpoint: "http://127.0.0.1:1234/v1/chat/completions".into(),
            credential_id: None,
            instruction: "Correct punctuation without adding content.".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    Keyboard,
    Clipboard,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputSettings {
    pub mode: OutputMode,
    pub key_delay_ms: u64,
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            mode: OutputMode::Keyboard,
            key_delay_ms: 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShortcutSettings {
    pub record: String,
}

impl Default for ShortcutSettings {
    fn default() -> Self {
        Self {
            record: "Super+Shift+Space".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlaySize {
    Small,
    Medium,
    Large,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenAnchor {
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlaySettings {
    pub enabled: bool,
    pub show_partial_transcript: bool,
    pub size: OverlaySize,
    pub screen_anchor: ScreenAnchor,
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            show_partial_transcript: true,
            size: OverlaySize::Medium,
            screen_anchor: ScreenAnchor::BottomCenter,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SettingsGetRequest {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsResponse {
    pub revision: u64,
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsReplaceRequest {
    pub expected_revision: u64,
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialPutRequest {
    pub credential_id: String,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialIdRequest {
    pub credential_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialStatusResponse {
    pub credential_id: String,
    pub stored: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioDevicesResponse {
    pub devices: Vec<AudioDevice>,
    pub selected_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendChoice {
    pub id: String,
    pub name: String,
    pub available: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendsResponse {
    pub transcription: Vec<BackendChoice>,
    pub post_processing: Vec<BackendChoice>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryListRequest {
    pub limit: u32,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub session_id: Uuid,
    pub raw_text: String,
    pub processed_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewritten_text: Option<String>,
    pub output_text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryListResponse {
    pub entries: Vec<HistoryEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticCheck {
    pub id: String,
    pub status: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticsResponse {
    pub checks: Vec<DiagnosticCheck>,
    pub log_path: Option<String>,
}
