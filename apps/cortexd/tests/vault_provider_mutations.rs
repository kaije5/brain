//! Atomic create/update/delete/rename with optimistic concurrency (SCRUM-108).

use std::collections::BTreeSet;

use cortex_application::{
    KnowledgeDelete, KnowledgeProvider, KnowledgeUpdate, ProviderError, ProviderTaskPriority,
    ProviderTaskStatus, TaskComplete, TaskCreate, TaskProvider, TaskSchedulingMetadata,
};
use cortex_domain::{OperationId, ProviderResourceKind, WorkspaceId};
use cortexd::{MarkdownVaultProvider, VaultProviderConfig, VaultProviderMode, VaultScope};
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

fn scheduling() -> TaskSchedulingMetadata {
    TaskSchedulingMetadata::new(
        None,
        None,
        None,
        None,
        None,
        Option::<String>::None,
        Option::<String>::None,
    )
    .expect("empty scheduling is valid")
}

#[tokio::test]
async fn knowledge_create_update_delete_round_trip_atomically() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);

    let created = KnowledgeProvider::create(
        &provider,
        cortex_application::KnowledgeCreate::new(
            workspace_id,
            OperationId::new(),
            "Round Trip",
            "original body",
        )
        .expect("valid create"),
    )
    .await
    .expect("create succeeds");
    let resource = created.resource().clone();
    let first_revision = created.current().expect("created provenance").clone();
    assert_eq!(
        first_revision.resource().resource_id(),
        resource.resource_id()
    );

    // An external edit lands first; the caller's revision is now stale and
    // the update must conflict carrying the current provenance.
    std::fs::write(
        directory.path().join("Documents").join("round-trip.md"),
        "---
title: Round Trip
---

externally edited body
",
    )
    .expect("external edit");
    let stale = KnowledgeProvider::update(
        &provider,
        KnowledgeUpdate::new(
            resource.clone(),
            OperationId::new(),
            first_revision.observed_revision().clone(),
            "Stale",
            "stale body",
        )
        .expect("valid update"),
    )
    .await;
    match stale {
        Err(ProviderError::Conflict { current }) => {
            assert_ne!(
                current.observed_revision(),
                first_revision.observed_revision()
            );
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    let unchanged =
        std::fs::read_to_string(directory.path().join("Documents").join("round-trip.md"))
            .expect("file still present");
    assert!(unchanged.contains("externally edited body"));
    assert!(!unchanged.contains("stale body"));

    // Reconcile: re-read observes the external state, then the update with
    // the fresh revision succeeds.
    let reconciled = KnowledgeProvider::get(&provider, &resource)
        .await
        .expect("get succeeds")
        .expect("document found");
    let updated = KnowledgeProvider::update(
        &provider,
        KnowledgeUpdate::new(
            resource.clone(),
            OperationId::new(),
            reconciled.item().provenance().observed_revision().clone(),
            "Round Trip 2",
            "externally edited body
reconciled update",
        )
        .expect("valid update"),
    )
    .await
    .expect("update succeeds");
    assert_ne!(
        updated.current().expect("current").observed_revision(),
        reconciled.item().provenance().observed_revision()
    );

    // Delete with the fresh revision succeeds.
    let deleted = KnowledgeProvider::delete(
        &provider,
        KnowledgeDelete::new(
            resource.clone(),
            OperationId::new(),
            updated
                .current()
                .expect("current")
                .observed_revision()
                .clone(),
        )
        .expect("valid delete"),
    )
    .await
    .expect("delete succeeds");
    assert!(deleted.current().is_none());
    let get_after_delete = KnowledgeProvider::get(&provider, &resource)
        .await
        .expect("get runs");
    assert!(get_after_delete.is_none());
}

#[tokio::test]
async fn task_create_complete_delete_preserves_identity_and_unknowns() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);

    let created = TaskProvider::create(
        &provider,
        TaskCreate::new(
            workspace_id,
            OperationId::new(),
            cortex_domain::TaskId::new(),
            "Write report",
            "report body",
            ProviderTaskPriority::High,
            scheduling(),
        )
        .expect("valid create"),
    )
    .await
    .expect("create succeeds");

    // The file lives under Tasks/ and carries the managed frontmatter.
    let task_path = created.resource().resource_id().as_str().to_owned();
    let _ = task_path;
    let mut seeded_file: Option<std::path::PathBuf> = None;
    for entry in std::fs::read_dir(directory.path().join("Tasks")).expect("tasks dir") {
        seeded_file = Some(entry.expect("entry").path());
    }
    let seeded_file = seeded_file.expect("task file created");
    let on_disk = std::fs::read_to_string(&seeded_file).expect("readable");
    assert!(on_disk.contains("type: task"));
    assert!(on_disk.contains("status: todo"));
    assert!(on_disk.contains("priority: high"));
    assert!(on_disk.contains("brain_id: "));

    // Complete it: status flips to done in frontmatter only.
    let parsed = cortex_vault::parse_document(&on_disk).expect("parses");
    let task = cortex_vault::parse_task(&parsed, "task").expect("valid task");
    let resource = created.resource().clone();
    let read = TaskProvider::get(&provider, &resource)
        .await
        .expect("get succeeds")
        .expect("task found");
    let revision = read.item().provenance().observed_revision().clone();

    let completed = TaskProvider::complete(
        &provider,
        TaskComplete::new(resource.clone(), OperationId::new(), revision).expect("valid complete"),
    )
    .await
    .expect("complete succeeds");
    let after = std::fs::read_to_string(&seeded_file).expect("readable");
    assert!(after.contains("status: done"));
    assert!(!after.contains("status: todo"));
    // Body prose is never touched by frontmatter rewrites.
    assert!(after.contains("report body"));

    // Delete with the fresh revision.
    let fresh = TaskProvider::get(&provider, &resource)
        .await
        .expect("get succeeds")
        .expect("task found");
    TaskProvider::delete(
        &provider,
        cortex_application::TaskDelete::new(
            resource.clone(),
            OperationId::new(),
            fresh.item().provenance().observed_revision().clone(),
        )
        .expect("valid delete"),
    )
    .await
    .expect("delete succeeds");
    assert!(!seeded_file.exists());
    assert_eq!(task.status(), ProviderTaskStatus::Todo);
    let _ = completed;
}

#[tokio::test]
async fn stale_task_revision_completing_is_a_conflict_not_an_overwrite() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);

    let created = TaskProvider::create(
        &provider,
        TaskCreate::new(
            workspace_id,
            OperationId::new(),
            cortex_domain::TaskId::new(),
            "Conflicted",
            "body",
            ProviderTaskPriority::Normal,
            scheduling(),
        )
        .expect("valid create"),
    )
    .await
    .expect("create succeeds");
    let resource = created.resource().clone();

    // An external device edits the file first.
    let path = directory.path().join("Tasks").join(
        std::fs::read_dir(directory.path().join("Tasks"))
            .expect("tasks dir")
            .flatten()
            .next()
            .expect("task file")
            .file_name(),
    );
    let external = std::fs::read_to_string(&path).expect("readable").replace(
        "priority: normal",
        "priority: urgent\nexternal_note: changed elsewhere",
    );
    std::fs::write(&path, external).expect("external edit");

    // Brain tries to complete with a stale revision: conflict, and the
    // external change is still present (no blind overwrite).
    let stale = cortex_vault::parse_document(&std::fs::read_to_string(&path).expect("readable"))
        .expect("parses");
    let stale_task = cortex_vault::parse_task(&stale, "task").expect("valid");
    let stale_revision = stale_task_before_external_edit();
    let _ = stale_task;
    let result = TaskProvider::complete(
        &provider,
        TaskComplete::new(resource, OperationId::new(), stale_revision).expect("valid complete"),
    )
    .await;
    assert!(matches!(result, Err(ProviderError::Conflict { .. })));
    let after = std::fs::read_to_string(&path).expect("readable");
    assert!(after.contains("external_note: changed elsewhere"));
    assert!(after.contains("priority: urgent"));
    assert!(after.contains("status: todo"));
}

fn stale_task_before_external_edit() -> cortex_domain::ObservedRevision {
    // A revision observed before the external edit. Any deterministic token
    // that differs from the current content works; reuse a plausible value.
    cortex_domain::ObservedRevision::new("rev-0000000000000000").expect("bounded")
}

#[tokio::test]
async fn rename_moves_the_file_and_tasks_keep_their_identity() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);

    let created = TaskProvider::create(
        &provider,
        TaskCreate::new(
            workspace_id,
            OperationId::new(),
            cortex_domain::TaskId::new(),
            "Movable task",
            "body",
            ProviderTaskPriority::Normal,
            scheduling(),
        )
        .expect("valid create"),
    )
    .await
    .expect("create succeeds");
    let resource = created.resource().clone();

    let file_name = std::fs::read_dir(directory.path().join("Tasks"))
        .expect("tasks dir")
        .flatten()
        .next()
        .expect("task file")
        .file_name();
    let old_relative = format!("Tasks/{}", file_name.to_str().expect("utf-8 file name"));
    let old_path = directory.path().join("Tasks").join(&file_name);
    let new_relative = "Tasks/archived/moved.md";
    std::fs::create_dir_all(directory.path().join("Tasks/archived")).expect("archive dir");
    provider
        .rename(&old_relative, new_relative, ProviderResourceKind::Task)
        .expect("rename succeeds");
    assert!(!old_path.exists());

    // Identity follows brain_id, not the path: the task is found after move.
    let read = TaskProvider::get(&provider, &resource)
        .await
        .expect("get succeeds")
        .expect("task found after move");
    assert_eq!(read.item().title(), "moved");
}

#[tokio::test]
async fn unique_path_allocation_never_overwrites_an_existing_file() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let create = |title: String| {
        cortex_application::KnowledgeCreate::new(workspace_id, OperationId::new(), title, "body")
            .expect("valid create")
    };
    let first = KnowledgeProvider::create(&provider, create("Same Title".to_owned()))
        .await
        .expect("first create");
    let second = KnowledgeProvider::create(&provider, create("Same Title".to_owned()))
        .await
        .expect("second create");
    assert_ne!(
        first.resource().resource_id().as_str(),
        second.resource().resource_id().as_str()
    );
    assert!(
        directory
            .path()
            .join("Documents")
            .join("same-title.md")
            .exists()
    );
    assert!(
        directory
            .path()
            .join("Documents")
            .join("same-title-2.md")
            .exists()
    );
}
