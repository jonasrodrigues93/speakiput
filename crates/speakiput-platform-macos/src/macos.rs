use std::{
    collections::HashSet,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use arboard::Clipboard;
use async_trait::async_trait;
use enigo::{Direction, Enigo, Keyboard, Settings};
use macos_accessibility_client::accessibility::{
    application_is_trusted, application_is_trusted_with_prompt,
};
use rdev::{Event, EventType, Key};
use speakiput_platform::{
    CapabilityReporter, FocusService, FocusedTarget, InsertionMethod, InsertionResult,
    PlatformCapabilities, PlatformError, ShortcutService, TextOutput,
};

const TERMINALS: &[&str] = &[
    "alacritty",
    "ghostty",
    "hyper",
    "iterm2",
    "kitty",
    "rio",
    "terminal",
    "tabby",
    "warp",
    "wezterm",
];

#[derive(Debug, Default)]
pub struct MacPlatform;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
struct Modifiers {
    command: bool,
    control: bool,
    option: bool,
    shift: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MacShortcut {
    modifiers: Modifiers,
    key: Key,
}

#[derive(Default)]
struct ListenerState {
    binding: Option<MacShortcut>,
    activations: Option<tokio::sync::mpsc::Sender<()>>,
}

pub struct MacShortcutService {
    listener_state: Arc<StdMutex<ListenerState>>,
    listener_started: AtomicBool,
    activations: tokio::sync::Mutex<Option<tokio::sync::mpsc::Receiver<()>>>,
}

impl Default for MacShortcutService {
    fn default() -> Self {
        Self {
            listener_state: Arc::new(StdMutex::new(ListenerState::default())),
            listener_started: AtomicBool::new(false),
            activations: tokio::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl FocusService for MacPlatform {
    async fn focused_target(&self) -> Result<FocusedTarget, PlatformError> {
        tokio::task::spawn_blocking(query_focused_target)
            .await
            .map_err(|error| PlatformError::Operation(error.to_string()))?
    }

    async fn refocus(&self, target: &FocusedTarget) -> Result<(), PlatformError> {
        let Some(window_id) = target.window_id.as_deref() else {
            return Ok(());
        };
        let pid = window_id
            .strip_prefix("macos:")
            .ok_or_else(|| PlatformError::Operation("invalid macOS window id".into()))?
            .parse::<u32>()
            .map_err(|error| PlatformError::Operation(error.to_string()))?;
        tokio::task::spawn_blocking(move || refocus_process(pid))
            .await
            .map_err(|error| PlatformError::Operation(error.to_string()))?
    }
}

#[async_trait]
impl TextOutput for MacPlatform {
    async fn insert(
        &self,
        text: &str,
        method: InsertionMethod,
        key_delay_ms: u64,
    ) -> Result<InsertionResult, PlatformError> {
        let text = text.to_owned();
        tokio::task::spawn_blocking(move || insert_text(&text, method, key_delay_ms))
            .await
            .map_err(|error| PlatformError::Operation(error.to_string()))??;
        Ok(InsertionResult { method })
    }
}

#[async_trait]
impl ShortcutService for MacShortcutService {
    async fn register_record_shortcut(&self, shortcut: &str) -> Result<(), PlatformError> {
        let binding = parse_shortcut(shortcut)?;
        if !application_is_trusted_with_prompt() {
            return Err(PlatformError::PermissionDenied(
                "Accessibility permission is required for global shortcuts and text insertion; enable speakiput in System Settings > Privacy & Security > Accessibility".into(),
            ));
        }
        self.start_listener();
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        {
            let mut state = self
                .listener_state
                .lock()
                .map_err(|_| PlatformError::Operation("shortcut state lock is poisoned".into()))?;
            state.binding = Some(binding);
            state.activations = Some(sender);
        }
        *self.activations.lock().await = Some(receiver);
        Ok(())
    }

    async fn next_activation(&self) -> Result<(), PlatformError> {
        let mut activations = self.activations.lock().await;
        let receiver = activations
            .as_mut()
            .ok_or_else(|| PlatformError::Operation("global shortcut is not registered".into()))?;
        receiver
            .recv()
            .await
            .ok_or_else(|| PlatformError::Operation("global shortcut listener stopped".into()))
    }

    async fn unregister_all(&self) -> Result<(), PlatformError> {
        {
            let mut state = self
                .listener_state
                .lock()
                .map_err(|_| PlatformError::Operation("shortcut state lock is poisoned".into()))?;
            state.binding = None;
            state.activations = None;
        }
        *self.activations.lock().await = None;
        Ok(())
    }
}

impl MacShortcutService {
    fn start_listener(&self) {
        if self.listener_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let state = Arc::clone(&self.listener_state);
        std::thread::Builder::new()
            .name("speakiput-macos-hotkey".into())
            .spawn(move || {
                let mut held = HashSet::new();
                let result = rdev::listen(move |event| handle_event(&event, &state, &mut held));
                if let Err(error) = result {
                    eprintln!("speakiput: macOS global shortcut listener stopped: {error:?}");
                }
            })
            .expect("failed to start macOS global shortcut listener");
    }
}

impl CapabilityReporter for MacPlatform {
    fn capabilities(&self) -> PlatformCapabilities {
        let accessibility = application_is_trusted();
        PlatformCapabilities {
            global_shortcut: accessibility,
            focused_target: accessibility,
            keyboard_injection: accessibility,
            clipboard: Clipboard::new().is_ok(),
            tray: true,
            autostart: false,
        }
    }
}

fn handle_event(event: &Event, state: &Arc<StdMutex<ListenerState>>, held: &mut HashSet<Key>) {
    match &event.event_type {
        EventType::KeyPress(key) => {
            if !held.insert(*key) {
                return;
            }
            let Ok(state) = state.lock() else { return };
            let Some(binding) = state.binding else { return };
            if *key != binding.key || !matches_modifiers(held, binding.modifiers) {
                return;
            }
            if let Some(sender) = &state.activations {
                let _ = sender.try_send(());
            }
        }
        EventType::KeyRelease(key) => {
            held.remove(key);
        }
        _ => {}
    }
}

fn matches_modifiers(held: &HashSet<Key>, required: Modifiers) -> bool {
    let actual = Modifiers {
        command: held.contains(&Key::MetaLeft) || held.contains(&Key::MetaRight),
        control: held.contains(&Key::ControlLeft) || held.contains(&Key::ControlRight),
        option: held.contains(&Key::Alt),
        shift: held.contains(&Key::ShiftLeft) || held.contains(&Key::ShiftRight),
    };
    actual == required
}

fn parse_shortcut(shortcut: &str) -> Result<MacShortcut, PlatformError> {
    let mut modifiers = Modifiers::default();
    let mut key = None;
    for part in shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.to_ascii_lowercase().as_str() {
            "super" | "meta" | "win" | "command" | "cmd" => modifiers.command = true,
            "ctrl" | "control" => modifiers.control = true,
            "alt" | "option" => modifiers.option = true,
            "shift" => modifiers.shift = true,
            _ if key.is_none() => key = Some(parse_key(part)?),
            _ => {
                return Err(PlatformError::Operation(
                    "shortcut must contain exactly one non-modifier key".into(),
                ));
            }
        }
    }
    let key = key.ok_or_else(|| PlatformError::Operation("shortcut has no key".into()))?;
    Ok(MacShortcut { modifiers, key })
}

fn parse_key(value: &str) -> Result<Key, PlatformError> {
    let normalized = value.to_ascii_lowercase();
    let key = match normalized.as_str() {
        "space" => Key::Space,
        "tab" => Key::Tab,
        "return" | "enter" => Key::Return,
        "escape" | "esc" => Key::Escape,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "a" => Key::KeyA,
        "b" => Key::KeyB,
        "c" => Key::KeyC,
        "d" => Key::KeyD,
        "e" => Key::KeyE,
        "f" => Key::KeyF,
        "g" => Key::KeyG,
        "h" => Key::KeyH,
        "i" => Key::KeyI,
        "j" => Key::KeyJ,
        "k" => Key::KeyK,
        "l" => Key::KeyL,
        "m" => Key::KeyM,
        "n" => Key::KeyN,
        "o" => Key::KeyO,
        "p" => Key::KeyP,
        "q" => Key::KeyQ,
        "r" => Key::KeyR,
        "s" => Key::KeyS,
        "t" => Key::KeyT,
        "u" => Key::KeyU,
        "v" => Key::KeyV,
        "w" => Key::KeyW,
        "x" => Key::KeyX,
        "y" => Key::KeyY,
        "z" => Key::KeyZ,
        _ => {
            return Err(PlatformError::Operation(format!(
                "unsupported macOS shortcut key: {value}"
            )));
        }
    };
    Ok(key)
}

fn query_focused_target() -> Result<FocusedTarget, PlatformError> {
    let output = run_osascript(
        r#"tell application "System Events"
set frontProcess to first application process whose frontmost is true
set processId to unix id of frontProcess
set processName to name of frontProcess
set windowTitle to ""
try
    set windowTitle to name of front window of frontProcess
end try
return (processId as text) & linefeed & processName & linefeed & windowTitle
end tell"#,
    )?;
    let mut lines = output.lines();
    let pid = lines
        .next()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .ok_or_else(|| PlatformError::Operation("System Events returned no process id".into()))?;
    let application_id = nonempty(lines.next().unwrap_or_default().to_owned());
    let title = nonempty(lines.next().unwrap_or_default().to_owned());
    Ok(FocusedTarget {
        window_id: Some(format!("macos:{pid}")),
        is_terminal: application_id
            .as_deref()
            .is_some_and(is_terminal_application),
        application_id,
        title,
    })
}

fn refocus_process(pid: u32) -> Result<(), PlatformError> {
    let script = format!(
        "tell application \"System Events\" to set frontmost of (first application process whose unix id is {pid}) to true"
    );
    run_osascript(&script).map(|_| ())
}

fn insert_text(
    text: &str,
    method: InsertionMethod,
    key_delay_ms: u64,
) -> Result<(), PlatformError> {
    match method {
        InsertionMethod::ClipboardOnly => copy_text(text),
        InsertionMethod::Keyboard => {
            let mut enigo = Enigo::new(&Settings {
                open_prompt_to_get_permissions: false,
                ..Settings::default()
            })
            .map_err(|error| PlatformError::PermissionDenied(error.to_string()))?;
            enigo
                .text(text)
                .map_err(|error| PlatformError::Operation(error.to_string()))?;
            if key_delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(key_delay_ms));
            }
            Ok(())
        }
        InsertionMethod::ClipboardPaste => {
            copy_text(text)?;
            let mut enigo = Enigo::new(&Settings {
                open_prompt_to_get_permissions: false,
                ..Settings::default()
            })
            .map_err(|error| PlatformError::PermissionDenied(error.to_string()))?;
            enigo
                .key(enigo::Key::Meta, Direction::Press)
                .and_then(|()| enigo.key(enigo::Key::Unicode('v'), Direction::Click))
                .and_then(|()| enigo.key(enigo::Key::Meta, Direction::Release))
                .map_err(|error| PlatformError::Operation(error.to_string()))
        }
    }
}

fn copy_text(text: &str) -> Result<(), PlatformError> {
    Clipboard::new()
        .and_then(|mut clipboard| clipboard.set_text(text))
        .map_err(|error| PlatformError::Operation(error.to_string()))
}

fn run_osascript(script: &str) -> Result<String, PlatformError> {
    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| PlatformError::Operation(format!("osascript: {error}")))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if message.to_ascii_lowercase().contains("not authorized") {
        Err(PlatformError::PermissionDenied(message))
    } else {
        Err(PlatformError::Operation(message))
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

#[must_use]
pub fn is_terminal_application(application: &str) -> bool {
    let normalized = application.trim().to_ascii_lowercase();
    TERMINALS.contains(&normalized.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mac_command_shortcut() {
        assert_eq!(
            parse_shortcut("Super+Shift+Space").unwrap(),
            MacShortcut {
                modifiers: Modifiers {
                    command: true,
                    shift: true,
                    ..Modifiers::default()
                },
                key: Key::Space,
            }
        );
    }

    #[test]
    fn rejects_ambiguous_shortcut() {
        assert!(parse_shortcut("Control+A+B").is_err());
        assert!(parse_shortcut("Super+Shift").is_err());
    }

    #[test]
    fn matches_only_the_configured_modifier_set() {
        let held = HashSet::from([Key::MetaLeft, Key::ShiftLeft, Key::KeyA]);
        assert!(matches_modifiers(
            &held,
            Modifiers {
                command: true,
                shift: true,
                ..Modifiers::default()
            }
        ));
        assert!(!matches_modifiers(
            &held,
            Modifiers {
                command: true,
                ..Modifiers::default()
            }
        ));
    }

    #[test]
    fn identifies_common_terminal_applications() {
        assert!(is_terminal_application("Terminal"));
        assert!(is_terminal_application("iTerm2"));
        assert!(!is_terminal_application("Safari"));
    }
}
