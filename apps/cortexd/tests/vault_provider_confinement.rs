//! Adversarial confinement tests for the vault provider root/scope model
//! (SCRUM-106; storage plan §9 security model).

use std::collections::BTreeSet;

use cortex_domain::{ProviderResourceKind, WorkspaceId};
use cortexd::{
    MarkdownVaultProvider, VaultExclusion, VaultPathError, VaultProviderConfig, VaultProviderMode,
    VaultScope,
};
use tempfile::TempDir;

fn scopes(kinds: &[ProviderResourceKind]) -> BTreeSet<VaultScope> {
    kinds
        .iter()
        .map(|kind| match kind {
            ProviderResourceKind::Knowledge => VaultScope::new("knowledge").expect("valid"),
            ProviderResourceKind::Task => VaultScope::new("task").expect("valid"),
        })
        .collect()
}

fn open_vault(root: &std::path::Path) -> MarkdownVaultProvider {
    let config = VaultProviderConfig::new(
        "markdown-vault",
        root.to_path_buf(),
        VaultProviderMode::ReadWrite,
        scopes(&[ProviderResourceKind::Knowledge, ProviderResourceKind::Task]),
        BTreeSet::new(),
    )
    .expect("valid config");
    MarkdownVaultProvider::open(config, WorkspaceId::new()).expect("vault opens")
}

#[test]
fn open_canonicalizes_the_root_and_requires_a_directory() {
    let directory = TempDir::new().expect("temp dir");
    let provider = open_vault(directory.path());
    assert_eq!(
        provider.canonical_root(),
        directory.path().canonicalize().expect("canonical temp dir")
    );

    let missing = directory.path().join("does-not-exist");
    let config = VaultProviderConfig::new(
        "markdown-vault",
        missing,
        VaultProviderMode::ReadWrite,
        scopes(&[ProviderResourceKind::Knowledge]),
        BTreeSet::new(),
    )
    .expect("valid config");
    assert!(matches!(
        MarkdownVaultProvider::open(config, WorkspaceId::new()),
        Err(VaultPathError::InvalidRoot)
    ));
}

#[test]
fn confinement_accepts_ordinary_relative_paths() {
    let directory = TempDir::new().expect("temp dir");
    let provider = open_vault(directory.path());
    let confined = provider
        .confine("notes/project/atlas.md", ProviderResourceKind::Knowledge)
        .expect("confined");
    assert_eq!(confined.relative(), "notes/project/atlas.md");
    assert!(confined.absolute().starts_with(provider.canonical_root()));
    // A deeply nested create target whose tail does not exist is fine.
    assert!(
        provider
            .confine("notes/2026/new-file.md", ProviderResourceKind::Knowledge)
            .is_ok()
    );
}

#[test]
fn vault_relative_identity_is_platform_independent() {
    // SCRUM-134: the same fixture must resolve to the same provider resource
    // identity whether the caller (or a scan on another filesystem) hands the
    // path over with `/` or the platform separator. Resource identity is
    // always the canonical `/`-separated vault-relative path.
    let directory = TempDir::new().expect("temp dir");
    let provider = open_vault(directory.path());
    let forward = provider
        .confine("notes/2026/atlas.md", ProviderResourceKind::Knowledge)
        .expect("forward-slash path confines");
    // Callers address the provider with `/`-separated relative paths; the
    // scanner applies exactly this normalization to platform walk output
    // (see `indexable_paths`), so a document written on a Windows desktop
    // and read on Linux addresses the same resource without domain changes.
    let platform_relative = std::path::Path::new("notes")
        .join("2026")
        .join("atlas.md")
        .to_str()
        .expect("utf-8 path")
        .replace('\\', "/");
    let platform = provider
        .confine(platform_relative.as_str(), ProviderResourceKind::Knowledge)
        .expect("normalized platform path confines");
    assert_eq!(platform.relative(), forward.relative());
    assert_eq!(platform.absolute(), forward.absolute());

    // The scanned walk yields `/`-separated identities on every platform, so
    // documents written on a Windows desktop and read on Linux address the
    // same resources without domain changes.
    std::fs::create_dir_all(directory.path().join("notes").join("2026"))
        .expect("notes directory");
    std::fs::write(
        directory.path().join("notes").join("2026").join("atlas.md"),
        "# Atlas

shared body
",
    )
    .expect("seed note");
    let scanned = provider
        .indexable_paths()
        .expect("scan succeeds");
    assert!(
        scanned
            .iter()
            .any(|relative| relative == "notes/2026/atlas.md"),
        "scanned identity matches the confined identity, got {scanned:?}"
    );
}

#[test]
fn traversal_and_absolute_paths_are_rejected() {
    let directory = TempDir::new().expect("temp dir");
    let provider = open_vault(directory.path());

    for invalid in [
        "../outside.md",
        "notes/../../../outside.md",
        "/etc/passwd",
        "C:\\outside.md",
        "notes\\atlas.md",
        "",
        "   ",
        "\u{0}bad",
    ] {
        let error = provider.confine(invalid, ProviderResourceKind::Knowledge);
        assert!(
            matches!(
                error,
                Err(VaultPathError::InvalidPath | VaultPathError::Traversal)
            ),
            "path {invalid:?} must be rejected, got {error:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_rejected() {
    let directory = TempDir::new().expect("temp dir");
    let outside = TempDir::new().expect("outside temp dir");
    std::fs::write(outside.path().join("secret.md"), "outside content").expect("write");

    let provider = open_vault(directory.path());
    // A symlink inside the vault pointing at the outside directory.
    std::os::unix::fs::symlink(outside.path(), directory.path().join("escape"))
        .expect("symlink creation");

    assert!(matches!(
        provider.confine("escape/secret.md", ProviderResourceKind::Knowledge),
        Err(VaultPathError::SymlinkEscape)
    ));
    // Even a create target beneath a symlinked directory is rejected.
    assert!(matches!(
        provider.confine("escape/new.md", ProviderResourceKind::Knowledge),
        Err(VaultPathError::SymlinkEscape)
    ));

    // A symlink that stays inside the vault is allowed.
    std::fs::write(directory.path().join("real.md"), "inside").expect("write");
    std::os::unix::fs::symlink("real.md", directory.path().join("alias.md"))
        .expect("inner symlink");
    assert!(
        provider
            .confine("alias.md", ProviderResourceKind::Knowledge)
            .is_ok(),
        "in-root symlinks are not escapes"
    );
}

#[test]
fn configured_exclusions_are_enforced() {
    let directory = TempDir::new().expect("temp dir");
    let mut exclusions = BTreeSet::new();
    exclusions.insert(VaultExclusion::new(".obsidian").expect("valid"));
    exclusions.insert(VaultExclusion::new("archive/old").expect("valid"));
    let config = VaultProviderConfig::new(
        "markdown-vault",
        directory.path().to_path_buf(),
        VaultProviderMode::ReadWrite,
        scopes(&[ProviderResourceKind::Knowledge]),
        exclusions,
    )
    .expect("valid config");
    let provider = MarkdownVaultProvider::open(config, WorkspaceId::new()).expect("opens");

    assert!(matches!(
        provider.confine(".obsidian/app.json", ProviderResourceKind::Knowledge),
        Err(VaultPathError::Excluded)
    ));
    assert!(matches!(
        provider.confine("archive/old/2020.md", ProviderResourceKind::Knowledge),
        Err(VaultPathError::Excluded)
    ));
    // The exclusion matches its exact path and beneath it only.
    assert!(
        provider
            .confine("archive", ProviderResourceKind::Knowledge)
            .is_ok()
    );
    // Siblings of an exclusion are not excluded.
    assert!(
        provider
            .confine("archive/new/2026.md", ProviderResourceKind::Knowledge)
            .is_ok()
    );
    // The exclusion boundary is the vault root: an exclusion cannot reach
    // paths outside it because everything is confined first.
    assert!(
        provider
            .confine("notes/active.md", ProviderResourceKind::Knowledge)
            .is_ok()
    );
}

#[test]
fn resource_kinds_outside_the_configured_scopes_are_rejected() {
    let directory = TempDir::new().expect("temp dir");
    let config = VaultProviderConfig::new(
        "markdown-vault",
        directory.path().to_path_buf(),
        VaultProviderMode::ReadWrite,
        scopes(&[ProviderResourceKind::Knowledge]),
        BTreeSet::new(),
    )
    .expect("valid config");
    let provider = MarkdownVaultProvider::open(config, WorkspaceId::new()).expect("opens");

    assert!(
        provider
            .confine("Tasks/one.md", ProviderResourceKind::Knowledge)
            .is_ok()
    );
    assert!(matches!(
        provider.confine("Tasks/one.md", ProviderResourceKind::Task),
        Err(VaultPathError::OutOfScope)
    ));
}

#[test]
fn read_only_mode_is_visible_for_future_policy() {
    let directory = TempDir::new().expect("temp dir");
    let config = VaultProviderConfig::new(
        "markdown-vault",
        directory.path().to_path_buf(),
        VaultProviderMode::ReadOnly,
        scopes(&[ProviderResourceKind::Knowledge]),
        BTreeSet::new(),
    )
    .expect("valid config");
    let provider = MarkdownVaultProvider::open(config, WorkspaceId::new()).expect("opens");
    assert_eq!(provider.config().mode(), VaultProviderMode::ReadOnly);
}

#[test]
fn path_bound_is_enforced() {
    let directory = TempDir::new().expect("temp dir");
    let provider = open_vault(directory.path());
    let oversized = format!("{}\n", "x".repeat(1100));
    assert!(matches!(
        provider.confine(&oversized, ProviderResourceKind::Knowledge),
        Err(VaultPathError::InvalidPath)
    ));
}
