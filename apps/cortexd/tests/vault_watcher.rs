//! Debounced idempotent vault watching (SCRUM-113).

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use cortex_domain::WorkspaceId;
use cortex_search::DerivedVaultIndex;
use cortexd::{
    AppliedEvent, CoalescedEvent, EventQueue, MarkdownVaultProvider, VaultEvent,
    VaultProviderConfig, VaultProviderMode, VaultScope, apply_event,
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
