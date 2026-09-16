//! Normalized knowledge.*/task.* MCP capabilities (SCRUM-119).
//!
//! Acceptance checks: identity cannot be spoofed (the principal is
//! gateway/daemon-injected and tool arguments never influence it), public
//! errors stay safe, and authorization is exercised for every mutation
//! tool.

use cortex_mcp::{McpError, McpPrincipal, McpServer};
use cortexd::{DaemonConfig, LocalDaemon};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

fn mutations() -> Vec<(&'static str, Value)> {
    vec![
        ("knowledge.create", json!({"title": "t", "content": "c"})),
        (
            "knowledge.update",
            json!({"resource_id": "path:x.md", "expected_revision": "rev-1",
               "title": "t", "content": "c"}),
        ),
        (
            "knowledge.delete",
            json!({"resource_id": "path:x.md", "expected_revision": "rev-1"}),
        ),
        ("task.create", json!({"title": "t"})),
        (
            "task.update",
            json!({"resource_id": "task:x", "expected_revision": "rev-1", "title": "t"}),
        ),
        (
            "task.complete",
            json!({"resource_id": "task:x", "expected_revision": "rev-1"}),
        ),
        (
            "task.delete",
            json!({"resource_id": "task:x", "expected_revision": "rev-1"}),
        ),
        (
            "task.restore",
            json!({"resource_id": "task:x", "expected_revision": "rev-1"}),
        ),
        (
            "memory.create",
            json!({"statement": "s", "normalized_subject": "s",
        "normalized_predicate": "p", "normalized_object": "o",
        "sources": [{"source_id": Uuid::now_v7()}]}),
        ),
        (
            "memory.delete",
            json!({"entity_id": Uuid::now_v7(), "expected_revision": 1}),
        ),
    ]
}

async fn server_with_grants(grants: bool) -> (TempDir, McpServer, McpPrincipal) {
    let directory = TempDir::new().expect("temporary directory");
    let config = if grants {
        DaemonConfig::for_test(directory.path())
    } else {
        DaemonConfig::for_test(directory.path()).with_bootstrap_grants(Vec::new())
    };
    let daemon = LocalDaemon::start(config).await.expect("daemon starts");
    let principal = McpPrincipal::from_authenticated(daemon.paired_client());
    (directory, McpServer::new(), principal)
}

#[tokio::test]
async fn every_mutation_tool_is_policy_gated_for_unauthorized_principals() {
    let (_dir, server, principal) = server_with_grants(false).await;
    for (name, arguments) in mutations() {
        let denied = server
            .call_tool_as(&principal, name, arguments.clone())
            .await
            .expect_err("unauthorized mutation must be denied");
        assert_eq!(
            denied.code, "cortex_permission_denied",
            "tool {name} must be policy gated"
        );
        // Public errors are safe: no path, storage, or policy detail.
        assert!(!denied.message.contains('/'), "{}", denied.message);
        assert!(!denied.message.to_lowercase().contains("sqlite"));
    }
}

#[tokio::test]
async fn tool_arguments_cannot_inject_identity_or_authorization() {
    let (_dir, server, principal) = server_with_grants(true).await;
    // A granted mutation succeeds and its identity comes from the
    // authenticated principal; injecting authorization fields into the
    // payload is rejected as invalid input before dispatch.
    let created = server
        .call_tool_as(
            &principal,
            "knowledge.create",
            json!({"title": "injection probe", "content": "body"}),
        )
        .await
        .expect("granted create dispatches");
    assert!(created["resource"]["resource_id"].is_string());

    for injection in [
        json!({"query": "x", "workspace_id": "attacker"}),
        json!({"query": "x", "principal_id": "attacker"}),
        json!({"query": "x", "grants": ["cortex_note_delete"]}),
    ] {
        let rejected = server
            .call_tool_as(&principal, "knowledge.retrieve", injection)
            .await;
        assert!(
            matches!(rejected, Err(McpError { code, .. }) if code == "cortex_invalid_input"),
            "injected authorization fields must be rejected"
        );
    }
}

#[tokio::test]
async fn normalized_task_lifecycle_runs_against_the_daemon_vault() {
    let (_dir, server, principal) = server_with_grants(true).await;

    let created = server
        .call_tool_as(&principal, "task.create", json!({"title": "mcp lifecycle"}))
        .await
        .expect("task create dispatches");
    assert_eq!(created["resource"]["kind"], "task");

    let listed = server
        .call_tool_as(&principal, "task.list", json!({"limit": 10}))
        .await
        .expect("task list dispatches");
    assert_eq!(listed["freshness"], "current");
    let row = listed["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|task| {
            task["title"]
                .as_str()
                .is_some_and(|t| t.contains("mcp-lifecycle"))
        })
        .expect("created task is listed")
        .clone();
    let resource_id = row["resource_id"].as_str().expect("resource id");
    let revision = row["revision"].as_str().expect("revision");

    let completed = server
        .call_tool_as(
            &principal,
            "task.complete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        )
        .await
        .expect("task complete dispatches");
    let fresh = completed["revision"]["revision"]
        .as_str()
        .expect("post-mutation revision");

    // The consumed revision is stale: a typed safe error, not an overwrite.
    let conflict = server
        .call_tool_as(
            &principal,
            "task.complete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        )
        .await
        .expect_err("stale revision conflicts");
    // The conflict is a typed, safe public error carrying the correlation
    // of the rejected attempt and none of the file detail.
    assert_eq!(conflict.code, "cortex_conflict");
    assert!(conflict.correlation_id.is_some());
    assert!(!conflict.message.contains('/'));
    let _ = fresh;
}
