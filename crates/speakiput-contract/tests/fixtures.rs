use std::{fs, path::PathBuf};

use serde::de::DeserializeOwned;
use serde_json::Value;
use speakiput_contract::{
    ClientHelloRequest, ClientHelloResponse, CredentialPutRequest, CredentialStatusResponse,
    DictationState, Envelope, MessageKind, RecordingLevelEvent, RecordingStartRequest,
    RecordingStartResponse, Settings, SettingsResponse, StableErrorCode, StateChangedEvent,
    StateSnapshot, TranscriptFinalEvent, TranscriptPartialEvent,
};

fn contract_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts/v1")
}

fn load_fixture(name: &str) -> (Value, Envelope) {
    let text = fs::read_to_string(contract_root().join("fixtures").join(name)).unwrap();
    let value: Value = serde_json::from_str(&text).unwrap();
    let envelope: Envelope = serde_json::from_value(value.clone()).unwrap();
    envelope.validate().unwrap();
    (value, envelope)
}

fn assert_payload<T: DeserializeOwned>(envelope: &Envelope) {
    let _: T = envelope.payload_as().unwrap();
}

#[test]
fn every_canonical_fixture_matches_schema_and_round_trips() {
    let schema: Value = serde_json::from_str(
        &fs::read_to_string(contract_root().join("envelope.schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();

    let mut paths = fs::read_dir(contract_root().join("fixtures"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    assert!(!paths.is_empty());

    for path in paths {
        let text = fs::read_to_string(&path).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        if let Err(error) = validator.validate(&value) {
            panic!("{} failed schema validation: {error}", path.display());
        }
        let envelope: Envelope = serde_json::from_value(value.clone()).unwrap();
        envelope.validate().unwrap();
        assert_eq!(
            serde_json::to_value(envelope).unwrap(),
            value,
            "{}",
            path.display()
        );
    }
}

#[test]
fn canonical_payloads_have_rust_dtos() {
    let cases: &[(&str, MessageKind)] = &[
        ("client-hello.request.json", MessageKind::Request),
        ("client-hello.response.json", MessageKind::Response),
        ("credentials-put.request.json", MessageKind::Request),
        ("credentials-put.response.json", MessageKind::Response),
        ("recording-start.request.json", MessageKind::Request),
        ("recording-start.response.json", MessageKind::Response),
        ("state-get.response.json", MessageKind::Response),
        ("state-recording.event.json", MessageKind::Event),
        ("transcript-final.event.json", MessageKind::Event),
        ("recording-level.event.json", MessageKind::Event),
        ("transcript-partial.event.json", MessageKind::Event),
        ("settings-get.response.json", MessageKind::Response),
    ];
    for (name, expected_kind) in cases {
        let (_, envelope) = load_fixture(name);
        assert_eq!(envelope.kind, *expected_kind, "{name}");
        match *name {
            "client-hello.request.json" => assert_payload::<ClientHelloRequest>(&envelope),
            "client-hello.response.json" => assert_payload::<ClientHelloResponse>(&envelope),
            "credentials-put.request.json" => assert_payload::<CredentialPutRequest>(&envelope),
            "credentials-put.response.json" => {
                assert_payload::<CredentialStatusResponse>(&envelope);
            }
            "recording-start.request.json" => assert_payload::<RecordingStartRequest>(&envelope),
            "recording-start.response.json" => assert_payload::<RecordingStartResponse>(&envelope),
            "state-get.response.json" => assert_payload::<StateSnapshot>(&envelope),
            "state-recording.event.json" => assert_payload::<StateChangedEvent>(&envelope),
            "transcript-final.event.json" => assert_payload::<TranscriptFinalEvent>(&envelope),
            "recording-level.event.json" => assert_payload::<RecordingLevelEvent>(&envelope),
            "transcript-partial.event.json" => assert_payload::<TranscriptPartialEvent>(&envelope),
            "settings-get.response.json" => assert_payload::<SettingsResponse>(&envelope),
            _ => unreachable!(),
        }
    }
}

#[test]
fn error_fixture_uses_stable_error_dto() {
    let (_, envelope) = load_fixture("settings-conflict.response.json");
    assert!(envelope.error.is_some());
}

#[test]
fn every_stable_error_code_round_trips() {
    for code in [
        StableErrorCode::InvalidArgument,
        StableErrorCode::InvalidState,
        StableErrorCode::NotFound,
        StableErrorCode::Conflict,
        StableErrorCode::Unavailable,
        StableErrorCode::PermissionDenied,
        StableErrorCode::Timeout,
        StableErrorCode::Unsupported,
        StableErrorCode::ProtocolMismatch,
        StableErrorCode::Internal,
    ] {
        let value = serde_json::to_value(code).unwrap();
        assert_eq!(
            serde_json::from_value::<StableErrorCode>(value).unwrap(),
            code
        );
    }
}

#[test]
fn state_transition_table_rejects_invalid_shortcuts() {
    assert!(DictationState::Idle.can_transition_to(DictationState::Recording));
    assert!(DictationState::Recording.can_transition_to(DictationState::Transcribing));
    assert!(!DictationState::Idle.can_transition_to(DictationState::Injecting));
    assert!(!DictationState::Recording.can_transition_to(DictationState::PostProcessing));
}

#[test]
fn default_settings_are_valid_and_ranges_are_enforced() {
    let mut settings = Settings::default();
    settings.validate().unwrap();
    settings.post_processing.prompt_rewrite_enabled = true;
    settings.post_processing.prompt_rewrite_instruction.clear();
    assert_eq!(
        settings.validate().unwrap_err().field,
        "post_processing.prompt_rewrite_instruction"
    );
    settings.post_processing.prompt_rewrite_instruction = Settings::default()
        .post_processing
        .prompt_rewrite_instruction;
    settings.audio.noise_gate_threshold = 0;
    assert_eq!(
        settings.validate().unwrap_err().field,
        "audio.noise_gate_threshold"
    );
    settings.audio.noise_gate_threshold = 3;
    settings.audio.speech_confirmation_ms = 60;
    settings.validate().unwrap();
    settings.audio.speech_confirmation_ms = 1_001;
    assert_eq!(
        settings.validate().unwrap_err().field,
        "audio.speech_confirmation_ms"
    );
    settings.audio.speech_confirmation_ms = 180;
    settings.general.auto_stop_ms = 0;
    assert_eq!(
        settings.validate().unwrap_err().field,
        "general.auto_stop_ms"
    );
}
