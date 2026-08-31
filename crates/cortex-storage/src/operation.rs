use cortex_application::{
    AggregateChange, ApplicationError, AtomicMutation, AtomicMutationPort, MutationResult,
};
use cortex_domain::{
    EntityId, Lifecycle, MemoryAssertion, Note, Revision, Source, Task, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::{
    audit::insert_event_in_transaction,
    database::storage_error,
    repositories::{
        decode_lifecycle, encode_lifecycle, encode_memory_status, encode_task_status, id_text,
        parse_id,
    },
};

#[derive(Clone)]
pub struct OperationStore {
    pool: SqlitePool,
}

impl OperationStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn load_result(
        &self,
        workspace_id: WorkspaceId,
        operation_id: cortex_domain::OperationId,
    ) -> Result<Option<MutationResult>, ApplicationError> {
        let outcome: Option<String> = sqlx::query_scalar(
            "SELECT outcome_json FROM operation WHERE workspace_id = ? AND operation_id = ?",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(operation_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("operation lookup failed"))?;
        outcome.map(|value| decode_result(&value)).transpose()
    }
}

impl AtomicMutationPort for OperationStore {
    async fn execute_once(
        &self,
        mutation: AtomicMutation,
    ) -> Result<MutationResult, ApplicationError> {
        if let Some(result) = self
            .load_result(mutation.workspace_id, mutation.operation_id)
            .await?
        {
            return Ok(result);
        }

        let outcome_json = encode_result(mutation.result)?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| storage_error("transaction begin failed"))?;

        let reservation = sqlx::query(
            "INSERT INTO operation (workspace_id, operation_id, outcome_json) VALUES (?, ?, ?)",
        )
        .bind(id_text(mutation.workspace_id))
        .bind(id_text(mutation.operation_id))
        .bind(outcome_json)
        .execute(&mut *transaction)
        .await;
        if reservation.is_err() {
            transaction
                .rollback()
                .await
                .map_err(|_| storage_error("transaction rollback failed"))?;
            if let Some(result) = self
                .load_result(mutation.workspace_id, mutation.operation_id)
                .await?
            {
                return Ok(result);
            }
            return Err(storage_error("operation reservation failed"));
        }

        for change in &mutation.changes {
            apply_change(&mut transaction, mutation.workspace_id, change).await?;
        }
        insert_event_in_transaction(&mut transaction, &mutation.audit_event).await?;
        transaction
            .commit()
            .await
            .map_err(|_| storage_error("transaction commit failed"))?;
        Ok(mutation.result)
    }
}

async fn apply_change(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    change: &AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        AggregateChange::InsertNote(_)
        | AggregateChange::ReplaceNote { .. }
        | AggregateChange::DeleteNote { .. }
        | AggregateChange::RestoreNote { .. } => {
            apply_note_change(transaction, workspace_id, change).await
        }
        AggregateChange::InsertTask(_)
        | AggregateChange::ReplaceTask { .. }
        | AggregateChange::DeleteTask { .. }
        | AggregateChange::RestoreTask { .. } => {
            apply_task_change(transaction, workspace_id, change).await
        }
        AggregateChange::InsertMemory(_)
        | AggregateChange::ReplaceMemory { .. }
        | AggregateChange::DeleteMemory { .. }
        | AggregateChange::RestoreMemory { .. } => {
            apply_memory_change(transaction, workspace_id, change).await
        }
        AggregateChange::InsertSource(source) => {
            insert_source(transaction, workspace_id, source).await
        }
        AggregateChange::LinkMemorySource {
            memory_id,
            source_id,
        } => insert_memory_source(transaction, workspace_id, *memory_id, *source_id).await,
    }
}

async fn apply_note_change(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    change: &AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        AggregateChange::InsertNote(note) => insert_note(transaction, workspace_id, note).await,
        AggregateChange::ReplaceNote {
            entity_id,
            expected_revision,
            note,
        } => {
            replace_note(
                transaction,
                workspace_id,
                *entity_id,
                *expected_revision,
                note,
            )
            .await
        }
        AggregateChange::DeleteNote {
            entity_id,
            expected_revision,
        } => {
            set_lifecycle(
                transaction,
                "note",
                workspace_id,
                *entity_id,
                *expected_revision,
                Lifecycle::Deleted,
            )
            .await
        }
        AggregateChange::RestoreNote {
            entity_id,
            expected_revision,
        } => {
            set_lifecycle(
                transaction,
                "note",
                workspace_id,
                *entity_id,
                *expected_revision,
                Lifecycle::Active,
            )
            .await
        }
        _ => Err(ApplicationError::Internal),
    }
}

async fn apply_task_change(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    change: &AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        AggregateChange::InsertTask(task) => insert_task(transaction, workspace_id, task).await,
        AggregateChange::ReplaceTask {
            entity_id,
            expected_revision,
            task,
        } => {
            replace_task(
                transaction,
                workspace_id,
                *entity_id,
                *expected_revision,
                task,
            )
            .await
        }
        AggregateChange::DeleteTask {
            entity_id,
            expected_revision,
        } => {
            set_lifecycle(
                transaction,
                "task",
                workspace_id,
                *entity_id,
                *expected_revision,
                Lifecycle::Deleted,
            )
            .await
        }
        AggregateChange::RestoreTask {
            entity_id,
            expected_revision,
        } => {
            set_lifecycle(
                transaction,
                "task",
                workspace_id,
                *entity_id,
                *expected_revision,
                Lifecycle::Active,
            )
            .await
        }
        _ => Err(ApplicationError::Internal),
    }
}

async fn apply_memory_change(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    change: &AggregateChange,
) -> Result<(), ApplicationError> {
    match change {
        AggregateChange::InsertMemory(memory) => {
            insert_memory(transaction, workspace_id, memory).await
        }
        AggregateChange::ReplaceMemory {
            entity_id,
            expected_revision,
            memory,
        } => {
            replace_memory(
                transaction,
                workspace_id,
                *entity_id,
                *expected_revision,
                memory,
            )
            .await
        }
        AggregateChange::DeleteMemory {
            entity_id,
            expected_revision,
        } => {
            set_lifecycle(
                transaction,
                "memory_assertion",
                workspace_id,
                *entity_id,
                *expected_revision,
                Lifecycle::Deleted,
            )
            .await
        }
        AggregateChange::RestoreMemory {
            entity_id,
            expected_revision,
        } => {
            set_lifecycle(
                transaction,
                "memory_assertion",
                workspace_id,
                *entity_id,
                *expected_revision,
                Lifecycle::Active,
            )
            .await
        }
        _ => Err(ApplicationError::Internal),
    }
}

async fn insert_note(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    note: &Note,
) -> Result<(), ApplicationError> {
    ensure_workspace(workspace_id, note.workspace_id())?;
    sqlx::query(
        "INSERT INTO note (id, workspace_id, title, content, revision, lifecycle) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id_text(note.id()))
    .bind(id_text(workspace_id))
    .bind(note.title())
    .bind(note.content())
    .bind(revision_i64(note.revision())?)
    .bind(encode_lifecycle(note.lifecycle()))
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("note insert failed"))?;
    Ok(())
}

async fn replace_note(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    entity_id: EntityId,
    expected_revision: Revision,
    note: &Note,
) -> Result<(), ApplicationError> {
    ensure_replacement(
        workspace_id,
        entity_id,
        expected_revision,
        note.workspace_id(),
        note.id(),
        note.revision(),
    )?;
    let result = sqlx::query(
        "UPDATE note SET title = ?, content = ?, revision = ?, lifecycle = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE workspace_id = ? AND id = ? AND revision = ?",
    )
    .bind(note.title())
    .bind(note.content())
    .bind(revision_i64(note.revision())?)
    .bind(encode_lifecycle(note.lifecycle()))
    .bind(id_text(workspace_id))
    .bind(id_text(entity_id))
    .bind(revision_i64(expected_revision)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("note replace failed"))?;
    require_updated(result.rows_affected(), "note")
}

async fn insert_task(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    task: &Task,
) -> Result<(), ApplicationError> {
    ensure_workspace(workspace_id, task.workspace_id())?;
    sqlx::query(
        "INSERT INTO task (id, workspace_id, title, due_at, status, revision, lifecycle) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id_text(task.id()))
    .bind(id_text(workspace_id))
    .bind(task.title())
    .bind(task.due_at().map(|value| value.to_rfc3339()))
    .bind(encode_task_status(task.status()))
    .bind(revision_i64(task.revision())?)
    .bind(encode_lifecycle(task.lifecycle()))
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("task insert failed"))?;
    Ok(())
}

async fn replace_task(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    entity_id: EntityId,
    expected_revision: Revision,
    task: &Task,
) -> Result<(), ApplicationError> {
    ensure_replacement(
        workspace_id,
        entity_id,
        expected_revision,
        task.workspace_id(),
        task.id(),
        task.revision(),
    )?;
    let result = sqlx::query(
        "UPDATE task SET title = ?, due_at = ?, status = ?, revision = ?, lifecycle = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE workspace_id = ? AND id = ? AND revision = ?",
    )
    .bind(task.title())
    .bind(task.due_at().map(|value| value.to_rfc3339()))
    .bind(encode_task_status(task.status()))
    .bind(revision_i64(task.revision())?)
    .bind(encode_lifecycle(task.lifecycle()))
    .bind(id_text(workspace_id))
    .bind(id_text(entity_id))
    .bind(revision_i64(expected_revision)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("task replace failed"))?;
    require_updated(result.rows_affected(), "task")
}

async fn insert_source(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    source: &Source,
) -> Result<(), ApplicationError> {
    ensure_workspace(workspace_id, source.workspace_id())?;
    sqlx::query(
        "INSERT INTO source (id, workspace_id, reference, revision, lifecycle) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id_text(source.id()))
    .bind(id_text(workspace_id))
    .bind(source.reference())
    .bind(revision_i64(source.revision())?)
    .bind(encode_lifecycle(source.lifecycle()))
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("source insert failed"))?;
    Ok(())
}

async fn insert_memory(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    memory: &MemoryAssertion,
) -> Result<(), ApplicationError> {
    ensure_workspace(workspace_id, memory.workspace_id())?;
    sqlx::query(
        "INSERT INTO memory_assertion \
         (id, workspace_id, statement, normalized_subject, normalized_predicate, normalized_object, \
          supersedes_id, status, revision, lifecycle) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id_text(memory.id()))
    .bind(id_text(workspace_id))
    .bind(memory.statement())
    .bind(memory.normalized_subject())
    .bind(memory.normalized_predicate())
    .bind(memory.normalized_object())
    .bind(memory.supersedes().map(id_text))
    .bind(encode_memory_status(memory.status()))
    .bind(revision_i64(memory.revision())?)
    .bind(encode_lifecycle(memory.lifecycle()))
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("memory insert failed"))?;
    for source in memory.sources() {
        insert_memory_source(transaction, workspace_id, memory.id(), source.source_id).await?;
    }
    Ok(())
}

async fn replace_memory(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    entity_id: EntityId,
    expected_revision: Revision,
    memory: &MemoryAssertion,
) -> Result<(), ApplicationError> {
    ensure_replacement(
        workspace_id,
        entity_id,
        expected_revision,
        memory.workspace_id(),
        memory.id(),
        memory.revision(),
    )?;
    let result = sqlx::query(
        "UPDATE memory_assertion SET statement = ?, normalized_subject = ?, normalized_predicate = ?, \
         normalized_object = ?, supersedes_id = ?, status = ?, revision = ?, lifecycle = ?, \
         updated_at = CURRENT_TIMESTAMP WHERE workspace_id = ? AND id = ? AND revision = ?",
    )
    .bind(memory.statement())
    .bind(memory.normalized_subject())
    .bind(memory.normalized_predicate())
    .bind(memory.normalized_object())
    .bind(memory.supersedes().map(id_text))
    .bind(encode_memory_status(memory.status()))
    .bind(revision_i64(memory.revision())?)
    .bind(encode_lifecycle(memory.lifecycle()))
    .bind(id_text(workspace_id))
    .bind(id_text(entity_id))
    .bind(revision_i64(expected_revision)?)
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("memory replace failed"))?;
    require_updated(result.rows_affected(), "memory")?;
    sqlx::query("DELETE FROM memory_source WHERE workspace_id = ? AND memory_id = ?")
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .execute(&mut **transaction)
        .await
        .map_err(|_| storage_error("memory provenance replace failed"))?;
    for source in memory.sources() {
        insert_memory_source(transaction, workspace_id, entity_id, source.source_id).await?;
    }
    Ok(())
}

async fn insert_memory_source(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    memory_id: EntityId,
    source_id: EntityId,
) -> Result<(), ApplicationError> {
    sqlx::query(
        "INSERT OR IGNORE INTO memory_source (workspace_id, memory_id, source_id) VALUES (?, ?, ?)",
    )
    .bind(id_text(workspace_id))
    .bind(id_text(memory_id))
    .bind(id_text(source_id))
    .execute(&mut **transaction)
    .await
    .map_err(|_| storage_error("memory provenance insert failed"))?;
    Ok(())
}

async fn set_lifecycle(
    transaction: &mut Transaction<'_, Sqlite>,
    table: &'static str,
    workspace_id: WorkspaceId,
    entity_id: EntityId,
    expected_revision: Revision,
    lifecycle: Lifecycle,
) -> Result<(), ApplicationError> {
    let new_revision = expected_revision.next().map_err(ApplicationError::from)?;
    let query = match table {
        "note" => {
            "UPDATE note SET lifecycle = ?, revision = ?, updated_at = CURRENT_TIMESTAMP \
             WHERE workspace_id = ? AND id = ? AND revision = ?"
        }
        "task" => {
            "UPDATE task SET lifecycle = ?, revision = ?, updated_at = CURRENT_TIMESTAMP \
             WHERE workspace_id = ? AND id = ? AND revision = ?"
        }
        "memory_assertion" => {
            "UPDATE memory_assertion SET lifecycle = ?, revision = ?, updated_at = CURRENT_TIMESTAMP \
             WHERE workspace_id = ? AND id = ? AND revision = ?"
        }
        _ => return Err(ApplicationError::Internal),
    };
    let result = sqlx::query(query)
        .bind(encode_lifecycle(lifecycle))
        .bind(revision_i64(new_revision)?)
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .bind(revision_i64(expected_revision)?)
        .execute(&mut **transaction)
        .await
        .map_err(|_| storage_error("lifecycle update failed"))?;
    require_updated(result.rows_affected(), table)
}

fn ensure_workspace(expected: WorkspaceId, actual: WorkspaceId) -> Result<(), ApplicationError> {
    if expected != actual {
        return Err(ApplicationError::Internal);
    }
    Ok(())
}

fn ensure_replacement(
    workspace_id: WorkspaceId,
    entity_id: EntityId,
    expected_revision: Revision,
    actual_workspace_id: WorkspaceId,
    actual_entity_id: EntityId,
    actual_revision: Revision,
) -> Result<(), ApplicationError> {
    ensure_workspace(workspace_id, actual_workspace_id)?;
    if entity_id != actual_entity_id || actual_revision != expected_revision.next()? {
        return Err(ApplicationError::Internal);
    }
    Ok(())
}

fn require_updated(rows: u64, entity: &'static str) -> Result<(), ApplicationError> {
    if rows == 1 {
        Ok(())
    } else {
        Err(ApplicationError::Conflict { entity })
    }
}

fn revision_i64(revision: Revision) -> Result<i64, ApplicationError> {
    i64::try_from(revision.get()).map_err(|_| storage_error("revision exceeds sqlite range"))
}

#[derive(Serialize, Deserialize)]
struct StoredMutationResult {
    version: u8,
    entity_id: String,
    revision: u64,
    lifecycle: String,
    audit_correlation_id: String,
}

fn encode_result(result: MutationResult) -> Result<String, ApplicationError> {
    let stored = StoredMutationResult {
        version: 1,
        entity_id: id_text(result.entity_id),
        revision: result.revision.get(),
        lifecycle: encode_lifecycle(result.lifecycle).to_owned(),
        audit_correlation_id: result.audit_correlation_id.to_string(),
    };
    serde_json::to_string(&stored).map_err(|_| storage_error("operation outcome encoding failed"))
}

fn decode_result(value: &str) -> Result<MutationResult, ApplicationError> {
    let stored: StoredMutationResult = serde_json::from_str(value)
        .map_err(|_| storage_error("operation outcome decoding failed"))?;
    if stored.version != 1 {
        return Err(storage_error("unsupported operation outcome version"));
    }
    Ok(MutationResult {
        entity_id: parse_id(&stored.entity_id)?,
        revision: Revision::rehydrate(stored.revision).map_err(ApplicationError::from)?,
        lifecycle: decode_lifecycle(&stored.lifecycle)?,
        audit_correlation_id: Uuid::parse_str(&stored.audit_correlation_id)
            .map_err(|_| storage_error("invalid operation correlation id"))?,
    })
}
