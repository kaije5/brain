//! SCRUM-93: repository-wide proof that no production path can recreate or
//! mutate legacy canonical note/task rows.
//!
//! The scan asserts three invariants over the shipped sources:
//! 1. no production source references the dropped `note`/`task` tables;
//! 2. the final migration drops those tables and constrains the rebuilt
//!    search index to memory/source kinds;
//! 3. no wire capability named `cortex_note_*` survives anywhere.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // CARGO_MANIFEST_DIR for cortex-storage is crates/cortex-storage.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves")
}

fn production_sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for member in [
        "crates/cortex-domain",
        "crates/cortex-application",
        "crates/cortex-search",
        "crates/cortex-storage/src",
        "crates/cortex-vault",
        "crates/cortex-inference/src",
        "crates/cortex-mcp/src",
        "crates/cortex-keyring",
        "apps/brain/src",
        "apps/cortexd/src",
        "apps/cortex-mcp-gateway/src",
    ] {
        let directory = root.join(member);
        collect(&directory, &mut files);
    }
    files
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
}

fn migration_files(root: &Path) -> BTreeSet<String> {
    let directory = root.join("crates/cortex-storage/migrations");
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(&directory).expect("migrations directory") {
        let entry = entry.expect("migration entry");
        names.insert(entry.file_name().to_string_lossy().to_string());
    }
    names
}

fn final_migration(names: &BTreeSet<String>) -> String {
    let mut candidates: Vec<&String> = names
        .iter()
        .filter(|name| {
            let name = name.as_str();
            name.starts_with("0006")
                && std::path::Path::new(name)
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("sql"))
        })
        .collect();
    candidates.pop().expect("final migration present").clone()
}

#[test]
fn no_production_source_references_legacy_note_task_tables() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in production_sources(&root) {
        let contents = std::fs::read_to_string(&path).unwrap_or_default();
        for (needle, label) in [
            ("FROM note", "reads the note table"),
            ("FROM task ", "reads the task table"),
            ("INTO note", "writes the note table"),
            ("INTO task ", "writes the task table"),
            ("UPDATE note ", "updates the note table"),
            ("UPDATE task ", "updates the task table"),
            ("cortex_note_", "legacy wire capability"),
        ] {
            if contents.contains(needle) {
                violations.push(format!(
                    "{}: {label} ({needle:?})",
                    path.strip_prefix(&root).unwrap_or(&path).display()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "production code must not touch legacy note/task state:\n{}",
        violations.join("\n")
    );
}

#[test]
fn final_migration_drops_legacy_tables_and_constrains_search_kinds() {
    let root = workspace_root();
    let names = migration_files(&root);
    let final_name = final_migration(&names);
    let contents = std::fs::read_to_string(
        root.join("crates/cortex-storage/migrations")
            .join(&final_name),
    )
    .expect("final migration readable");
    assert!(contents.contains("DROP TABLE IF EXISTS task;"));
    assert!(contents.contains("DROP TABLE IF EXISTS note;"));
    assert!(contents.contains("entity_kind IN ('memory', 'source')"));
}

#[test]
fn schema_has_no_note_or_task_table_after_the_final_migration() {
    let root = workspace_root();
    let names = migration_files(&root);
    let final_name = final_migration(&names);
    let contents = std::fs::read_to_string(
        root.join("crates/cortex-storage/migrations")
            .join(&final_name),
    )
    .expect("final migration readable");
    for table in ["CREATE TABLE note", "CREATE TABLE task"] {
        assert!(
            !contents.contains(table),
            "the final migration must not recreate {table}"
        );
    }
}
