//! SCRUM-130: the daemon knowledge-search surface fuses provenance-bearing
//! vault chunks with Cortex-owned AI memories over authenticated IPC.

use cortexd::{DaemonConfig, DaemonRequest, LocalDaemon, PROTOCOL_VERSION, WireResult};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn knowledge_search_returns_provenance_bearing_vault_chunks() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");

    let vault_root = directory.path().join("vault");
    std::fs::create_dir_all(vault_root.join("notes")).expect("notes directory");
    std::fs::write(
        vault_root.join("notes").join("launch.md"),
        "# Launch\n\nThe launch retrospective captured three follow-ups.\n",
    )
    .expect("seed note");
    assert!(
        daemon.refresh_vault_index(),
        "derived index rebuilds from vault content"
    );

    let paired = daemon.paired_client();
    let response = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_knowledge_search".to_owned(),
            payload: json!({ "query": "retrospective", "limit": 5 }),
        })
        .await
        .expect("authenticated search request");

    let WireResult::Success { value } = response.result else {
        panic!("search succeeds");
    };
    let hits = value.as_array().expect("result array");
    assert!(!hits.is_empty(), "vault chunk is returned");
    let chunk = &hits[0];
    assert_eq!(chunk["kind"], "vault_chunk");
    assert_eq!(chunk["provider"], "markdown-vault");
    let resource_id = chunk["resource_id"].as_str().expect("resource id");
    assert!(
        resource_id.contains("launch"),
        "hit points at the source note"
    );
    assert!(chunk["chunk"].as_u64().expect("chunk ordinal") >= 1);
    let snippet = chunk["snippet"].as_str().expect("snippet");
    assert!(!snippet.is_empty(), "snippet carries chunk text");
    assert_eq!(chunk["semantic_degraded"], json!(true));
}
