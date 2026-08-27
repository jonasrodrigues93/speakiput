use std::path::Path;

use i_slint_backend_testing::{TestingBackend, TestingBackendOptions};
use image::{ImageBuffer, Rgba};
use slint::ComponentHandle;
use speakiput::{RecordingOverlay, UiState};

#[test]
fn render_recording_overlay() {
    slint::platform::set_platform(Box::new(TestingBackend::new(TestingBackendOptions {
        mock_time: true,
        threading: false,
        renderer_name: Some("software".into()),
    })))
    .expect("testing platform should initialize");

    let overlay = RecordingOverlay::new().expect("overlay should initialize");
    overlay.set_size_preset("Small".into());
    overlay.set_backend_state(UiState::Recording);
    overlay.set_audio_level(0.72);
    overlay.show().expect("overlay should show");
    overlay.window().request_redraw();
    let frame = overlay
        .window()
        .take_snapshot()
        .expect("overlay should render to an image");
    let image = ImageBuffer::<Rgba<u8>, _>::from_raw(
        frame.width(),
        frame.height(),
        frame.as_bytes().to_vec(),
    )
    .expect("overlay dimensions should match its pixel buffer");
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
    std::fs::create_dir_all(&output).expect("snapshot directory should be created");
    image
        .save(output.join("recording-overlay.png"))
        .expect("overlay snapshot should be written");

    overlay.set_backend_state(UiState::PostProcessing);
    overlay.window().request_redraw();
    let processing_frame = overlay
        .window()
        .take_snapshot()
        .expect("processing overlay should render to an image");
    let processing_image = ImageBuffer::<Rgba<u8>, _>::from_raw(
        processing_frame.width(),
        processing_frame.height(),
        processing_frame.as_bytes().to_vec(),
    )
    .expect("processing overlay dimensions should match its pixel buffer");
    processing_image
        .save(output.join("processing-overlay.png"))
        .expect("processing overlay snapshot should be written");
}
