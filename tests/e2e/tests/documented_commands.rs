use std::{
    fs,
    path::{Path, PathBuf},
};

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
fn operator_guides_only_reference_real_local_commands_and_safe_configuration() {
    let readme = read("README.md");
    let setup = read("docs/operations/local-setup.md");
    let chatgpt = read("docs/operations/chatgpt-mcp.md");
    let backup = read("docs/operations/backup-restore.md");
    let diagnostics = read("docs/operations/diagnostics.md");

    assert!(readme.contains("cargo test -p cortex-e2e"));
    assert!(setup.contains("CORTEX_DATABASE"));
    assert!(setup.contains("cortexd"));
    assert!(setup.contains("brain status"));
    assert!(setup.contains("brain note create"));
    assert!(setup.contains("brain task add"));
    assert!(setup.contains("brain memory search"));
    assert!(chatgpt.contains("CORTEX_GATEWAY_CONFIG"));
    assert!(chatgpt.contains("local_port"));
    assert!(chatgpt.contains("paired_subjects"));
    assert!(chatgpt.contains("outbound-only"));
    assert!(chatgpt.contains("127.0.0.1"));
    assert!(!chatgpt.contains("0.0.0.0"));
    assert!(backup.contains("SQLite"));
    assert!(backup.contains("stop"));
    assert!(backup.contains("restore"));
    assert!(diagnostics.contains("brain doctor"));
    assert!(diagnostics.contains("brain logs"));
    assert!(diagnostics.contains("offline"));
    assert!(
        ![&readme, &setup, &chatgpt, &backup, &diagnostics,]
            .iter()
            .any(|document| document.contains("CORTEX_MODEL_SECRET_REF=<"))
    );
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
        "docs/threat-model/cortex-v0.1.md",
    ] {
        assert!(
            workspace_root().join(path).is_file(),
            "missing release artifact: {path}"
        );
    }
}
