use cortex_application::{
    AggregateChange, AtomicMutation, AtomicMutationPort, CommandContext, MemoryRepository,
    MutationResult, NoteRepository,
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
    let first_audit_id = first_mutation.audit_event.id;
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
    let duplicate = note_mutation(context, replacement.clone())?;
    let duplicate_audit_id = duplicate.audit_event.id;
    let second = operations
        .execute_once(duplicate)
        .await
        .map_err(debug_error)?;

    assert_eq!(second, first);
    assert_eq!(
        NoteRepository::find(&database.repositories(), workspace_id, note.id())
            .await
            .map_err(debug_error)?,
        Some(note)
    );
    assert_eq!(
        NoteRepository::find(&database.repositories(), workspace_id, replacement.id())
            .await
            .map_err(debug_error)?,
        None
    );
    assert!(
        database
            .audit_port()
            .find(workspace_id, first_audit_id)
            .await
            .map_err(debug_error)?
            .is_some()
    );
    assert_eq!(
        database
            .audit_port()
            .find(workspace_id, duplicate_audit_id)
            .await
            .map_err(debug_error)?,
        None
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_duplicate_operations_return_one_durable_result() -> Result<(), String> {
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let context = context(workspace_id, principal_id, OperationId::new());
    let first_note = Note::create(NoteInput {
        workspace_id,
        title: "first concurrent candidate".to_owned(),
        content: "only one candidate may persist".to_owned(),
    })
    .map_err(debug_error)?;
    let second_note = Note::create(NoteInput {
        workspace_id,
        title: "second concurrent candidate".to_owned(),
        content: "the replay must return the winning result".to_owned(),
    })
    .map_err(debug_error)?;
    let first_mutation = note_mutation(context, first_note.clone())?;
    let second_mutation = note_mutation(context, second_note.clone())?;
    let first_store = operations.clone();
    let second_store = operations.clone();

    let first = tokio::spawn(async move { first_store.execute_once(first_mutation).await });
    let second = tokio::spawn(async move { second_store.execute_once(second_mutation).await });
    let (first, second) = tokio::join!(first, second);
    let first = first
        .map_err(|error| format!("first task failed: {error}"))?
        .map_err(debug_error)?;
    let second = second
        .map_err(|error| format!("second task failed: {error}"))?
        .map_err(debug_error)?;

    assert_eq!(first, second);
    let (winning_note, losing_note) = if first.entity_id == first_note.id() {
        (first_note, second_note)
    } else {
        assert_eq!(first.entity_id, second_note.id());
        (second_note, first_note)
    };
    assert_eq!(
        NoteRepository::find(&database.repositories(), workspace_id, winning_note.id())
            .await
            .map_err(debug_error)?,
        Some(winning_note)
    );
    assert_eq!(
        NoteRepository::find(&database.repositories(), workspace_id, losing_note.id())
            .await
            .map_err(debug_error)?,
        None
    );
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
    let memory_id = memory.id();
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
    let audit_id = mutation.audit_event.id;

    assert!(operations.execute_once(mutation).await.is_err());
    assert_eq!(
        MemoryRepository::find(&database.repositories(), workspace_id, memory_id)
            .await
            .map_err(debug_error)?,
        None
    );
    assert_eq!(
        database
            .audit_port()
            .find(workspace_id, audit_id)
            .await
            .map_err(debug_error)?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn audit_insert_failure_rolls_back_entity_audit_and_operation() -> Result<(), String> {
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let note = Note::create(NoteInput {
        workspace_id,
        title: "audit rollback".to_owned(),
        content: "the aggregate must not survive a failed audit write".to_owned(),
    })
    .map_err(debug_error)?;
    let context = context(workspace_id, principal_id, OperationId::new());
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    let failed_audit = audit(&context, note.id(), "cortex_unknown_capability");
    let failed_audit_id = failed_audit.id;
    let failed_mutation = AtomicMutation::new(
        context,
        vec![AggregateChange::InsertNote(note.clone())],
        result,
        failed_audit,
    )
    .map_err(debug_error)?;

    assert!(operations.execute_once(failed_mutation).await.is_err());
    assert_eq!(
        NoteRepository::find(&database.repositories(), workspace_id, note.id())
            .await
            .map_err(debug_error)?,
        None
    );
    assert_eq!(
        database
            .audit_port()
            .find(workspace_id, failed_audit_id)
            .await
            .map_err(debug_error)?,
        None
    );

    let retry_audit = audit(&context, note.id(), "cortex_note_create");
    let retry_audit_id = retry_audit.id;
    let retry = AtomicMutation::new(
        context,
        vec![AggregateChange::InsertNote(note.clone())],
        result,
        retry_audit,
    )
    .map_err(debug_error)?;
    assert_eq!(
        operations.execute_once(retry).await.map_err(debug_error)?,
        result
    );
    assert_eq!(
        NoteRepository::find(&database.repositories(), workspace_id, note.id())
            .await
            .map_err(debug_error)?,
        Some(note)
    );
    assert!(
        database
            .audit_port()
            .find(workspace_id, retry_audit_id)
            .await
            .map_err(debug_error)?
            .is_some()
    );
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
