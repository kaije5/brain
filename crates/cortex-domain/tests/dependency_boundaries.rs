//! Architecture guards for the provider-neutral domain boundary (SCRUM-96).
//!
//! These tests fail when the domain crate gains a dependency it must not
//! have, or when Obsidian, iCloud, sync, transport, filesystem, or storage
//! types leak into `cortex-domain` sources. They are deliberately strict:
//! the approved dependency set is closed, so any addition — including an
//! innocuous-looking utility crate — requires an explicit ADR and an update
//! to this allowlist.

use std::{collections::BTreeSet, path::PathBuf};

fn manifest() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(&path).expect("domain manifest is readable")
}

/// Keys declared under `[dependencies]`; only direct dependencies count.
fn declared_dependencies() -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    let mut in_section = false;
    for line in manifest().lines() {
        if let Some(header) = line.trim().strip_prefix('[') {
            in_section = header.starts_with("dependencies");
            continue;
        }
        if in_section && let Some((key, _)) = line.split_once('=') {
            let key = key.trim();
            if !key.is_empty() && !key.starts_with('#') {
                dependencies.insert(key.to_owned());
            }
        }
    }
    dependencies
}

fn domain_sources() -> Vec<(String, String)> {
    let source_directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    let entries =
        std::fs::read_dir(&source_directory).expect("domain source directory is readable");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|extension| extension == "rs") {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_owned();
            let text = std::fs::read_to_string(&path).expect("domain source is readable");
            sources.push((name, text));
        }
    }
    sources.sort();
    sources
}

#[test]
fn domain_dependencies_stay_within_the_approved_allowlist() {
    let mut allowlist = BTreeSet::new();
    allowlist.insert("chrono".to_owned());
    allowlist.insert("uuid".to_owned());
    assert_eq!(
        declared_dependencies(),
        allowlist,
        "cortex-domain dependencies changed; any addition needs an ADR and an allowlist update"
    );
}

#[test]
fn domain_sources_never_reference_transport_filesystem_or_vendor_types() {
    let forbidden = [
        // Filesystem and storage assumptions.
        "std::fs",
        "std::path",
        "std::net",
        "sqlite",
        // Vendor and sync-product names.
        "obsidian",
        "icloud",
        // Transport and process types.
        "reqwest",
        "tokio",
        "hyper",
        "http",
        "process::command",
    ];
    for (name, text) in domain_sources() {
        // Doc comments discuss the boundary itself; the guard inspects code.
        let code: String = text
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        let lowered = code.to_lowercase();
        for token in forbidden {
            assert!(
                !lowered.contains(token),
                "`cortex-domain/src/{name}` must not reference `{token}`; \
                 transport, filesystem, storage, and vendor types stay outside the domain"
            );
        }
    }
}

#[test]
fn domain_sources_keep_user_content_out_of_debug_output() {
    // Every provider identity/revision wrapper redacts its Debug output; the
    // guard keeps that property from regressing silently.
    for (name, text) in domain_sources() {
        if name == "provider.rs" {
            assert!(
                text.contains("[redacted]"),
                "`cortex-domain/src/{name}` must keep Debug output redacted"
            );
        }
    }
}
