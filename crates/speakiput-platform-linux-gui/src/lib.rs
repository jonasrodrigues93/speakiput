//! Linux-specific Slint/winit window integration.

use std::sync::atomic::{AtomicBool, Ordering};

use slint::winit_030::{EventLoopBuilder, SlintEvent, winit};
use winit::platform::x11::{EventLoopBuilderExtX11, WindowAttributesExtX11, WindowType};

static NEXT_WINDOW_IS_OVERLAY: AtomicBool = AtomicBool::new(false);
static OVERLAY_WINDOW_CREATED: AtomicBool = AtomicBool::new(false);

/// Configures the Linux window backend without leaking X11/Wayland details
/// into the portable presentation code.
#[must_use]
pub fn configure_backend(
    selector: slint::BackendSelector,
    window_icon: Option<winit::window::Icon>,
    app_id: &'static str,
) -> slint::BackendSelector {
    let selector = selector.with_winit_window_attributes_hook(move |attributes| {
        let is_overlay = NEXT_WINDOW_IS_OVERLAY.swap(false, Ordering::SeqCst);
        let attributes = attributes.with_window_icon(window_icon.clone());
        let attributes = WindowAttributesExtX11::with_name(attributes, app_id, app_id);
        let attributes = winit::platform::wayland::WindowAttributesExtWayland::with_name(
            attributes, app_id, app_id,
        );
        if is_overlay {
            OVERLAY_WINDOW_CREATED.store(true, Ordering::SeqCst);
            attributes
                .with_override_redirect(true)
                .with_x11_window_type(vec![WindowType::Notification])
        } else {
            attributes
        }
    });

    if should_force_x11(
        std::env::var("XDG_CURRENT_DESKTOP").ok().as_deref(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    ) {
        let mut event_loop: EventLoopBuilder =
            winit::event_loop::EventLoop::<SlintEvent>::with_user_event();
        event_loop.with_x11();
        eprintln!("speakiput: using XWayland for focus-safe GNOME overlay windows");
        selector.with_winit_event_loop_builder(event_loop)
    } else {
        selector
    }
}

/// Marks the next lazily-created native window as the recording overlay.
/// The marker is consumed synchronously by the winit attributes hook.
pub fn prepare_overlay_creation() {
    if !OVERLAY_WINDOW_CREATED.load(Ordering::SeqCst) {
        NEXT_WINDOW_IS_OVERLAY.store(true, Ordering::SeqCst);
    }
}

fn should_force_x11(desktop: Option<&str>, wayland: bool, x11: bool) -> bool {
    wayland && x11 && desktop.is_some_and(|desktop| desktop.to_ascii_lowercase().contains("gnome"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xwayland_is_limited_to_gnome_wayland_with_an_x_display() {
        assert!(should_force_x11(Some("ubuntu:GNOME"), true, true));
        assert!(!should_force_x11(Some("GNOME"), true, false));
        assert!(!should_force_x11(Some("KDE"), true, true));
        assert!(!should_force_x11(Some("GNOME"), false, true));
    }
}
