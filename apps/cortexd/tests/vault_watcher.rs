//! Debounced idempotent vault watching (SCRUM-113).

use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};

use cortex_domain::{
    ContentHash, ObservedRevision, ProviderId, ProviderResourceId, ProviderResourceKind,
    ProviderResourceRef, WorkspaceId,
};
use cortex_search::{ChunkProvenance, DerivedVaultIndex};
use cortexd::{
    AppliedEvent, CoalescedEvent, EventQueue, MarkdownVaultProvider, ReconciliationError,
    VaultEvent, VaultExclusion, VaultProviderConfig, VaultProviderMode, VaultReconciler,
    VaultScope, apply_event, rebuild_vault, reconcile_vault,
};
use tempfile::TempDir;

fn provider_for(root: &std::path::Path, workspace_id: WorkspaceId) -> MarkdownVaultProvider {
    let config = VaultProviderConfig::new(
        "markdown-vault",
        root.to_path_buf(),
        VaultProviderMode::ReadWrite,
        all_scopes(),
        BTreeSet::new(),
    )
    .expect("valid config");
    MarkdownVaultProvider::open(config, workspace_id).expect("vault opens")
}

fn all_scopes() -> BTreeSet<VaultScope> {
    let mut set = BTreeSet::new();
    set.insert(VaultScope::new("knowledge").expect("valid"));
    set.insert(VaultScope::new("task").expect("valid"));
    set
}

#[test]
fn deterministic_scan_lists_each_confined_markdown_path_once_in_lexical_order() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(directory.path().join("Tasks")).expect("tasks directory");
    std::fs::create_dir_all(directory.path().join("Private")).expect("private directory");
    std::fs::write(directory.path().join("z.md"), "zeta\n").expect("z note");
    std::fs::write(directory.path().join("a.md"), "alpha\n").expect("a note");
    std::fs::write(
        directory.path().join("Tasks/01926c8f-88f9-7d33-9a1b-2c7d33bd0a12.md"),
        "---\ntype: task\nbrain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a12\nstatus: todo\npriority: high\n---\n\nTask\n",
    )
    .expect("task");
    std::fs::write(directory.path().join("ignored.txt"), "ignored\n").expect("text file");
    std::fs::write(directory.path().join("Private/secret.md"), "secret\n").expect("excluded note");

    let mut exclusions = BTreeSet::new();
    exclusions.insert(VaultExclusion::new("Private").expect("valid exclusion"));
    let config = VaultProviderConfig::new(
        "markdown-vault",
        directory.path().to_path_buf(),
        VaultProviderMode::ReadWrite,
        all_scopes(),
        exclusions,
    )
    .expect("valid config");
    let provider = MarkdownVaultProvider::open(config, WorkspaceId::new()).expect("vault opens");

    assert_eq!(
        provider.indexable_paths().expect("complete scan"),
        vec![
            "Tasks/01926c8f-88f9-7d33-9a1b-2c7d33bd0a12.md".to_owned(),
            "a.md".to_owned(),
            "z.md".to_owned(),
        ]
    );
}

#[test]
fn reconciliation_recovers_missed_additions_updates_and_unchanged_files() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    std::fs::write(directory.path().join("note.md"), "first token\n").expect("seed note");

    let added = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    assert_eq!(
        (added.indexed, added.unchanged, added.removed, added.skipped),
        (1, 0, 0, 0)
    );
    assert_eq!(index.document_count(), 1);

    let unchanged = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    assert_eq!(
        (
            unchanged.indexed,
            unchanged.unchanged,
            unchanged.removed,
            unchanged.skipped,
        ),
        (0, 1, 0, 0)
    );

    std::fs::write(directory.path().join("note.md"), "second token\n").expect("update note");
    let updated = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    assert_eq!(
        (
            updated.indexed,
            updated.unchanged,
            updated.removed,
            updated.skipped
        ),
        (1, 0, 0, 0)
    );
    assert!(
        cortex_search::lexical(
            &index,
            "first token",
            NonZeroUsize::new(10).expect("non-zero")
        )
        .expect("search")
        .is_empty()
    );
    assert_eq!(
        cortex_search::lexical(
            &index,
            "second token",
            NonZeroUsize::new(10).expect("non-zero")
        )
        .expect("search")
        .len(),
        1
    );
}

#[test]
fn reconciliation_recovers_missed_delete_and_move() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    std::fs::write(directory.path().join("old.md"), "move token\n").expect("seed note");
    let _ = reconcile_vault(&provider, &mut index).expect("complete reconciliation");

    std::fs::rename(
        directory.path().join("old.md"),
        directory.path().join("new.md"),
    )
    .expect("move note without watcher event");
    let moved = reconcile_vault(&provider, &mut index).expect("complete reconciliation");

    assert_eq!(
        (moved.indexed, moved.unchanged, moved.removed, moved.skipped),
        (1, 0, 1, 0)
    );
    let resources: Vec<&str> = index
        .documents()
        .map(|resource| resource.resource_id().as_str())
        .collect();
    assert_eq!(resources, vec!["path:new.md"]);

    std::fs::remove_file(directory.path().join("new.md")).expect("delete note");
    let deleted = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    assert_eq!(
        (
            deleted.indexed,
            deleted.unchanged,
            deleted.removed,
            deleted.skipped
        ),
        (0, 0, 1, 0)
    );
    assert_eq!(index.document_count(), 0);
}

#[test]
fn reconciliation_preserves_unrelated_provider_state() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let foreign = ProviderResourceRef::new(
        WorkspaceId::new(),
        ProviderId::new("other-provider").expect("provider id"),
        ProviderResourceId::new("foreign-document").expect("resource id"),
        ProviderResourceKind::Knowledge,
    );
    let provenance = ChunkProvenance::new(
        ContentHash::new([7; 32]),
        ObservedRevision::new("rev-foreign").expect("revision"),
    );
    let mut index = DerivedVaultIndex::new();
    cortex_search::index_document(&mut index, &foreign, "foreign token\n", &provenance)
        .expect("index foreign resource");

    let report = reconcile_vault(&provider, &mut index).expect("complete reconciliation");

    assert_eq!(
        (
            report.indexed,
            report.unchanged,
            report.removed,
            report.skipped
        ),
        (0, 0, 0, 0)
    );
    assert!(index.document(&foreign).is_some());
}

#[test]
fn reconciliation_uses_stable_task_identity_across_moves() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(directory.path().join("Tasks")).expect("tasks directory");
    let task_id = "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12";
    let original = directory.path().join("Tasks/original.md");
    std::fs::write(
        &original,
        format!(
            "---\ntype: task\nbrain_id: {task_id}\nstatus: todo\npriority: high\n---\n\nStable task token\n"
        ),
    )
    .expect("task");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();

    let first = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    assert_eq!((first.indexed, first.skipped), (1, 0));
    let resources: Vec<&str> = index
        .documents()
        .map(|resource| resource.resource_id().as_str())
        .collect();
    assert_eq!(resources, vec![task_id]);

    std::fs::rename(&original, directory.path().join("Tasks/moved.md"))
        .expect("move task without watcher event");
    let moved = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    assert_eq!((moved.indexed, moved.unchanged, moved.removed), (0, 1, 0));
    let resources: Vec<&str> = index
        .documents()
        .map(|resource| resource.resource_id().as_str())
        .collect();
    assert_eq!(resources, vec![task_id]);
}

#[test]
fn reconciliation_skips_malformed_task_schema() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(directory.path().join("Tasks")).expect("tasks directory");
    std::fs::write(
        directory.path().join("Tasks/malformed.md"),
        "---\ntype: task\nstatus: todo\npriority: high\n---\n\nMissing identity\n",
    )
    .expect("malformed task");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();

    let report = reconcile_vault(&provider, &mut index).expect("complete reconciliation");

    assert_eq!((report.indexed, report.skipped), (0, 1));
    assert_eq!(index.document_count(), 0);
}

#[test]
fn reconciliation_rejects_duplicate_task_identity_before_mutating_index() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(directory.path().join("Tasks")).expect("tasks directory");
    std::fs::write(directory.path().join("keep.md"), "keep token\n").expect("keep note");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    let _ = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    let duplicate = "---\ntype: task\nbrain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a12\nstatus: todo\npriority: high\n---\n\nDuplicate task\n";
    std::fs::write(directory.path().join("Tasks/one.md"), duplicate).expect("first task");
    std::fs::write(directory.path().join("Tasks/two.md"), duplicate).expect("second task");

    let error = reconcile_vault(&provider, &mut index).expect_err("duplicate identity rejected");

    assert_eq!(error, ReconciliationError::DuplicateResource);
    assert_eq!(index.document_count(), 1);
    assert_eq!(
        cortex_search::lexical(
            &index,
            "keep token",
            NonZeroUsize::new(10).expect("non-zero")
        )
        .expect("search")
        .len(),
        1
    );
}

#[test]
fn incomplete_bounded_scan_preserves_existing_index_state() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::write(directory.path().join("keep.md"), "keep token\n").expect("keep note");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    let _ = reconcile_vault(&provider, &mut index).expect("complete reconciliation");
    let overflow = directory.path().join("overflow");
    std::fs::create_dir(&overflow).expect("overflow directory");
    for ordinal in 0..10_001_u32 {
        std::fs::write(overflow.join(format!("{ordinal:05}.txt")), []).expect("bounded fixture");
    }

    let error = reconcile_vault(&provider, &mut index).expect_err("partial scan rejected");

    assert_eq!(error, ReconciliationError::ScanIncomplete);
    assert_eq!(index.document_count(), 1);
    assert_eq!(
        cortex_search::lexical(
            &index,
            "keep token",
            NonZeroUsize::new(10).expect("non-zero")
        )
        .expect("search")
        .len(),
        1
    );
}

#[test]
fn periodic_reconciliation_runs_immediately_then_only_when_due() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    std::fs::write(directory.path().join("note.md"), "periodic token\n").expect("seed note");
    let mut index = DerivedVaultIndex::new();
    let mut reconciler = VaultReconciler::new(NonZeroU64::new(1_000).expect("non-zero"));

    let first = reconciler
        .reconcile_if_due(&provider, &mut index, 100)
        .expect("complete reconciliation")
        .expect("first call runs");
    assert_eq!((first.indexed, first.unchanged), (1, 0));
    assert!(
        reconciler
            .reconcile_if_due(&provider, &mut index, 1_099)
            .expect("complete reconciliation")
            .is_none()
    );
    let boundary = reconciler
        .reconcile_if_due(&provider, &mut index, 1_100)
        .expect("complete reconciliation")
        .expect("interval boundary runs");
    assert_eq!((boundary.indexed, boundary.unchanged), (0, 1));
}

#[test]
fn rebuild_replaces_all_derived_state_with_deterministic_vault_state() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    std::fs::write(directory.path().join("b.md"), "bravo token\n").expect("b note");
    std::fs::write(directory.path().join("a.md"), "alpha token\n").expect("a note");

    let mut expected = DerivedVaultIndex::new();
    let expected_report = rebuild_vault(&provider, &mut expected).expect("complete rebuild");
    assert_eq!(
        (
            expected_report.indexed,
            expected_report.unchanged,
            expected_report.removed,
            expected_report.skipped,
        ),
        (2, 0, 0, 0)
    );

    let mut actual = DerivedVaultIndex::new();
    std::fs::write(directory.path().join("stale.md"), "stale token\n").expect("stale note");
    let _ = reconcile_vault(&provider, &mut actual).expect("complete reconciliation");
    std::fs::remove_file(directory.path().join("stale.md")).expect("remove stale authority");
    let foreign = ProviderResourceRef::new(
        WorkspaceId::new(),
        ProviderId::new("other-provider").expect("provider id"),
        ProviderResourceId::new("foreign-document").expect("resource id"),
        ProviderResourceKind::Knowledge,
    );
    cortex_search::index_document(
        &mut actual,
        &foreign,
        "foreign token\n",
        &ChunkProvenance::new(
            ContentHash::new([9; 32]),
            ObservedRevision::new("rev-foreign").expect("revision"),
        ),
    )
    .expect("index foreign resource");

    let report = rebuild_vault(&provider, &mut actual).expect("complete rebuild");

    assert_eq!(
        (
            report.indexed,
            report.unchanged,
            report.removed,
            report.skipped
        ),
        (2, 0, 0, 0)
    );
    let expected_documents: Vec<_> = expected
        .document_entries()
        .map(|(resource, entry)| (resource.clone(), entry.clone()))
        .collect();
    let actual_documents: Vec<_> = actual
        .document_entries()
        .map(|(resource, entry)| (resource.clone(), entry.clone()))
        .collect();
    assert_eq!(actual_documents, expected_documents);
    let expected_chunks: Vec<_> = expected
        .chunks()
        .map(|(reference, chunk)| (reference.clone(), chunk.clone()))
        .collect();
    let actual_chunks: Vec<_> = actual
        .chunks()
        .map(|(reference, chunk)| (reference.clone(), chunk.clone()))
        .collect();
    assert_eq!(actual_chunks, expected_chunks);
    assert!(actual.document(&foreign).is_none());
}

#[test]
fn bursts_coalesce_into_one_latest_state_event_per_path() {
    let mut queue = EventQueue::new(500);
    queue.push(
        VaultEvent::Updated {
            relative: "notes/a.md".to_owned(),
        },
        100,
    );
    queue.push(
        VaultEvent::Updated {
            relative: "notes/a.md".to_owned(),
        },
        200,
    );
    queue.push(
        VaultEvent::Updated {
            relative: "notes/a.md".to_owned(),
        },
        300,
    );

    // Within the debounce window of 500ms measured at 400: nothing ready.
    let drained = queue.drain(400);
    assert!(drained.is_empty());
    assert_eq!(queue.pending_count(), 3);

    // At 801 the full window after the last hint has elapsed.
    let drained = queue.drain(801);
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].event,
        VaultEvent::Updated {
            relative: "notes/a.md".to_owned()
        }
    );
    assert_eq!(drained[0].observed_at, 300, "newest raw timestamp wins");
}

#[test]
fn newer_same_path_event_delays_the_entire_burst() {
    let mut queue = EventQueue::new(500);
    queue.push(
        VaultEvent::Updated {
            relative: "notes/a.md".to_owned(),
        },
        100,
    );
    queue.push(
        VaultEvent::Deleted {
            relative: "notes/a.md".to_owned(),
        },
        600,
    );

    // The first hint is individually old at 700, but it belongs to a burst
    // whose latest event has not yet crossed the debounce window.
    assert!(queue.drain(700).is_empty());
    assert_eq!(queue.pending_count(), 2);

    let drained = queue.drain(1_101);
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].event,
        VaultEvent::Deleted {
            relative: "notes/a.md".to_owned()
        }
    );
}

#[test]
fn time_zero_event_waits_through_its_full_debounce_window() {
    let mut queue = EventQueue::new(500);
    queue.push(
        VaultEvent::Created {
            relative: "zero.md".to_owned(),
        },
        0,
    );

    assert!(queue.drain(500).is_empty());
    assert_eq!(queue.drain(501).len(), 1);
}

#[test]
fn distinct_paths_coalesce_independently_in_first_touched_order() {
    let mut queue = EventQueue::new(100);
    queue.push(
        VaultEvent::Created {
            relative: "a.md".to_owned(),
        },
        10,
    );
    queue.push(
        VaultEvent::Created {
            relative: "b.md".to_owned(),
        },
        20,
    );
    queue.push(
        VaultEvent::Updated {
            relative: "a.md".to_owned(),
        },
        30,
    );

    let drained = queue.drain(200);
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].event.target(), "a.md");
    assert_eq!(drained[1].event.target(), "b.md");
}

#[test]
fn rename_collapses_to_its_final_location() {
    let mut queue = EventQueue::new(100);
    queue.push(
        VaultEvent::Deleted {
            relative: "old.md".to_owned(),
        },
        10,
    );
    queue.push(
        VaultEvent::Created {
            relative: "new.md".to_owned(),
        },
        20,
    );

    // Modeled as a rename: both paths coalesce independently (delete of the
    // old path, create of the new).
    let drained = queue.drain(200);
    assert_eq!(drained.len(), 2);

    // A true rename event coalesces onto its final location.
    let mut queue = EventQueue::new(100);
    queue.push(
        VaultEvent::Renamed {
            from: "old.md".to_owned(),
            to: "new.md".to_owned(),
        },
        10,
    );
    let drained = queue.drain(200);
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].event.target(), "new.md");
}

#[tokio::test]
async fn applying_created_and_updated_events_is_idempotent() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    std::fs::create_dir_all(directory.path().join("notes")).expect("notes dir");
    std::fs::write(
        directory.path().join("notes/a.md"),
        "---\ntitle: A\n---\n\nbody a\n",
    )
    .expect("seed file");

    let mut index = DerivedVaultIndex::new();
    let event = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "notes/a.md".to_owned(),
        },
        observed_at: 100,
    };

    let first = apply_event(&provider, &mut index, &event);
    assert_eq!(first, AppliedEvent::Indexed);
    assert!(index.document_count() >= 1);
    let document_count_after_first = index.document_count();
    let chunk_count_after_first = index.chunk_count();

    // Re-applying the identical event changes nothing (hash gate).
    let second = apply_event(&provider, &mut index, &event);
    assert_eq!(second, AppliedEvent::Unchanged);
    assert_eq!(index.document_count(), document_count_after_first);
    assert_eq!(index.chunk_count(), chunk_count_after_first);
}

#[tokio::test]
async fn content_changes_are_indexed_unchanged_files_are_skipped() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    std::fs::write(directory.path().join("doc.md"), "v1\n").expect("seed");
    let mut index = DerivedVaultIndex::new();

    let updated = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "doc.md".to_owned(),
        },
        observed_at: 100,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &updated),
        AppliedEvent::Indexed
    );

    // No content change: the next event for the same file is a no-op.
    assert_eq!(
        apply_event(&provider, &mut index, &updated),
        AppliedEvent::Unchanged
    );

    // Content change: re-indexed.
    std::fs::write(directory.path().join("doc.md"), "v2 with novel words\n")
        .expect("external edit");
    assert_eq!(
        apply_event(&provider, &mut index, &updated),
        AppliedEvent::Indexed
    );
    let hits = cortex_search::lexical(&index, "novel", NonZeroUsize::new(10).expect("non-zero"))
        .expect("lexical works");
    assert_eq!(hits.len(), 1);
}

#[tokio::test]
async fn changed_content_replaces_all_prior_resource_chunks() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let path = directory.path().join("doc.md");
    std::fs::write(&path, "stale-token ".repeat(2_000)).expect("seed");
    let mut index = DerivedVaultIndex::new();
    let event = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "doc.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Indexed
    );
    std::fs::write(&path, "fresh-token\n").expect("replace");
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Indexed
    );
    assert!(
        cortex_search::lexical(
            &index,
            "stale-token",
            NonZeroUsize::new(10).expect("non-zero")
        )
        .expect("search")
        .is_empty()
    );
}

#[tokio::test]
async fn failed_replacement_parse_preserves_prior_index_state() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let path = directory.path().join("doc.md");
    std::fs::write(&path, "stable-token\n").expect("seed");
    let mut index = DerivedVaultIndex::new();
    let event = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "doc.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Indexed
    );

    // Unterminated frontmatter is rejected by the vault parser. The previous
    // value looked YAML-invalid but was accepted as ordinary document text.
    std::fs::write(&path, "---\ntitle: broken\n\nbody without close\n").expect("invalid markdown");
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Skipped
    );
    assert_eq!(
        cortex_search::lexical(
            &index,
            "stable-token",
            NonZeroUsize::new(10).expect("non-zero")
        )
        .expect("search")
        .len(),
        1
    );
}

#[tokio::test]
async fn parsed_empty_content_clears_prior_index_state() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let path = directory.path().join("doc.md");
    std::fs::write(&path, "previous content\n").expect("seed");
    let mut index = DerivedVaultIndex::new();
    let event = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "doc.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Indexed
    );

    std::fs::write(&path, "").expect("empty");
    assert_ne!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Skipped
    );
    assert_eq!(index.chunk_count(), 0);
    assert_eq!(index.document_count(), 0);
}

#[tokio::test]
async fn missing_created_hint_removes_existing_index_resource() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let path = directory.path().join("doc.md");
    std::fs::write(&path, "present\n").expect("seed");
    let mut index = DerivedVaultIndex::new();
    let event = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "doc.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Indexed
    );
    std::fs::remove_file(path).expect("remove");
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Removed
    );
    assert_eq!(index.chunk_count(), 0);
}

#[tokio::test]
async fn deleted_events_only_remove_what_exists() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    std::fs::write(directory.path().join("doc.md"), "content\n").expect("seed");
    let mut index = DerivedVaultIndex::new();

    let indexed = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "doc.md".to_owned(),
        },
        observed_at: 100,
    };
    apply_event(&provider, &mut index, &indexed);

    let deleted = CoalescedEvent {
        event: VaultEvent::Deleted {
            relative: "doc.md".to_owned(),
        },
        observed_at: 200,
    };
    std::fs::remove_file(directory.path().join("doc.md")).expect("external delete");
    assert_eq!(
        apply_event(&provider, &mut index, &deleted),
        AppliedEvent::Removed
    );
    assert_eq!(index.chunk_count(), 0);

    // Deleting again (file already gone, nothing indexed) is a no-op.
    assert_eq!(
        apply_event(&provider, &mut index, &deleted),
        AppliedEvent::Unchanged
    );
}

#[tokio::test]
async fn task_delete_event_removes_the_stable_task_resource() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(directory.path().join("Tasks")).expect("tasks directory");
    let task_id = "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12";
    let task_path = directory.path().join("Tasks/deleted.md");
    std::fs::write(
        &task_path,
        format!(
            "---\ntype: task\nbrain_id: {task_id}\nstatus: todo\npriority: high\n---\n\nDeleted task token\n"
        ),
    )
    .expect("task");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    let updated = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "Tasks/deleted.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &updated),
        AppliedEvent::Indexed
    );
    assert_eq!(
        index
            .documents()
            .next()
            .expect("task document")
            .resource_id()
            .as_str(),
        task_id
    );

    std::fs::remove_file(task_path).expect("delete task");
    let deleted = CoalescedEvent {
        event: VaultEvent::Deleted {
            relative: "Tasks/deleted.md".to_owned(),
        },
        observed_at: 2,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &deleted),
        AppliedEvent::Removed
    );
    assert_eq!(index.document_count(), 0);
    assert_eq!(index.chunk_count(), 0);
}

#[tokio::test]
async fn stale_delete_hint_indexes_the_current_confined_file_state() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    std::fs::write(directory.path().join("doc.md"), "still present\n").expect("seed");
    let mut index = DerivedVaultIndex::new();
    let deleted = CoalescedEvent {
        event: VaultEvent::Deleted {
            relative: "doc.md".to_owned(),
        },
        observed_at: 100,
    };

    assert_eq!(
        apply_event(&provider, &mut index, &deleted),
        AppliedEvent::Indexed
    );
    assert!(index.chunk_count() > 0);
}

#[tokio::test]
async fn unconfined_delete_hints_are_skipped_before_resource_identity_is_derived() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    let traversal = CoalescedEvent {
        event: VaultEvent::Deleted {
            relative: "../outside.md".to_owned(),
        },
        observed_at: 100,
    };

    assert_eq!(
        apply_event(&provider, &mut index, &traversal),
        AppliedEvent::Skipped
    );
    assert_eq!(index.document_count(), 0);
    assert_eq!(index.chunk_count(), 0);
}

#[tokio::test]
async fn excluded_delete_hints_are_skipped() {
    let directory = TempDir::new().expect("temp dir");
    let mut exclusions = BTreeSet::new();
    exclusions.insert(cortexd::VaultExclusion::new(".obsidian").expect("valid"));
    let config = VaultProviderConfig::new(
        "markdown-vault",
        directory.path().to_path_buf(),
        VaultProviderMode::ReadWrite,
        all_scopes(),
        exclusions,
    )
    .expect("config");
    let provider = MarkdownVaultProvider::open(config, WorkspaceId::new()).expect("provider");
    let mut index = DerivedVaultIndex::new();
    let event = CoalescedEvent {
        event: VaultEvent::Deleted {
            relative: ".obsidian/state.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Skipped
    );
}

#[tokio::test]
async fn accepted_but_unrepresentable_long_path_is_skipped_without_aliasing() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    let relative = format!("{}.md", "a".repeat(600));
    let event = CoalescedEvent {
        event: VaultEvent::Deleted { relative },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &event),
        AppliedEvent::Skipped
    );
    assert_eq!(index.document_count(), 0);
}

#[tokio::test]
async fn excluded_and_oversized_paths_are_skipped_without_error() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let mut exclusions = BTreeSet::new();
    exclusions.insert(cortexd::VaultExclusion::new(".obsidian").expect("valid"));
    let config = VaultProviderConfig::new(
        "markdown-vault",
        directory.path().to_path_buf(),
        VaultProviderMode::ReadWrite,
        all_scopes(),
        exclusions,
    )
    .expect("valid config");
    let provider = MarkdownVaultProvider::open(config, workspace_id).expect("opens");
    let mut index = DerivedVaultIndex::new();

    // Excluded path.
    std::fs::create_dir_all(directory.path().join(".obsidian")).expect("dir");
    std::fs::write(
        directory.path().join(".obsidian/plugins.md"),
        "secret plugin config\n",
    )
    .expect("excluded file");
    let excluded = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: ".obsidian/plugins.md".to_owned(),
        },
        observed_at: 100,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &excluded),
        AppliedEvent::Skipped
    );
    assert_eq!(index.chunk_count(), 0);

    // Oversized file: skipped, never fatal.
    let oversized = directory.path().join("huge.md");
    std::fs::write(&oversized, "x".repeat(2 * 1024 * 1024)).expect("oversized file");
    let oversized_event = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "huge.md".to_owned(),
        },
        observed_at: 100,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &oversized_event),
        AppliedEvent::Skipped
    );

    // Unparseable (non-UTF-8) content: skipped.
    let binary = directory.path().join("binary.md");
    std::fs::write(&binary, [0xFF, 0xFE, 0x00]).expect("binary file");
    let binary_event = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "binary.md".to_owned(),
        },
        observed_at: 100,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &binary_event),
        AppliedEvent::Skipped
    );
}

#[tokio::test]
async fn rename_applies_as_delete_plus_create() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    std::fs::write(directory.path().join("old.md"), "moved content\n").expect("seed");
    let mut index = DerivedVaultIndex::new();

    let indexed = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "old.md".to_owned(),
        },
        observed_at: 100,
    };
    apply_event(&provider, &mut index, &indexed);
    assert!(index.chunk_count() > 0);

    // The watcher applies the actual rename transition atomically at index
    // level: the old resource is removed and the destination is indexed.
    std::fs::rename(
        directory.path().join("old.md"),
        directory.path().join("new.md"),
    )
    .expect("rename");
    let renamed = CoalescedEvent {
        event: VaultEvent::Renamed {
            from: "old.md".to_owned(),
            to: "new.md".to_owned(),
        },
        observed_at: 200,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &renamed),
        AppliedEvent::Indexed
    );

    let hits = cortex_search::lexical(
        &index,
        "moved content",
        NonZeroUsize::new(10).expect("non-zero"),
    )
    .expect("lexical works");
    assert_eq!(hits.len(), 1);
    assert_eq!(
        hits[0].reference.resource.resource_id().as_str(),
        "path:new.md"
    );
}

#[tokio::test]
async fn task_rename_event_preserves_the_stable_task_resource() {
    let directory = TempDir::new().expect("temp dir");
    std::fs::create_dir_all(directory.path().join("Tasks")).expect("tasks directory");
    let task_id = "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12";
    let original = directory.path().join("Tasks/original.md");
    std::fs::write(
        &original,
        format!(
            "---\ntype: task\nbrain_id: {task_id}\nstatus: todo\npriority: high\n---\n\nRenamed task token\n"
        ),
    )
    .expect("task");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let mut index = DerivedVaultIndex::new();
    let updated = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "Tasks/original.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &updated),
        AppliedEvent::Indexed
    );

    std::fs::rename(&original, directory.path().join("Tasks/moved.md")).expect("rename task");
    let renamed = CoalescedEvent {
        event: VaultEvent::Renamed {
            from: "Tasks/original.md".to_owned(),
            to: "Tasks/moved.md".to_owned(),
        },
        observed_at: 2,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &renamed),
        AppliedEvent::Unchanged
    );
    let resources: Vec<&str> = index
        .documents()
        .map(|resource| resource.resource_id().as_str())
        .collect();
    assert_eq!(resources, vec![task_id]);
    assert_eq!(index.document_count(), 1);
}

#[tokio::test]
async fn queued_rename_followed_by_update_removes_old_resource() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    std::fs::write(directory.path().join("old.md"), "old unique token\n").expect("seed");
    let mut index = DerivedVaultIndex::new();
    let old = CoalescedEvent {
        event: VaultEvent::Updated {
            relative: "old.md".to_owned(),
        },
        observed_at: 1,
    };
    assert_eq!(
        apply_event(&provider, &mut index, &old),
        AppliedEvent::Indexed
    );
    std::fs::rename(
        directory.path().join("old.md"),
        directory.path().join("new.md"),
    )
    .expect("rename");
    std::fs::write(directory.path().join("new.md"), "new unique token\n").expect("update");
    let mut queue = EventQueue::new(10);
    queue.push(
        VaultEvent::Renamed {
            from: "old.md".to_owned(),
            to: "new.md".to_owned(),
        },
        1,
    );
    queue.push(
        VaultEvent::Updated {
            relative: "new.md".to_owned(),
        },
        2,
    );
    let drained = queue.drain(13);
    assert_eq!(drained.len(), 1);
    assert_eq!(
        drained[0].event,
        VaultEvent::Renamed {
            from: "old.md".to_owned(),
            to: "new.md".to_owned()
        }
    );
    assert_eq!(drained[0].observed_at, 2);
    assert_eq!(
        apply_event(&provider, &mut index, &drained[0]),
        AppliedEvent::Indexed
    );
    assert!(
        cortex_search::lexical(
            &index,
            "old unique",
            NonZeroUsize::new(10).expect("non-zero")
        )
        .expect("search")
        .is_empty()
    );
}
