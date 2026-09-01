use std::num::NonZeroUsize;

use cortex_application::{
    AggregateChange, AtomicMutation, AtomicMutationPort, Capability, CommandContext, Embedding,
    EntityKind, MutationResult, SearchIndex,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, Lifecycle, MemoryAssertion, MemoryAssertionInput,
    OperationId, PolicyDecision, PrincipalId, Revision, Source, SourceInput, SourceRef,
    WorkspaceId,
};
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

#[tokio::test]
async fn lexical_memory_candidates_require_at_least_one_active_source() -> Result<(), String> {
    let (_temp, database, workspace_id, principal_id) = authorized_database().await?;
    let no_active = vec![source(workspace_id, "deleted-only", Lifecycle::Deleted)?];
    let active = source(workspace_id, "active", Lifecycle::Active)?;
    let one_active = vec![
        source(workspace_id, "also-deleted", Lifecycle::Deleted)?,
        active.clone(),
    ];
    let hidden = index_memory_with_sources(
        &database,
        workspace_id,
        principal_id,
        "Provenance sentinel hidden",
        no_active,
    )
    .await?;
    let visible = index_memory_with_sources(
        &database,
        workspace_id,
        principal_id,
        "Provenance sentinel visible",
        one_active,
    )
    .await?;

    let hits = SearchIndex::lexical_candidates(
        &database.repositories(),
        workspace_id,
        principal_id,
        "Provenance sentinel",
        NonZeroUsize::new(10).ok_or("non-zero limit required")?,
    )
    .await
    .map_err(debug_error)?;

    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].entity_id, visible.id());
    assert_ne!(hits[0].entity_id, hidden.id());
    assert_eq!(
        hits[0].sources,
        vec![SourceRef {
            source_id: active.id()
        }]
    );
    Ok(())
}

#[tokio::test]
async fn semantic_memory_candidates_require_at_least_one_active_source() -> Result<(), String> {
    let (_temp, database, workspace_id, principal_id) = authorized_database().await?;
    let no_active = vec![source(workspace_id, "deleted-only", Lifecycle::Deleted)?];
    let active = source(workspace_id, "active", Lifecycle::Active)?;
    let one_active = vec![
        source(workspace_id, "also-deleted", Lifecycle::Deleted)?,
        active.clone(),
    ];
    let hidden = index_memory_with_sources(
        &database,
        workspace_id,
        principal_id,
        "Semantic provenance hidden",
        no_active,
    )
    .await?;
    let visible = index_memory_with_sources(
        &database,
        workspace_id,
        principal_id,
        "Semantic provenance visible",
        one_active,
    )
    .await?;
    let embedding = Embedding::new("nomic", "1", vec![1.0, 0.0]).map_err(debug_error)?;
    for memory in [&hidden, &visible] {
        database
            .repositories()
            .upsert_embedding(
                workspace_id,
                memory.id(),
                embedding.model_id(),
                embedding.model_version(),
                embedding.dimensions(),
                &embedding.to_le_bytes(),
            )
            .await
            .map_err(debug_error)?;
    }

    let records = SearchIndex::semantic_records(
        &database.repositories(),
        workspace_id,
        principal_id,
        &embedding,
        NonZeroUsize::new(10).ok_or("non-zero limit required")?,
    )
    .await
    .map_err(debug_error)?;

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].candidate.entity_id, visible.id());
    assert_ne!(records[0].candidate.entity_id, hidden.id());
    assert_eq!(
        records[0].candidate.sources,
        vec![SourceRef {
            source_id: active.id()
        }]
    );
    Ok(())
}

async fn authorized_database() -> Result<(TempDir, SqliteDatabase, WorkspaceId, PrincipalId), String>
{
    let temp = TempDir::new().map_err(debug_error)?;
    let path = temp.path().join("cortex.db");
    let database = SqliteDatabase::connect_and_migrate(path)
        .await
        .map_err(debug_error)?;
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    let repositories = database.repositories();
    repositories
        .create_workspace(workspace_id, "workspace")
        .await
        .map_err(debug_error)?;
    repositories
        .create_principal(workspace_id, principal_id, "principal")
        .await
        .map_err(debug_error)?;
    repositories
        .grant_capability(workspace_id, principal_id, Capability::KnowledgeRetrieve)
        .await
        .map_err(debug_error)?;
    Ok((temp, database, workspace_id, principal_id))
}

fn source(
    workspace_id: WorkspaceId,
    reference: &str,
    lifecycle: Lifecycle,
) -> Result<Source, String> {
    Source::rehydrate(
        cortex_domain::EntityId::new(),
        workspace_id,
        reference.to_owned(),
        Revision::initial(),
        lifecycle,
    )
    .map_err(debug_error)
}

async fn index_memory_with_sources(
    database: &SqliteDatabase,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    statement: &str,
    sources: Vec<Source>,
) -> Result<MemoryAssertion, String> {
    let memory = MemoryAssertion::create(MemoryAssertionInput {
        workspace_id,
        statement: statement.to_owned(),
        normalized_subject: "provenance".to_owned(),
        normalized_predicate: "requires".to_owned(),
        normalized_object: statement.to_ascii_lowercase(),
        sources: sources
            .iter()
            .map(|source| SourceRef {
                source_id: source.id(),
            })
            .collect(),
    })
    .map_err(debug_error)?;
    persist_memory(database, principal_id, sources, memory.clone()).await?;
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
    Ok(memory)
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
    persist_memory(database, principal_id, vec![source.clone()], memory.clone()).await?;
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
    persist_memory(database, principal_id, vec![source], memory.clone()).await?;
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
    hits: &[cortex_application::SearchCandidate],
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
    sources: Vec<Source>,
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
    let mut changes = sources
        .iter()
        .cloned()
        .map(AggregateChange::InsertSource)
        .collect::<Vec<_>>();
    changes.push(AggregateChange::InsertMemory(memory.clone()));
    changes.extend(
        sources
            .iter()
            .map(|source| AggregateChange::LinkMemorySource {
                memory_id: memory.id(),
                source_id: source.id(),
            }),
    );
    let mutation = AtomicMutation::new(
        context,
        Capability::MemoryCreate,
        None,
        changes,
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
