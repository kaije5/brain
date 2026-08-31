use cortex_application::{ApplicationError, AuditPort};
use cortex_domain::{AuditEvent, AuditEventId, AuditResult, PolicyDecision, PolicyDeny};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

use crate::database::storage_error;

#[derive(Clone)]
pub struct SqliteAuditPort {
    pool: SqlitePool,
}

impl SqliteAuditPort {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Loads one redacted audit event by its opaque identifier.
    ///
    /// # Errors
    ///
    /// Returns a redacted storage error for malformed durable state or query failure.
    pub async fn find(
        &self,
        workspace_id: cortex_domain::WorkspaceId,
        id: AuditEventId,
    ) -> Result<Option<AuditEvent>, ApplicationError> {
        let row = sqlx::query(
            "SELECT id, workspace_id, principal_id, operation_id, correlation_id, \
             capability, target_id, policy_decision, result \
             FROM audit_event WHERE workspace_id = ? AND id = ?",
        )
        .bind(uuid_text(workspace_id))
        .bind(uuid_text(id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("audit lookup failed"))?;
        row.map(|row| decode_event(&row)).transpose()
    }
}

impl AuditPort for SqliteAuditPort {
    async fn append(&self, event: AuditEvent) -> Result<(), ApplicationError> {
        insert_event(&self.pool, &event).await
    }
}

pub(crate) async fn insert_event<'e, E>(
    executor: E,
    event: &AuditEvent,
) -> Result<(), ApplicationError>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    canonical_capability(event.capability)?;
    sqlx::query(
        "INSERT INTO audit_event \
         (id, workspace_id, principal_id, operation_id, correlation_id, capability, target_id, \
          policy_decision, result, redacted_metadata) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, '{}')",
    )
    .bind(uuid_text(event.id))
    .bind(uuid_text(event.workspace_id))
    .bind(uuid_text(event.principal_id))
    .bind(uuid_text(event.operation_id))
    .bind(event.correlation_id.to_string())
    .bind(event.capability)
    .bind(event.target_id.map(uuid_text))
    .bind(encode_policy(event.policy_decision))
    .bind(encode_result(event.result))
    .execute(executor)
    .await
    .map_err(|_| storage_error("audit append failed"))?;
    Ok(())
}

pub(crate) async fn insert_event_in_transaction(
    transaction: &mut Transaction<'_, Sqlite>,
    event: &AuditEvent,
) -> Result<(), ApplicationError> {
    insert_event(&mut **transaction, event).await
}

fn decode_event(row: &sqlx::sqlite::SqliteRow) -> Result<AuditEvent, ApplicationError> {
    let target: Option<String> = row
        .try_get("target_id")
        .map_err(|_| storage_error("invalid audit row"))?;
    Ok(AuditEvent {
        id: decode_id(row, "id")?,
        workspace_id: decode_id(row, "workspace_id")?,
        principal_id: decode_id(row, "principal_id")?,
        operation_id: decode_id(row, "operation_id")?,
        correlation_id: parse_uuid(row, "correlation_id")?,
        capability: canonical_capability(
            row.try_get("capability")
                .map_err(|_| storage_error("invalid audit row"))?,
        )?,
        target_id: target.map(|value| parse_id(&value)).transpose()?,
        policy_decision: decode_policy(
            row.try_get("policy_decision")
                .map_err(|_| storage_error("invalid audit row"))?,
        )?,
        result: decode_result(
            row.try_get("result")
                .map_err(|_| storage_error("invalid audit row"))?,
        )?,
    })
}

fn decode_id<T>(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<T, ApplicationError>
where
    T: TryFrom<Uuid, Error = cortex_domain::DomainError>,
{
    let value: String = row
        .try_get(column)
        .map_err(|_| storage_error("invalid audit row"))?;
    parse_id(&value)
}

fn parse_id<T>(value: &str) -> Result<T, ApplicationError>
where
    T: TryFrom<Uuid, Error = cortex_domain::DomainError>,
{
    let uuid = Uuid::parse_str(value).map_err(|_| storage_error("invalid persisted id"))?;
    T::try_from(uuid).map_err(ApplicationError::from)
}

fn parse_uuid(row: &sqlx::sqlite::SqliteRow, column: &str) -> Result<Uuid, ApplicationError> {
    let value: String = row
        .try_get(column)
        .map_err(|_| storage_error("invalid audit row"))?;
    Uuid::parse_str(&value).map_err(|_| storage_error("invalid persisted uuid"))
}

fn uuid_text<T>(value: T) -> String
where
    Uuid: From<T>,
{
    Uuid::from(value).to_string()
}

fn encode_policy(value: PolicyDecision) -> &'static str {
    match value {
        PolicyDecision::Allow => "allow",
        PolicyDecision::Deny(PolicyDeny::MissingGrant) => "deny_missing_grant",
    }
}

fn decode_policy(value: &str) -> Result<PolicyDecision, ApplicationError> {
    match value {
        "allow" => Ok(PolicyDecision::Allow),
        "deny_missing_grant" => Ok(PolicyDecision::Deny(PolicyDeny::MissingGrant)),
        _ => Err(storage_error("invalid audit policy")),
    }
}

fn encode_result(value: AuditResult) -> &'static str {
    match value {
        AuditResult::Succeeded => "succeeded",
        AuditResult::Rejected => "rejected",
        AuditResult::Failed => "failed",
    }
}

fn decode_result(value: &str) -> Result<AuditResult, ApplicationError> {
    match value {
        "succeeded" => Ok(AuditResult::Succeeded),
        "rejected" => Ok(AuditResult::Rejected),
        "failed" => Ok(AuditResult::Failed),
        _ => Err(storage_error("invalid audit result")),
    }
}

fn canonical_capability(value: &str) -> Result<&'static str, ApplicationError> {
    match value {
        "cortex_knowledge_search" => Ok("cortex_knowledge_search"),
        "cortex_memory_correct" => Ok("cortex_memory_correct"),
        "cortex_memory_create" => Ok("cortex_memory_create"),
        "cortex_memory_delete" => Ok("cortex_memory_delete"),
        "cortex_memory_restore" => Ok("cortex_memory_restore"),
        "cortex_memory_search" => Ok("cortex_memory_search"),
        "cortex_note_create" => Ok("cortex_note_create"),
        "cortex_note_delete" => Ok("cortex_note_delete"),
        "cortex_note_restore" => Ok("cortex_note_restore"),
        "cortex_note_search" => Ok("cortex_note_search"),
        "cortex_note_update" => Ok("cortex_note_update"),
        "cortex_task_complete" => Ok("cortex_task_complete"),
        "cortex_task_create" => Ok("cortex_task_create"),
        "cortex_task_delete" => Ok("cortex_task_delete"),
        "cortex_task_list" => Ok("cortex_task_list"),
        "cortex_task_restore" => Ok("cortex_task_restore"),
        "cortex_task_update" => Ok("cortex_task_update"),
        _ => Err(storage_error("invalid audit capability")),
    }
}

#[cfg(test)]
mod tests {
    use cortex_application::AuditPort;
    use cortex_domain::{
        AuditEvent, AuditEventId, AuditResult, OperationId, PolicyDecision, PrincipalId,
        WorkspaceId,
    };
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::SqliteDatabase;

    #[tokio::test]
    async fn audit_schema_is_redacted_and_append_only() -> Result<(), String> {
        let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
        let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
            .await
            .map_err(|error| format!("migration failed: {error:?}"))?;
        let workspace_id = WorkspaceId::new();
        let principal_id = PrincipalId::new();
        let repositories = database.repositories();
        repositories
            .create_workspace(workspace_id, "owner")
            .await
            .map_err(|error| format!("workspace failed: {error:?}"))?;
        repositories
            .create_principal(workspace_id, principal_id, "owner")
            .await
            .map_err(|error| format!("principal failed: {error:?}"))?;
        let event = AuditEvent {
            id: AuditEventId::new(),
            workspace_id,
            principal_id,
            operation_id: OperationId::new(),
            correlation_id: Uuid::now_v7(),
            capability: "cortex_note_search",
            target_id: None,
            policy_decision: PolicyDecision::Allow,
            result: AuditResult::Succeeded,
        };
        let audit = database.audit_port();
        audit
            .append(event)
            .await
            .map_err(|error| format!("append failed: {error:?}"))?;

        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('audit_event')")
                .fetch_all(database.test_pool())
                .await
                .map_err(|error| format!("schema query failed: {error}"))?;
        assert!(columns.iter().all(|column| {
            !["content", "payload", "prompt", "secret"]
                .iter()
                .any(|sensitive| column.contains(sensitive))
        }));
        let update = sqlx::query("UPDATE audit_event SET result = 'failed'")
            .execute(database.test_pool())
            .await;
        let delete = sqlx::query("DELETE FROM audit_event")
            .execute(database.test_pool())
            .await;
        assert!(update.is_err());
        assert!(delete.is_err());
        Ok(())
    }
}
