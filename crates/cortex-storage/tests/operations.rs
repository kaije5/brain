use cortex_application::{
    AggregateChange, ApplicationError, AtomicMutation, AtomicMutationPort, AuditPort, Capability,
    CommandContext, MemoryRepository, MutationResult, NoteRepository, OperationResultRepository,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, MemoryAssertion, MemoryAssertionInput, Note, NoteInput,
    OperationId, PolicyDecision, PrincipalId, ProviderId, ProviderResourceId, ProviderResourceKind,
    ProviderResourceRef, ResourceTarget, SourceRef, WorkspaceId,
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

#[tokio::test]
async fn durable_replay_rejects_a_different_principal() -> Result<(), String> {
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let operation_id = OperationId::new();
    let note = Note::create(NoteInput {
        workspace_id,
        title: "principal bound".to_owned(),
        content: "one actor only".to_owned(),
    })
    .map_err(debug_error)?;
    let original = context(workspace_id, principal_id, operation_id);
    operations
        .execute_once(note_mutation(original, note.clone())?)
        .await
        .map_err(debug_error)?;
    let different_principal = context(workspace_id, PrincipalId::new(), operation_id);

    let replay = operations
        .execute_once(note_mutation(different_principal, note)?)
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    Ok(())
}

#[tokio::test]
async fn durable_replay_rejects_a_different_capability() -> Result<(), String> {
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let context = context(workspace_id, principal_id, OperationId::new());
    let note = Note::create(NoteInput {
        workspace_id,
        title: "capability bound".to_owned(),
        content: "one command only".to_owned(),
    })
    .map_err(debug_error)?;
    operations
        .execute_once(note_mutation(context, note.clone())?)
        .await
        .map_err(debug_error)?;
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    let different_capability = AtomicMutation::new(
        context,
        Capability::TaskCreate,
        None,
        vec![AggregateChange::InsertNote(note)],
        result,
        audit(&context, result.entity_id, "cortex_task_create"),
    )
    .map_err(debug_error)?;

    let replay = operations.execute_once(different_capability).await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    Ok(())
}

#[tokio::test]
async fn durable_replay_rejects_a_different_target() -> Result<(), String> {
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let context = context(workspace_id, principal_id, OperationId::new());
    let first = Note::create(NoteInput {
        workspace_id,
        title: "first target".to_owned(),
        content: "persisted".to_owned(),
    })
    .map_err(debug_error)?;
    let first_id = first.id();
    operations
        .execute_once(bound_note_mutation(
            context,
            first,
            Capability::NoteUpdate,
            Some(cortex_domain::ResourceTarget::CortexEntity(first_id)),
        )?)
        .await
        .map_err(debug_error)?;
    let second = Note::create(NoteInput {
        workspace_id,
        title: "second target".to_owned(),
        content: "must not persist".to_owned(),
    })
    .map_err(debug_error)?;

    let replay = operations
        .execute_once(bound_note_mutation(
            context,
            second.clone(),
            Capability::NoteUpdate,
            Some(cortex_domain::ResourceTarget::CortexEntity(second.id())),
        )?)
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    assert_eq!(
        NoteRepository::find(&database.repositories(), workspace_id, second.id())
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
        Capability::MemoryCreate,
        None,
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
    let command_context = context(workspace_id, principal_id, OperationId::new());
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: command_context.correlation_id,
    };
    let existing_context = context(workspace_id, principal_id, OperationId::new());
    let existing_audit = audit(&existing_context, note.id(), "cortex_note_create");
    database
        .audit_port()
        .append(existing_audit.clone())
        .await
        .map_err(debug_error)?;
    let mut failed_audit = audit(&command_context, note.id(), "cortex_note_create");
    failed_audit.id = existing_audit.id;
    let failed_audit_id = failed_audit.id;
    let failed_mutation = AtomicMutation::new(
        command_context,
        Capability::NoteCreate,
        None,
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
        Some(existing_audit)
    );

    let retry_audit = audit(&command_context, note.id(), "cortex_note_create");
    let retry_audit_id = retry_audit.id;
    let retry = AtomicMutation::new(
        command_context,
        Capability::NoteCreate,
        None,
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
    bound_note_mutation(context, note, Capability::NoteCreate, None)
}

fn bound_note_mutation(
    context: CommandContext,
    note: Note,
    capability: Capability,
    target: Option<cortex_domain::ResourceTarget>,
) -> Result<AtomicMutation, String> {
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: context.correlation_id,
    };
    AtomicMutation::new(
        context,
        capability,
        target,
        vec![AggregateChange::InsertNote(note)],
        result,
        audit(&context, result.entity_id, capability.metadata().mcp_name),
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
        target: Some(cortex_domain::ResourceTarget::CortexEntity(target_id)),
        provider_metadata: None,
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    }
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}

#[tokio::test]
async fn durable_replay_rejects_a_provider_target_change() -> Result<(), String> {
    let (database, _temp, workspace_id, principal_id) = database().await?;
    let operations = database.operation_store();
    let operation_id = OperationId::new();
    let note = Note::create(NoteInput {
        workspace_id,
        title: "provider bound".to_owned(),
        content: "one resource only".to_owned(),
    })
    .map_err(debug_error)?;
    let provider = ProviderResourceRef::new(
        workspace_id,
        ProviderId::new("primary-vault").map_err(debug_error)?,
        ProviderResourceId::new("01K4RESOURCE").map_err(debug_error)?,
        ProviderResourceKind::Knowledge,
    );
    let original_context = context(workspace_id, principal_id, operation_id);
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: original_context.correlation_id,
    };
    operations
        .execute_once(bound_note_mutation(
            original_context,
            note.clone(),
            Capability::NoteUpdate,
            Some(ResourceTarget::ProviderResource(provider)),
        )?)
        .await
        .map_err(debug_error)?;

    let other_provider = ProviderResourceRef::new(
        workspace_id,
        ProviderId::new("primary-vault").map_err(debug_error)?,
        ProviderResourceId::new("01K4OTHER").map_err(debug_error)?,
        ProviderResourceKind::Knowledge,
    );
    let replay_context = context(workspace_id, principal_id, operation_id);
    let result = MutationResult {
        entity_id: note.id(),
        revision: note.revision(),
        lifecycle: note.lifecycle(),
        audit_correlation_id: replay_context.correlation_id,
    };
    let replay = operations
        .execute_once(bound_note_mutation(
            replay_context,
            note,
            Capability::NoteUpdate,
            Some(ResourceTarget::ProviderResource(other_provider)),
        )?)
        .await;

    assert_eq!(
        replay,
        Err(ApplicationError::Conflict {
            entity: "operation"
        })
    );
    Ok(())
}

#[tokio::test]
async fn legacy_operation_outcomes_decode_with_entity_targets() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database_path = temp.path().join("cortex.db");
    let database = SqliteDatabase::connect_and_migrate(database_path.clone())
        .await
        .map_err(debug_error)?;
    let raw_pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database_path.display()))
        .await
        .map_err(|error| format!("raw pool failed: {error}"))?;
    let operations = database.operation_store();
    let workspace_id = WorkspaceId::new();
    let principal_id = PrincipalId::new();
    database
        .repositories()
        .create_workspace(workspace_id, "owner")
        .await
        .map_err(debug_error)?;
    database
        .repositories()
        .create_principal(workspace_id, principal_id, "owner")
        .await
        .map_err(debug_error)?;
    let note = Note::create(NoteInput {
        workspace_id,
        title: "legacy outcome".to_owned(),
        content: "version two".to_owned(),
    })
    .map_err(debug_error)?;
    let operation_id = OperationId::new();
    let context = context(workspace_id, principal_id, operation_id);
    // A version 2 outcome JSON stores a plain entity UUID target.
    let outcome_v2 = format!(
        concat!(
            "{{\"version\":2,\"principal_id\":\"{}\",\"capability\":\"cortex_note_update\",",
            "\"target_id\":\"{}\",\"entity_id\":\"{}\",\"revision\":1,\"lifecycle\":\"active\",",
            "\"audit_correlation_id\":\"{}\"}}"
        ),
        Uuid::from(principal_id),
        Uuid::from(note.id()),
        Uuid::from(note.id()),
        context.correlation_id
    );
    seed_operation(raw_pool.clone(), workspace_id, operation_id, &outcome_v2).await?;

    let recorded = operations
        .find_result(workspace_id, operation_id)
        .await
        .map_err(debug_error)?
        .ok_or("legacy operation missing")?;
    assert_eq!(
        recorded.identity.target,
        Some(ResourceTarget::CortexEntity(note.id()))
    );
    assert_eq!(recorded.result.entity_id, note.id());
    Ok(())
}

async fn seed_operation(
    raw_pool: sqlx::SqlitePool,
    workspace_id: WorkspaceId,
    operation_id: OperationId,
    outcome_json: &str,
) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO operation (workspace_id, operation_id, outcome_json) VALUES (?, ?, ?)",
    )
    .bind(Uuid::from(workspace_id).to_string())
    .bind(Uuid::from(operation_id).to_string())
    .bind(outcome_json)
    .execute(&raw_pool)
    .await
    .map_err(|error| format!("operation seed failed: {error}"))?;
    Ok(())
}
