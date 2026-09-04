use cortex_application::Capability;
use cortexd::{
    AuthenticatedIpcClient, DaemonConfig, DaemonError, DaemonRequest, LocalDaemon,
    PROTOCOL_VERSION, WireResult,
};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn file_backed_ipc_client_authenticates_to_the_served_daemon() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let database_path = directory.path().join("cortex.db");
    let config = DaemonConfig::from_database_path(database_path.clone()).expect("config");
    let daemon = std::sync::Arc::new(LocalDaemon::start(config).await.expect("daemon starts"));
    let principal_id = daemon.ownership_identity().1;
    let (shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(std::sync::Arc::clone(&daemon).serve(shutdown));
    tokio::task::yield_now().await;

    let client = AuthenticatedIpcClient::from_database_path(&database_path)
        .expect("protected enrollment is readable");
    assert_eq!(Uuid::from(client.principal_id()), principal_id);
    let request_id = Uuid::now_v7();
    let response = client
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_daemon_status".to_owned(),
            payload: json!({}),
        })
        .await
        .expect("paired IPC request");
    assert_eq!(response.request_id, request_id);
    assert!(matches!(response.result, WireResult::Success { .. }));

    shutdown_sender
        .send(true)
        .expect("server receives shutdown");
    serving.await.expect("server task").expect("clean shutdown");
}

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
async fn unpaired_clients_are_rejected_before_the_authenticated_handler() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let response = daemon
        .unpaired_status()
        .expect_err("unpaired client must fail closed");
    assert_eq!(response, DaemonError::Unauthenticated);
}

#[tokio::test]
async fn paired_unsupported_wire_requests_return_redacted_typed_errors() {
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
            capability: "not_a_cortex_capability".to_owned(),
            payload: json!({}),
        })
        .await
        .expect("paired request returns wire result");

    assert_eq!(response.request_id, request_id);
    assert_eq!(
        response.result,
        WireResult::Error {
            code: "unsupported_capability".to_owned()
        }
    );
}

#[tokio::test]
async fn denied_mutation_returns_a_safe_policy_result_without_dispatching_state_change() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(
        DaemonConfig::for_test(directory.path())
            .with_bootstrap_grants(vec![Capability::NoteSearch]),
    )
    .await
    .expect("daemon should start");
    let response = daemon
        .paired_client()
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_note_create".to_owned(),
            payload: json!({"title": "must not persist", "content": "denied"}),
        })
        .await
        .expect("daemon returns a typed result");
    assert_eq!(
        response.result,
        WireResult::Error {
            code: "permission_denied".to_owned()
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
async fn task_list_is_authorized_audited_and_returns_only_active_workspace_tasks() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let paired = daemon.paired_client();
    let created = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_task_create".to_owned(),
            payload: json!({"title":"listed","due_at":null}),
        })
        .await
        .expect("create response");
    assert!(matches!(created.result, WireResult::Success { .. }));
    let response = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_task_list".to_owned(),
            payload: json!({"limit":20}),
        })
        .await
        .expect("list response");
    let WireResult::Success { value } = response.result else {
        panic!("list succeeds");
    };
    assert_eq!(value[0]["title"], "listed");
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
