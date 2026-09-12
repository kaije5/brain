use cortex_application::{ApplicationError, AuditPort};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, ContentHash, ObservedRevision, PolicyDecision,
    PolicyDeny, ProviderAuditMetadata, ProviderId, ProviderResourceId, ProviderResourceKind,
    ResourceTarget,
};
use serde::{Deserialize, Serialize};
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
             capability, target_id, policy_decision, result, redacted_metadata \
             FROM audit_event WHERE workspace_id = ? AND id = ?",
        )
        .bind(uuid_text(workspace_id))
        .bind(uuid_text(id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("audit lookup failed"))?;
        row.map(|row| decode_event(&row)).transpose()
    }

    /// Counts redacted decision records for one correlation within a workspace.
    ///
    /// # Errors
    /// Returns a redacted storage error if the audit query fails.
    pub async fn count_for_correlation(
        &self,
        workspace_id: cortex_domain::WorkspaceId,
        correlation_id: Uuid,
    ) -> Result<u64, ApplicationError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_event WHERE workspace_id = ? AND correlation_id = ?",
        )
        .bind(uuid_text(workspace_id))
        .bind(correlation_id.to_string())
        .fetch_one(&self.pool)
        .await
        .map_err(|_| storage_error("audit count failed"))?;
        u64::try_from(count).map_err(|_| storage_error("invalid audit count"))
    }

    /// Loads the single audit event for a correlation within one workspace.
    ///
    /// # Errors
    /// Returns a redacted storage error for duplicate, malformed, or unavailable audit state.
    pub async fn find_for_correlation(
        &self,
        workspace_id: cortex_domain::WorkspaceId,
        correlation_id: Uuid,
    ) -> Result<Option<AuditEvent>, ApplicationError> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, principal_id, operation_id, correlation_id, capability, \
             target_id, policy_decision, result, redacted_metadata FROM audit_event \
             WHERE workspace_id = ? AND correlation_id = ? LIMIT 2",
        )
        .bind(uuid_text(workspace_id))
        .bind(correlation_id.to_string())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| storage_error("audit correlation lookup failed"))?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => decode_event(row).map(Some),
            _ => Err(storage_error("duplicate audit correlation")),
        }
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
    let metadata_json = encode_metadata(event.provider_metadata.as_ref())?;
    sqlx::query(
        "INSERT INTO audit_event \
         (id, workspace_id, principal_id, operation_id, correlation_id, capability, target_id, \
          policy_decision, result, redacted_metadata) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(uuid_text(event.id))
    .bind(uuid_text(event.workspace_id))
    .bind(uuid_text(event.principal_id))
    .bind(uuid_text(event.operation_id))
    .bind(event.correlation_id.to_string())
    .bind(event.capability)
    .bind(encode_target(event.target.as_ref())?)
    .bind(encode_policy(event.policy_decision))
    .bind(encode_result(event.result))
    .bind(metadata_json)
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
    let metadata: String = row
        .try_get("redacted_metadata")
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
        target: target.as_deref().map(decode_target).transpose()?,
        provider_metadata: decode_metadata(&metadata)?,
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

/// Encodes an audit target into the `target_id` column.
///
/// `Cortex` entities remain plain `UUIDv7` text so pre-provider rows and values
/// share one legacy-compatible representation; provider targets are tagged
/// JSON because their identity is opaque and not workspace-scoped UUIDs.
pub(crate) fn encode_target(
    target: Option<&ResourceTarget>,
) -> Result<Option<String>, ApplicationError> {
    let encoded = match target {
        None => None,
        Some(ResourceTarget::CortexEntity(entity_id)) => Some(uuid_text(*entity_id)),
        Some(ResourceTarget::ProviderResource(resource)) => Some(
            serde_json::to_string(&StoredAuditTarget::ProviderResource {
                workspace_id: uuid_text(resource.workspace_id()),
                provider_id: resource.provider_id().as_str().to_owned(),
                resource_id: resource.resource_id().as_str().to_owned(),
                resource_kind: encode_resource_kind(resource.kind()).to_owned(),
            })
            .map_err(|_| storage_error("audit target encoding failed"))?,
        ),
        Some(ResourceTarget::ProviderScope {
            provider_id,
            workspace_id,
            resource_kind,
        }) => Some(
            serde_json::to_string(&StoredAuditTarget::ProviderScope {
                workspace_id: uuid_text(*workspace_id),
                provider_id: provider_id.as_str().to_owned(),
                resource_kind: encode_resource_kind(*resource_kind).to_owned(),
            })
            .map_err(|_| storage_error("audit target encoding failed"))?,
        ),
    };
    Ok(encoded)
}

pub(crate) fn decode_target(value: &str) -> Result<ResourceTarget, ApplicationError> {
    // Plain UUID text is the legacy (and current) Cortex entity encoding.
    if let Ok(uuid) = Uuid::parse_str(value) {
        return Ok(ResourceTarget::CortexEntity(
            cortex_domain::EntityId::try_from(uuid).map_err(ApplicationError::from)?,
        ));
    }
    let stored: StoredAuditTarget =
        serde_json::from_str(value).map_err(|_| storage_error("invalid audit target"))?;
    match stored {
        StoredAuditTarget::ProviderResource {
            workspace_id,
            provider_id,
            resource_id,
            resource_kind,
        } => Ok(ResourceTarget::ProviderResource(
            cortex_domain::ProviderResourceRef::new(
                parse_id(&workspace_id)?,
                ProviderId::new(provider_id).map_err(ApplicationError::from)?,
                ProviderResourceId::new(resource_id).map_err(ApplicationError::from)?,
                decode_resource_kind(&resource_kind)?,
            ),
        )),
        StoredAuditTarget::ProviderScope {
            workspace_id,
            provider_id,
            resource_kind,
        } => Ok(ResourceTarget::ProviderScope {
            workspace_id: parse_id(&workspace_id)?,
            provider_id: ProviderId::new(provider_id).map_err(ApplicationError::from)?,
            resource_kind: decode_resource_kind(&resource_kind)?,
        }),
    }
}

fn encode_resource_kind(kind: ProviderResourceKind) -> &'static str {
    match kind {
        ProviderResourceKind::Knowledge => "knowledge",
        ProviderResourceKind::Task => "task",
    }
}

fn decode_resource_kind(value: &str) -> Result<ProviderResourceKind, ApplicationError> {
    match value {
        "knowledge" => Ok(ProviderResourceKind::Knowledge),
        "task" => Ok(ProviderResourceKind::Task),
        _ => Err(storage_error("invalid provider resource kind")),
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum StoredAuditTarget {
    ProviderResource {
        workspace_id: String,
        provider_id: String,
        resource_id: String,
        resource_kind: String,
    },
    ProviderScope {
        workspace_id: String,
        provider_id: String,
        resource_kind: String,
    },
}

fn encode_metadata(metadata: Option<&ProviderAuditMetadata>) -> Result<String, ApplicationError> {
    let Some(metadata) = metadata else {
        return Ok("{}".to_owned());
    };
    let stored = StoredAuditMetadata {
        provider: Some(StoredProviderAuditMetadata {
            before_revision: metadata
                .before_revision
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            before_hash: metadata.before_hash.as_ref().map(encode_hash),
            after_revision: metadata
                .after_revision
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            after_hash: metadata.after_hash.as_ref().map(encode_hash),
        }),
    };
    serde_json::to_string(&stored).map_err(|_| storage_error("audit metadata encoding failed"))
}

fn decode_metadata(value: &str) -> Result<Option<ProviderAuditMetadata>, ApplicationError> {
    if value.trim().is_empty() || value.trim() == "{}" {
        return Ok(None);
    }
    let stored: StoredAuditMetadata =
        serde_json::from_str(value).map_err(|_| storage_error("invalid audit metadata"))?;
    let Some(provider) = stored.provider else {
        return Ok(None);
    };
    Ok(Some(ProviderAuditMetadata::new(
        provider
            .before_revision
            .map(|value| ObservedRevision::new(value).map_err(ApplicationError::from))
            .transpose()?,
        provider
            .before_hash
            .map(|value| decode_hash(&value))
            .transpose()?,
        provider
            .after_revision
            .map(|value| ObservedRevision::new(value).map_err(ApplicationError::from))
            .transpose()?,
        provider
            .after_hash
            .map(|value| decode_hash(&value))
            .transpose()?,
    )))
}

#[derive(Serialize, Deserialize)]
struct StoredAuditMetadata {
    provider: Option<StoredProviderAuditMetadata>,
}

#[derive(Serialize, Deserialize)]
struct StoredProviderAuditMetadata {
    before_revision: Option<String>,
    before_hash: Option<String>,
    after_revision: Option<String>,
    after_hash: Option<String>,
}

fn encode_hash(hash: &ContentHash) -> String {
    let mut encoded = String::with_capacity(hash.as_bytes().len() * 2);
    for byte in hash.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_hash(value: &str) -> Result<ContentHash, ApplicationError> {
    if value.len() != 64 {
        return Err(storage_error("invalid audit content hash"));
    }
    let mut bytes = [0_u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| storage_error("invalid audit content hash"))?;
    }
    Ok(ContentHash::new(bytes))
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
        PolicyDecision::Deny(PolicyDeny::TargetOutsideWorkspace) => "deny_target_outside_workspace",
    }
}

fn decode_policy(value: &str) -> Result<PolicyDecision, ApplicationError> {
    match value {
        "allow" => Ok(PolicyDecision::Allow),
        "deny_missing_grant" => Ok(PolicyDecision::Deny(PolicyDeny::MissingGrant)),
        "deny_target_outside_workspace" => {
            Ok(PolicyDecision::Deny(PolicyDeny::TargetOutsideWorkspace))
        }
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
        "cortex_knowledge_create" => Ok("cortex_knowledge_create"),
        "cortex_knowledge_update" => Ok("cortex_knowledge_update"),
        "cortex_knowledge_delete" => Ok("cortex_knowledge_delete"),
        "cortex_agent_run" => Ok("cortex_agent_run"),
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
        "cortex_remote_enroll" => Ok("cortex_remote_enroll"),
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
            target: None,
            provider_metadata: None,
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
