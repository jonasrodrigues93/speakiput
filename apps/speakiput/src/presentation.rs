use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use speakiput_contract::{DictationState, OutputMode, OverlaySize, ScreenAnchor, Settings};

use crate::{AppTray, RecordingOverlay, SettingsWindow, UiState, view_model::GuiViewModel};

#[allow(clippy::too_many_lines)]
pub fn apply_model(
    window: &SettingsWindow,
    overlay: &RecordingOverlay,
    tray: &AppTray,
    model: &GuiViewModel,
    apply_settings: bool,
) -> Result<(), slint::PlatformError> {
    let state = ui_state(model.state);
    let status = status_text(model);
    window.set_backend_state(state);
    window.set_status_text(status.clone().into());
    window.set_save_error_field(
        model
            .settings_error_field
            .clone()
            .unwrap_or_default()
            .into(),
    );
    window.set_save_error(
        model
            .settings_error_message
            .clone()
            .or_else(|| {
                model.settings_stale.then(|| {
                    "Settings changed in another process; saving will require a reload.".into()
                })
            })
            .unwrap_or_default()
            .into(),
    );
    window.set_settings_stale(model.settings_stale);
    if apply_settings {
        apply_settings_model(window, model);
    }
    window.set_input_device_options(string_model(model.audio_devices.iter().map(|device| {
        if device.is_default {
            format!("{} — {} (default)", device.id, device.name)
        } else {
            format!("{} — {}", device.id, device.name)
        }
    })));
    window.set_transcription_backend_options(string_model(
        model
            .transcription_backends
            .iter()
            .filter(|choice| choice.available)
            .map(|choice| choice.id.clone()),
    ));
    window.set_post_processing_backend_options(string_model(
        model
            .post_processing_backends
            .iter()
            .filter(|choice| choice.available)
            .map(|choice| choice.id.clone()),
    ));
    window.set_shortcut_supported(
        model
            .capabilities
            .iter()
            .any(|item| item == "global_shortcut"),
    );
    window.set_keyboard_insertion_supported(
        model
            .capabilities
            .iter()
            .any(|item| item == "keyboard_insertion"),
    );
    window.set_credential_store_supported(
        model
            .capabilities
            .iter()
            .any(|item| item == "credential_store"),
    );
    let overlay_supported = model
        .capabilities
        .iter()
        .any(|item| item == "focus_safe_overlay");
    window.set_overlay_supported(overlay_supported);
    window.set_history_items(string_model(
        model
            .history
            .iter()
            .map(|entry| format!("{}  {}", entry.created_at, entry.output_text)),
    ));
    window.set_diagnostic_items(string_model(
        model
            .diagnostics
            .iter()
            .map(|check| format!("{} · {} — {}", check.status, check.id, check.message)),
    ));
    window.set_diagnostic_log_path(
        model
            .diagnostics_log_path
            .clone()
            .unwrap_or_default()
            .into(),
    );

    overlay.set_backend_state(state);
    overlay.set_audio_level(model.audio_level);
    overlay.set_size_preset(size_label(model.settings.overlay.size).into());

    tray.set_status_text(format!("speakiput — {status}").into());
    tray.set_recording(model.state == DictationState::Recording);

    let overlay_active = overlay_supported
        && model.settings.overlay.enabled
        && model.active_session_id.is_some()
        && matches!(
            model.state,
            DictationState::Recording
                | DictationState::Transcribing
                | DictationState::PostProcessing
                | DictationState::Injecting
                | DictationState::Error
        );
    if overlay_active {
        overlay.show()?;
        overlay.set_presented(true);
        position_overlay(window, overlay, model.settings.overlay.screen_anchor);
    } else if overlay.get_presented() {
        overlay.hide()?;
        overlay.set_presented(false);
    }
    Ok(())
}

fn position_overlay(settings: &SettingsWindow, overlay: &RecordingOverlay, anchor: ScreenAnchor) {
    let monitor = settings
        .window()
        .with_winit_window(winit::window::Window::current_monitor)
        .flatten()
        .or_else(|| {
            overlay
                .window()
                .with_winit_window(winit::window::Window::current_monitor)
                .flatten()
        });
    let Some(monitor) = monitor else {
        return;
    };
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let _ = overlay.window().with_winit_window(|window| {
        window.set_window_level(winit::window::WindowLevel::AlwaysOnTop);
        let _ = window.set_cursor_hittest(false);
        let window_size = window.outer_size();
        let (x, y) = anchored_coordinates(
            monitor_position.x,
            monitor_position.y,
            monitor_size.width,
            monitor_size.height,
            window_size.width,
            window_size.height,
            anchor,
            24,
        );
        window.set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
    });
}

#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn anchored_coordinates(
    screen_x: i32,
    screen_y: i32,
    screen_width: u32,
    screen_height: u32,
    window_width: u32,
    window_height: u32,
    anchor: ScreenAnchor,
    margin: i32,
) -> (i32, i32) {
    let screen_width = i64::from(screen_width);
    let screen_height = i64::from(screen_height);
    let window_width = i64::from(window_width.min(u32::try_from(screen_width).unwrap_or(u32::MAX)));
    let window_height =
        i64::from(window_height.min(u32::try_from(screen_height).unwrap_or(u32::MAX)));
    let margin = i64::from(margin.max(0));
    let left = i64::from(screen_x) + margin;
    let top = i64::from(screen_y) + margin;
    let right = i64::from(screen_x) + screen_width - window_width - margin;
    let bottom = i64::from(screen_y) + screen_height - window_height - margin;
    let center_x = i64::from(screen_x) + (screen_width - window_width) / 2;
    let center_y = i64::from(screen_y) + (screen_height - window_height) / 2;
    let (x, y) = match anchor {
        ScreenAnchor::TopLeft => (left, top),
        ScreenAnchor::TopCenter => (center_x, top),
        ScreenAnchor::TopRight => (right, top),
        ScreenAnchor::CenterLeft => (left, center_y),
        ScreenAnchor::Center => (center_x, center_y),
        ScreenAnchor::CenterRight => (right, center_y),
        ScreenAnchor::BottomLeft => (left, bottom),
        ScreenAnchor::BottomCenter => (center_x, bottom),
        ScreenAnchor::BottomRight => (right, bottom),
    };
    (
        i32::try_from(x.max(i64::from(screen_x))).unwrap_or(screen_x),
        i32::try_from(y.max(i64::from(screen_y))).unwrap_or(screen_y),
    )
}

fn apply_settings_model(window: &SettingsWindow, model: &GuiViewModel) {
    window.set_language(model.settings.general.language.clone().into());
    window.set_auto_stop_ms(to_i32(model.settings.general.auto_stop_ms));
    window.set_history_enabled(model.settings.general.history_enabled);
    let selected_device = model
        .audio_devices
        .iter()
        .find(|device| device.id == model.settings.audio.input_device_id)
        .map_or_else(
            || model.settings.audio.input_device_id.clone(),
            device_label,
        );
    window.set_input_device(selected_device.into());
    window.set_phrase_silence_ms(to_i32(model.settings.audio.phrase_silence_ms));
    window.set_transcription_backend(model.settings.transcription.backend_id.clone().into());
    window.set_transcription_model(model.settings.transcription.model_id.clone().into());
    window.set_transcription_model_path(
        model
            .settings
            .transcription
            .model_path
            .clone()
            .unwrap_or_default()
            .into(),
    );
    window.set_transcription_prompt(
        model
            .settings
            .transcription
            .prompt
            .clone()
            .unwrap_or_default()
            .into(),
    );
    window.set_transcription_vocabulary(model.settings.transcription.vocabulary.join(", ").into());
    window.set_remove_filler_words(model.settings.transcription.remove_filler_words);
    window.set_filler_words(model.settings.transcription.filler_words.join(", ").into());
    window.set_post_processing_enabled(model.settings.post_processing.enabled);
    window.set_post_processing_backend(model.settings.post_processing.backend_id.clone().into());
    window.set_post_processing_model(model.settings.post_processing.model_id.clone().into());
    window.set_post_processing_endpoint(model.settings.post_processing.endpoint.clone().into());
    window.set_post_processing_credential_id(
        model
            .settings
            .post_processing
            .credential_id
            .clone()
            .unwrap_or_default()
            .into(),
    );
    window.set_post_processing_credential_secret("".into());
    window
        .set_post_processing_instruction(model.settings.post_processing.instruction.clone().into());
    window.set_overlay_enabled(model.settings.overlay.enabled);
    window.set_partial_transcript_enabled(model.settings.overlay.show_partial_transcript);
    window.set_overlay_size(size_label(model.settings.overlay.size).into());
    window.set_overlay_position(anchor_label(model.settings.overlay.screen_anchor).into());
    window.set_shortcut(model.settings.shortcut.record.clone().into());
    window.set_output_mode(output_label(model.settings.output.mode).into());
    window.set_key_delay_ms(to_i32(model.settings.output.key_delay_ms));
}

fn device_label(device: &speakiput_contract::AudioDevice) -> String {
    if device.is_default {
        format!("{} — {} (default)", device.id, device.name)
    } else {
        format!("{} — {}", device.id, device.name)
    }
}

#[must_use]
pub fn settings_from_window(window: &SettingsWindow) -> Settings {
    let mut settings = Settings::default();
    settings.general.language = window.get_language().to_string();
    settings.general.auto_stop_ms = nonnegative(window.get_auto_stop_ms());
    settings.general.history_enabled = window.get_history_enabled();
    settings.audio.input_device_id = selected_id(&window.get_input_device());
    settings.audio.phrase_silence_ms = nonnegative(window.get_phrase_silence_ms());
    settings.transcription.backend_id = window.get_transcription_backend().to_string();
    settings.transcription.model_id = window.get_transcription_model().to_string();
    settings.transcription.model_path = nonempty(window.get_transcription_model_path().to_string());
    settings.transcription.prompt = nonempty(window.get_transcription_prompt().to_string());
    settings.transcription.vocabulary = window
        .get_transcription_vocabulary()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect();
    settings.transcription.remove_filler_words = window.get_remove_filler_words();
    settings.transcription.filler_words = window
        .get_filler_words()
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect();
    settings.post_processing.enabled = window.get_post_processing_enabled();
    settings.post_processing.backend_id = window.get_post_processing_backend().to_string();
    settings.post_processing.model_id = window.get_post_processing_model().to_string();
    settings.post_processing.endpoint = window.get_post_processing_endpoint().to_string();
    settings.post_processing.credential_id =
        nonempty(window.get_post_processing_credential_id().to_string());
    settings.post_processing.instruction = window.get_post_processing_instruction().to_string();
    settings.overlay.enabled = window.get_overlay_enabled();
    settings.overlay.show_partial_transcript = window.get_partial_transcript_enabled();
    settings.overlay.size = parse_size(&window.get_overlay_size());
    settings.overlay.screen_anchor = parse_anchor(&window.get_overlay_position());
    settings.shortcut.record = window.get_shortcut().to_string();
    settings.output.mode = parse_output(&window.get_output_mode());
    settings.output.key_delay_ms = nonnegative(window.get_key_delay_ms());
    settings
}

#[must_use]
pub fn credential_secret_from_window(window: &SettingsWindow) -> Option<String> {
    nonempty(window.get_post_processing_credential_secret().to_string())
}

#[must_use]
pub const fn ui_state(state: DictationState) -> UiState {
    match state {
        DictationState::Starting => UiState::Starting,
        DictationState::Idle | DictationState::ShuttingDown => UiState::Idle,
        DictationState::Recording => UiState::Recording,
        DictationState::Transcribing => UiState::Transcribing,
        DictationState::PostProcessing => UiState::PostProcessing,
        DictationState::Injecting => UiState::Injecting,
        DictationState::Error => UiState::Error,
    }
}

#[must_use]
pub fn status_text(model: &GuiViewModel) -> String {
    if let Some(error) = &model.error {
        return error.clone();
    }
    match model.state {
        DictationState::Starting => "Connecting…",
        DictationState::Idle => "Ready",
        DictationState::Recording => "Listening…",
        DictationState::Transcribing => "Transcribing…",
        DictationState::PostProcessing => "Cleaning text…",
        DictationState::Injecting => "Inserting text…",
        DictationState::Error => "Backend error",
        DictationState::ShuttingDown => "Backend shutting down…",
    }
    .into()
}

fn string_model(items: impl IntoIterator<Item = String>) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        items
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}

fn nonnegative(value: i32) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn to_i32(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

fn nonempty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn selected_id(value: &str) -> String {
    value.split(" — ").next().unwrap_or(value).to_owned()
}

const fn size_label(value: OverlaySize) -> &'static str {
    match value {
        OverlaySize::Small => "Small",
        OverlaySize::Medium => "Medium",
        OverlaySize::Large => "Large",
    }
}

fn parse_size(value: &str) -> OverlaySize {
    match value {
        "Small" => OverlaySize::Small,
        "Large" => OverlaySize::Large,
        _ => OverlaySize::Medium,
    }
}

const fn anchor_label(value: ScreenAnchor) -> &'static str {
    match value {
        ScreenAnchor::TopLeft => "Top left",
        ScreenAnchor::TopCenter => "Top center",
        ScreenAnchor::TopRight => "Top right",
        ScreenAnchor::CenterLeft => "Center left",
        ScreenAnchor::Center => "Center",
        ScreenAnchor::CenterRight => "Center right",
        ScreenAnchor::BottomLeft => "Bottom left",
        ScreenAnchor::BottomCenter => "Bottom center",
        ScreenAnchor::BottomRight => "Bottom right",
    }
}

fn parse_anchor(value: &str) -> ScreenAnchor {
    match value {
        "Top left" => ScreenAnchor::TopLeft,
        "Top center" => ScreenAnchor::TopCenter,
        "Top right" => ScreenAnchor::TopRight,
        "Center left" => ScreenAnchor::CenterLeft,
        "Center" => ScreenAnchor::Center,
        "Center right" => ScreenAnchor::CenterRight,
        "Bottom left" => ScreenAnchor::BottomLeft,
        "Bottom right" => ScreenAnchor::BottomRight,
        _ => ScreenAnchor::BottomCenter,
    }
}

const fn output_label(value: OutputMode) -> &'static str {
    match value {
        OutputMode::Keyboard => "Keyboard",
        OutputMode::Clipboard => "Clipboard",
        OutputMode::None => "None",
    }
}

fn parse_output(value: &str) -> OutputMode {
    match value {
        "Clipboard" => OutputMode::Clipboard,
        "None" => OutputMode::None,
        _ => OutputMode::Keyboard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_anchor_stays_inside_the_display() {
        for anchor in [
            ScreenAnchor::TopLeft,
            ScreenAnchor::TopCenter,
            ScreenAnchor::TopRight,
            ScreenAnchor::CenterLeft,
            ScreenAnchor::Center,
            ScreenAnchor::CenterRight,
            ScreenAnchor::BottomLeft,
            ScreenAnchor::BottomCenter,
            ScreenAnchor::BottomRight,
        ] {
            let (x, y) = anchored_coordinates(100, 50, 1920, 1080, 520, 168, anchor, 24);
            assert!((100..=1500).contains(&x), "{anchor:?}: x={x}");
            assert!((50..=962).contains(&y), "{anchor:?}: y={y}");
        }
    }

    #[test]
    fn oversized_overlay_is_clamped_to_display_origin() {
        assert_eq!(
            anchored_coordinates(80, 40, 320, 200, 900, 500, ScreenAnchor::BottomRight, 24),
            (80, 40)
        );
    }
}
