mod support;

#[cfg(windows)]
use support::assert_deployed_daemon_degrades_on_missing_model_secret;
use support::{Harness, configure_model_secret_for_platform};

#[cfg(windows)]
#[tokio::test]
async fn deployed_model_secret_reference_failure_degrades_explicitly() {
    assert_deployed_daemon_degrades_on_missing_model_secret().await;
}

#[cfg(windows)]
#[test]
fn windows_normal_harness_exercises_the_platform_secret_reference() {
    let mut command = tokio::process::Command::new("cortexd-fixture");
    configure_model_secret_for_platform(&mut command, true, Some("keyring:cortex/test"));

    let configured_secret = command
        .as_std()
        .get_envs()
        .find(|(name, _)| {
            name.to_string_lossy()
                .eq_ignore_ascii_case("CORTEX_MODEL_SECRET_REF")
        })
        .and_then(|(_, value)| value);
    assert_eq!(
        configured_secret,
        Some(std::ffi::OsStr::new("keyring:cortex/test"))
    );
}

#[test]
fn non_windows_model_environment_explicitly_removes_an_inherited_secret() {
    let mut command = tokio::process::Command::new("cortexd-fixture");
    command.env("CORTEX_MODEL_SECRET_REF", "keyring:inherited/value");
    configure_model_secret_for_platform(&mut command, false, None);

    let configured_secret = command
        .as_std()
        .get_envs()
        .find(|(name, _)| {
            name.to_string_lossy()
                .eq_ignore_ascii_case("CORTEX_MODEL_SECRET_REF")
        })
        .map(|(_, value)| value);
    assert!(matches!(configured_secret, Some(None)));
}

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
    assert!(!result.first_semantic_degraded());
    harness
        .assert_cli_text_search("Nemotron", "Cortex uses Nemotron as its local AI.")
        .await;
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
    assert!(!mcp.first_semantic_degraded());
    harness.shutdown().await;
}
