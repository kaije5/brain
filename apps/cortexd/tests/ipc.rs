use cortex_application::Capability;
use cortex_domain::PrincipalId;
use cortex_storage::SqliteDatabase;
use cortexd::{
    AuthenticatedIpcClient, DaemonConfig, DaemonError, DaemonRequest, LocalDaemon,
    PROTOCOL_VERSION, WireResult,
};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn remote_enrollment_keeps_identity_grants_and_audit_distinct_across_restart() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let database_path = directory.path().join("cortex.db");
    let mut config = DaemonConfig::from_database_path(database_path.clone()).expect("config");
    let owner_id = PrincipalId::try_from(config.owner_principal_id()).expect("owner id");
    let remote_id = PrincipalId::new();
    let enrollment_path = config
        .enroll_remote_principal(remote_id, &[Capability::NoteCreate])
        .expect("remote enrollment");

    let daemon = std::sync::Arc::new(LocalDaemon::start(config).await.expect("daemon starts"));
    let (shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(std::sync::Arc::clone(&daemon).serve(shutdown));
    tokio::task::yield_now().await;
    let owner = AuthenticatedIpcClient::from_database_path(&database_path).expect("owner client");
    let remote =
        AuthenticatedIpcClient::from_enrollment_path(&enrollment_path).expect("remote client");
    assert_eq!(owner.principal_id(), owner_id);
    assert_eq!(remote.principal_id(), remote_id);

    let remote_correlation = Uuid::now_v7();
    let created = remote
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: remote_correlation,
            principal_id: owner_id.into(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_note_create".to_owned(),
            payload: json!({"title":"remote", "content":"separate actor"}),
        })
        .await
        .expect("remote response");
    assert!(matches!(created.result, WireResult::Success { .. }));
    shutdown_sender.send(true).expect("shutdown");
    serving.await.expect("server task").expect("clean shutdown");

    let database = SqliteDatabase::connect_and_migrate(&database_path)
        .await
        .expect("database");
    let audit = database
        .audit_port()
        .find_for_correlation(daemon.ownership_workspace_id(), remote_correlation)
        .await
        .expect("audit lookup")
        .expect("remote audit");
    assert_eq!(audit.principal_id, remote_id);
    assert_ne!(audit.principal_id, owner_id);
    database
        .repositories()
        .revoke_capability(
            daemon.ownership_workspace_id(),
            remote_id,
            Capability::NoteCreate,
        )
        .await
        .expect("revoke remote grant");
    drop(database);

    let restarted = std::sync::Arc::new(
        LocalDaemon::start(
            DaemonConfig::from_database_path(database_path.clone()).expect("restart config"),
        )
        .await
        .expect("restart daemon"),
    );
    let (shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(std::sync::Arc::clone(&restarted).serve(shutdown));
    tokio::task::yield_now().await;
    let denied = remote
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: owner_id.into(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_note_create".to_owned(),
            payload: json!({"title":"denied", "content":"revocation persists"}),
        })
        .await
        .expect("typed denial");
    assert_eq!(
        denied.result,
        WireResult::Error {
            code: "permission_denied".to_owned()
        }
    );
    shutdown_sender.send(true).expect("shutdown");
    serving.await.expect("server task").expect("clean shutdown");
}

#[tokio::test]
#[allow(clippy::too_many_lines)] // Keeps the owner, restart, paired, denied, and audit lifecycle together.
async fn owner_enrolls_a_paired_remote_principal_over_local_ipc_with_only_requested_grants() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let database_path = directory.path().join("cortex.db");
    let daemon = std::sync::Arc::new(
        LocalDaemon::start(
            DaemonConfig::from_database_path(database_path.clone()).expect("owner config"),
        )
        .await
        .expect("daemon starts"),
    );
    let (shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(std::sync::Arc::clone(&daemon).serve(shutdown));
    tokio::task::yield_now().await;

    let owner = AuthenticatedIpcClient::from_database_path(&database_path).expect("owner client");
    let enrollment_request_id = Uuid::now_v7();
    let response = owner
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: enrollment_request_id,
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_remote_enroll".to_owned(),
            payload: json!({
                "subject": "chatgpt-owner-subject",
                "grants": ["cortex_note_create"]
            }),
        })
        .await
        .expect("owner receives typed enrollment result");
    let WireResult::Success { value } = response.result else {
        panic!("owner enrollment should succeed");
    };
    assert!(value["principal_id"].as_str().is_some());
    assert!(value["ipc_enrollment_path"].as_str().is_some());
    assert_eq!(
        value["gateway_paired_subject"]["subject"],
        "chatgpt-owner-subject"
    );
    assert_eq!(
        value["gateway_paired_subject"]["principal_id"],
        value["principal_id"]
    );
    assert!(value.get("signing_key").is_none());
    let enrollment_path = value["ipc_enrollment_path"]
        .as_str()
        .map(std::path::PathBuf::from)
        .expect("safe enrollment path");
    let principal_id = value["principal_id"]
        .as_str()
        .expect("returned principal ID")
        .to_owned();
    let repeated = owner
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_remote_enroll".to_owned(),
            payload: json!({
                "subject": "chatgpt-owner-subject",
                "grants": ["cortex_note_create"]
            }),
        })
        .await
        .expect("idempotent owner response");
    let WireResult::Success { value } = repeated.result else {
        panic!("same subject/grants should be idempotent");
    };
    assert_eq!(value["principal_id"], principal_id);
    let remote = AuthenticatedIpcClient::from_enrollment_path(&enrollment_path)
        .expect("protected remote enrollment");

    shutdown_sender.send(true).expect("shutdown");
    serving.await.expect("server task").expect("clean shutdown");
    let database = SqliteDatabase::connect_and_migrate(&database_path)
        .await
        .expect("audit database");
    let audit = database
        .audit_port()
        .find_for_correlation(daemon.ownership_workspace_id(), enrollment_request_id)
        .await
        .expect("audit lookup")
        .expect("owner enrollment audit");
    assert_eq!(audit.capability, "cortex_remote_enroll");
    let restarted = std::sync::Arc::new(
        LocalDaemon::start(
            DaemonConfig::from_database_path(database_path.clone()).expect("restart config"),
        )
        .await
        .expect("restarted daemon"),
    );
    let (shutdown_sender, shutdown) = tokio::sync::watch::channel(false);
    let serving = tokio::spawn(std::sync::Arc::clone(&restarted).serve(shutdown));
    tokio::task::yield_now().await;

    let created = remote
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_note_create".to_owned(),
            payload: json!({"title":"remote","content":"paired actor"}),
        })
        .await
        .expect("paired remote response");
    assert!(matches!(created.result, WireResult::Success { .. }));

    let denied = remote
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_memory_search".to_owned(),
            payload: json!({"query":"must be denied", "limit":20}),
        })
        .await
        .expect("typed denial");
    assert_eq!(
        denied.result,
        WireResult::Error {
            code: "permission_denied".to_owned()
        }
    );
    let forbidden_enrollment = remote
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_remote_enroll".to_owned(),
            payload: json!({
                "subject": "attacker-subject",
                "grants": ["cortex_note_create"]
            }),
        })
        .await
        .expect("typed owner-only denial");
    assert_eq!(
        forbidden_enrollment.result,
        WireResult::Error {
            code: "permission_denied".to_owned()
        }
    );

    shutdown_sender.send(true).expect("shutdown");
    serving.await.expect("server task").expect("clean shutdown");
}

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
