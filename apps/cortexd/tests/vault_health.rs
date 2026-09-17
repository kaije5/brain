//! SCRUM-133/136: doctor/status report secret-free vault health, keep
//! operating through sync outages with explicit staleness, and rebuild the
//! derived index on demand.

use cortexd::{DaemonConfig, DaemonRequest, LocalDaemon, PROTOCOL_VERSION, WireResult};
use serde_json::json;
use tempfile::TempDir;
use uuid::Uuid;

async fn diagnostics(daemon: &LocalDaemon, capability: &str) -> serde_json::Value {
    let paired = daemon.paired_client();
    let response = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: capability.to_owned(),
            payload: json!({}),
        })
        .await
        .expect("authenticated diagnostics request");
    let WireResult::Success { value } = response.result else {
        panic!("diagnostics succeed");
    };
    value
}

async fn search(daemon: &LocalDaemon, query: &str) -> Vec<serde_json::Value> {
    let paired = daemon.paired_client();
    let response = paired
        .request(&DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: "cortex_knowledge_search".to_owned(),
            payload: json!({ "query": query, "limit": 5 }),
        })
        .await
        .expect("authenticated search request");
    let WireResult::Success { value } = response.result else {
        panic!("search succeeds");
    };
    value.as_array().expect("hits array").clone()
}

#[tokio::test]
async fn doctor_and_status_report_secret_free_vault_health() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");

    for capability in ["cortex_daemon_doctor", "cortex_daemon_status"] {
        let payload = diagnostics(&daemon, capability).await;
        let vault = &payload["vault"];
        assert_eq!(vault["configured"], json!(true));
        assert_eq!(vault["provider_id"], json!("markdown-vault"));
        // Configuration facts: root path and scopes, never content.
        let root = vault["root"].as_str().expect("root is a string");
        assert!(root.contains("vault"), "root points at the vault directory");
        assert_eq!(vault["mode"], json!("read_write"));
        let scopes = vault["scopes"].as_array().expect("scopes array");
        assert!(!scopes.is_empty(), "configured scopes are listed");
        assert_eq!(vault["root_accessible"], json!(true));
        assert_eq!(vault["fresh"], json!(true));
        assert!(vault["index"]["refreshed_at"].is_string());
        assert!(vault["index"]["indexed"].is_u64());
        assert_eq!(vault["semantic"], json!("degraded"));
        // Secret-free: no credential or principal material in the vault block.
        let rendered = vault.to_string().to_lowercase();
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("bearer"));
        assert!(!rendered.contains("pairing"));
    }
}

#[tokio::test]
async fn vault_outage_degrades_health_while_local_operation_continues() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let vault_root = directory.path().join("vault");
    std::fs::write(vault_root.join("seed.md"), "reachable note\n").expect("seed note");
    assert!(daemon.refresh_vault_index(), "initial rebuild succeeds");

    // Simulate a sync outage/mount loss: the configured root disappears.
    std::fs::remove_dir_all(&vault_root).expect("remove vault root");
    let refresh = daemon.refresh_vault_index();
    assert!(!refresh, "refresh against a missing root fails");

    let vault = &diagnostics(&daemon, "cortex_daemon_status").await["vault"];
    assert_eq!(vault["fresh"], json!(false), "health reports staleness");
    assert_eq!(vault["root_accessible"], json!(false));

    // Recovery: the root returns and one refresh restores full health.
    std::fs::create_dir_all(&vault_root).expect("recreate vault root");
    std::fs::write(vault_root.join("recovered.md"), "back online\n").expect("rewrite note");
    assert!(daemon.refresh_vault_index(), "recovery rebuild succeeds");
    let vault = &diagnostics(&daemon, "cortex_daemon_doctor").await["vault"];
    assert_eq!(vault["fresh"], json!(true));
    assert_eq!(vault["root_accessible"], json!(true));
    assert_eq!(vault["index"]["indexed"], json!(1));
}

#[tokio::test]
async fn index_rebuild_restores_retrieval_after_full_index_loss() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let vault_root = directory.path().join("vault");
    std::fs::create_dir_all(&vault_root).expect("vault root");
    std::fs::write(
        vault_root.join("rebuild.md"),
        "# Rebuild\n\nThe rebuild procedure restores retrieval.\n",
    )
    .expect("seed note");
    assert!(daemon.refresh_vault_index());

    assert_eq!(search(&daemon, "rebuild").await.len(), 1);

    // Full index loss: the derived index is rebuildable from Markdown.
    {
        let mut index = daemon.vault_index_for_test().write().expect("index lock");
        *index = cortex_search::DerivedVaultIndex::new();
    }
    assert!(search(&daemon, "rebuild").await.is_empty());

    assert!(daemon.refresh_vault_index(), "rebuild succeeds");
    assert_eq!(search(&daemon, "rebuild").await.len(), 1);
}

#[tokio::test]
async fn adversarial_markdown_is_skipped_with_typed_health_not_crashes() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let daemon = LocalDaemon::start(DaemonConfig::for_test(directory.path()))
        .await
        .expect("daemon should start");
    let vault_root = directory.path().join("vault");
    std::fs::write(vault_root.join("clean.md"), "clean note body\n").expect("clean note");
    // Malformed frontmatter: a deterministic parse failure, not a panic.
    std::fs::write(
        vault_root.join("broken.md"),
        "---\nnot: [closed: frontmatter\n---\nbody\n",
    )
    .expect("malformed note");
    // A pathological unicode note must not crash the scan; bodies are
    // chunk-bounded by the parser.
    std::fs::write(
        vault_root.join("weird.md"),
        format!("# {}\n\n{}", "é".repeat(512), "z".repeat(4096)),
    )
    .expect("weird note");

    assert!(daemon.refresh_vault_index());
    let vault = &diagnostics(&daemon, "cortex_daemon_doctor").await["vault"];
    let index = &vault["index"];
    assert!(
        index["skipped"].as_u64().expect("skipped count") >= 1,
        "malformed documents are reported as skipped, never fatal"
    );
    assert_eq!(vault["fresh"], json!(true));
}
