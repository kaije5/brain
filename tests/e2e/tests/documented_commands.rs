use std::{
    fs,
    path::{Path, PathBuf},
};

use brain::{Cli, command_request};
use clap::Parser;
use cortex_mcp_gateway::GatewayConfig;
use tokio::process::Command;
use uuid::Uuid;

mod support;

fn read(path: &str) -> String {
    fs::read_to_string(workspace_root().join(path))
        .unwrap_or_else(|error| panic!("{path} must be documented: {error}"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap_or_else(|| panic!("e2e manifest must be nested under the workspace root"))
        .to_path_buf()
}

#[test]
fn documented_brain_commands_parse_and_map_to_the_real_daemon_contract() {
    let commands = all_documented_brain_commands();
    assert!(
        !commands.is_empty(),
        "guides must contain executable brain commands"
    );
    for command in commands {
        let cli = Cli::try_parse_from(command).expect("documented brain command must parse");
        if is_daemon_command(&cli) {
            command_request(&cli).expect("documented brain command must map to daemon IPC");
        }
    }
}

/// `brain config` and `brain secret` are local client operations with no
/// daemon IPC contract; they are covered by brain's own unit tests.
fn is_daemon_command(cli: &Cli) -> bool {
    !matches!(
        cli.command,
        Some(brain::Command::Config(_) | brain::Command::Secret(_))
    )
}

#[tokio::test]
async fn every_documented_brain_command_executes_against_the_real_daemon_boundary() {
    let harness = support::Harness::start().await;
    for command in all_documented_brain_commands() {
        let cli = Cli::try_parse_from(&command).expect("documented brain command must parse");
        if !is_daemon_command(&cli) {
            continue;
        }
        let output = Command::new(env!("CARGO_BIN_EXE_brain-e2e"))
            .args(&command[1..])
            .env("CORTEX_DATABASE", harness.database_path())
            .output()
            .await
            .unwrap_or_else(|error| panic!("documented command must execute: {error}"));
        assert!(
            output.status.success(),
            "documented command {:?} failed: {}",
            command,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn an_invalid_brain_form_added_to_any_release_artifact_fails_validation() {
    let mut document = read("README.md");
    document.push_str("\n`brain made-up-command`\n");
    assert!(validate_documented_brain_commands(&document).is_err());
}

#[test]
fn documented_gateway_shape_parses_after_safe_fixture_substitution_and_rejects_public_listen() {
    let guide = read("docs/operations/chatgpt-mcp.md");
    let shape = fenced_json_after(&guide, "The following is a **shape-only** configuration")
        .expect("gateway guide must contain its JSON shape");
    let fixture = shape
        .replace("<issuer-host>", "issuer.example")
        .replace("<audience>", "cortex")
        .replace("<stable-oidc-subject>", "paired-subject")
        .replace("<paired-principal-uuidv7>", &Uuid::now_v7().to_string())
        .replace("<protected-enrollment-file>", "remote-enrollment.json")
        .replace("<relay-host>", "relay.example")
        .replace("<relay-tls-server-name>", "relay.example")
        .replace("<opaque-route-id>", "route-1")
        .replace("<relay-public-host>", "cortex.example")
        .replace("<client-certificate-pem>", "client.pem")
        .replace("<protected-client-private-key-pem>", "client.key");
    GatewayConfig::parse(&fixture)
        .expect("documented gateway shape must parse after fixture substitution");
    let unsafe_fixture = fixture.replacen(
        "\"local_port\": 0,",
        "\"local_port\": 0, \"public_listen_addr\": \"0.0.0.0:443\",",
        1,
    );
    assert!(GatewayConfig::parse(&unsafe_fixture).is_err());
    assert!(!guide.contains("0.0.0.0"));
    assert!(!guide.contains("CORTEX_MODEL_SECRET_REF=<"));
}

#[test]
fn release_guides_cover_recoverable_deletion_redaction_and_residual_risks() {
    let setup = read("docs/operations/local-setup.md");
    let backup = read("docs/operations/backup-restore.md");
    let diagnostics = read("docs/operations/diagnostics.md");
    let threat_model = read("docs/threat-model/cortex-v0.1.md");

    assert!(setup.contains("owner"));
    assert!(backup.contains("recoverable"));
    assert!(backup.contains("irreversible purge"));
    assert!(diagnostics.contains("redacted"));
    assert!(threat_model.contains("Release verification evidence"));
    assert!(threat_model.contains("Residual risks"));
    assert!(threat_model.contains("outbound-only"));
}

#[test]
fn required_operation_guides_are_tracked_as_release_artifacts() {
    for path in [
        "README.md",
        "docs/operations/local-setup.md",
        "docs/operations/chatgpt-mcp.md",
        "docs/operations/backup-restore.md",
        "docs/operations/diagnostics.md",
        "docs/operations/linux-vault-topology.md",
        "docs/threat-model/cortex-v0.1.md",
    ] {
        assert!(
            workspace_root().join(path).is_file(),
            "missing release artifact: {path}"
        );
    }
}

#[test]
fn remote_pairing_guide_uses_the_owner_provisioning_command() {
    let guide = read("docs/operations/chatgpt-mcp.md");
    assert!(guide.contains("cargo run -p brain -- --output json remote enroll"));
    assert!(guide.contains("restart_required"));
}

#[test]
fn linux_runbook_documents_the_replaceable_sync_topology() {
    // SCRUM-135: the Linux process topology and the preferred Obsidian
    // Headless Sync runbook are tracked release artifacts. The daemon is
    // the single vault owner; sync is an external, replaceable unit and
    // outages degrade with explicit freshness reporting.
    let runbook = read("docs/operations/linux-vault-topology.md");
    assert!(runbook.contains("cortexd.service"));
    assert!(runbook.contains("obsidian-headless-sync.service"));
    assert!(runbook.contains("Refresh=on-failure") || runbook.contains("Restart=on-failure"));
    assert!(runbook.contains("single vault owner"));
    assert!(runbook.contains("fresh"));
    assert!(runbook.contains("reconcile_vault") || runbook.contains("reconciliation"));
}

fn all_documented_brain_commands() -> Vec<Vec<String>> {
    RELEASE_ARTIFACTS
        .iter()
        .flat_map(|path| documented_brain_commands(&read(path)))
        .collect()
}

const RELEASE_ARTIFACTS: [&str; 6] = [
    "README.md",
    "docs/operations/local-setup.md",
    "docs/operations/chatgpt-mcp.md",
    "docs/operations/linux-vault-topology.md",
    "docs/operations/backup-restore.md",
    "docs/operations/diagnostics.md",
];

fn validate_documented_brain_commands(document: &str) -> Result<(), String> {
    for command in documented_brain_commands(document) {
        let cli = Cli::try_parse_from(command).map_err(|error| error.to_string())?;
        command_request(&cli).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn documented_brain_commands(document: &str) -> Vec<Vec<String>> {
    let fenced = document.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("cargo run -p brain -- ")
            .map(|arguments| {
                let mut argv = vec!["brain".to_owned()];
                argv.extend(shell_words(arguments));
                argv
            })
    });
    let inline = document.lines().flat_map(|line| {
        line.split('`').filter_map(|snippet| {
            snippet.strip_prefix("brain ").map(|arguments| {
                let mut argv = vec!["brain".to_owned()];
                argv.extend(shell_words(arguments));
                argv
            })
        })
    });
    fenced.chain(inline).collect()
}

fn shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in input.chars() {
        match (quote, character) {
            (Some(delimiter), character) if character == delimiter => quote = None,
            (None, '\'' | '\"') => quote = Some(character),
            (None, character) if character.is_whitespace() => {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(character),
        }
    }
    assert!(
        quote.is_none(),
        "documented command has an unterminated quote"
    );
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn fenced_json_after(document: &str, marker: &str) -> Option<String> {
    let tail = document.split_once(marker)?.1;
    let json = tail.split_once("```json")?.1.split_once("```")?.0;
    Some(json.trim().to_owned())
}
