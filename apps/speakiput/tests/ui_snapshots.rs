use std::path::Path;

use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use image::{ImageBuffer, Rgba};
use slint::{ComponentHandle, ModelRc, PhysicalSize, SharedString, VecModel};
use speakiput::{SettingsPage, SettingsWindow, UiState};

#[test]
fn render_every_settings_section() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .expect("testing platform should initialize");

    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
    std::fs::create_dir_all(&output).expect("snapshot directory should be created");
    let requested = std::env::var("SPEAKIPUT_SNAPSHOT_PAGE").ok();

    for (name, page) in [
        ("01-general", SettingsPage::General),
        ("02-recording", SettingsPage::Recording),
        ("03-audio", SettingsPage::Audio),
        ("04-transcription", SettingsPage::Transcription),
        ("05-post-processing", SettingsPage::PostProcessing),
        ("06-overlay", SettingsPage::Overlay),
        ("07-shortcut", SettingsPage::Shortcut),
        ("08-text-insertion", SettingsPage::Output),
        ("09-history", SettingsPage::History),
        ("10-diagnostics", SettingsPage::Diagnostics),
    ] {
        if requested
            .as_deref()
            .is_some_and(|requested| requested != name)
        {
            continue;
        }
        // Render two windows and alternate their viewport. The testing backend
        // can return only damaged regions for the first instance after a page
        // change, so retain the frame with the most complete window chrome.
        let mut frames = Vec::new();
        for _ in 0..2 {
            let window = SettingsWindow::new().expect("Settings window should initialize");
            window.set_active_page(page);
            configure(&window);
            window.show().expect("Settings window should show");
            window.hide().expect("Settings window should hide");
            window.show().expect("Settings window should show again");
            for width in [941, 940] {
                window.window().set_size(PhysicalSize::new(width, 680));
                window.window().request_redraw();
                let frame = window
                    .window()
                    .take_snapshot()
                    .expect("section should render to an image");
                let bytes = frame.as_bytes().to_vec();
                frames.push((
                    frame.width(),
                    frame.height(),
                    chrome_detail_score(&bytes, frame.width(), frame.height()),
                    bytes,
                ));
            }
        }
        let pixels = frames
            .into_iter()
            .max_by_key(|(_, _, score, _)| *score)
            .expect("at least one complete frame should render");
        let image = ImageBuffer::<Rgba<u8>, _>::from_raw(pixels.0, pixels.1, pixels.3)
            .expect("snapshot dimensions should match its pixel buffer");
        image
            .save(output.join(format!("{name}.png")))
            .expect("snapshot should be written");
    }
}

fn chrome_detail_score(pixels: &[u8], width: u32, height: u32) -> u64 {
    let width = width as usize;
    let height = height as usize;
    let pixel = |x: usize, y: usize| &pixels[(y * width + x) * 4..][..3];
    let mut score = 0;
    for y in 0..height.saturating_sub(1) {
        for x in 0..width.saturating_sub(1) {
            let chrome = if x < 220 {
                !(75..=580).contains(&y)
            } else {
                !(100..=580).contains(&y)
            };
            if !chrome {
                continue;
            }
            score += pixel(x, y)
                .iter()
                .zip(pixel(x + 1, y))
                .chain(pixel(x, y).iter().zip(pixel(x, y + 1)))
                .map(|(left, right)| u64::from(left.abs_diff(*right)))
                .sum::<u64>();
        }
    }
    score
}

fn configure(window: &SettingsWindow) {
    window.window().set_size(PhysicalSize::new(940, 680));
    window.set_backend_state(UiState::Idle);
    window.set_status_text("Ready · Vulkan".into());
    window.set_overlay_supported(true);
    window.set_shortcut_supported(true);
    window.set_keyboard_insertion_supported(true);
    window.set_credential_store_supported(true);
    window.set_transcription_model_path(
        "/mnt/data/ai/models/speechnote/ggml-large-v3-turbo-q5_0.bin".into(),
    );
    window.set_shortcut("Control+F9".into());
    window.set_input_device_options(model(["default — Default microphone"]));
    window.set_transcription_backend_options(model(["local-whisper"]));
    window.set_post_processing_backend_options(model(["openai-compatible"]));
    window.set_history_items(model([
        "2026-08-23  Exemplo de ditado armazenado localmente.",
        "2026-08-22  Segundo item do histórico.",
    ]));
    window.set_diagnostic_items(model([
        "ok · local_whisper — Model loaded",
        "ok · global_shortcut — Control+F9 registered",
        "ok · vulkan_acceleration — AMD Radeon 780M",
    ]));
}

fn model<const N: usize>(values: [&str; N]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        values
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    ))
}
