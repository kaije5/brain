use cortex_application::{
    AggregateChange, AtomicMutation, AtomicMutationPort, CommandContext, MemoryRepository,
    MutationResult, NoteRepository, SourceRepository, TaskRepository,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, MemoryAssertion, MemoryAssertionInput, Note, NoteInput,
    OperationId, PolicyDecision, PrincipalId, Source, SourceInput, SourceRef, Task, TaskInput,
    WorkspaceId,
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

    let note = Note::create(NoteInput {
        workspace_id,
        title: "Cortex".to_owned(),
        content: "Local knowledge".to_owned(),
    })
    .map_err(debug_error)?;
    let task = Task::create(TaskInput {
        workspace_id,
        title: "Verify persistence".to_owned(),
        due_at: None,
    })
    .map_err(debug_error)?;
    let source = Source::create(SourceInput {
        workspace_id,
        reference: "note:cortex".to_owned(),
    })
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
    let context = context(workspace_id, principal_id);
    let audit = audit(&context, note.id());
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    let mutation = AtomicMutation::new(
        context,
        vec![
            AggregateChange::InsertNote(note.clone()),
            AggregateChange::InsertTask(task.clone()),
            AggregateChange::InsertSource(source.clone()),
            AggregateChange::InsertMemory(memory.clone()),
        ],
        result,
        audit,
    )
    .map_err(debug_error)?;
    operations
        .execute_once(mutation)
        .await
        .map_err(debug_error)?;

    assert_eq!(
        NoteRepository::find(&repositories, workspace_id, note.id())
            .await
            .map_err(debug_error)?,
        Some(note.clone())
    );
    assert_eq!(
        TaskRepository::find(&repositories, workspace_id, task.id())
            .await
            .map_err(debug_error)?,
        Some(task)
    );
    assert_eq!(
        SourceRepository::find(&repositories, workspace_id, source.id())
            .await
            .map_err(debug_error)?,
        Some(source)
    );
    assert_eq!(
        MemoryRepository::find(&repositories, workspace_id, memory.id())
            .await
            .map_err(debug_error)?,
        Some(memory)
    );
    assert_eq!(
        NoteRepository::find(&repositories, other_workspace_id, note.id())
            .await
            .map_err(debug_error)?,
        None
    );
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

fn audit(context: &CommandContext, target_id: cortex_domain::EntityId) -> AuditEvent {
    AuditEvent {
        id: AuditEventId::new(),
        workspace_id: context.workspace_id,
        principal_id: context.principal_id,
        operation_id: context.operation_id,
        correlation_id: context.correlation_id,
        capability: "cortex_note_create",
        target_id: Some(target_id),
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    }
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
