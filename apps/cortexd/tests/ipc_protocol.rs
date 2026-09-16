//! Local IPC protocol v2 contract tests (SCRUM-117).
//!
//! Provider resources are addressed by stable provider resource identity and
//! opaque observed revisions. These tests cover the new schemas, the typed
//! conflict/not-found error surface, and the rejection of obsolete or
//! ambiguous payloads. Backwards compatibility with v1 is explicitly not
//! provided.

use std::path::Path;

use cortexd::{
    AuthenticatedLocalClient, DaemonConfig, DaemonRequest, DaemonResponse, LocalDaemon,
    PROTOCOL_VERSION, WireResult,
};
use serde_json::{Value, json};
use tempfile::TempDir;
use uuid::Uuid;

async fn start_daemon(directory: &Path) -> (std::sync::Arc<LocalDaemon>, AuthenticatedLocalClient) {
    let daemon = std::sync::Arc::new(
        LocalDaemon::start(DaemonConfig::for_test(directory))
            .await
            .expect("daemon starts"),
    );
    let client = daemon.paired_client();
    (daemon, client)
}

fn request(capability: &str, payload: Value) -> DaemonRequest {
    DaemonRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: Uuid::now_v7(),
        principal_id: Uuid::now_v7(),
        operation_id: Uuid::now_v7(),
        capability: capability.to_owned(),
        payload,
    }
}

fn success_value(response: DaemonResponse) -> Value {
    match response.result {
        WireResult::Success { value } => value,
        WireResult::Error { code } => panic!("expected success, got error code {code}"),
    }
}

fn error_code(response: DaemonResponse) -> String {
    match response.result {
        WireResult::Error { code } => code,
        WireResult::Success { .. } => panic!("expected an error result"),
    }
}

async fn create_task(client: &AuthenticatedLocalClient, title: &str) -> Value {
    success_value(
        client
            .request(&request("cortex_task_create", json!({"title": title})))
            .await
            .expect("task create response"),
    )
}

async fn list_tasks(client: &AuthenticatedLocalClient) -> Value {
    success_value(
        client
            .request(&request("cortex_task_list", json!({"limit": 20})))
            .await
            .expect("task list response"),
    )
}

#[tokio::test]
async fn obsolete_v1_entity_payloads_are_rejected() {
    let directory = TempDir::new().expect("temporary directory");
    let (_daemon, client) = start_daemon(directory.path()).await;

    // The legacy SQLite-entity shape (uuid entity + numeric revision) is not
    // a valid provider resource address: it is rejected as a payload error.
    let obsolete = client
        .request(&request(
            "cortex_task_complete",
            json!({"entity_id": Uuid::now_v7().to_string(), "expected_revision": 3}),
        ))
        .await
        .expect("typed response");
    assert_eq!(error_code(obsolete), "invalid_request");

    let obsolete_note = client
        .request(&request(
            "cortex_knowledge_delete",
            json!({"entity_id": Uuid::now_v7(), "expected_revision": 7}),
        ))
        .await
        .expect("typed response");
    assert_eq!(error_code(obsolete_note), "invalid_request");
}

#[tokio::test]
async fn ambiguous_or_oversized_payloads_are_rejected() {
    let directory = TempDir::new().expect("temporary directory");
    let (_daemon, client) = start_daemon(directory.path()).await;

    // Unknown fields make a payload ambiguous; they are rejected outright.
    let ambiguous = client
        .request(&request(
            "cortex_task_complete",
            json!({
                "resource_id": "task:x",
                "expected_revision": "rev-1",
                "force": true
            }),
        ))
        .await
        .expect("typed response");
    assert_eq!(error_code(ambiguous), "invalid_request");

    // Resource ids and revisions are bounded: overlong opaque values fail
    // payload validation instead of reaching the provider.
    let oversized = client
        .request(&request(
            "cortex_task_complete",
            json!({
                "resource_id": "r".repeat(600),
                "expected_revision": "rev-1"
            }),
        ))
        .await
        .expect("typed response");
    assert_eq!(error_code(oversized), "invalid_request");

    let blank_revision = client
        .request(&request(
            "cortex_task_complete",
            json!({"resource_id": "task:x", "expected_revision": "   "}),
        ))
        .await
        .expect("typed response");
    assert_eq!(error_code(blank_revision), "invalid_request");
}

#[tokio::test]
async fn protocol_version_mismatch_is_rejected_before_dispatch() {
    let directory = TempDir::new().expect("temporary directory");
    let (_daemon, client) = start_daemon(directory.path()).await;

    let mut stale = request("cortex_task_list", json!({"limit": 5}));
    stale.protocol_version = PROTOCOL_VERSION - 1;
    let rejected = client.request(&stale).await.expect("typed response");
    assert_eq!(error_code(rejected), "invalid_request");
}

#[tokio::test]
async fn task_lifecycle_carries_resource_identity_and_opaque_revisions() {
    let directory = TempDir::new().expect("temporary directory");
    let (_daemon, client) = start_daemon(directory.path()).await;

    // Create: the response carries provider resource identity and the
    // creation's observed revision.
    let created = create_task(&client, "protocol lifecycle").await;
    let resource_id = created["resource"]["resource_id"]
        .as_str()
        .expect("resource id is a string")
        .to_owned();
    assert_eq!(created["resource"]["kind"], "task");
    assert_eq!(created["resource"]["provider_id"], "markdown-vault");
    assert_eq!(
        created["previous_revision"],
        Value::Null,
        "a creation has no previous revision"
    );
    let revision = created["revision"]["revision"]
        .as_str()
        .expect("revision is an opaque string")
        .to_owned();

    // List: rows expose the same resource identity and revision.
    let listed = list_tasks(&client).await;
    assert_eq!(listed["freshness"], "current");
    let row = listed["tasks"]
        .as_array()
        .expect("tasks array")
        .iter()
        .find(|task| task["resource_id"] == json!(resource_id))
        .expect("created task is listed");
    assert_eq!(row["revision"], json!(revision));

    // Complete with the observed revision succeeds and mints a new one.
    let completed = success_value(
        client
            .request(&request(
                "cortex_task_complete",
                json!({"resource_id": resource_id, "expected_revision": revision}),
            ))
            .await
            .expect("complete response"),
    );
    let fresh_revision = completed["revision"]["revision"]
        .as_str()
        .expect("post-mutation revision")
        .to_owned();
    assert_ne!(fresh_revision, revision, "the mutation advanced the file");
    assert_eq!(
        completed["previous_revision"]["revision"],
        json!(revision),
        "the outcome carries the revision the write was based on"
    );

    // Completing again from the stale revision is a typed conflict, not a
    // silent overwrite.
    let conflict = client
        .request(&request(
            "cortex_task_complete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        ))
        .await
        .expect("conflict response");
    assert_eq!(error_code(conflict), "conflict");

    // An unknown resource is a typed not-found.
    let missing = client
        .request(&request(
            "cortex_task_complete",
            json!({"resource_id": "task:does-not-exist", "expected_revision": "rev-1"}),
        ))
        .await
        .expect("not-found response");
    assert_eq!(error_code(missing), "not_found");
}

#[tokio::test]
async fn note_updates_address_vault_paths_with_opaque_revisions() {
    let directory = TempDir::new().expect("temporary directory");
    let (_daemon, client) = start_daemon(directory.path()).await;

    let created = success_value(
        client
            .request(&request(
                "cortex_knowledge_create",
                json!({"title": "Design", "content": "first draft"}),
            ))
            .await
            .expect("note create response"),
    );
    let resource_id = created["resource"]["resource_id"]
        .as_str()
        .expect("path-addressed resource id")
        .to_owned();
    assert!(resource_id.starts_with("path:"), "notes are path-addressed");
    let revision = created["revision"]["revision"]
        .as_str()
        .expect("creation revision")
        .to_owned();

    // Update from the observed revision succeeds.
    let updated = success_value(
        client
            .request(&request(
                "cortex_knowledge_update",
                json!({
                    "resource_id": resource_id,
                    "expected_revision": revision,
                    "title": "Design",
                    "content": "second draft"
                }),
            ))
            .await
            .expect("note update response"),
    );
    let fresh = updated["revision"]["revision"]
        .as_str()
        .expect("updated revision")
        .to_owned();
    assert_ne!(fresh, revision);

    // A stale write is a typed conflict that leaves the file untouched.
    let conflict = client
        .request(&request(
            "cortex_knowledge_update",
            json!({
                "resource_id": resource_id,
                "expected_revision": revision,
                "title": "Design",
                "content": "lost write"
            }),
        ))
        .await
        .expect("conflict response");
    assert_eq!(error_code(conflict), "conflict");

    // Deleting from a stale revision also conflicts; from the fresh one it
    // succeeds and the response carries the final provenance.
    let stale_delete = client
        .request(&request(
            "cortex_knowledge_delete",
            json!({"resource_id": resource_id, "expected_revision": revision}),
        ))
        .await
        .expect("stale delete response");
    assert_eq!(error_code(stale_delete), "conflict");

    let deleted = success_value(
        client
            .request(&request(
                "cortex_knowledge_delete",
                json!({"resource_id": resource_id, "expected_revision": fresh}),
            ))
            .await
            .expect("delete response"),
    );
    assert_eq!(
        deleted["revision"],
        Value::Null,
        "deletion has no successor"
    );
    assert_eq!(
        deleted["previous_revision"]["revision"],
        json!(fresh),
        "deletion carries the revision it removed"
    );
}
