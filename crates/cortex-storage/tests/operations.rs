use cortex_application::{
    AggregateChange, AtomicMutation, AtomicMutationPort, CommandContext, MutationResult,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, MemoryAssertion, MemoryAssertionInput, Note, NoteInput,
    OperationId, PolicyDecision, PrincipalId, SourceRef, WorkspaceId,
};
use cortex_storage::SqliteDatabase;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn repeated_operation_id_returns_original_result_without_second_effects() -> Result<(), String>
{
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let note = Note::create(NoteInput {
        workspace_id,
        title: "idempotent".to_owned(),
        content: "original".to_owned(),
    })
    .map_err(debug_error)?;
    let context = context(workspace_id, principal_id, OperationId::new());
    let first_mutation = note_mutation(context, note.clone())?;
    let first = operations
        .execute_once(first_mutation)
        .await
        .map_err(debug_error)?;

    let replacement = Note::create(NoteInput {
        workspace_id,
        title: "must not persist".to_owned(),
        content: "duplicate operation".to_owned(),
    })
    .map_err(debug_error)?;
    let duplicate = note_mutation(context, replacement)?;
    let second = operations
        .execute_once(duplicate)
        .await
        .map_err(debug_error)?;

    assert_eq!(second, first);
    assert_eq!(
        database
            .repositories()
            .note_count()
            .await
            .map_err(debug_error)?,
        1
    );
    assert_eq!(
        database
            .audit_port()
            .event_count()
            .await
            .map_err(debug_error)?,
        1
    );
    assert_eq!(operations.operation_count().await.map_err(debug_error)?, 1);
    Ok(())
}

#[tokio::test]
async fn failed_mutation_rolls_back_entity_audit_and_operation() -> Result<(), String> {
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let missing_source = SourceRef {
        source_id: cortex_domain::EntityId::new(),
    };
    let memory = MemoryAssertion::create(MemoryAssertionInput {
        workspace_id,
        statement: "unsupported persisted claim".to_owned(),
        normalized_subject: "claim".to_owned(),
        normalized_predicate: "support".to_owned(),
        normalized_object: "missing".to_owned(),
        sources: vec![missing_source],
    })
    .map_err(debug_error)?;
    let context = context(workspace_id, principal_id, OperationId::new());
    let result = MutationResult {
        entity_id: memory.id(),
        revision: memory.revision(),
        lifecycle: memory.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    let mutation = AtomicMutation::new(
        context,
        vec![AggregateChange::InsertMemory(memory)],
        result,
        audit(&context, result.entity_id, "cortex_memory_create"),
    )
    .map_err(debug_error)?;

    assert!(operations.execute_once(mutation).await.is_err());
    assert_eq!(
        database
            .repositories()
            .memory_count()
            .await
            .map_err(debug_error)?,
        0
    );
    assert_eq!(
        database
            .audit_port()
            .event_count()
            .await
            .map_err(debug_error)?,
        0
    );
    assert_eq!(operations.operation_count().await.map_err(debug_error)?, 0);
    Ok(())
}

async fn database() -> Result<(SqliteDatabase, TempDir, WorkspaceId, PrincipalId), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    let repositories = database.repositories();
    repositories
        .create_workspace(workspace_id, "owner")
        .await
        .map_err(debug_error)?;
    repositories
        .create_principal(workspace_id, principal_id, "owner")
        .await
        .map_err(debug_error)?;
    Ok((database, temp, workspace_id, principal_id))
}

fn context(
    workspace_id: WorkspaceId,
    principal_id: PrincipalId,
    operation_id: OperationId,
) -> CommandContext {
    CommandContext::from_authenticated(workspace_id, principal_id, operation_id, Uuid::now_v7())
}

fn note_mutation(context: CommandContext, note: Note) -> Result<AtomicMutation, String> {
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    AtomicMutation::new(
        context,
        vec![AggregateChange::InsertNote(note)],
        result,
        audit(&context, result.entity_id, "cortex_note_create"),
    )
    .map_err(debug_error)
}

fn audit(
    context: &CommandContext,
    target_id: cortex_domain::EntityId,
    capability: &'static str,
) -> AuditEvent {
    AuditEvent {
        id: AuditEventId::new(),
        workspace_id: context.workspace_id,
        principal_id: context.principal_id,
        operation_id: context.operation_id,
        correlation_id: context.correlation_id,
        capability,
        target_id: Some(target_id),
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    }
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
