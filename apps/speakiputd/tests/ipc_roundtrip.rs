use std::{sync::Arc, time::Duration};

use serde_json::json;
use speakiput_client::{BackendClient, BackendService, ClientError, UnixBackendClient};
use speakiput_contract::{
    AudioDevice, ClientHelloResponse, DictationState, Envelope, Settings, SettingsResponse,
    StableErrorCode, StateSnapshot,
};
use speakiput_storage::{JsonSettingsRepository, JsonlHistoryRepository};
use speakiputd::{server::serve_until, service::SpeakiputService};
use tempfile::TempDir;
use tokio::sync::oneshot;

fn service(directory: &TempDir) -> Arc<dyn BackendService> {
    Arc::new(SpeakiputService::new(
        Arc::new(JsonSettingsRepository::new(
            directory.path().join("settings.json"),
        )),
        Arc::new(JsonlHistoryRepository::new(
            directory.path().join("history.jsonl"),
        )),
        vec!["focus_safe_overlay".into(), "history".into()],
        vec![AudioDevice {
            id: "default".into(),
            name: "Default microphone".into(),
            is_default: true,
        }],
    ))
}

async fn connect(directory: &TempDir) -> (UnixBackendClient, oneshot::Sender<()>) {
    let socket = directory.path().join("speakiputd.sock");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let backend = service(directory);
    let server_socket = socket.clone();
    tokio::spawn(async move {
        serve_until(server_socket, backend, async {
            let _ = shutdown_rx.await;
        })
        .await
        .unwrap();
    });
    for _ in 0..100 {
        if let Ok(client) = UnixBackendClient::connect(&socket).await {
            return (client, shutdown_tx);
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    panic!("server did not create its socket");
}

async fn hello(client: &UnixBackendClient) -> ClientHelloResponse {
    client
        .request(Envelope::request(
            "client.hello",
            json!({
                "supported_versions": ["1.0"],
                "client": { "name": "ipc-test", "version": "0.1.0" },
                "subscriptions": ["*"]
            }),
        ))
        .await
        .unwrap()
        .payload_as()
        .unwrap()
}

#[tokio::test]
async fn production_transport_handshakes_snapshots_and_streams_events() {
    let directory = TempDir::new().unwrap();
    let (client, shutdown) = connect(&directory).await;
    let mut events = client.subscribe();
    assert_eq!(hello(&client).await.selected_version, "1.0");

    let state: StateSnapshot = client
        .request(Envelope::request("state.get", json!({})))
        .await
        .unwrap()
        .payload_as()
        .unwrap();
    assert_eq!(state.state, DictationState::Idle);

    client
        .request(Envelope::request("history.clear", json!({})))
        .await
        .unwrap();
    let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(event.name, "history.cleared");
    assert_eq!(event.sequence, Some(1));

    let unavailable = client
        .request(Envelope::request("recording.start", json!({})))
        .await
        .unwrap_err();
    assert!(matches!(
        unavailable,
        ClientError::Protocol(error) if error.code == StableErrorCode::Unavailable
    ));

    shutdown.send(()).unwrap();
}

#[tokio::test]
async fn server_requires_hello_before_other_requests() {
    let directory = TempDir::new().unwrap();
    let (client, shutdown) = connect(&directory).await;
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        client.request(Envelope::request("state.get", json!({}))),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    shutdown.send(()).unwrap();
}

#[tokio::test]
async fn daemon_restart_changes_instance_and_preserves_settings() {
    let directory = TempDir::new().unwrap();
    let (first_client, first_shutdown) = connect(&directory).await;
    hello(&first_client).await;
    let first_instance = first_client
        .request(Envelope::request("state.get", json!({})))
        .await
        .unwrap()
        .instance_id
        .unwrap();

    let mut settings = Settings::default();
    settings.general.language = "en".into();
    first_client
        .request(Envelope::request(
            "settings.replace",
            json!({ "expected_revision": 0, "settings": settings }),
        ))
        .await
        .unwrap();
    first_shutdown.send(()).unwrap();
    drop(first_client);

    let socket = directory.path().join("speakiputd.sock");
    for _ in 0..100 {
        if !socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    let (second_client, second_shutdown) = connect(&directory).await;
    hello(&second_client).await;
    let snapshot = second_client
        .request(Envelope::request("state.get", json!({})))
        .await
        .unwrap();
    assert_ne!(snapshot.instance_id, Some(first_instance));
    let stored: SettingsResponse = second_client
        .request(Envelope::request("settings.get", json!({})))
        .await
        .unwrap()
        .payload_as()
        .unwrap();
    assert_eq!(stored.revision, 1);
    assert_eq!(stored.settings.general.language, "en");
    second_shutdown.send(()).unwrap();
}
