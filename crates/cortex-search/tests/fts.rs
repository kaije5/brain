use std::num::NonZeroUsize;

use cortex_application::{
    AggregateChange, AtomicMutation, AtomicMutationPort, Capability, CommandContext, MutationResult,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, Lifecycle, MemoryAssertion, MemoryAssertionInput,
    OperationId, PolicyDecision, PrincipalId, Revision, Source, SourceInput, SourceRef,
    WorkspaceId,
};
use cortex_search::{EntityKind, SearchIndex};
use cortex_storage::SqliteDatabase;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn fts_filters_workspace_grant_lifecycle_and_returns_provenance() -> Result<(), String> {
    let temp = TempDir::new().map_err(debug_error)?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
    let repositories = database.repositories();
    let workspace_id = WorkspaceId::new();
    let other_workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    let denied_principal_id = PrincipalId::new();
    setup_workspace(
        &repositories,
        workspace_id,
        principal_id,
        denied_principal_id,
    )
    .await?;
    setup_workspace(
        &repositories,
        other_workspace_id,
        PrincipalId::new(),
        PrincipalId::new(),
    )
    .await?;
    repositories
        .grant_capability(workspace_id, principal_id, Capability::KnowledgeRetrieve)
        .await
        .map_err(debug_error)?;

    let (source, memory) = index_active_memory(&database, workspace_id, principal_id).await?;
    index_deleted_memory(&database, workspace_id, principal_id).await?;

    let limit = NonZeroUsize::new(10).ok_or("non-zero limit required")?;
    let hits = SearchIndex::lexical_candidates(
        &repositories,
        workspace_id,
        principal_id,
        "Nemotron",
        limit,
    )
    .await
    .map_err(debug_error)?;

    assert_visible_memory(&hits, &memory, &source);
    assert_empty_search(&repositories, workspace_id, denied_principal_id, limit).await?;
    assert_empty_search(&repositories, other_workspace_id, principal_id, limit).await?;
    Ok(())
}

async fn index_active_memory(
    database: &SqliteDatabase,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
) -> Result<(Source, MemoryAssertion), String> {
    let source = Source::create(SourceInput {
        workspace_id,
        reference: "decision:local-model".to_owned(),
    })
    .map_err(debug_error)?;
    let memory = memory(
        workspace_id,
        "Cortex uses Nemotron as its local AI.",
        &source,
    )?;
    persist_memory(database, principal_id, source.clone(), memory.clone()).await?;
    database
        .repositories()
        .upsert_search_document(
            workspace_id,
            memory.id(),
            EntityKind::Memory,
            memory.statement(),
        )
        .await
        .map_err(debug_error)?;
    Ok((source, memory))
}

async fn index_deleted_memory(
    database: &SqliteDatabase,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
) -> Result<(), String> {
    let source = Source::create(SourceInput {
        workspace_id,
        reference: "decision:deleted".to_owned(),
    })
    .map_err(debug_error)?;
    let memory = MemoryAssertion::rehydrate(
        cortex_domain::EntityId::new(),
        workspace_id,
        "Deleted Nemotron claim".to_owned(),
        "cortex".to_owned(),
        "uses".to_owned(),
        "nemotron".to_owned(),
        vec![SourceRef {
            source_id: source.id(),
        }],
        None,
        cortex_domain::MemoryStatus::Active,
        Revision::initial(),
        Lifecycle::Deleted,
    )
    .map_err(debug_error)?;
    persist_memory(database, principal_id, source, memory.clone()).await?;
    database
        .repositories()
        .upsert_search_document(
            workspace_id,
            memory.id(),
            EntityKind::Memory,
            memory.statement(),
        )
        .await
        .map_err(debug_error)
}

fn assert_visible_memory(
    hits: &[cortex_search::SearchCandidate],
    memory: &MemoryAssertion,
    source: &Source,
) {
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entity_id, memory.id());
    assert_eq!(hits[0].kind, EntityKind::Memory);
    assert_eq!(hits[0].snippet, memory.statement());
    assert_eq!(
        hits[0].sources,
        vec![SourceRef {
            source_id: source.id()
        }]
    );
}

async fn assert_empty_search(
    repositories: &cortex_storage::SqliteRepositories,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    limit: NonZeroUsize,
) -> Result<(), String> {
    let hits = SearchIndex::lexical_candidates(
        repositories,
        workspace_id,
        principal_id,
        "Nemotron",
        limit,
    )
    .await
    .map_err(debug_error)?;
    assert!(hits.is_empty());
    Ok(())
}

async fn setup_workspace(
    repositories: &cortex_storage::SqliteRepositories,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    denied_principal_id: PrincipalId,
) -> Result<(), String> {
    repositories
        .create_workspace(workspace_id, "workspace")
        .await
        .map_err(debug_error)?;
    repositories
        .create_principal(workspace_id, principal_id, "principal")
        .await
        .map_err(debug_error)?;
    repositories
        .create_principal(workspace_id, denied_principal_id, "denied")
        .await
        .map_err(debug_error)
}

fn memory(
    workspace_id: WorkspaceId,
    statement: &str,
    source: &Source,
) -> Result<MemoryAssertion, String> {
    MemoryAssertion::create(MemoryAssertionInput {
        workspace_id,
        statement: statement.to_owned(),
        normalized_subject: "cortex".to_owned(),
        normalized_predicate: "uses".to_owned(),
        normalized_object: "nemotron".to_owned(),
        sources: vec![SourceRef {
            source_id: source.id(),
        }],
    })
    .map_err(debug_error)
}

async fn persist_memory(
    database: &SqliteDatabase,
    principal_id: PrincipalId,
    source: Source,
    memory: MemoryAssertion,
) -> Result<(), String> {
    let context = CommandContext::from_authenticated(
        memory.workspace_id(),
        principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    );
    let result = MutationResult {
        entity_id: memory.id(),
        revision: memory.revision(),
        lifecycle: memory.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    let mutation = AtomicMutation::new(
        context,
        Capability::MemoryCreate,
        None,
        vec![
            AggregateChange::InsertSource(source.clone()),
            AggregateChange::InsertMemory(memory.clone()),
            AggregateChange::LinkMemorySource {
                memory_id: memory.id(),
                source_id: source.id(),
            },
        ],
        result,
        AuditEvent {
            id: AuditEventId::new(),
            workspace_id: memory.workspace_id(),
            principal_id,
            operation_id: context.operation_id,
            correlation_id: context.correlation_id,
            capability: Capability::MemoryCreate.metadata().mcp_name,
            target_id: Some(memory.id()),
            policy_decision: PolicyDecision::Allow,
            result: AuditResult::Succeeded,
        },
    )
    .map_err(debug_error)?;
    database
        .operation_store()
        .execute_once(mutation)
        .await
        .map(|_| ())
        .map_err(debug_error)
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
