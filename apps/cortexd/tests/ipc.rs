use cortexd::{
    DaemonConfig, DaemonError, DaemonRequest, LocalDaemon, PROTOCOL_VERSION, WireResult,
};
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
    let paired = daemon.paired_client();
    let response = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            pairing_proof: paired.pairing_proof(),
            capability: "cortex_daemon_status".to_owned(),
            payload: json!({}),
        })
        .await
        .expect("authenticated status request should succeed");

    assert_eq!(response.request_id, request_id);
    assert_eq!(response.protocol_version, PROTOCOL_VERSION);
    let WireResult::Success { value } = response.result else {
        panic!("status should succeed");
    };
    assert_eq!(value["correlation_id"], request_id.to_string());
    assert_ne!(value["principal_id"], json!(Uuid::nil().to_string()));
}

#[tokio::test]
async fn unpaired_and_unsupported_wire_requests_return_redacted_typed_errors() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let request_id = Uuid::now_v7();
    let response = daemon
        .handle_wire_request(DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            pairing_proof: None,
            capability: "cortex_note_create".to_owned(),
            payload: json!({"title": "x", "content": "y"}),
        })
        .await;

    assert_eq!(response.request_id, request_id);
    assert_eq!(
        response.result,
        WireResult::Error {
            code: "unauthenticated".to_owned()
        }
    );
}

#[tokio::test]
async fn paired_note_create_is_dispatched_through_the_daemon_owned_application_service() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let paired = daemon.paired_client();
    let response = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            pairing_proof: paired.pairing_proof(),
            capability: "cortex_note_create".to_owned(),
            payload: json!({"title": "local", "content": "daemon owned"}),
        })
        .await
        .expect("paired request should have a wire response");

    let WireResult::Success { value } = response.result else {
        panic!("note create should reach application service");
    };
    assert!(value["entity_id"].as_str().is_some());
    assert_eq!(value["revision"], 1);
}

#[tokio::test]
async fn paired_unsupported_capability_returns_a_redacted_wire_error() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let paired = daemon.paired_client();
    let response = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            pairing_proof: paired.pairing_proof(),
            capability: "not_a_cortex_capability".to_owned(),
            payload: json!({}),
        })
        .await
        .expect("wire response");
    assert_eq!(
        response.result,
        WireResult::Error {
            code: "unsupported_capability".to_owned()
        }
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
