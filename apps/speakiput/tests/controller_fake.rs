#![allow(clippy::float_cmp)]

use std::sync::Arc;

use serde_json::json;
use speakiput::controller::GuiController;
use speakiput_client::BackendClient;
use speakiput_contract::{
    DictationState, MAX_FRAME_BYTES, ProtocolError, Settings, StableErrorCode,
};
use speakiput_testing::{ScriptedStep, scripted_client};
use uuid::Uuid;

fn bootstrap_steps(settings: &Settings) -> Vec<ScriptedStep> {
    vec![
        ScriptedStep::success(
            "client.hello",
            json!({
                "selected_version": "1.0",
                "server": { "name": "fake", "version": "0.1.0" },
                "capabilities": ["global_shortcut", "keyboard_insertion", "focus_safe_overlay"],
                "limits": { "max_frame_bytes": MAX_FRAME_BYTES }
            }),
        ),
        ScriptedStep::success(
            "state.get",
            json!({
                "state": "idle",
                "active_session_id": null,
                "settings_revision": 4,
                "capabilities": ["global_shortcut", "keyboard_insertion", "focus_safe_overlay"],
                "health": "ready"
            }),
        ),
        ScriptedStep::success(
            "settings.get",
            json!({ "revision": 4, "settings": settings }),
        ),
        ScriptedStep::success(
            "audio_devices.list",
            json!({
                "devices": [{ "id": "default", "name": "Default microphone", "is_default": true }],
                "selected_id": "default"
            }),
        ),
        ScriptedStep::success(
            "backends.list",
            json!({
                "transcription": [{ "id": "local-whisper", "name": "Local Whisper", "available": true, "unavailable_reason": null }],
                "post_processing": [{ "id": "openai-compatible", "name": "OpenAI compatible", "available": true, "unavailable_reason": null }]
            }),
        ),
        ScriptedStep::success(
            "history.list",
            json!({ "entries": [], "next_cursor": null }),
        ),
        ScriptedStep::success(
            "diagnostics.get",
            json!({
                "checks": [{ "id": "backend", "status": "ok", "message": "Ready" }],
                "log_path": null
            }),
        ),
    ]
}

fn controller_with_steps(steps: Vec<ScriptedStep>) -> GuiController {
    let (client, _) = scripted_client(steps);
    let client: Arc<dyn BackendClient> = Arc::new(client);
    GuiController::new(client)
}

#[tokio::test]
async fn complete_script_drives_overlay_from_backend_events() {
    let settings = Settings::default();
    let session = Uuid::new_v4();
    let mut steps = bootstrap_steps(&settings);
    steps.push(
        ScriptedStep::success(
            "recording.start",
            json!({ "session_id": session, "state": "recording" }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "idle", "current": "recording", "session_id": session }),
        )
        .with_event(
            "recording.level",
            json!({ "session_id": session, "level": 0.72 }),
        )
        .with_event(
            "transcript.partial",
            json!({ "session_id": session, "text": "texto parcial" }),
        ),
    );
    steps.push(
        ScriptedStep::success(
            "recording.stop",
            json!({ "session_id": session, "accepted": true }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "recording", "current": "transcribing", "session_id": session }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "transcribing", "current": "post_processing", "session_id": session }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "post_processing", "current": "injecting", "session_id": session }),
        )
        .with_event(
            "transcript.final",
            json!({
                "session_id": session,
                "raw_text": "texto bruto",
                "processed_text": "Texto final.",
                "output_text": "Texto final.",
                "post_processed": true,
                "insertion": { "status": "inserted", "method": "keyboard" },
                "transcription_backend": "local-whisper",
                "language": "pt",
                "duration_ms": 920
            }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "injecting", "current": "idle", "session_id": session }),
        ),
    );

    let mut controller = controller_with_steps(steps);
    controller.bootstrap().await.unwrap();
    assert_eq!(controller.model.settings, settings);
    assert_eq!(controller.model.settings_revision, 4);
    assert_eq!(controller.model.state, DictationState::Idle);
    assert_eq!(controller.model.audio_devices.len(), 1);
    assert_eq!(controller.model.diagnostics.len(), 1);

    controller.start_or_stop().await.unwrap();
    for _ in 0..3 {
        controller.next_event().await.unwrap();
    }
    assert_eq!(controller.model.state, DictationState::Recording);
    assert_eq!(controller.model.partial_text, "texto parcial");
    assert!((controller.model.audio_level - 0.72).abs() < f32::EPSILON);

    controller.start_or_stop().await.unwrap();
    for _ in 0..5 {
        controller.next_event().await.unwrap();
    }
    assert_eq!(controller.model.state, DictationState::Idle);
    assert!(controller.model.partial_text.is_empty());
    assert_eq!(controller.model.audio_level, 0.0);
}

#[tokio::test]
async fn stale_settings_conflict_is_preserved_for_the_ui() {
    let settings = Settings::default();
    let mut steps = bootstrap_steps(&settings);
    steps.push(ScriptedStep::error(
        "settings.replace",
        ProtocolError {
            code: StableErrorCode::Conflict,
            message: "settings revision is stale".into(),
            retryable: true,
            details: serde_json::Map::new(),
        },
    ));
    let mut updated = settings.clone();
    updated.general.language = "en".into();
    steps.push(ScriptedStep::success(
        "settings.get",
        json!({ "revision": 5, "settings": updated }),
    ));
    let mut controller = controller_with_steps(steps);
    controller.bootstrap().await.unwrap();
    let error = controller
        .save_settings(controller.model.settings.clone())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("settings revision is stale"));
    assert_eq!(controller.model.settings_revision, 4);
    assert_eq!(
        controller.model.settings_error_field.as_deref(),
        Some("settings")
    );
    controller.reload_settings().await.unwrap();
    assert_eq!(controller.model.settings_revision, 5);
    assert_eq!(controller.model.settings.general.language, "en");
}
