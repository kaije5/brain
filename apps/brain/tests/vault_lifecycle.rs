//! Vault-backed CLI command tests (SCRUM-118).
//!
//! Every command runs against a real daemon whose local vault fixture is
//! provisioned by the daemon itself; no test touches storage directly.

use brain::{Cli, CliEnvelope, CommandRequest, error_hint, render_text};
use clap::Parser;
use serde_json::{Value, json};
use uuid::Uuid;

fn command(cli: &Cli) -> brain::CommandRequest {
    brain::command_request(cli).expect("maps")
}

fn success_value(response: cortexd::DaemonResponse) -> Value {
    match response.result {
        cortexd::WireResult::Success { value } => value,
        cortexd::WireResult::Error { code } => panic!("expected success, got {code}"),
    }
}

fn error_code(response: cortexd::DaemonResponse) -> String {
    match response.result {
        cortexd::WireResult::Error { code } => code,
        cortexd::WireResult::Success { .. } => panic!("expected an error result"),
    }
}

#[tokio::test]
async fn note_update_and_delete_round_trip_through_the_daemon_vault() {
    let directory = tempfile::TempDir::new().expect("temporary directory");
    let daemon = cortexd::LocalDaemon::start(cortexd::DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let client = daemon.paired_client();

    let cli =
        Cli::try_parse_from(["brain", "note", "create", "Scratch", "first draft"]).expect("parses");
    let created = success_value(
        client
            .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
            .await
            .expect("create response"),
    );
    let resource_id = created["resource"]["resource_id"]
        .as_str()
        .expect("resource id")
        .to_owned();
    let revision = created["revision"]["revision"]
        .as_str()
        .expect("creation revision")
        .to_owned();

    // A stale update is a typed conflict surfaced to the CLI surface.
    let cli = Cli::try_parse_from([
        "brain",
        "note",
        "update",
        &resource_id,
        "--revision",
        &revision,
        "--title",
        "Scratch",
        "--content",
        "stale write",
    ])
    .expect("parses");
    // First advance the note with a fresh write…
    let updated = success_value(
        client
            .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
            .await
            .expect("update response"),
    );
    let fresh = updated["revision"]["revision"]
        .as_str()
        .expect("updated revision")
        .to_owned();
    assert_ne!(fresh, revision);

    // …then replay the same base revision: typed conflict.
    let stale = client
        .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
        .await
        .expect("stale update response");
    assert_eq!(error_code(stale), "conflict");
    assert_eq!(
        error_hint("conflict"),
        "the resource changed since you last observed it; refresh the revision and retry"
    );

    // Delete from the fresh revision succeeds and clears the successor.
    let cli = Cli::try_parse_from([
        "brain",
        "note",
        "delete",
        &resource_id,
        "--revision",
        &fresh,
    ])
    .expect("parses");
    let deleted = success_value(
        client
            .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
            .await
            .expect("delete response"),
    );
    assert_eq!(deleted["revision"], Value::Null);
}

#[tokio::test]
async fn task_complete_uses_the_resource_id_and_opaque_revision_from_the_list() {
    let directory = tempfile::TempDir::new().expect("temporary directory");
    let daemon = cortexd::LocalDaemon::start(cortexd::DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon starts");
    let client = daemon.paired_client();

    let cli = Cli::try_parse_from(["brain", "task", "add", "finish the cutover"]).expect("parses");
    success_value(
        client
            .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
            .await
            .expect("task add response"),
    );

    let cli = Cli::try_parse_from(["brain", "task", "list", "--limit", "10"]).expect("parses");
    let listed = success_value(
        client
            .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
            .await
            .expect("task list response"),
    );
    assert_eq!(listed["freshness"], "current");
    let row = &listed["tasks"][0];
    let resource_id = row["resource_id"].as_str().expect("resource id").to_owned();
    let revision = row["revision"].as_str().expect("revision").to_owned();

    // The CLI completes with the machine-readable revision from the list.
    let cli = Cli::try_parse_from([
        "brain",
        "task",
        "complete",
        &resource_id,
        "--revision",
        &revision,
    ])
    .expect("parses");
    success_value(
        client
            .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
            .await
            .expect("complete response"),
    );

    // The consumed revision is stale for a second completion: typed conflict.
    let conflict = client
        .request(&command(&cli).into_daemon_request(Uuid::now_v7()))
        .await
        .expect("conflict response");
    assert_eq!(error_code(conflict), "conflict");
}

#[test]
fn text_rendering_surfaces_conflicts_degraded_state_and_revisions() {
    // Typed conflicts render with a safe recovery hint.
    let envelope: CliEnvelope<Value> = CliEnvelope::error("conflict");
    let rendered = render_text(&envelope);
    assert!(rendered.contains("error: conflict"));
    assert!(rendered.contains("refresh the revision and retry"));

    // A stale task list renders the explicit degraded warning plus rows with
    // their revision prefixes; a current one does not warn.
    let stale = CliEnvelope::success(json!({
        "freshness": "stale",
        "tasks": [
            {"title": "alpha task", "status": "todo",
             "revision": "rev-0123456789abcdef", "due_at": null}
        ]
    }));
    let rendered = render_text(&stale);
    assert!(rendered.contains("warning: vault index is stale"));
    assert!(rendered.contains("[todo] alpha task (rev rev-0123)"));

    let current = CliEnvelope::success(json!({
        "freshness": "current",
        "tasks": [{"title": "alpha task", "status": "todo",
                   "revision": "rev-0123456789abcdef"}]
    }));
    let rendered = render_text(&current);
    assert!(!rendered.contains("warning:"));
    assert!(rendered.contains("[todo] alpha task"));

    // Machine-readable JSON keeps the full envelope: code, revisions,
    // provenance identity, and freshness all survive verbatim.
    let envelope = CliEnvelope::success(json!({
        "resource": {"provider_id": "markdown-vault", "resource_id": "path:Design.md",
                      "kind": "knowledge"},
        "previous_revision": null,
        "revision": {"revision": "rev-abc"}
    }));
    let rendered = brain::render_json(&envelope).expect("json renders");
    assert!(rendered.contains("\"resource_id\":\"path:Design.md\""));
    assert!(rendered.contains("\"revision\":\"rev-abc\""));
}

#[test]
fn task_complete_maps_resource_id_and_opaque_revision() {
    let cli = Cli::try_parse_from([
        "brain",
        "task",
        "complete",
        "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12",
        "--revision",
        "rev-0123456789abcdef",
    ])
    .expect("parses");
    let request: CommandRequest = command(&cli);
    assert_eq!(request.capability, "cortex_task_complete");
    assert_eq!(
        request.payload,
        json!({"resource_id": "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12",
               "expected_revision": "rev-0123456789abcdef"})
    );
}

#[test]
fn note_update_and_delete_map_resource_payloads() {
    let cli = Cli::try_parse_from([
        "brain",
        "note",
        "update",
        "path:Design.md",
        "--revision",
        "rev-abc",
        "--title",
        "Design",
        "--content",
        "second draft",
    ])
    .expect("parses");
    let request = command(&cli);
    assert_eq!(request.capability, "cortex_knowledge_update");
    assert_eq!(
        request.payload,
        json!({"resource_id": "path:Design.md", "expected_revision": "rev-abc",
               "title": "Design", "content": "second draft"})
    );

    let cli = Cli::try_parse_from([
        "brain",
        "note",
        "delete",
        "path:Design.md",
        "--revision",
        "rev-abc",
    ])
    .expect("parses");
    let request = command(&cli);
    assert_eq!(request.capability, "cortex_knowledge_delete");
    assert_eq!(
        request.payload,
        json!({"resource_id": "path:Design.md", "expected_revision": "rev-abc"})
    );
}
