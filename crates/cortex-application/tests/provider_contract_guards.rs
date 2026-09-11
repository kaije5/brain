//! Architecture guards for the application provider boundary (SCRUM-96).
//!
//! Proves the legacy-isolation rules of the approved SCRUM-90 design: the
//! provider contracts never depend on the legacy `SQLite` note/task
//! abstractions, the legacy repository traits are never adapted to the new
//! ports, and Obsidian, iCloud, sync, filesystem, and transport types stay
//! out of `cortex-application`.

use std::{collections::BTreeSet, path::PathBuf};

fn manifest() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    std::fs::read_to_string(&path).expect("application manifest is readable")
}

/// Keys declared under `[dependencies]`; only direct dependencies count.
fn declared_dependencies() -> BTreeSet<String> {
    let mut dependencies = BTreeSet::new();
    let mut in_section = false;
    for line in manifest().lines() {
        if let Some(header) = line.trim().strip_prefix('[') {
            in_section =
                header.starts_with("dependencies") && !header.starts_with("dev-dependencies");
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

fn source(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(name);
    std::fs::read_to_string(&path).expect("application source is readable")
}

/// Code lines only: doc comments discuss the boundary itself and are not
/// code references.
fn code_lines(text: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn application_dependencies_stay_within_the_approved_allowlist() {
    let mut allowlist = BTreeSet::new();
    allowlist.insert("chrono".to_owned());
    allowlist.insert("cortex-domain".to_owned());
    allowlist.insert("serde_json".to_owned());
    allowlist.insert("uuid".to_owned());
    assert_eq!(
        declared_dependencies(),
        allowlist,
        "cortex-application dependencies changed; any addition needs an ADR and an allowlist update"
    );
}

#[test]
fn provider_contract_modules_never_reference_vendors_transport_or_legacy_state() {
    let modules = ["provider.rs", "knowledge.rs", "task_provider.rs"];
    let forbidden = [
        // Vendor and sync-product names.
        "obsidian",
        "icloud",
        // Filesystem, storage, and transport assumptions.
        "std::fs",
        "std::path",
        "std::net",
        "sqlite",
        "reqwest",
        "tokio",
        "hyper",
        // Legacy SQLite-backed aggregates and repositories: the new boundary
        // must not wrap, extend, or depend on them.
        "noterepository",
        "taskrepository",
        "memoryrepository",
        "aggregatechange",
        "note::",
        "task::",
    ];
    for module in modules {
        let lowered = code_lines(&source(module)).to_lowercase();
        for token in forbidden {
            assert!(
                !lowered.contains(token),
                "`cortex-application/src/{module}` must not reference `{token}`; \
                 provider contracts stay vendor-, transport-, filesystem-, and legacy-free"
            );
        }
    }
}

#[test]
fn legacy_repository_traits_are_never_adapted_to_the_provider_ports() {
    // The migration keeps the legacy repositories untouched: introducing
    // provider-port surface into their module would create the compatibility
    // abstraction the design explicitly rejects.
    let query_rs = code_lines(&source("query.rs")).to_lowercase();
    for provider_token in ["knowledgeprovider", "taskprovider", "providerresourceref"] {
        assert!(
            !query_rs.contains(provider_token),
            "`cortex-application/src/query.rs` must not reference `{provider_token}`; \
             legacy repositories are never adapted to the provider ports"
        );
    }
    // And symmetrically: the provider ports never extend the legacy command
    // surface behind a new abstraction.
    for module in ["knowledge.rs", "task_provider.rs"] {
        let lowered = code_lines(&source(module)).to_lowercase();
        for legacy_token in ["noteinput", "taskinput", "mutationresult"] {
            assert!(
                !lowered.contains(legacy_token),
                "`cortex-application/src/{module}` must not reference `{legacy_token}`; \
                 no feature may extend the legacy aggregate surface through the new ports"
            );
        }
    }
}

#[test]
fn provider_error_and_result_types_stay_bounded_and_typed() {
    // The design requires typed provider results with bounded collections;
    // this guard keeps the shared bound enforced at the single definition.
    let provider_rs = source("provider.rs");
    assert!(
        provider_rs.contains("MAX_PROVIDER_RESULTS"),
        "provider page bound must remain a single enforced constant"
    );
    let knowledge_rs = source("knowledge.rs");
    let task_provider_rs = source("task_provider.rs");
    assert!(
        knowledge_rs.contains("validate_limit") || task_provider_rs.contains("validate_limit"),
        "provider query bounds must stay enforced through validation"
    );
}
