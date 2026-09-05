mod support;

use support::Harness;

#[tokio::test]
async fn cli_created_memory_is_retrievable_through_paired_mcp_with_provenance() {
    let harness = Harness::start().await;
    let created = harness
        .cli_remember("Cortex uses Nemotron as its local AI.")
        .await;
    created.assert_success();

    let result = harness.mcp_search("Which local AI did we choose?").await;
    assert_eq!(
        result.first_statement(),
        "Cortex uses Nemotron as its local AI."
    );
    assert_eq!(result.first_sources(), &[harness.source_id()]);
    harness.shutdown().await;
}

#[tokio::test]
async fn local_agent_mutation_is_visible_to_cli_and_paired_mcp() {
    let harness = Harness::start().await;
    harness
        .agent_remember("Cortex stores one canonical local state.")
        .await
        .assert_success();

    let cli = harness.cli_memory_search("canonical local state").await;
    cli.assert_first_statement("Cortex stores one canonical local state.");
    let mcp = harness.mcp_search("canonical local state").await;
    assert_eq!(
        mcp.first_statement(),
        "Cortex stores one canonical local state."
    );
    assert_eq!(mcp.first_sources(), &[harness.source_id()]);
    harness.shutdown().await;
}
