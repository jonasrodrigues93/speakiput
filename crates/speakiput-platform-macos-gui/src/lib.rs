//! macOS Slint/winit integration.

#[cfg(target_os = "macos")]
use slint::winit_030::winit;

/// Applies the portable window attributes supported by macOS.
#[cfg(target_os = "macos")]
#[must_use]
pub fn configure_backend(
    selector: slint::BackendSelector,
    window_icon: Option<winit::window::Icon>,
    _app_id: &'static str,
) -> slint::BackendSelector {
    selector.with_winit_window_attributes_hook(move |attributes| {
        attributes.with_window_icon(window_icon.clone())
    })
}

/// macOS does not need the Linux overlay creation marker.
#[cfg(target_os = "macos")]
pub fn prepare_overlay_creation() {}
