//! Confined enumeration and read operations over a seeded tempdir vault
//! (SCRUM-104).

use std::collections::BTreeSet;
use std::num::NonZeroUsize;

use cortex_application::{
    KnowledgeDocument, KnowledgeProvider, KnowledgeQuery, ProviderError, ProviderTaskPriority,
    ProviderTaskStatus, TaskProvider, TaskQuery,
};
use cortex_domain::{ProviderResourceKind, WorkspaceId};
use cortexd::{
    MarkdownVaultProvider, VaultExclusion, VaultProviderConfig, VaultProviderMode, VaultScope,
};
use tempfile::TempDir;

fn scopes() -> BTreeSet<VaultScope> {
    let mut set = BTreeSet::new();
    set.insert(VaultScope::new("knowledge").expect("knowledge scope"));
    set.insert(VaultScope::new("task").expect("task scope"));
    set
}

fn provider_for(root: &std::path::Path, workspace_id: WorkspaceId) -> MarkdownVaultProvider {
    let mut exclusions = BTreeSet::new();
    exclusions.insert(VaultExclusion::new(".obsidian").expect("valid exclusion"));
    let config = VaultProviderConfig::new(
        "markdown-vault",
        root.to_path_buf(),
        VaultProviderMode::ReadWrite,
        scopes(),
        exclusions,
    )
    .expect("valid config");
    MarkdownVaultProvider::open(config, workspace_id).expect("vault opens")
}

fn seeded_vault() -> (TempDir, WorkspaceId, MarkdownVaultProvider) {
    let directory = TempDir::new().expect("temp dir");
    let root = directory.path();
    std::fs::create_dir_all(root.join("notes/project")).expect("nested dirs");
    std::fs::create_dir_all(root.join("Tasks")).expect("tasks dir");
    std::fs::create_dir_all(root.join(".obsidian")).expect("excluded dir");
    std::fs::write(
        root.join("notes/project/atlas.md"),
        "---\ntitle: Atlas\n---\n\nAtlas migration body.\n",
    )
    .expect("seed doc");
    std::fs::write(
        root.join("notes/plain.md"),
        "# Plain Heading\n\nPlain body with keyword zebra.\n",
    )
    .expect("seed doc");
    std::fs::write(
        root.join("Tasks/one.md"),
        "---\ntype: task\nbrain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a12\nstatus: todo\npriority: high\ndue: 2026-09-17\n---\n\nFirst task body.\n",
    )
    .expect("seed task");
    std::fs::write(
        root.join("Tasks/two.md"),
        "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: done\npriority: low\n---\n\nSecond task mentioning zebra.\n",
    )
    .expect("seed task");
    std::fs::write(
        root.join(".obsidian/hidden.md"),
        "---\ntitle: Hidden\n---\n\nmust not enumerate\n",
    )
    .expect("seed excluded");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(root, workspace_id);
    (directory, workspace_id, provider)
}

#[tokio::test]
async fn enumeration_is_bounded_confined_and_workspace_scoped() {
    let (_temp, workspace_id, provider) = seeded_vault();
    let page = provider
        .enumerate_knowledge(NonZeroUsize::new(10).expect("non-zero"))
        .expect("enumeration succeeds");
    let references = page.items();
    assert_eq!(references.len(), 2, "excluded .obsidian must not enumerate");
    for reference in references {
        assert_eq!(reference.workspace_id(), workspace_id);
        assert_eq!(reference.kind(), ProviderResourceKind::Knowledge);
        assert!(reference.resource_id().as_str().starts_with("path:"));
    }

    // The limit is honored.
    let limited = provider
        .enumerate_knowledge(NonZeroUsize::new(1).expect("non-zero"))
        .expect("enumeration succeeds");
    assert_eq!(limited.items().len(), 1);
}

#[tokio::test]
async fn knowledge_get_returns_parsed_document_with_provenance() {
    let (_temp, _workspace_id, provider) = seeded_vault();
    let page = provider
        .enumerate_knowledge(NonZeroUsize::new(10).expect("non-zero"))
        .expect("enumeration succeeds");
    let reference = page
        .items()
        .iter()
        .find(|reference| reference.resource_id().as_str().contains("atlas"))
        .expect("atlas enumerated")
        .clone();

    let read = KnowledgeProvider::get(&provider, &reference)
        .await
        .expect("get succeeds")
        .expect("document found");
    let document: &KnowledgeDocument = read.item();
    assert_eq!(document.title(), "Atlas");
    assert!(document.body().contains("Atlas migration body"));
    let provenance = document.provenance();
    assert_eq!(provenance.resource(), &reference);
    assert!(provenance.observed_revision().as_str().starts_with("rev-"));
}

#[tokio::test]
async fn knowledge_get_for_a_missing_resource_is_typed_not_found() {
    let (_temp, workspace_id, provider) = seeded_vault();
    let resource = cortex_domain::ProviderResourceRef::new(
        workspace_id,
        cortex_domain::ProviderId::new("markdown-vault").expect("valid id"),
        cortex_domain::ProviderResourceId::new("path:notes/ghost.md").expect("valid id"),
        ProviderResourceKind::Knowledge,
    );
    // Missing files are a normal read outcome, not an error.
    let result = KnowledgeProvider::get(&provider, &resource).await;
    assert_eq!(result, Ok(None));
}

#[tokio::test]
async fn wrong_workspace_references_are_rejected() {
    let (_temp, _workspace, provider) = seeded_vault();
    let resource = cortex_domain::ProviderResourceRef::new(
        WorkspaceId::new(),
        cortex_domain::ProviderId::new("markdown-vault").expect("valid id"),
        cortex_domain::ProviderResourceId::new("path:notes/plain.md").expect("valid id"),
        ProviderResourceKind::Knowledge,
    );
    assert!(matches!(
        KnowledgeProvider::get(&provider, &resource).await,
        Err(ProviderError::Validation {
            field: "resource_kind"
        })
    ));
}

#[tokio::test]
async fn knowledge_search_filters_by_text_and_stays_bounded() {
    let (_temp, workspace_id, provider) = seeded_vault();
    let query = KnowledgeQuery::new(
        workspace_id,
        "zebra",
        NonZeroUsize::new(5).expect("non-zero"),
    )
    .expect("valid query");
    let matches = KnowledgeProvider::search(&provider, &query)
        .await
        .expect("search succeeds");
    assert_eq!(matches.items().len(), 1);
    assert_eq!(matches.items()[0].title(), "Plain Heading");

    let list = KnowledgeQuery::list(workspace_id, NonZeroUsize::new(1).expect("non-zero"))
        .expect("valid query");
    let limited = KnowledgeProvider::search(&provider, &list)
        .await
        .expect("search succeeds");
    assert_eq!(limited.items().len(), 1);
}

#[tokio::test]
async fn tasks_enumerate_by_brain_id_and_read_into_the_contract() {
    let (_temp, workspace_id, provider) = seeded_vault();
    let query = TaskQuery::new(
        workspace_id,
        Option::<String>::None,
        NonZeroUsize::new(10).expect("non-zero"),
    )
    .expect("valid query");
    let page = TaskProvider::search(&provider, &query)
        .await
        .expect("task search succeeds");
    assert_eq!(page.items().len(), 2);

    let todo = page
        .items()
        .iter()
        .find(|task| task.status() == ProviderTaskStatus::Todo)
        .expect("todo task present");
    assert_eq!(todo.priority(), ProviderTaskPriority::High);
    assert!(todo.scheduling().due_at().is_some());
    assert!(todo.body().contains("First task body"));

    // Get by the enumerated identity returns the same task.
    let resource = todo.provenance().resource().clone();
    let read = TaskProvider::get(&provider, &resource)
        .await
        .expect("get succeeds")
        .expect("task found");
    assert_eq!(read.item(), todo);
}

#[tokio::test]
async fn task_search_filters_by_text() {
    let (_temp, workspace_id, provider) = seeded_vault();
    let query = TaskQuery::new(
        workspace_id,
        Some("zebra"),
        NonZeroUsize::new(10).expect("non-zero"),
    )
    .expect("valid query");
    let matches = TaskProvider::search(&provider, &query)
        .await
        .expect("search succeeds");
    assert_eq!(matches.items().len(), 1);
    assert_eq!(matches.items()[0].status(), ProviderTaskStatus::Completed);
}

#[tokio::test]
async fn atomic_create_writes_a_parseable_document_in_a_dedicated_directory() {
    let directory = TempDir::new().expect("temp dir");
    let workspace_id = WorkspaceId::new();
    let provider = provider_for(directory.path(), workspace_id);
    let created = KnowledgeProvider::create(
        &provider,
        cortex_application::KnowledgeCreate::new(
            workspace_id,
            cortex_domain::OperationId::new(),
            "Atlas Plan",
            "created body",
        )
        .expect("valid create"),
    )
    .await
    .expect("create succeeds");
    let resource = created.resource();
    assert!(
        resource
            .resource_id()
            .as_str()
            .starts_with("path:Documents/")
    );

    let read = KnowledgeProvider::get(&provider, resource)
        .await
        .expect("get succeeds")
        .expect("created document found");
    assert_eq!(read.item().title(), "Atlas Plan");
    assert_eq!(read.item().body(), "
created body
");
}

#[test]
fn empty_vault_enumerates_to_an_empty_page() {
    let directory = TempDir::new().expect("temp dir");
    let provider = provider_for(directory.path(), WorkspaceId::new());
    let page = provider
        .enumerate_knowledge(NonZeroUsize::new(10).expect("non-zero"))
        .expect("enumeration succeeds");
    assert!(page.items().is_empty());
}
