use cortex_application::{
    AggregateChange, ApplicationError, AtomicMutation, AtomicMutationPort, Capability,
    MutationResult, OperationIdentity, OperationResultRepository, RecordedOperation,
};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, EntityId, Lifecycle, MemoryAssertion, Note, OperationId,
    PolicyDecision, PrincipalId, Revision, Source, Task, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
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
    ) -> Result<Option<RecordedOperation>, ApplicationError> {
        let outcome: Option<String> = sqlx::query_scalar(
            "SELECT outcome_json FROM operation WHERE workspace_id = ? AND operation_id = ?",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(operation_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("operation lookup failed"))?;
        outcome.map(|value| decode_operation(&value)).transpose()
    }

    /// Reserves and commits remote enrollment, its principal/grants, and its one audit event in
    /// one `SQLite` transaction. Protected enrollment artifacts are intentionally outside this
    /// method: callers may reconcile them only after this durable boundary succeeds.
    ///
    /// # Errors
    /// Returns a redacted validation, conflict, or storage error without writing a partial
    /// principal, grant, operation, or audit record.
    #[allow(clippy::too_many_lines)] // The transaction's ordered durable boundary is review-critical.
    pub async fn enroll_remote_once(
        &self,
        request: RemoteEnrollmentRequest,
    ) -> Result<RemoteEnrollmentRecord, ApplicationError> {
        request.validate()?;
        if let Some(record) = self.load_remote_operation(&request).await? {
            return Ok(record);
        }

        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|_| storage_error("remote enrollment transaction begin failed"))?;

        if let Some(record) = load_remote_subject(&mut transaction, &request).await? {
            transaction
                .rollback()
                .await
                .map_err(|_| storage_error("remote enrollment rollback failed"))?;
            return Ok(record);
        }

        let existing_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM remote_enrollment WHERE workspace_id = ?")
                .bind(id_text(request.workspace_id))
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| storage_error("remote enrollment count failed"))?;
        if existing_count >= i64::from(request.max_remote_clients) {
            return Err(ApplicationError::Conflict {
                entity: "remote_enrollment",
            });
        }

        let record = RemoteEnrollmentRecord {
            subject: request.subject.clone(),
            principal_id: request.principal_id,
            grants: request.grants.clone(),
            pairing_verifier: request.pairing_verifier,
            correlation_id: request.correlation_id,
        };
        let outcome_json = encode_remote_operation(&request, &record)?;
        let reservation = sqlx::query(
            "INSERT INTO operation (workspace_id, operation_id, outcome_json) VALUES (?, ?, ?)",
        )
        .bind(id_text(request.workspace_id))
        .bind(id_text(request.operation_id))
        .bind(outcome_json)
        .execute(&mut *transaction)
        .await;
        if reservation.is_err() {
            transaction
                .rollback()
                .await
                .map_err(|_| storage_error("remote enrollment rollback failed"))?;
            if let Some(record) = self.load_durable_remote_subject(&request).await? {
                return Ok(record);
            }
            return self
                .load_remote_operation(&request)
                .await?
                .ok_or_else(|| storage_error("remote enrollment operation reservation failed"));
        }

        sqlx::query("INSERT INTO principal (id, workspace_id, name) VALUES (?, ?, ?)")
            .bind(id_text(request.principal_id))
            .bind(id_text(request.workspace_id))
            .bind("paired-remote")
            .execute(&mut *transaction)
            .await
            .map_err(|_| storage_error("remote enrollment principal insert failed"))?;
        for capability in &request.grants {
            sqlx::query(
                "INSERT INTO capability_grant (workspace_id, principal_id, capability) VALUES (?, ?, ?)",
            )
            .bind(id_text(request.workspace_id))
            .bind(id_text(request.principal_id))
            .bind(capability.metadata().mcp_name)
            .execute(&mut *transaction)
            .await
            .map_err(|_| storage_error("remote enrollment grant insert failed"))?;
        }
        sqlx::query(
            "INSERT INTO remote_enrollment \
             (workspace_id, subject, principal_id, pairing_verifier, grants_json, correlation_id) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(id_text(request.workspace_id))
        .bind(&request.subject)
        .bind(id_text(request.principal_id))
        .bind(request.pairing_verifier.to_vec())
        .bind(encode_grants(&request.grants)?)
        .bind(request.correlation_id.to_string())
        .execute(&mut *transaction)
        .await
        .map_err(|_| storage_error("remote enrollment insert failed"))?;
        insert_event_in_transaction(
            &mut transaction,
            &AuditEvent {
                id: AuditEventId::new(),
                workspace_id: request.workspace_id,
                principal_id: request.owner_principal_id,
                operation_id: request.operation_id,
                correlation_id: request.correlation_id,
                capability: "cortex_remote_enroll",
                target_id: None,
                policy_decision: PolicyDecision::Allow,
                result: AuditResult::Succeeded,
            },
        )
        .await?;
        transaction
            .commit()
            .await
            .map_err(|_| storage_error("remote enrollment commit failed"))?;
        Ok(record)
    }

    async fn load_remote_operation(
        &self,
        request: &RemoteEnrollmentRequest,
    ) -> Result<Option<RemoteEnrollmentRecord>, ApplicationError> {
        let outcome: Option<String> = sqlx::query_scalar(
            "SELECT outcome_json FROM operation WHERE workspace_id = ? AND operation_id = ?",
        )
        .bind(id_text(request.workspace_id))
        .bind(id_text(request.operation_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("remote enrollment operation lookup failed"))?;
        let Some(outcome) = outcome else {
            return Ok(None);
        };
        let stored: StoredRemoteEnrollment =
            serde_json::from_str(&outcome).map_err(|_| ApplicationError::Conflict {
                entity: "operation",
            })?;
        if stored.version != 1 || stored.kind != "remote_enrollment" {
            return Err(ApplicationError::Conflict {
                entity: "operation",
            });
        }
        if stored.owner_principal_id != id_text(request.owner_principal_id)
            || stored.subject != request.subject
            || decode_grants(&stored.grants)? != request.grants
        {
            return Err(ApplicationError::Conflict {
                entity: "operation",
            });
        }
        let record = stored.into_record()?;
        Ok(Some(record))
    }

    async fn load_durable_remote_subject(
        &self,
        request: &RemoteEnrollmentRequest,
    ) -> Result<Option<RemoteEnrollmentRecord>, ApplicationError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| storage_error("remote enrollment subject transaction failed"))?;
        let record = load_remote_subject(&mut transaction, request).await?;
        transaction
            .rollback()
            .await
            .map_err(|_| storage_error("remote enrollment rollback failed"))?;
        Ok(record)
    }
}

/// A validated administrative enrollment request accepted only from the daemon-owned adapter.
#[derive(Clone, Debug)]
pub struct RemoteEnrollmentRequest {
    pub workspace_id: WorkspaceId,
    pub owner_principal_id: PrincipalId,
    pub operation_id: OperationId,
    pub correlation_id: Uuid,
    pub subject: String,
    pub principal_id: PrincipalId,
    pub grants: Vec<Capability>,
    pub pairing_verifier: [u8; 32],
    pub max_remote_clients: u8,
}

impl RemoteEnrollmentRequest {
    fn validate(&self) -> Result<(), ApplicationError> {
        if self.subject.trim().is_empty()
            || self.subject.len() > 256
            || self.subject.chars().any(char::is_control)
            || self.grants.is_empty()
            || self.grants.windows(2).any(|pair| pair[0] >= pair[1])
            || self.max_remote_clients == 0
        {
            return Err(ApplicationError::Validation {
                field: "remote_enrollment",
            });
        }
        Ok(())
    }
}

/// Public, non-secret durable enrollment state used to reconcile its protected artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemoteEnrollmentRecord {
    pub subject: String,
    pub principal_id: PrincipalId,
    pub grants: Vec<Capability>,
    pub pairing_verifier: [u8; 32],
    pub correlation_id: Uuid,
}

#[derive(Deserialize, Serialize)]
struct StoredRemoteEnrollment {
    version: u8,
    kind: String,
    owner_principal_id: String,
    subject: String,
    principal_id: String,
    grants: Vec<String>,
    pairing_verifier: [u8; 32],
    correlation_id: String,
}

impl StoredRemoteEnrollment {
    fn into_record(self) -> Result<RemoteEnrollmentRecord, ApplicationError> {
        Ok(RemoteEnrollmentRecord {
            subject: self.subject,
            principal_id: parse_id(&self.principal_id)
                .map_err(|_| storage_error("invalid remote enrollment principal"))?,
            grants: decode_grants(&self.grants)?,
            pairing_verifier: self.pairing_verifier,
            correlation_id: Uuid::parse_str(&self.correlation_id)
                .map_err(|_| storage_error("invalid remote enrollment correlation"))?,
        })
    }
}

fn encode_remote_operation(
    request: &RemoteEnrollmentRequest,
    record: &RemoteEnrollmentRecord,
) -> Result<String, ApplicationError> {
    serde_json::to_string(&StoredRemoteEnrollment {
        version: 1,
        kind: "remote_enrollment".to_owned(),
        owner_principal_id: id_text(request.owner_principal_id),
        subject: record.subject.clone(),
        principal_id: id_text(record.principal_id),
        grants: record
            .grants
            .iter()
            .map(|capability| capability.metadata().mcp_name.to_owned())
            .collect(),
        pairing_verifier: record.pairing_verifier,
        correlation_id: record.correlation_id.to_string(),
    })
    .map_err(|_| storage_error("remote enrollment operation encoding failed"))
}

async fn load_remote_subject(
    transaction: &mut Transaction<'_, Sqlite>,
    request: &RemoteEnrollmentRequest,
) -> Result<Option<RemoteEnrollmentRecord>, ApplicationError> {
    let row = sqlx::query(
        "SELECT principal_id, pairing_verifier, grants_json, correlation_id FROM remote_enrollment \
         WHERE workspace_id = ? AND subject = ?",
    )
    .bind(id_text(request.workspace_id))
    .bind(&request.subject)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(|_| storage_error("remote enrollment subject lookup failed"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let principal_text: String = row
        .try_get("principal_id")
        .map_err(|_| storage_error("invalid remote enrollment row"))?;
    let verifier: Vec<u8> = row
        .try_get("pairing_verifier")
        .map_err(|_| storage_error("invalid remote enrollment row"))?;
    let verifier: [u8; 32] = verifier
        .try_into()
        .map_err(|_| storage_error("invalid remote enrollment verifier"))?;
    let grants_json: String = row
        .try_get("grants_json")
        .map_err(|_| storage_error("invalid remote enrollment row"))?;
    let grants: Vec<String> = serde_json::from_str(&grants_json)
        .map_err(|_| storage_error("invalid remote enrollment grants"))?;
    let correlation_id: String = row
        .try_get("correlation_id")
        .map_err(|_| storage_error("invalid remote enrollment row"))?;
    let record = RemoteEnrollmentRecord {
        subject: request.subject.clone(),
        principal_id: parse_id(&principal_text)?,
        grants: decode_grants(&grants)?,
        pairing_verifier: verifier,
        correlation_id: Uuid::parse_str(&correlation_id)
            .map_err(|_| storage_error("invalid remote enrollment correlation"))?,
    };
    if record.grants != request.grants {
        return Err(ApplicationError::Conflict {
            entity: "remote_enrollment",
        });
    }
    Ok(Some(record))
}

fn encode_grants(grants: &[Capability]) -> Result<String, ApplicationError> {
    serde_json::to_string(
        &grants
            .iter()
            .map(|capability| capability.metadata().mcp_name)
            .collect::<Vec<_>>(),
    )
    .map_err(|_| storage_error("remote enrollment grants encoding failed"))
}

fn decode_grants(grants: &[String]) -> Result<Vec<Capability>, ApplicationError> {
    let decoded = grants
        .iter()
        .map(|grant| {
            Capability::from_mcp_name(grant)
                .ok_or_else(|| storage_error("invalid remote enrollment grant"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if decoded.is_empty() || decoded.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(storage_error("invalid remote enrollment grants"));
    }
    Ok(decoded)
}

impl OperationResultRepository for OperationStore {
    async fn find_result(
        &self,
        workspace_id: WorkspaceId,
        operation_id: cortex_domain::OperationId,
    ) -> Result<Option<RecordedOperation>, ApplicationError> {
        self.load_result(workspace_id, operation_id).await
    }
}

impl AtomicMutationPort for OperationStore {
    async fn execute_once(
        &self,
        mutation: AtomicMutation,
    ) -> Result<MutationResult, ApplicationError> {
        if let Some(recorded) = self
            .load_result(mutation.workspace_id, mutation.operation_id)
            .await?
        {
            return replay_result(recorded, mutation.identity);
        }

        let outcome_json = encode_operation(&mutation)?;
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
            if let Some(recorded) = self
                .load_result(mutation.workspace_id, mutation.operation_id)
                .await?
            {
                return replay_result(recorded, mutation.identity);
            }
            return Err(storage_error("operation reservation failed"));
        }

        for change in &mutation.changes {
            apply_change(&mut transaction, mutation.workspace_id, change).await?;
            sync_search_document(&mut transaction, mutation.workspace_id, change).await?;
        }
        insert_event_in_transaction(&mut transaction, &mutation.audit_event).await?;
        transaction
            .commit()
            .await
            .map_err(|_| storage_error("transaction commit failed"))?;
        Ok(mutation.result)
    }
}

async fn sync_search_document(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    change: &AggregateChange,
) -> Result<(), ApplicationError> {
    let entity = match change {
        AggregateChange::InsertNote(note) => Some(("note", note.id())),
        AggregateChange::ReplaceNote { entity_id, .. }
        | AggregateChange::DeleteNote { entity_id, .. }
        | AggregateChange::RestoreNote { entity_id, .. } => Some(("note", *entity_id)),
        AggregateChange::InsertTask(task) => Some(("task", task.id())),
        AggregateChange::ReplaceTask { entity_id, .. }
        | AggregateChange::DeleteTask { entity_id, .. }
        | AggregateChange::RestoreTask { entity_id, .. } => Some(("task", *entity_id)),
        AggregateChange::InsertMemory(memory) => Some(("memory", memory.id())),
        AggregateChange::ReplaceMemory { entity_id, .. }
        | AggregateChange::DeleteMemory { entity_id, .. }
        | AggregateChange::RestoreMemory { entity_id, .. } => Some(("memory", *entity_id)),
        AggregateChange::InsertSource(source) => Some(("source", source.id())),
        AggregateChange::LinkMemorySource { .. } => None,
    };
    let Some((kind, entity_id)) = entity else {
        return Ok(());
    };
    let snippet = active_search_snippet(transaction, workspace_id, entity_id, kind).await?;
    if let Some(snippet) = snippet {
        let content_hash = Sha256::digest(snippet.as_bytes()).to_vec();
        sqlx::query(
            "INSERT INTO search_document \
             (workspace_id, entity_id, entity_kind, snippet, content_hash) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(workspace_id, entity_id) DO UPDATE SET \
             entity_kind = excluded.entity_kind, snippet = excluded.snippet, \
             content_hash = excluded.content_hash, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .bind(kind)
        .bind(snippet)
        .bind(content_hash)
        .execute(&mut **transaction)
        .await
        .map_err(|_| storage_error("search document update failed"))?;
    } else {
        sqlx::query("DELETE FROM search_document WHERE workspace_id = ? AND entity_id = ?")
            .bind(id_text(workspace_id))
            .bind(id_text(entity_id))
            .execute(&mut **transaction)
            .await
            .map_err(|_| storage_error("search document delete failed"))?;
    }
    Ok(())
}

async fn active_search_snippet(
    transaction: &mut Transaction<'_, Sqlite>,
    workspace_id: WorkspaceId,
    entity_id: EntityId,
    kind: &str,
) -> Result<Option<String>, ApplicationError> {
    let query = match kind {
        "note" => {
            "SELECT title || char(10) || content FROM note \
             WHERE workspace_id = ? AND id = ? AND lifecycle = 'active'"
        }
        "task" => {
            "SELECT title FROM task \
             WHERE workspace_id = ? AND id = ? AND lifecycle = 'active'"
        }
        "memory" => {
            "SELECT statement FROM memory_assertion \
             WHERE workspace_id = ? AND id = ? AND lifecycle = 'active' AND status = 'active'"
        }
        "source" => {
            "SELECT reference FROM source \
             WHERE workspace_id = ? AND id = ? AND lifecycle = 'active'"
        }
        _ => return Err(ApplicationError::Internal),
    };
    sqlx::query_scalar(query)
        .bind(id_text(workspace_id))
        .bind(id_text(entity_id))
        .fetch_optional(&mut **transaction)
        .await
        .map_err(|_| storage_error("search document lookup failed"))
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
    principal_id: String,
    capability: String,
    target_id: Option<String>,
    entity_id: String,
    revision: u64,
    lifecycle: String,
    audit_correlation_id: String,
}

fn encode_operation(mutation: &AtomicMutation) -> Result<String, ApplicationError> {
    let result = mutation.result;
    let stored = StoredMutationResult {
        version: 2,
        principal_id: id_text(mutation.identity.principal_id),
        capability: mutation.identity.capability.metadata().mcp_name.to_owned(),
        target_id: mutation.identity.target_id.map(id_text),
        entity_id: id_text(result.entity_id),
        revision: result.revision.get(),
        lifecycle: encode_lifecycle(result.lifecycle).to_owned(),
        audit_correlation_id: result.audit_correlation_id.to_string(),
    };
    serde_json::to_string(&stored).map_err(|_| storage_error("operation outcome encoding failed"))
}

fn decode_operation(value: &str) -> Result<RecordedOperation, ApplicationError> {
    let stored: StoredMutationResult = serde_json::from_str(value)
        .map_err(|_| storage_error("operation outcome decoding failed"))?;
    if stored.version != 2 {
        return Err(storage_error("unsupported operation outcome version"));
    }
    let capability = Capability::from_mcp_name(&stored.capability)
        .ok_or_else(|| storage_error("invalid operation capability"))?;
    Ok(RecordedOperation {
        identity: OperationIdentity::new(
            parse_id(&stored.principal_id)?,
            capability,
            stored.target_id.as_deref().map(parse_id).transpose()?,
        ),
        result: MutationResult {
            entity_id: parse_id(&stored.entity_id)?,
            revision: Revision::rehydrate(stored.revision).map_err(ApplicationError::from)?,
            lifecycle: decode_lifecycle(&stored.lifecycle)?,
            audit_correlation_id: Uuid::parse_str(&stored.audit_correlation_id)
                .map_err(|_| storage_error("invalid operation correlation id"))?,
        },
    })
}

fn replay_result(
    recorded: RecordedOperation,
    requested: OperationIdentity,
) -> Result<MutationResult, ApplicationError> {
    if recorded.identity == requested {
        Ok(recorded.result)
    } else {
        Err(ApplicationError::Conflict {
            entity: "operation",
        })
    }
}
