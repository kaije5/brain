mod support;

use support::Harness;

#[tokio::test]
async fn mcp_delete_hides_the_exact_daemon_record_and_restore_returns_it() {
    let harness = Harness::start().await;
    let created = harness
        .mcp_remember("Recycle-bin deletion is recoverable.")
        .await;
    let entity = created.entity_id();
    created.assert_success();
    harness
        .assert_successful_audit(created.correlation_id(), "cortex_memory_create", entity)
        .await;

    let deleted = harness.mcp_delete(entity, 1).await;
    deleted.assert_lifecycle("deleted");
    harness
        .cli_memory_search("Recycle-bin deletion")
        .await
        .assert_not_contains(entity);

    let restored = harness.mcp_restore(entity, 2).await;
    restored.assert_lifecycle("active");
    harness
        .cli_memory_search("Recycle-bin deletion")
        .await
        .assert_contains(entity);
    harness
        .assert_successful_audit(deleted.correlation_id(), "cortex_memory_delete", entity)
        .await;
    harness
        .assert_successful_audit(restored.correlation_id(), "cortex_memory_restore", entity)
        .await;
    harness.shutdown().await;
}
