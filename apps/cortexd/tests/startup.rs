use cortex_application::{ApplicationError, SecretRef, SecretStore};
use cortexd::{DaemonConfig, DaemonError, LocalDaemon};
use tempfile::TempDir;
use tokio::sync::watch;

#[tokio::test]
async fn daemon_migrates_before_accepting_ipc_and_rejects_unpaired_client() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");

    assert!(daemon.migrations_applied().expect("migration state"));
    assert!(matches!(
        daemon.unpaired_status(),
        Err(DaemonError::Unauthenticated)
    ));
}

struct RecordingSecretStore;

impl SecretStore for RecordingSecretStore {
    async fn resolve(&self, reference: &SecretRef) -> Result<SecretRef, ApplicationError> {
        SecretRef::new(reference.as_str())
    }
}

#[tokio::test]
async fn daemon_resolves_configured_secret_references_only_during_composition() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let config = DaemonConfig::for_test(directory.path()).with_inference_secret(
        SecretRef::new("secret://local-model").expect("valid secret reference"),
    );

    let daemon = LocalDaemon::start_with_secret_store(config, &RecordingSecretStore)
        .await
        .expect("daemon should resolve composition secret");

    assert!(daemon.migrations_applied().expect("migration state"));
}

#[tokio::test]
async fn daemon_stops_cleanly_when_shutdown_is_requested() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = std::sync::Arc::new(
        LocalDaemon::start(DaemonConfig::for_test(directory.path()))
            .await
            .expect("daemon should start"),
    );
    let (shutdown_sender, shutdown) = watch::channel(false);
    shutdown_sender
        .send(true)
        .expect("shutdown receiver exists");

    daemon
        .serve(shutdown)
        .await
        .expect("shutdown should complete without a transport error");
}

#[tokio::test]
async fn file_backed_configuration_discovers_the_same_owner_and_endpoint_after_restart() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let database_path = directory.path().join("cortex.db");
    let first = LocalDaemon::start(
        DaemonConfig::from_database_path(database_path.clone()).expect("config"),
    )
    .await
    .expect("first daemon should start");
    let first_client = first.paired_client();
    let first_status = first_client
        .request(&cortexd::DaemonRequest {
            protocol_version: cortexd::PROTOCOL_VERSION,
            request_id: uuid::Uuid::now_v7(),
            principal_id: uuid::Uuid::now_v7(),
            operation_id: uuid::Uuid::now_v7(),
            pairing_proof: first_client.pairing_proof(),
            capability: "cortex_daemon_status".to_owned(),
            payload: serde_json::json!({}),
        })
        .await
        .expect("status");
    let second =
        LocalDaemon::start(DaemonConfig::from_database_path(database_path).expect("config"))
            .await
            .expect("second daemon should start");
    let second_client = second.paired_client();
    let second_status = second_client
        .request(&cortexd::DaemonRequest {
            protocol_version: cortexd::PROTOCOL_VERSION,
            request_id: uuid::Uuid::now_v7(),
            principal_id: uuid::Uuid::now_v7(),
            operation_id: uuid::Uuid::now_v7(),
            pairing_proof: second_client.pairing_proof(),
            capability: "cortex_daemon_status".to_owned(),
            payload: serde_json::json!({}),
        })
        .await
        .expect("status");
    let cortexd::WireResult::Success { value: first } = first_status.result else {
        panic!("first status");
    };
    let cortexd::WireResult::Success { value: second } = second_status.result else {
        panic!("second status");
    };
    assert_eq!(first["workspace_id"], second["workspace_id"]);
    assert_eq!(first["principal_id"], second["principal_id"]);
}
