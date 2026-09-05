mod support;

use cortex_application::Capability;
use support::Harness;

#[tokio::test]
async fn unpaired_mcp_client_is_denied_before_dispatch() {
    let harness = Harness::start().await;
    harness.assert_unpaired_denied().await;
    harness.shutdown().await;
}

#[tokio::test]
async fn paired_client_without_delete_grant_is_denied_and_audited() {
    let harness = Harness::start_with_remote_grants(vec![
        Capability::MemoryCreate,
        Capability::MemorySearch,
        Capability::MemoryRestore,
    ])
    .await;
    let created = harness.cli_remember("Delete grant remains explicit.").await;
    let entity = created.entity_id();

    let denied = harness.mcp_delete(entity, 1).await;
    denied.assert_permission_denied();
    harness.assert_delete_denial_audited(&denied).await;
    harness
        .cli_memory_search("Delete grant remains explicit")
        .await
        .assert_contains(entity);
    harness.shutdown().await;
}

#[tokio::test]
async fn agent_receives_only_the_paired_principals_granted_tools() {
    let harness =
        Harness::start_with_remote_grants(vec![Capability::AgentRun, Capability::MemorySearch])
            .await;
    let offered = harness.remote_agent_tools().await;
    assert!(offered.iter().any(|name| name == "cortex_memory_search"));
    assert!(!offered.iter().any(|name| name == "cortex_memory_create"));
    assert!(!offered.iter().any(|name| name == "cortex_agent_run"));
    harness.shutdown().await;
}
