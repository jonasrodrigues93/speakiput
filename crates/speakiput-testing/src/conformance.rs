use std::time::Duration;

use serde_json::json;
use speakiput_client::{BackendClient, ClientError};
use speakiput_contract::{
    ClientHelloResponse, DictationState, Envelope, RecordingStartResponse, RecordingStopResponse,
    StableErrorCode, StateChangedEvent, TranscriptFinalEvent, decode_frame, encode_frame,
};
use uuid::Uuid;

use crate::{ScriptedStep, scripted_client};

#[allow(clippy::too_many_lines)]
pub async fn run() -> Result<(), String> {
    let session_id = Uuid::new_v4();
    let conflict = speakiput_contract::ProtocolError {
        code: StableErrorCode::Conflict,
        message: "Settings changed since revision 1".into(),
        retryable: true,
        details: serde_json::Map::from_iter([
            ("expected_revision".into(), json!(1)),
            ("current_revision".into(), json!(2)),
        ]),
    };
    let steps = vec![
        ScriptedStep::success(
            "client.hello",
            json!({
                "selected_version": "1.0",
                "server": { "name": "speakiputd", "version": "0.1.0" },
                "capabilities": ["local_whisper", "post_processing", "history", "credential_store", "keyboard_insertion", "clipboard", "global_shortcut", "focus_safe_overlay"],
                "limits": { "max_frame_bytes": 1_048_576 }
            }),
        ),
        ScriptedStep::success(
            "state.get",
            json!({
                "state": "idle",
                "active_session_id": null,
                "settings_revision": 1,
                "capabilities": ["local_whisper", "post_processing", "history", "credential_store", "keyboard_insertion", "clipboard", "global_shortcut", "focus_safe_overlay"],
                "health": "ready"
            }),
        ),
        ScriptedStep::success(
            "recording.start",
            json!({ "session_id": session_id, "state": "recording" }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "idle", "current": "recording", "session_id": session_id }),
        )
        .with_event(
            "recording.level",
            json!({ "session_id": session_id, "level": 0.5 }),
        ),
        ScriptedStep::success(
            "recording.stop",
            json!({ "session_id": session_id, "accepted": true }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "recording", "current": "transcribing", "session_id": session_id }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "transcribing", "current": "post_processing", "session_id": session_id }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "post_processing", "current": "injecting", "session_id": session_id }),
        )
        .with_event(
            "state.changed",
            json!({ "previous": "injecting", "current": "idle", "session_id": session_id }),
        )
        .with_event(
            "transcript.final",
            json!({
                "session_id": session_id,
                "raw_text": "teste",
                "processed_text": "Teste.",
                "output_text": "Teste.",
                "post_processed": true,
                "insertion": { "status": "inserted", "method": "keyboard" },
                "transcription_backend": "local-whisper",
                "language": "pt",
                "duration_ms": 400
            }),
        ),
        ScriptedStep::error("settings.replace", conflict),
    ];
    let (client, backend) = scripted_client(steps);
    let mut events = client.subscribe();

    let hello_request = Envelope::request(
        "client.hello",
        json!({
            "supported_versions": ["1.0"],
            "client": { "name": "conformance", "version": "0.1.0" },
            "subscriptions": ["state", "session", "transcript", "settings"]
        }),
    );
    let framed = encode_frame(&hello_request).map_err(|error| error.to_string())?;
    let hello_request = decode_frame(&framed).map_err(|error| error.to_string())?;
    let hello = client
        .request(hello_request)
        .await
        .map_err(|error| error.to_string())?;
    let _: ClientHelloResponse = hello.payload_as().map_err(|error| error.to_string())?;

    let snapshot = client
        .request(Envelope::request("state.get", json!({})))
        .await
        .map_err(|error| error.to_string())?;
    let snapshot: speakiput_contract::StateSnapshot =
        snapshot.payload_as().map_err(|error| error.to_string())?;
    if snapshot.state != DictationState::Idle {
        return Err("snapshot did not start idle".into());
    }

    let start = client
        .request(Envelope::request(
            "recording.start",
            json!({ "language": "pt" }),
        ))
        .await
        .map_err(|error| error.to_string())?;
    let start: RecordingStartResponse = start.payload_as().map_err(|error| error.to_string())?;
    if start.session_id != session_id || start.state != DictationState::Recording {
        return Err("recording.start returned inconsistent session/state".into());
    }

    let mut seen_sequences = Vec::new();
    let state_event = receive_event(&mut events, &mut seen_sequences).await?;
    let state: StateChangedEvent = state_event
        .payload_as()
        .map_err(|error| error.to_string())?;
    if state.previous != DictationState::Idle
        || !state.previous.can_transition_to(state.current)
        || state.session_id != Some(session_id)
    {
        return Err("invalid initial state transition".into());
    }
    let level_event = receive_event(&mut events, &mut seen_sequences).await?;
    if level_event.name != "recording.level" {
        return Err("recording level event is missing".into());
    }

    let stop = client
        .request(Envelope::request(
            "recording.stop",
            json!({ "session_id": session_id }),
        ))
        .await
        .map_err(|error| error.to_string())?;
    let stop: RecordingStopResponse = stop.payload_as().map_err(|error| error.to_string())?;
    if !stop.accepted || stop.session_id != session_id {
        return Err("recording.stop was not accepted for the active session".into());
    }

    let mut previous = DictationState::Recording;
    for _ in 0..4 {
        let event = receive_event(&mut events, &mut seen_sequences).await?;
        let transition: StateChangedEvent =
            event.payload_as().map_err(|error| error.to_string())?;
        if transition.previous != previous || !previous.can_transition_to(transition.current) {
            return Err("invalid state transition in terminal flow".into());
        }
        previous = transition.current;
    }
    if previous != DictationState::Idle {
        return Err("terminal flow did not return to idle".into());
    }
    let terminal = receive_event(&mut events, &mut seen_sequences).await?;
    let terminal: TranscriptFinalEvent =
        terminal.payload_as().map_err(|error| error.to_string())?;
    if terminal.session_id != session_id {
        return Err("terminal event belongs to a different session".into());
    }

    let conflict = client
        .request(Envelope::request(
            "settings.replace",
            json!({ "expected_revision": 1, "settings": speakiput_contract::Settings::default() }),
        ))
        .await;
    if !matches!(
        conflict,
        Err(ClientError::Protocol(ref error)) if error.code == StableErrorCode::Conflict && error.retryable
    ) {
        return Err("stable conflict error was not propagated".into());
    }

    if seen_sequences != (1..=7).collect::<Vec<_>>() || backend.remaining_steps().await != 0 {
        return Err("event ordering or script consumption failed".into());
    }
    Ok(())
}

async fn receive_event(
    events: &mut speakiput_client::EventSubscription,
    seen_sequences: &mut Vec<u64>,
) -> Result<Envelope, String> {
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .map_err(|_| "timed out waiting for event".to_owned())?
        .map_err(|error| error.to_string())?;
    let sequence = event
        .sequence
        .ok_or_else(|| "event has no sequence".to_owned())?;
    if seen_sequences
        .last()
        .is_some_and(|previous| sequence != previous + 1)
    {
        return Err("event sequence has a gap".into());
    }
    seen_sequences.push(sequence);
    Ok(event)
}
