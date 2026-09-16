use cortex_application::{
    AggregateChange, AtomicMutation, AtomicMutationPort, Capability, CommandContext,
    MemoryRepository, MutationResult, SourceRepository,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, Lifecycle, MemoryAssertion,
    MemoryAssertionInput, OperationId, PolicyDecision, PrincipalId, ResourceTarget, Revision,
    Source, SourceInput, SourceRef, WorkspaceId,
};
use cortex_storage::{SqliteDatabase, SqliteRepositories};
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn repositories_round_trip_each_aggregate_without_crossing_workspaces() -> Result<(), String>
{
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
    let repositories = database.repositories();
    let operations = database.operation_store();
    let (workspace_id, other_workspace_id, principal_id) = setup(&repositories).await?;

    let source = Source::create(SourceInput {
        workspace_id,
        reference: "memory:cortex".to_owned(),
    })
    .map_err(debug_error)?;
    let deleted_source = Source::rehydrate(
        EntityId::new(),
        workspace_id,
        "deleted evidence".to_owned(),
        Revision::initial(),
        Lifecycle::Deleted,
    )
    .map_err(debug_error)?;
    let memory = MemoryAssertion::create(MemoryAssertionInput {
        workspace_id,
        statement: "Cortex is local first".to_owned(),
        normalized_subject: "cortex".to_owned(),
        normalized_predicate: "architecture".to_owned(),
        normalized_object: "local-first".to_owned(),
        sources: vec![SourceRef {
            source_id: source.id(),
        }],
    })
    .map_err(debug_error)?;
    let create_context = context(workspace_id, principal_id);
    let create_audit = audit(&create_context, memory.id(), "cortex_memory_create");
    let result = MutationResult {
        entity_id: memory.id(),
        revision: memory.revision(),
        lifecycle: memory.lifecycle(),
        audit_correlation_id: create_context.correlation_id,
    };
    let mutation = AtomicMutation::new(
        create_context,
        Capability::MemoryCreate,
        None,
        vec![
            AggregateChange::InsertSource(source.clone()),
            AggregateChange::InsertSource(deleted_source.clone()),
            AggregateChange::InsertMemory(memory.clone()),
        ],
        result,
        create_audit,
    )
    .map_err(debug_error)?;
    operations
        .execute_once(mutation)
        .await
        .map_err(debug_error)?;

    assert_source_visibility(&repositories, workspace_id, source, deleted_source).await?;
    assert_memory_visible(&repositories, workspace_id, memory.clone()).await?;
    assert_eq!(
        MemoryRepository::find(&repositories, other_workspace_id, memory.id())
            .await
            .map_err(debug_error)?,
        None
    );

    assert_memory_tombstone_visibility(
        &operations,
        &repositories,
        workspace_id,
        principal_id,
        &memory,
    )
    .await?;
    Ok(())
}

async fn assert_source_visibility(
    repositories: &SqliteRepositories,
    workspace_id: WorkspaceId,
    active: Source,
    deleted: Source,
) -> Result<(), String> {
    assert_eq!(
        SourceRepository::find(repositories, workspace_id, active.id())
            .await
            .map_err(debug_error)?,
        Some(active)
    );
    assert_eq!(
        SourceRepository::find(repositories, workspace_id, deleted.id())
            .await
            .map_err(debug_error)?,
        None
    );
    Ok(())
}

async fn assert_memory_visible(
    repositories: &SqliteRepositories,
    workspace_id: WorkspaceId,
    memory: MemoryAssertion,
) -> Result<(), String> {
    assert_eq!(
        MemoryRepository::find(repositories, workspace_id, memory.id())
            .await
            .map_err(debug_error)?,
        Some(memory)
    );
    Ok(())
}

async fn assert_memory_tombstone_visibility(
    operations: &impl AtomicMutationPort,
    repositories: &SqliteRepositories,
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    memory: &MemoryAssertion,
) -> Result<(), String> {
    let delete_context = context(workspace_id, principal_id);
    let delete_revision = memory.revision().next().map_err(debug_error)?;
    let delete_result = MutationResult {
        entity_id: memory.id(),
        revision: delete_revision,
        lifecycle: Lifecycle::Deleted,
        audit_correlation_id: delete_context.correlation_id,
    };
    let delete = AtomicMutation::new(
        delete_context,
        Capability::MemoryDelete,
        Some(ResourceTarget::CortexEntity(memory.id())),
        vec![AggregateChange::DeleteMemory {
            entity_id: memory.id(),
            expected_revision: memory.revision(),
        }],
        delete_result,
        audit(&delete_context, memory.id(), "cortex_memory_delete"),
    )
    .map_err(debug_error)?;
    operations.execute_once(delete).await.map_err(debug_error)?;
    assert_eq!(
        MemoryRepository::find(repositories, workspace_id, memory.id())
            .await
            .map_err(debug_error)?,
        None
    );
    let tombstone = MemoryRepository::find_history(repositories, workspace_id, memory.id())
        .await
        .map_err(debug_error)?
        .ok_or("memory tombstone missing")?;
    assert_eq!(tombstone.lifecycle(), Lifecycle::Deleted);
    assert_eq!(tombstone.revision(), delete_revision);
    Ok(())
}

async fn setup(
    repositories: &SqliteRepositories,
) -> Result<(WorkspaceId, WorkspaceId, PrincipalId), String> {
    let workspace_id = WorkspaceId::new();
    let other_workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    repositories
        .create_workspace(workspace_id, "owner")
        .await
        .map_err(debug_error)?;
    repositories
        .create_principal(workspace_id, principal_id, "owner")
        .await
        .map_err(debug_error)?;
    repositories
        .create_workspace(other_workspace_id, "other")
        .await
        .map_err(debug_error)?;
    Ok((workspace_id, other_workspace_id, principal_id))
}

fn context(workspace_id: WorkspaceId, principal_id: PrincipalId) -> CommandContext {
    CommandContext::from_authenticated(
        workspace_id,
        principal_id,
        OperationId::new(),
        Uuid::now_v7(),
    )
}

fn audit(context: &CommandContext, target_id: EntityId, capability: &'static str) -> AuditEvent {
    AuditEvent {
        id: AuditEventId::new(),
        workspace_id: context.workspace_id,
        principal_id: context.principal_id,
        operation_id: context.operation_id,
        correlation_id: context.correlation_id,
        capability,
        target: Some(ResourceTarget::CortexEntity(target_id)),
        provider_metadata: None,
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    }
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
