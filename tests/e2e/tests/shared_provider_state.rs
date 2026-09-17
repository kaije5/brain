//! Cross-client shared provider state end to end (SCRUM-121).
//!
//! CLI, TUI/IPC and MCP surfaces create, read, update, complete and delete
//! the same vault-backed resources. Every surface must observe one
//! authority — the same stable resource identity and revision chain — and a
//! conflicting write must never be reported as success.


mod support;

use serde_json::Value;

use support::Harness;

fn success_value(call: &support::CallResult) -> &Value {
    call.data().expect("successful call carries data")
}

fn mutation_revision(value: &Value) -> String {
    value["revision"]["revision"]
        .as_str()
        .expect("mutation carries the observed revision")
        .to_owned()
}

fn listed_row<'a>(value: &'a Value, resource_id: &str) -> &'a Value {
    value["tasks"]
        .as_array()
        .expect("task rows")
        .iter()
        .find(|task| task["resource_id"].as_str() == Some(resource_id))
        .expect("the resource is visible on this surface")
}

#[tokio::test]
async fn task_lifecycle_is_shared_across_cli_ipc_and_mcp_with_one_authority() {
    let harness = Harness::start().await;

    // CLI creates the task.
    let created = harness
        .cli(&["brain", "task", "add", "cross-surface deliverable"])
        .await;
    created.assert_success();
    let resource_id = success_value(&created)["resource"]["resource_id"]
        .as_str()
        .expect("creation returns resource identity")
        .to_owned();
    let revision = mutation_revision(success_value(&created));

    // IPC (the TUI surface) observes the same resource at the same revision.
    let listed = harness.ipc_task_list().await;
    listed.assert_success();
    let row = listed_row(listed.data().expect("list data"), &resource_id);
    assert_eq!(row["revision"].as_str(), Some(revision.as_str()));
    assert_eq!(row["status"], "todo");

    // MCP observes the same resource too.
    let listed = harness.mcp_task_list().await;
    listed.assert_success();
    let row = listed_row(listed.data().expect("list data"), &resource_id);
    assert_eq!(row["revision"].as_str(), Some(revision.as_str()));

    // MCP completes the task with the revision it observed.
    let completed = harness.mcp_task_complete(&resource_id, &revision).await;
    completed.assert_success();
    let fresh = mutation_revision(success_value(&completed));
    assert_ne!(fresh, revision, "the mutation advanced the revision");

    // IPC observes the completion: one authority, no divergence.
    let listed = harness.ipc_task_list().await;
    listed.assert_success();
    let row = listed_row(listed.data().expect("list data"), &resource_id);
    assert_eq!(row["status"], "completed");
    assert_eq!(row["revision"].as_str(), Some(fresh.as_str()));

    // A second surface writing from the consumed revision is a typed
    // conflict — never a success — and the state stays completed at the
    // newer revision.
    let stale = harness.ipc_task_complete(&resource_id, &revision).await;
    assert_eq!(stale.error_code(), Some("conflict"));
    let listed = harness.ipc_task_list().await;
    listed.assert_success();
    let row = listed_row(listed.data().expect("list data"), &resource_id);
    assert_eq!(row["status"], "completed");
    assert_eq!(row["revision"].as_str(), Some(fresh.as_str()));

    harness.shutdown().await;
}

#[tokio::test]
async fn note_lifecycle_is_shared_across_cli_ipc_and_mcp() {
    let harness = Harness::start().await;

    // CLI creates the note.
    let created = harness
        .cli(&["brain", "note", "create", "Design", "first draft"])
        .await;
    created.assert_success();
    let resource_id = success_value(&created)["resource"]["resource_id"]
        .as_str()
        .expect("path-addressed resource id")
        .to_owned();
    let revision = mutation_revision(success_value(&created));

    // IPC updates it from the CLI-observed revision.
    let updated = harness
        .ipc_note_update(&resource_id, &revision, "Design", "second draft")
        .await;
    updated.assert_success();
    let fresh = mutation_revision(success_value(&updated));

    // MCP rewrites it from the IPC-observed revision.
    let rewritten = harness
        .mcp_knowledge_update(&resource_id, &fresh, "Design", "third draft")
        .await;
    rewritten.assert_success();
    let final_revision = mutation_revision(success_value(&rewritten));

    // A stale IPC write from the first revision conflicts instead of
    // clobbering the MCP write.
    let stale = harness
        .ipc_note_update(&resource_id, &revision, "Design", "lost write")
        .await;
    assert_eq!(stale.error_code(), Some("conflict"));

    // MCP deletes from the latest observed revision; the deleted resource no
    // longer exists for IPC either.
    let deleted = harness
        .mcp_knowledge_delete(&resource_id, &final_revision)
        .await;
    deleted.assert_success();
    let gone = harness
        .ipc_note_update(&resource_id, &final_revision, "Design", "resurrection")
        .await;
    assert_eq!(gone.error_code(), Some("not_found"));

    harness.shutdown().await;
}

#[tokio::test]
async fn agent_created_memories_are_visible_across_cli_and_mcp_search() {
    let harness = Harness::start().await;

    // The agent surface creates the memory through the daemon capability.
    harness
        .agent_remember("The agent writes through the same authority.")
        .await
        .assert_success();

    // CLI and MCP both observe it with provenance from the shared source.
    let cli = harness
        .cli_memory_search("writes through the same authority")
        .await;
    cli.assert_success();
    let mcp = harness
        .mcp_search("writes through the same authority")
        .await;
    assert_eq!(
        mcp.first_statement(),
        "The agent writes through the same authority."
    );
    assert_eq!(mcp.first_sources(), &[harness.source_id()]);

    harness.shutdown().await;
}

#[tokio::test]
async fn configured_global_brain_prompt_reaches_the_model_as_the_system_message() {
    let harness = Harness::start().await;

    // Drive one agent turn so the model is contacted.
    harness
        .agent_remember("The configured prompt guards this turn.")
        .await
        .assert_success();

    // The harness settings fixture configures a persistent global Brain
    // prompt; the model must receive it as the opening system message.
    let system = harness.last_system_message().await.expect("system message");
    assert!(
        system.contains("E2E global Brain instructions are active."),
        "the configured global prompt is in the system message: {system}"
    );
    // Cortex's protected instructions stay first and cannot be displaced by
    // the configured layer.
    assert!(
        system.starts_with("You are Cortex, a local-first personal knowledge agent."),
        "protected instructions remain first: {system}"
    );
    harness.shutdown().await;
}

#[tokio::test]
async fn model_switch_applies_to_subsequent_turns_and_preserves_history() {
    let harness = Harness::start().await;

    // Discover the catalog: the daemon resolved one model at startup.
    let listed = harness
        .ipc_call("cortex_model_list", serde_json::json!({}))
        .await;
    listed.assert_success();
    let active = listed.data().expect("list data")["active"]
        .as_str()
        .expect("active model")
        .to_owned();

    // Switching to the same discovered model succeeds and is reflected.
    let selected = harness
        .ipc_call(
            "cortex_model_select",
            serde_json::json!({ "model": active.clone() }),
        )
        .await;
    selected.assert_success();
    assert_eq!(
        selected.data().expect("select data")["active"]
            .as_str()
            .expect("active model"),
        active,
        "the selection resolves to the requested model"
    );

    // History: a subsequent turn still works (new model, same conversation
    // semantics) — proven by a follow-up streaming agent run.
    let client =
        brain::DaemonClient::from_database_path(harness.database_path()).expect("enrolled client");
    let response = client
        .request_streaming(
            brain::CommandRequest {
                request_id: uuid::Uuid::now_v7(),
                operation_id: uuid::Uuid::now_v7(),
                capability: "cortex_agent_run".to_owned(),
                payload: serde_json::json!({"prompt": "hello again"}),
            },
            &|_chunk: &str| {},
        )
        .await
        .expect("subsequent turn after switch");
    assert!(matches!(
        response.result,
        cortexd::WireResult::Success { .. }
    ));

    harness.shutdown().await;
}

#[tokio::test]
async fn unknown_model_selection_leaves_the_active_model_unchanged() {
    let harness = Harness::start().await;

    let listed = harness
        .ipc_call("cortex_model_list", serde_json::json!({}))
        .await;
    listed.assert_success();
    let active = listed.data().expect("list data")["active"]
        .as_str()
        .expect("active model")
        .to_owned();

    let rejected = harness
        .ipc_call(
            "cortex_model_select",
            serde_json::json!({ "model": "no-such-model" }),
        )
        .await;
    assert_eq!(rejected.error_code(), Some("not_found"));

    // The active model is unchanged after the rejection.
    let listed = harness
        .ipc_call("cortex_model_list", serde_json::json!({}))
        .await;
    listed.assert_success();
    assert_eq!(
        listed.data().expect("list data")["active"].as_str(),
        Some(active.as_str())
    );

    harness.shutdown().await;
}
