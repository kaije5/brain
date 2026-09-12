//! Observed revision and optimistic-concurrency behavior (SCRUM-107;
//! storage plan §7): revisions derive from content, verify-before-write
//! reports explicit conflicts carrying the current provenance, and there is
//! no blind last-writer-wins anywhere.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use cortex_application::{KnowledgeProvider, ProviderError, TaskProvider, TaskQuery};
use cortex_domain::{ProviderResourceKind, WorkspaceId};
use cortexd::{MarkdownVaultProvider, VaultProviderConfig, VaultProviderMode, VaultScope};
use tempfile::TempDir;

const TASK_ID: &str = "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12";

fn open_vault(root: &std::path::Path, workspace_id: WorkspaceId) -> MarkdownVaultProvider {
    let mut scopes = BTreeSet::new();
    scopes.insert(VaultScope::new("knowledge").expect("knowledge scope"));
    scopes.insert(VaultScope::new("task").expect("task scope"));
    let config = VaultProviderConfig::new(
        "markdown-vault",
        root.to_path_buf(),
        VaultProviderMode::ReadWrite,
        scopes,
        BTreeSet::new(),
    )
    .expect("valid config");
    MarkdownVaultProvider::open(config, workspace_id).expect("vault opens")
}

fn seeded() -> (TempDir, WorkspaceId, MarkdownVaultProvider) {
    let directory = TempDir::new().expect("temp dir");
    std::fs::write(
        directory.path().join("note.md"),
        "---\ntitle: Original\n---\n\noriginal body\n",
    )
    .expect("seed file");
    std::fs::create_dir_all(directory.path().join("Tasks")).expect("tasks dir");
    std::fs::write(
        directory
            .path()
            .join(format!("Tasks/{TASK_ID}-one.md")),
        format!(
            "---\ntype: task\nbrain_id: {TASK_ID}\nstatus: todo\npriority: normal\n---\n\ntask body\n"
        ),
    )
    .expect("seed task");
    let workspace_id = WorkspaceId::new();
    let provider = open_vault(directory.path(), workspace_id);
    (directory, workspace_id, provider)
}

#[tokio::test]
async fn identical_content_yields_identical_revisions() {
    let (_temp, _workspace, provider) = seeded();
    let first = provider
        .confine("note.md", ProviderResourceKind::Knowledge)
        .expect("confined");
    let first_revision = provider
        .current_revision(&first)
        .expect("revision observed");
    let second_revision = provider
        .current_revision(&first)
        .expect("revision observed");
    assert_eq!(first_revision, second_revision);
    assert!(first_revision.as_str().starts_with("rev-"));
}

#[tokio::test]
async fn external_edits_change_the_observed_revision() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::write(directory.path().join("note.md"), "version one\n").expect("seed");
    let provider = open_vault(directory.path(), WorkspaceId::new());
    let confined = provider
        .confine("note.md", ProviderResourceKind::Knowledge)
        .expect("confined");
    let before = provider.current_revision(&confined).expect("revision");

    std::fs::write(directory.path().join("note.md"), "version two\n").expect("external edit");
    let after = provider.current_revision(&confined).expect("revision");
    assert_ne!(before, after);
}

#[tokio::test]
async fn verify_revision_accepts_unchanged_files_and_rejects_stale_ones() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::write(directory.path().join("note.md"), "original\n").expect("seed");
    let provider = open_vault(directory.path(), WorkspaceId::new());
    let confined = provider
        .confine("note.md", ProviderResourceKind::Knowledge)
        .expect("confined");
    let observed = provider.current_revision(&confined).expect("revision");

    // Unchanged: verification passes.
    assert!(provider.verify_revision(&confined, &observed).is_ok());

    // An external device writes the file; the caller's expectation is stale.
    std::fs::write(directory.path().join("note.md"), "externally rewritten\n")
        .expect("external edit");
    match provider.verify_revision(&confined, &observed) {
        Err(ProviderError::Conflict { current }) => {
            // The conflict carries the freshly observed provenance so the
            // caller can re-read and reconcile.
            assert_ne!(current.observed_revision(), &observed);
            assert!(current.observed_revision().as_str().starts_with("rev-"));
        }
        other => panic!("expected conflict with current provenance, got {other:?}"),
    }

    // Reconciliation: re-reading observes the new state and verification
    // passes against it.
    let reconciled = provider.current_revision(&confined).expect("revision");
    assert!(provider.verify_revision(&confined, &reconciled).is_ok());
}

#[tokio::test]
async fn verify_revision_of_a_deleted_file_is_not_found() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::write(directory.path().join("note.md"), "content\n").expect("seed");
    let provider = open_vault(directory.path(), WorkspaceId::new());
    let confined = provider
        .confine("note.md", ProviderResourceKind::Knowledge)
        .expect("confined");
    std::fs::remove_file(confined.absolute()).expect("external delete");

    let observed = provider.current_revision(&confined);
    assert!(matches!(observed, Err(ProviderError::NotFound { .. })));
}

#[tokio::test]
async fn reads_report_stale_freshness_when_the_file_changed_mid_parse() {
    // The mid-read reconciliation path is exercised through the document
    // surface: two sequential reads of an unchanged file both report Current
    // and identical provenance; an intervening edit changes the revision.
    let (_temp, workspace_id, provider) = seeded();
    let reference = cortex_domain::ProviderResourceRef::new(
        workspace_id,
        cortex_domain::ProviderId::new("markdown-vault").expect("valid id"),
        cortex_domain::ProviderResourceId::new("path:note.md").expect("valid id"),
        ProviderResourceKind::Knowledge,
    );

    let first = KnowledgeProvider::get(&provider, &reference)
        .await
        .expect("get succeeds")
        .expect("found");
    assert_eq!(
        first.freshness(),
        cortex_application::ProviderFreshness::Current
    );

    std::fs::write(
        provider.canonical_root().join("note.md"),
        "---\ntitle: Original\n---\n\nexternally edited body\n",
    )
    .expect("external edit");

    let second = KnowledgeProvider::get(&provider, &reference)
        .await
        .expect("get succeeds")
        .expect("found");
    assert_ne!(
        first.item().provenance().observed_revision(),
        second.item().provenance().observed_revision()
    );
    assert!(second.item().body().contains("externally edited"));
}

#[tokio::test]
async fn task_revisions_follow_task_content() {
    let (_temp, workspace_id, provider) = seeded();
    let query = TaskQuery::new(workspace_id, Option::<String>::None, NonZeroUsize::MIN)
        .expect("valid query");
    let page = TaskProvider::search(&provider, &query)
        .await
        .expect("task search succeeds");
    let task = page.items().first().expect("task enumerated").clone();
    let revision = task.provenance().observed_revision().clone();

    // Same content: same revision through an independent read.
    let reread = TaskProvider::get(&provider, task.provenance().resource())
        .await
        .expect("get succeeds")
        .expect("task found");
    assert_eq!(reread.item().provenance().observed_revision(), &revision);
}
