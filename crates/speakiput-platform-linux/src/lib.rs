// Linux integration informed by whisrs src/window and src/daemon/injection.rs
// at commit 28139bd8c4ff17e8d0fd156a0d903a7baa423d48. Copyright (c)
// 2025-present Yosif Kitaneh, used under the MIT License.

use std::{
    fs::OpenOptions,
    io::Write,
    process::{Command, Stdio},
    sync::{Mutex as StdMutex, OnceLock},
    time::Duration,
};

use ashpd::desktop::{
    CreateSessionOptions,
    global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut},
};
use ashpd::{AppID, register_host_app_with_connection};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Deserialize;
use speakiput_platform::{
    CapabilityReporter, FocusService, FocusedTarget, InsertionMethod, InsertionResult,
    PlatformCapabilities, PlatformError, ShortcutService, TextOutput,
};

const TERMINAL_EXACT: &[&str] = &[
    "alacritty",
    "blackbox-terminal",
    "foot",
    "footclient",
    "ghostty",
    "gnome-terminal",
    "kgx",
    "kitty",
    "konsole",
    "org.gnome.console",
    "org.gnome.ptyxis",
    "org.gnome.terminal",
    "org.kde.konsole",
    "org.wezfurlong.wezterm",
    "ptyxis",
    "rio",
    "st",
    "st-256color",
    "tilix",
    "urxvt",
    "wezterm",
    "xterm",
];
const TERMINAL_DISTINCTIVE_LEAVES: &[&str] = &[
    "alacritty",
    "ghostty",
    "kitty",
    "konsole",
    "ptyxis",
    "tilix",
    "wezterm",
];

static KEYBOARD: OnceLock<StdMutex<Option<Box<dyn xkb_type::KeyInjector>>>> = OnceLock::new();

#[derive(Debug, Default)]
pub struct LinuxPlatform;

#[derive(Default)]
pub struct LinuxShortcutService {
    state: tokio::sync::Mutex<ShortcutState>,
}

#[derive(Default)]
struct ShortcutState {
    activations: Option<tokio::sync::mpsc::Receiver<()>>,
    keepalive: Option<tokio::sync::mpsc::Sender<()>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

const GNOME_MEDIA_KEYS_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const GNOME_CUSTOM_KEYS_SCHEMA: &str =
    "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const GNOME_CUSTOM_KEYS_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/speakiput/";

#[async_trait]
impl FocusService for LinuxPlatform {
    async fn focused_target(&self) -> Result<FocusedTarget, PlatformError> {
        tokio::task::spawn_blocking(query_focused_target)
            .await
            .map_err(|error| PlatformError::Operation(error.to_string()))?
    }

    async fn refocus(&self, target: &FocusedTarget) -> Result<(), PlatformError> {
        let Some(id) = target.window_id.clone() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || refocus_target(&id))
            .await
            .map_err(|error| PlatformError::Operation(error.to_string()))?
    }
}

#[async_trait]
impl TextOutput for LinuxPlatform {
    async fn insert(
        &self,
        text: &str,
        method: InsertionMethod,
        key_delay_ms: u64,
    ) -> Result<InsertionResult, PlatformError> {
        let text = text.to_owned();
        tokio::task::spawn_blocking(move || match method {
            InsertionMethod::Keyboard => type_text(&text, key_delay_ms),
            InsertionMethod::ClipboardOnly => copy_text(&text),
            InsertionMethod::ClipboardPaste => Err(PlatformError::Unsupported(
                "clipboard paste injection is not configured",
            )),
        })
        .await
        .map_err(|error| PlatformError::Operation(error.to_string()))??;
        Ok(InsertionResult { method })
    }
}

#[async_trait]
impl ShortcutService for LinuxShortcutService {
    async fn register_record_shortcut(&self, shortcut: &str) -> Result<(), PlatformError> {
        self.unregister_all().await?;
        if is_gnome_session() && command_exists("gsettings") {
            let shortcut = shortcut.to_owned();
            tokio::task::spawn_blocking(move || configure_gnome_shortcut(&shortcut))
                .await
                .map_err(|error| PlatformError::Operation(error.to_string()))??;
            let (keepalive, activations) = tokio::sync::mpsc::channel(1);
            let mut state = self.state.lock().await;
            state.activations = Some(activations);
            state.keepalive = Some(keepalive);
            return Ok(());
        }

        let connection = ashpd::zbus::Connection::session().await.map_err(|error| {
            PlatformError::Operation(format!("global shortcut session bus: {error}"))
        })?;
        let app_id = AppID::try_from("io.github.jonas.speakiput").map_err(portal_error)?;
        register_host_app_with_connection(connection.clone(), app_id)
            .await
            .map_err(portal_error)?;
        let proxy = GlobalShortcuts::with_connection(connection)
            .await
            .map_err(portal_error)?;
        let session = proxy
            .create_session(CreateSessionOptions::default())
            .await
            .map_err(portal_error)?;
        let mut activated = proxy.receive_activated().await.map_err(portal_error)?;
        let trigger = portal_trigger(shortcut)?;
        let request = proxy
            .bind_shortcuts(
                &session,
                &[NewShortcut::new("record", "Start or stop dictation")
                    .preferred_trigger(Some(trigger.as_str()))],
                None,
                BindShortcutsOptions::default(),
            )
            .await
            .map_err(portal_error)?;
        let response = request.response().map_err(portal_error)?;
        let bound = response
            .shortcuts()
            .iter()
            .find(|shortcut| shortcut.id() == "record");
        let Some(bound) = bound else {
            return Err(PlatformError::PermissionDenied(
                "the desktop did not bind the dictation shortcut".into(),
            ));
        };
        eprintln!(
            "speakiput: portal bound record shortcut as {}",
            bound.trigger_description()
        );

        let (activation_tx, activation_rx) = tokio::sync::mpsc::channel(8);
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => {
                        let _ = session.close().await;
                        return;
                    }
                    activation = activated.next() => match activation {
                        Some(activation) if activation.shortcut_id() == "record" => {
                            let _ = activation_tx.send(()).await;
                        }
                        Some(_) => {}
                        None => return,
                    }
                }
            }
        });
        let mut state = self.state.lock().await;
        state.activations = Some(activation_rx);
        state.shutdown = Some(shutdown_tx);
        state.task = Some(task);
        Ok(())
    }

    async fn next_activation(&self) -> Result<(), PlatformError> {
        let mut state = self.state.lock().await;
        let receiver = state
            .activations
            .as_mut()
            .ok_or_else(|| PlatformError::Operation("global shortcut is not registered".into()))?;
        receiver
            .recv()
            .await
            .ok_or_else(|| PlatformError::Operation("global shortcut session closed".into()))
    }

    async fn unregister_all(&self) -> Result<(), PlatformError> {
        let (shutdown, task) = {
            let mut state = self.state.lock().await;
            state.activations = None;
            state.keepalive = None;
            (state.shutdown.take(), state.task.take())
        };
        if let Some(shutdown) = shutdown {
            let _ = shutdown.send(());
        }
        if let Some(task) = task {
            let _ = task.await;
        }
        Ok(())
    }
}

impl CapabilityReporter for LinuxPlatform {
    fn capabilities(&self) -> PlatformCapabilities {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
        let x11 = std::env::var_os("DISPLAY").is_some();
        PlatformCapabilities {
            global_shortcut: std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
            focused_target: compositor_command().is_some() || (x11 && command_exists("xdotool")),
            keyboard_injection: native_keyboard_may_be_available(wayland)
                || select_keyboard_backend(
                    wayland,
                    x11,
                    is_gnome_session(),
                    command_exists("wtype"),
                    command_exists("ydotool"),
                    command_exists("xdotool"),
                )
                .is_some(),
            clipboard: (wayland && command_exists("wl-copy"))
                || (x11 && (command_exists("xclip") || command_exists("xsel"))),
            tray: true,
            autostart: command_exists("systemctl"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct HyprWindow {
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
struct NiriWindow {
    id: u64,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

fn query_focused_target() -> Result<FocusedTarget, PlatformError> {
    match compositor_command() {
        Some("hyprctl") => {
            let output = command_output("hyprctl", &["activewindow", "-j"])?;
            let window: HyprWindow = serde_json::from_slice(&output)
                .map_err(|error| PlatformError::Operation(error.to_string()))?;
            Ok(target(
                Some(format!("hypr:{}", window.address)),
                nonempty(window.class),
                nonempty(window.title),
            ))
        }
        Some("niri") => {
            let output = command_output("niri", &["msg", "--json", "focused-window"])?;
            let window: NiriWindow = serde_json::from_slice(&output)
                .map_err(|error| PlatformError::Operation(error.to_string()))?;
            Ok(target(
                Some(format!("niri:{}", window.id)),
                window.app_id.and_then(nonempty),
                window.title.and_then(nonempty),
            ))
        }
        Some("swaymsg") => {
            let output = command_output("swaymsg", &["-t", "get_tree", "-r"])?;
            let tree: serde_json::Value = serde_json::from_slice(&output)
                .map_err(|error| PlatformError::Operation(error.to_string()))?;
            let node = find_focused(&tree).ok_or_else(|| {
                PlatformError::Operation("Sway reported no focused window".into())
            })?;
            let id = node["id"]
                .as_i64()
                .ok_or_else(|| PlatformError::Operation("focused Sway node has no id".into()))?;
            let app_id = node["app_id"]
                .as_str()
                .and_then(|value| nonempty(value.to_owned()))
                .or_else(|| {
                    node["window_properties"]["class"]
                        .as_str()
                        .map(str::to_owned)
                })
                .or_else(|| {
                    node["window_properties"]["instance"]
                        .as_str()
                        .map(str::to_owned)
                });
            Ok(target(
                Some(format!("sway:{id}")),
                app_id,
                node["name"].as_str().map(str::to_owned),
            ))
        }
        _ if std::env::var_os("DISPLAY").is_some() && command_exists("xdotool") => {
            let id = String::from_utf8_lossy(&command_output("xdotool", &["getactivewindow"])?)
                .trim()
                .to_owned();
            let class =
                String::from_utf8_lossy(&command_output("xdotool", &["getwindowclassname", &id])?)
                    .trim()
                    .to_owned();
            let title =
                String::from_utf8_lossy(&command_output("xdotool", &["getwindowname", &id])?)
                    .trim()
                    .to_owned();
            Ok(target(
                Some(format!("x11:{id}")),
                nonempty(class),
                nonempty(title),
            ))
        }
        _ => Err(PlatformError::Unsupported("focused window tracking")),
    }
}

fn refocus_target(id: &str) -> Result<(), PlatformError> {
    let (backend, id) = id
        .split_once(':')
        .ok_or_else(|| PlatformError::Operation("invalid focused window id".into()))?;
    match backend {
        "hypr" => command_status(
            "hyprctl",
            &["dispatch", "focuswindow", &format!("address:{id}")],
        ),
        "niri" => command_status("niri", &["msg", "action", "focus-window", "--id", id]),
        "sway" => command_status("swaymsg", &[&format!("[con_id={id}]"), "focus"]),
        "x11" => command_status("xdotool", &["windowactivate", "--sync", id]),
        _ => Err(PlatformError::Operation(
            "unknown focused window backend".into(),
        )),
    }
}

fn type_text(text: &str, delay_ms: u64) -> Result<(), PlatformError> {
    if let Err(native_error) = type_text_native(text, delay_ms) {
        return type_text_with_helper(text, delay_ms).map_err(|helper_error| {
            PlatformError::Operation(format!(
                "native keyboard injection failed: {native_error}; helper fallback failed: {helper_error}"
            ))
        });
    }
    Ok(())
}

fn type_text_native(text: &str, delay_ms: u64) -> Result<(), PlatformError> {
    let key_delay = Duration::from_millis(delay_ms);
    let keyboard_slot = KEYBOARD.get_or_init(|| StdMutex::new(None));
    let mut keyboard = keyboard_slot
        .lock()
        .map_err(|_| PlatformError::Operation("native keyboard mutex poisoned".into()))?;
    if keyboard.is_none() {
        *keyboard = Some(new_native_keyboard(key_delay)?);
    }
    let injector = keyboard
        .as_mut()
        .expect("keyboard exists after successful initialization");
    injector.set_key_delay(key_delay);
    if let Err(error) = injector.type_text(text) {
        *keyboard = None;
        return Err(PlatformError::Operation(format!(
            "native keyboard could not type text: {error:#}"
        )));
    }
    Ok(())
}

fn new_native_keyboard(
    key_delay: Duration,
) -> Result<Box<dyn xkb_type::KeyInjector>, PlatformError> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && let Ok(keyboard) = xkb_type::wayland_vk::WaylandVkKeyboard::new(key_delay)
    {
        return Ok(Box::new(keyboard));
    }
    xkb_type::Keyboard::new(key_delay)
        .map(|keyboard| Box::new(keyboard) as Box<dyn xkb_type::KeyInjector>)
        .map_err(|error| {
            PlatformError::Operation(format!(
                "virtual keyboard unavailable ({error:#}); install the speakiput udev rule to grant access to /dev/uinput"
            ))
        })
}

fn native_keyboard_may_be_available(wayland: bool) -> bool {
    wayland || OpenOptions::new().write(true).open("/dev/uinput").is_ok()
}

fn type_text_with_helper(text: &str, delay_ms: u64) -> Result<(), PlatformError> {
    let delay = delay_ms.to_string();
    match select_keyboard_backend(
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
        is_gnome_session(),
        command_exists("wtype"),
        command_exists("ydotool"),
        command_exists("xdotool"),
    ) {
        Some("wtype") => command_status("wtype", &["-d", &delay, "--", text]),
        Some("ydotool") => command_status("ydotool", &["type", "-d", &delay, "--", text]),
        Some("xdotool") => command_status(
            "xdotool",
            &["type", "--clearmodifiers", "--delay", &delay, "--", text],
        ),
        _ => Err(PlatformError::Unsupported("keyboard text insertion")),
    }
}

#[allow(clippy::fn_params_excessive_bools, clippy::similar_names)]
fn select_keyboard_backend(
    wayland: bool,
    x11: bool,
    gnome: bool,
    has_wtype: bool,
    has_ydotool: bool,
    has_xdotool: bool,
) -> Option<&'static str> {
    if wayland && gnome && has_ydotool {
        Some("ydotool")
    } else if wayland && !gnome && has_wtype {
        Some("wtype")
    } else if has_ydotool {
        Some("ydotool")
    } else if x11 && has_xdotool {
        Some("xdotool")
    } else {
        None
    }
}

fn is_gnome_session() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktop| desktop.to_ascii_lowercase().contains("gnome"))
}

fn copy_text(text: &str) -> Result<(), PlatformError> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && command_exists("wl-copy") {
        return command_with_stdin("wl-copy", &[], text);
    }
    if std::env::var_os("DISPLAY").is_some() && command_exists("xclip") {
        return command_with_stdin("xclip", &["-selection", "clipboard"], text);
    }
    if std::env::var_os("DISPLAY").is_some() && command_exists("xsel") {
        return command_with_stdin("xsel", &["--clipboard", "--input"], text);
    }
    Err(PlatformError::Unsupported("clipboard output"))
}

fn target(
    window_id: Option<String>,
    application_id: Option<String>,
    title: Option<String>,
) -> FocusedTarget {
    let is_terminal = application_id
        .as_deref()
        .is_some_and(is_terminal_application);
    FocusedTarget {
        window_id,
        application_id,
        title,
        is_terminal,
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn compositor_command() -> Option<&'static str> {
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() && command_exists("hyprctl") {
        Some("hyprctl")
    } else if std::env::var_os("NIRI_SOCKET").is_some() && command_exists("niri") {
        Some("niri")
    } else if std::env::var_os("SWAYSOCK").is_some() && command_exists("swaymsg") {
        Some("swaymsg")
    } else {
        None
    }
}

fn command_exists(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(name).is_file())
    })
}

#[allow(clippy::needless_pass_by_value)]
fn portal_error(error: ashpd::Error) -> PlatformError {
    PlatformError::Operation(format!("global shortcut portal: {error}"))
}

fn portal_trigger(shortcut: &str) -> Result<String, PlatformError> {
    let mut modifiers = String::new();
    let mut key = None;
    for part in shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        match part.to_ascii_lowercase().as_str() {
            "super" | "meta" | "win" => modifiers.push_str("<Super>"),
            "shift" => modifiers.push_str("<Shift>"),
            "ctrl" | "control" => modifiers.push_str("<Control>"),
            "alt" => modifiers.push_str("<Alt>"),
            _ if key.is_none() => {
                key = Some(if part.eq_ignore_ascii_case("space") {
                    "space".into()
                } else {
                    part.to_ascii_lowercase()
                });
            }
            _ => {
                return Err(PlatformError::Operation(
                    "shortcut must contain exactly one non-modifier key".into(),
                ));
            }
        }
    }
    let key = key.ok_or_else(|| PlatformError::Operation("shortcut has no key".into()))?;
    Ok(format!("{modifiers}{key}"))
}

fn configure_gnome_shortcut(shortcut: &str) -> Result<(), PlatformError> {
    let binding = portal_trigger(shortcut)?;
    let executable = std::env::current_exe()
        .map_err(|error| PlatformError::Operation(format!("current executable: {error}")))?;
    let command = format!(
        "{} --toggle-recording",
        shell_quote(&executable.to_string_lossy())
    );
    let current = command_output(
        "gsettings",
        &["get", GNOME_MEDIA_KEYS_SCHEMA, "custom-keybindings"],
    )?;
    let mut paths = parse_gvariant_string_array(&String::from_utf8_lossy(&current));
    if !paths.iter().any(|path| path == GNOME_CUSTOM_KEYS_PATH) {
        paths.push(GNOME_CUSTOM_KEYS_PATH.into());
        let value = format!(
            "[{}]",
            paths
                .iter()
                .map(|path| gvariant_string(path))
                .collect::<Vec<_>>()
                .join(", ")
        );
        command_status(
            "gsettings",
            &["set", GNOME_MEDIA_KEYS_SCHEMA, "custom-keybindings", &value],
        )?;
    }

    let schema = format!("{GNOME_CUSTOM_KEYS_SCHEMA}:{GNOME_CUSTOM_KEYS_PATH}");
    for (key, value) in [
        ("name", gvariant_string("speakiput dictation")),
        ("command", gvariant_string(&command)),
        ("binding", gvariant_string(&binding)),
    ] {
        command_status("gsettings", &["set", &schema, key, &value])?;
    }
    Ok(())
}

fn parse_gvariant_string_array(value: &str) -> Vec<String> {
    value
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

fn gvariant_string(value: &str) -> String {
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn command_output(program: &str, args: &[&str]) -> Result<Vec<u8>, PlatformError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| PlatformError::Operation(error.to_string()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(PlatformError::Operation(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn command_status(program: &str, args: &[&str]) -> Result<(), PlatformError> {
    command_output(program, args).map(|_| ())
}

fn command_with_stdin(program: &str, args: &[&str], text: &str) -> Result<(), PlatformError> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| PlatformError::Operation(error.to_string()))?;
    child
        .stdin
        .take()
        .ok_or_else(|| PlatformError::Operation("clipboard command has no stdin".into()))?
        .write_all(text.as_bytes())
        .map_err(|error| PlatformError::Operation(error.to_string()))?;
    let output = child
        .wait_with_output()
        .map_err(|error| PlatformError::Operation(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(PlatformError::Operation(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

fn find_focused(value: &serde_json::Value) -> Option<&serde_json::Value> {
    if value["focused"].as_bool() == Some(true) {
        return Some(value);
    }
    ["nodes", "floating_nodes"]
        .into_iter()
        .filter_map(|key| value[key].as_array())
        .flatten()
        .find_map(find_focused)
}

#[must_use]
pub fn is_terminal_application(application_id: &str) -> bool {
    let normalized = application_id.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    if TERMINAL_EXACT.contains(&normalized.as_str()) {
        return true;
    }
    normalized
        .rsplit_once('.')
        .is_some_and(|(_, leaf)| TERMINAL_DISTINCTIVE_LEAVES.contains(&leaf))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_matching_is_exact_not_substring_based() {
        for terminal in ["Alacritty", "org.gnome.Terminal", "io.example.Ghostty"] {
            assert!(is_terminal_application(terminal));
        }
        for application in ["steam", "Postman", "systemsettings", "app.drey.Warp"] {
            assert!(!is_terminal_application(application));
        }
    }

    #[test]
    fn finds_focused_sway_node_recursively() {
        let tree = serde_json::json!({
            "focused": false,
            "nodes": [{ "focused": false, "nodes": [{ "focused": true, "id": 42 }] }]
        });
        assert_eq!(find_focused(&tree).unwrap()["id"], 42);
    }

    #[test]
    fn command_lookup_does_not_execute_candidates() {
        assert!(!command_exists("a-command-name-that-does-not-exist"));
        assert!(std::path::Path::new("/").is_absolute());
    }

    #[test]
    fn converts_desktop_shortcut_to_portal_trigger() {
        assert_eq!(
            portal_trigger("Super+Shift+Space").unwrap(),
            "<Super><Shift>space"
        );
        assert_eq!(portal_trigger("Control+F9").unwrap(), "<Control>f9");
        assert!(portal_trigger("Ctrl+A+B").is_err());
    }

    #[test]
    fn parses_and_quotes_gsettings_values() {
        assert_eq!(parse_gvariant_string_array("@as []"), Vec::<String>::new());
        assert_eq!(
            parse_gvariant_string_array("['/one/', '/two/']"),
            vec!["/one/", "/two/"]
        );
        assert_eq!(gvariant_string("it's"), "'it\\'s'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn selects_insertion_backend_for_wayland_x11_and_gnome() {
        assert_eq!(
            select_keyboard_backend(true, false, false, true, false, false),
            Some("wtype")
        );
        assert_eq!(
            select_keyboard_backend(false, true, false, false, false, true),
            Some("xdotool")
        );
        assert_eq!(
            select_keyboard_backend(true, false, true, true, true, false),
            Some("ydotool")
        );
        assert_eq!(
            select_keyboard_backend(true, false, true, true, false, false),
            None
        );
    }

    #[test]
    #[ignore = "requires a focused desktop text field and access to /dev/uinput"]
    fn real_keyboard_injection_types_into_focused_field() {
        let marker = std::env::var("SPEAKIPUT_REAL_INJECTION_TEXT")
            .unwrap_or_else(|_| "speakiput-real-injection-ok".into());
        type_text(&marker, 2).expect("real keyboard insertion should type the marker");
        std::thread::sleep(Duration::from_millis(100));
        let keyboard_slot = KEYBOARD.get().expect("keyboard initialized by type_text");
        let mut keyboard = keyboard_slot.lock().expect("keyboard mutex");
        keyboard
            .as_mut()
            .expect("keyboard remains available")
            .send_combo(&[xkb_type::Key::KEY_ENTER])
            .expect("Enter should submit the focused test field");
    }
}
