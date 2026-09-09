mod support;

use support::Harness;

#[tokio::test]
async fn offline_embedding_service_keeps_lexical_cli_and_mcp_search_available() {
    let harness = Harness::start().await;
    harness
        .cli_remember("Cortex uses Nemotron as its local AI.")
        .await
        .assert_success();
    harness.stop_fake_model().await;

    let cli = harness.cli_memory_search("Nemotron").await;
    cli.assert_success();
    cli.assert_first_semantic_degraded(true);
    let mcp = harness.mcp_search("Nemotron").await;
    assert!(mcp.first_semantic_degraded());
    harness.shutdown().await;
}
