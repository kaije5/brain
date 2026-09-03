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
