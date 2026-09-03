use cortexd::{DaemonConfig, DaemonError, DaemonRequest, LocalDaemon, PROTOCOL_VERSION};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn authenticated_ipc_ignores_client_claimed_principal_and_preserves_correlation() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let request_id = Uuid::now_v7();
    let response = daemon
        .paired_client()
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_daemon_status".to_owned(),
            payload: json!({}),
        })
        .expect("authenticated status request should succeed");

    assert_eq!(response.request_id, request_id);
    assert_eq!(response.protocol_version, PROTOCOL_VERSION);
    assert_eq!(response.result["correlation_id"], request_id.to_string());
    assert_ne!(
        response.result["principal_id"],
        json!(Uuid::nil().to_string())
    );
}

#[tokio::test]
async fn malformed_or_oversized_ipc_envelopes_are_rejected_without_details() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let error = daemon.decode_request(&vec![b'x'; 65 * 1024]).unwrap_err();
    assert_eq!(error, DaemonError::InvalidRequest);
}
