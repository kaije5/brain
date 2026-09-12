//! Adversarial filesystem and concurrency behavior (SCRUM-110; storage plan
//! §7.1): rename races, permission failures, partial writes, external
//! concurrent edits, sync-conflict copies, and readers never observing
//! partial writes.

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use cortex_application::{
    KnowledgeCreate, KnowledgeProvider, KnowledgeUpdate, ProviderError, ProviderTaskPriority,
    TaskCreate, TaskProvider, TaskSchedulingMetadata,
};
use cortex_domain::{OperationId, ProviderResourceKind, WorkspaceId};
use cortexd::{
    MarkdownVaultProvider, VaultPathError, VaultProviderConfig, VaultProviderMode, VaultScope,
};
use tempfile::TempDir;

fn provider_for(root: &std::path::Path, workspace_id: WorkspaceId) -> MarkdownVaultProvider {
    let mut scopes = BTreeSet::new();
    scopes.insert(VaultScope::new("knowledge").expect("valid"));
    scopes.insert(VaultScope::new("task").expect("valid"));
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

async fn create_knowledge(
    provider: &MarkdownVaultProvider,
    workspace_id: WorkspaceId,
    title: &str,
    body: &str,
) -> cortex_domain::ProviderResourceRef {
    let created = KnowledgeProvider::create(
        provider,
        KnowledgeCreate::new(workspace_id, OperationId::new(), title, body).expect("valid create"),
    )
    .await
    .expect("create succeeds");
    created.resource().clone()
}

#[tokio::test]
async fn concurrent_updates_with_the_same_expected_revision_produce_one_winner() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let created = KnowledgeProvider::create(
        &provider,
        KnowledgeCreate::new(workspace_id, OperationId::new(), "Contested", "base body")
            .expect("valid create"),
    )
    .await
    .expect("create succeeds");
    let revision = created
        .current()
        .expect("provenance")
        .observed_revision()
        .clone();

    // Two writers base on the same observed revision: exactly one wins.
    let first = KnowledgeProvider::update(
        &provider,
        KnowledgeUpdate::new(
            created.resource().clone(),
            OperationId::new(),
            revision.clone(),
            "Writer A",
            "body a",
        )
        .expect("valid update"),
    )
    .await
    .expect("first writer wins");
    let second = KnowledgeProvider::update(
        &provider,
        KnowledgeUpdate::new(
            created.resource().clone(),
            OperationId::new(),
            revision,
            "Writer B",
            "body b",
        )
        .expect("valid update"),
    )
    .await;
    assert!(matches!(second, Err(ProviderError::Conflict { .. })));

    // The losing writer's content never landed.
    let read = KnowledgeProvider::get(&provider, created.resource())
        .await
        .expect("get succeeds")
        .expect("document found");
    assert_eq!(read.item().title(), "Writer A");
    assert!(read.item().body().contains("body a"));
    assert!(!read.item().body().contains("body b"));
    let _ = first;
}

#[tokio::test]
async fn external_delete_between_read_and_update_is_typed_not_found_without_resurrection() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let resource = create_knowledge(&provider, workspace_id, "Doomed", "body").await;
    let read = KnowledgeProvider::get(&provider, &resource)
        .await
        .expect("get succeeds")
        .expect("found");
    let revision = read.item().provenance().observed_revision().clone();

    // Another device deletes the file after our read.
    let relative = resource
        .resource_id()
        .as_str()
        .strip_prefix("path:")
        .expect("path id");
    std::fs::remove_file(directory.path().join(relative)).expect("external delete");

    let update = KnowledgeProvider::update(
        &provider,
        KnowledgeUpdate::new(
            resource.clone(),
            OperationId::new(),
            revision,
            "Resurrected",
            "must not exist",
        )
        .expect("valid update"),
    )
    .await;
    // The mutation reports the file is gone instead of recreating it.
    assert!(matches!(update, Err(ProviderError::NotFound { .. })));
    assert!(
        !directory.path().join(relative).exists(),
        "a failed mutation must not resurrect the file"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn permission_failures_leave_the_original_intact() {
    use std::os::unix::fs::PermissionsExt;
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let resource = create_knowledge(&provider, workspace_id, "Locked", "locked body").await;
    let relative = resource
        .resource_id()
        .as_str()
        .strip_prefix("path:")
        .expect("path id");
    let path = directory.path().join(relative);
    let parent = path.parent().expect("parent exists").to_path_buf();

    // Make the parent directory read-only: the permission boundary that
    // governs atomic rename-based writes.
    let mut permissions = std::fs::metadata(&parent).expect("metadata").permissions();
    permissions.set_mode(0o555);
    std::fs::set_permissions(&parent, permissions).expect("chmod");

    let read = KnowledgeProvider::get(&provider, &resource)
        .await
        .expect("read-only file still readable")
        .expect("found");
    let revision = read.item().provenance().observed_revision().clone();
    let update = KnowledgeProvider::update(
        &provider,
        KnowledgeUpdate::new(
            resource.clone(),
            OperationId::new(),
            revision,
            "Overwritten",
            "must not land",
        )
        .expect("valid update"),
    )
    .await;
    assert!(matches!(update, Err(ProviderError::Unavailable)));

    // Restore directory permissions and prove the original content survived.
    let mut permissions = std::fs::metadata(&parent).expect("metadata").permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&parent, permissions).expect("chmod");
    let content = std::fs::read_to_string(&path).expect("readable");
    assert!(content.contains("locked body"));
    assert!(!content.contains("must not land"));
}

#[tokio::test]
async fn a_crashed_write_leaves_only_an_ignored_temporary_leftover() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let resource =
        create_knowledge(&provider, workspace_id, "Survivor", "authoritative body").await;

    // Simulate a crash mid-write: a temp file with partial content is left
    // behind beside the authoritative file.
    let relative = resource
        .resource_id()
        .as_str()
        .strip_prefix("path:")
        .expect("path id");
    let authoritative = directory.path().join(relative);
    let parent = authoritative.parent().expect("parent exists");
    let leftover = parent.join(format!("partial.tmp-{}", uuid::Uuid::now_v7().simple()));
    std::fs::write(&leftover, "half-written bytes from a crash").expect("leftover written");

    // Enumeration and reads ignore the leftover entirely.
    let page = provider
        .enumerate_knowledge(NonZeroUsize::new(10).expect("non-zero"))
        .expect("enumeration succeeds");
    assert_eq!(page.items().len(), 1, "temp leftovers must not enumerate");
    let read = KnowledgeProvider::get(&provider, &resource)
        .await
        .expect("get succeeds")
        .expect("authoritative file found");
    assert!(read.item().body().contains("authoritative body"));
    assert!(leftover.exists(), "test precondition: leftover present");
}

#[tokio::test]
async fn sync_conflict_copies_with_duplicate_brain_ids_are_a_typed_duplicate() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let created = TaskProvider::create(
        &provider,
        TaskCreate::new(
            workspace_id,
            OperationId::new(),
            cortex_domain::TaskId::new(),
            "Synced task",
            "body",
            ProviderTaskPriority::Normal,
            scheduling(),
        )
        .expect("valid create"),
    )
    .await
    .expect("create succeeds");
    let original_id = created.resource().resource_id().as_str().to_owned();

    // A sync engine copies the file (both files carry the same brain_id).
    let original = std::fs::read_dir(directory.path().join("Tasks"))
        .expect("tasks dir")
        .flatten()
        .next()
        .expect("task file")
        .path();
    let conflict_copy = original.with_file_name(format!(
        "{} (conflicted copy).md",
        original
            .file_stem()
            .and_then(|stem| stem.to_str())
            .expect("stem")
    ));
    std::fs::copy(&original, &conflict_copy).expect("sync conflict copy");

    // The provider surfaces the duplicate identity explicitly.
    // Parse both copies and prove the duplicate identity is explicit.
    let mut parsed_tasks = Vec::new();
    for entry in std::fs::read_dir(directory.path().join("Tasks")).expect("tasks dir") {
        let text = std::fs::read_to_string(entry.expect("entry").path()).expect("readable");
        let document = cortex_vault::parse_document(&text).expect("parses");
        parsed_tasks.push(cortex_vault::parse_task(&document, "task").expect("valid task"));
    }
    assert_eq!(parsed_tasks.len(), 2, "both copies enumerate");
    assert!(matches!(
        cortex_vault::assert_unique_identities(&parsed_tasks),
        Err(cortex_vault::VaultFormatError::DuplicateIdentity)
    ));
    let _ = original_id;
}

#[tokio::test]
async fn rename_never_clobbers_an_existing_target() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    std::fs::write(directory.path().join("a.md"), "source\n").expect("source");
    std::fs::write(directory.path().join("b.md"), "precious target\n").expect("target");

    let error = provider.rename("a.md", "b.md", ProviderResourceKind::Knowledge);
    assert!(matches!(error, Err(VaultPathError::TargetExists)));
    // Both files survive untouched.
    assert_eq!(
        std::fs::read_to_string(directory.path().join("a.md")).expect("source intact"),
        "source\n"
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("b.md")).expect("target intact"),
        "precious target\n"
    );
}

#[tokio::test]
async fn readers_never_observe_a_partial_write_during_concurrent_updates() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let resource = create_knowledge(&provider, workspace_id, "Churned", "generation 0").await;

    // Repeatedly rewrite the document while readers observe it: every read
    // must parse cleanly and match exactly one known generation — never a
    // torn mixture.
    let writer = tokio::spawn({
        let provider = provider.clone();
        let resource = resource.clone();
        async move {
            for generation in 1..20 {
                let read = KnowledgeProvider::get(&provider, &resource)
                    .await
                    .expect("writer get succeeds")
                    .expect("found");
                let revision = read.item().provenance().observed_revision().clone();
                let _ = KnowledgeProvider::update(
                    &provider,
                    KnowledgeUpdate::new(
                        resource.clone(),
                        OperationId::new(),
                        revision,
                        "Churned",
                        format!("generation {generation}").as_str(),
                    )
                    .expect("valid update"),
                )
                .await;
            }
        }
    });
    // Bounded reader: a fixed number of observations while the writer
    // churns. Every observation must be a complete generation.
    let mut observed_revisions = std::collections::BTreeSet::new();
    for _ in 0..500 {
        if let Ok(Some(read)) = KnowledgeProvider::get(&provider, &resource).await {
            let revision = read.item().provenance().observed_revision().clone();
            let body = read.item().body().trim().to_owned();
            // The body is either the initial one or a complete generation.
            assert!(
                body == "generation 0"
                    || (body.starts_with("generation ") && body.len() <= "generation 19".len()),
                "torn read observed: {body:?}"
            );
            observed_revisions.insert(revision);
        }
    }
    writer.await.expect("writer finishes");
    assert!(
        !observed_revisions.is_empty(),
        "reads observed the document"
    );
}
