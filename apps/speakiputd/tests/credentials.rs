use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use speakiput_client::{BackendClient, BackendService, ClientError, InMemoryBackendClient};
use speakiput_contract::{AudioDevice, CredentialStatusResponse, Envelope, StableErrorCode};
use speakiput_storage::{
    CredentialRepository, JsonSettingsRepository, JsonlHistoryRepository, StorageError,
};
use speakiputd::service::SpeakiputService;

#[derive(Default)]
struct MemoryCredentials(Mutex<HashMap<String, String>>);

struct UnavailableCredentials;

impl CredentialRepository for UnavailableCredentials {
    fn put(&self, _: &str, _: &str) -> Result<(), StorageError> {
        Err(StorageError::Credential("secret service is locked".into()))
    }

    fn get(&self, _: &str) -> Result<Option<String>, StorageError> {
        Err(StorageError::Credential("secret service is locked".into()))
    }

    fn delete(&self, _: &str) -> Result<(), StorageError> {
        Err(StorageError::Credential("secret service is locked".into()))
    }
}

impl CredentialRepository for MemoryCredentials {
    fn put(&self, credential_id: &str, secret: &str) -> Result<(), StorageError> {
        self.0
            .lock()
            .unwrap()
            .insert(credential_id.into(), secret.into());
        Ok(())
    }

    fn get(&self, credential_id: &str) -> Result<Option<String>, StorageError> {
        Ok(self.0.lock().unwrap().get(credential_id).cloned())
    }

    fn delete(&self, credential_id: &str) -> Result<(), StorageError> {
        self.0.lock().unwrap().remove(credential_id);
        Ok(())
    }
}

async fn request(client: &impl BackendClient, name: &str, payload: serde_json::Value) -> Envelope {
    client
        .request(Envelope::request(name, payload))
        .await
        .unwrap()
}

#[tokio::test]
async fn credential_operations_never_return_the_secret() {
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentials::default());
    let service = Arc::new(
        SpeakiputService::new(
            Arc::new(JsonSettingsRepository::new(
                directory.path().join("settings.json"),
            )),
            Arc::new(JsonlHistoryRepository::new(
                directory.path().join("history.jsonl"),
            )),
            vec!["credential_store".into()],
            vec![AudioDevice {
                id: "default".into(),
                name: "Default".into(),
                is_default: true,
            }],
        )
        .with_credentials(credentials),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    request(
        &client,
        "client.hello",
        serde_json::json!({
            "supported_versions": ["1.0"],
            "client": { "name": "test", "version": "0.1.0" },
            "subscriptions": ["*"]
        }),
    )
    .await;

    let put = request(
        &client,
        "credentials.put",
        serde_json::json!({
            "credential_id": "provider",
            "secret": "never-return-this"
        }),
    )
    .await;
    let response: CredentialStatusResponse = put.payload_as().unwrap();
    assert!(response.stored);
    assert!(
        !serde_json::to_string(&put)
            .unwrap()
            .contains("never-return-this")
    );

    let status = request(
        &client,
        "credentials.status",
        serde_json::json!({ "credential_id": "provider" }),
    )
    .await;
    assert!(
        status
            .payload_as::<CredentialStatusResponse>()
            .unwrap()
            .stored
    );

    let deleted = request(
        &client,
        "credentials.delete",
        serde_json::json!({ "credential_id": "provider" }),
    )
    .await;
    assert!(
        !deleted
            .payload_as::<CredentialStatusResponse>()
            .unwrap()
            .stored
    );
}

#[tokio::test]
async fn credential_storage_failures_are_correlated_protocol_errors() {
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        SpeakiputService::new(
            Arc::new(JsonSettingsRepository::new(
                directory.path().join("settings.json"),
            )),
            Arc::new(JsonlHistoryRepository::new(
                directory.path().join("history.jsonl"),
            )),
            vec!["credential_store".into()],
            vec![],
        )
        .with_credentials(Arc::new(UnavailableCredentials)),
    );
    let backend: Arc<dyn BackendService> = service;
    let client = InMemoryBackendClient::connect(backend);
    request(
        &client,
        "client.hello",
        serde_json::json!({
            "supported_versions": ["1.0"],
            "client": { "name": "test", "version": "0.1.0" },
            "subscriptions": ["*"]
        }),
    )
    .await;

    let result = client
        .request(Envelope::request(
            "credentials.put",
            serde_json::json!({ "credential_id": "provider", "secret": "private" }),
        ))
        .await;
    match result {
        Err(ClientError::Protocol(error)) => {
            assert_eq!(error.code, StableErrorCode::Unavailable);
            assert!(error.retryable);
            assert!(!serde_json::to_string(&error).unwrap().contains("private"));
        }
        other => panic!("expected a correlated protocol error, got {other:?}"),
    }
}
