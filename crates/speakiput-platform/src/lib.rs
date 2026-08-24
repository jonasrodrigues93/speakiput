//! Cross-platform traits for OS-specific effects.

use async_trait::async_trait;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PlatformCapabilities {
    pub global_shortcut: bool,
    pub focused_target: bool,
    pub keyboard_injection: bool,
    pub clipboard: bool,
    pub tray: bool,
    pub autostart: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusedTarget {
    pub window_id: Option<String>,
    pub application_id: Option<String>,
    pub title: Option<String>,
    pub is_terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionMethod {
    Keyboard,
    ClipboardPaste,
    ClipboardOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsertionResult {
    pub method: InsertionMethod,
}

#[derive(Debug, Error)]
pub enum PlatformError {
    #[error("platform capability is unsupported: {0}")]
    Unsupported(&'static str),
    #[error("platform permission was denied: {0}")]
    PermissionDenied(String),
    #[error("platform operation failed: {0}")]
    Operation(String),
}

#[async_trait]
pub trait FocusService: Send + Sync {
    async fn focused_target(&self) -> Result<FocusedTarget, PlatformError>;
    async fn refocus(&self, target: &FocusedTarget) -> Result<(), PlatformError>;
}

#[async_trait]
pub trait TextOutput: Send + Sync {
    async fn insert(
        &self,
        text: &str,
        method: InsertionMethod,
        key_delay_ms: u64,
    ) -> Result<InsertionResult, PlatformError>;
}

#[async_trait]
pub trait ShortcutService: Send + Sync {
    async fn register_record_shortcut(&self, shortcut: &str) -> Result<(), PlatformError>;
    async fn next_activation(&self) -> Result<(), PlatformError>;
    async fn unregister_all(&self) -> Result<(), PlatformError>;
}

pub trait CapabilityReporter: Send + Sync {
    fn capabilities(&self) -> PlatformCapabilities;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayStatus {
    Idle,
    Recording,
    Processing,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    OpenSettings,
    StartRecording,
    StopRecording,
    QuitGui,
}

#[async_trait]
pub trait TrayService: Send + Sync {
    async fn set_status(&self, status: TrayStatus) -> Result<(), PlatformError>;
    async fn next_action(&self) -> Result<TrayAction, PlatformError>;
}
