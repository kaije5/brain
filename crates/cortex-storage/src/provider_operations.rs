//! Durable idempotency log for provider-backed mutations (SCRUM-116).
//!
//! The store retains, per workspace/operation pair, the exact command
//! identity that produced a provider mutation plus its typed outcome. Like
//! every provider record here it is redacted: identifiers, classifications,
//! and opaque revisions only — never titles, bodies, or paths beyond the
//! provider's own resource identifier.

use cortex_application::{
    ApplicationError, Capability, OperationIdentity, ProviderMutationOutcome, ProviderOperationLog,
    ProviderOperationRecord,
};
use cortex_domain::{OperationId, ResourceTarget, WorkspaceId};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::{database::storage_error, repositories::id_text};

#[derive(Clone)]
pub struct ProviderOperationStore {
    pool: SqlitePool,
}

impl ProviderOperationStore {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    async fn load(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
    ) -> Result<Option<ProviderOperationRecord>, ApplicationError> {
        let outcome: Option<String> = sqlx::query_scalar(
            "SELECT outcome_json FROM provider_operation WHERE workspace_id = ? AND operation_id = ?",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(operation_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("provider operation lookup failed"))?;
        outcome.map(|value| decode_record(&value)).transpose()
    }
}

impl ProviderOperationLog for ProviderOperationStore {
    async fn find(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
    ) -> Result<Option<ProviderOperationRecord>, ApplicationError> {
        self.load(workspace_id, operation_id).await
    }

    async fn record(
        &self,
        workspace_id: WorkspaceId,
        operation_id: OperationId,
        record: ProviderOperationRecord,
    ) -> Result<(), ApplicationError> {
        let outcome_json = encode_record(&record)?;
        let inserted = sqlx::query(
            "INSERT INTO provider_operation (workspace_id, operation_id, outcome_json) VALUES (?, ?, ?)",
        )
        .bind(id_text(workspace_id))
        .bind(id_text(operation_id))
        .bind(outcome_json)
        .execute(&self.pool)
        .await
        .map_err(|_| storage_error("provider operation record failed"))?;
        if inserted.rows_affected() != 1 {
            // The same operation was recorded concurrently: the existing
            // record wins and remains the idempotent replay result.
            return Err(ApplicationError::Conflict {
                entity: "operation",
            });
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct StoredRecord {
    principal_id: String,
    capability: String,
    target: Option<StoredTarget>,
    resource: StoredResource,
    previous_revision: Option<String>,
    current_revision: Option<String>,
}

#[derive(Serialize, Deserialize)]
enum StoredTarget {
    CortexEntity(String),
    ProviderResource(StoredResource),
    ProviderScope {
        provider_id: String,
        workspace_id: String,
        resource_kind: String,
    },
}

#[derive(Serialize, Deserialize)]
struct StoredResource {
    workspace_id: String,
    provider_id: String,
    resource_id: String,
    resource_kind: String,
}

fn encode_record(record: &ProviderOperationRecord) -> Result<String, ApplicationError> {
    let outcome = &record.outcome;
    let stored = StoredRecord {
        principal_id: id_text(record.identity.principal_id),
        capability: record.identity.capability.metadata().mcp_name.to_owned(),
        target: record.identity.target.as_ref().map(encode_target),
        resource: encode_resource(&outcome.resource),
        previous_revision: outcome
            .previous_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
        current_revision: outcome
            .current_revision
            .as_ref()
            .map(|revision| revision.as_str().to_owned()),
    };
    serde_json::to_string(&stored).map_err(|_| storage_error("provider operation encode failed"))
}

fn decode_record(value: &str) -> Result<ProviderOperationRecord, ApplicationError> {
    let stored: StoredRecord =
        serde_json::from_str(value).map_err(|_| storage_error("invalid provider operation"))?;
    let capability = Capability::from_mcp_name(&stored.capability)
        .ok_or_else(|| storage_error("unknown capability in provider operation"))?;
    let identity = OperationIdentity::new(
        parse_principal(&stored.principal_id)?,
        capability,
        stored.target.map(decode_target).transpose()?,
    );
    let outcome = ProviderMutationOutcome {
        resource: decode_resource(&stored.resource)?,
        previous_revision: stored
            .previous_revision
            .map(cortex_domain::ObservedRevision::new)
            .transpose()
            .map_err(ApplicationError::from)?,
        current_revision: stored
            .current_revision
            .map(cortex_domain::ObservedRevision::new)
            .transpose()
            .map_err(ApplicationError::from)?,
    };
    Ok(ProviderOperationRecord { identity, outcome })
}

fn encode_target(target: &ResourceTarget) -> StoredTarget {
    match target {
        ResourceTarget::CortexEntity(entity_id) => StoredTarget::CortexEntity(id_text(*entity_id)),
        ResourceTarget::ProviderResource(resource) => {
            StoredTarget::ProviderResource(encode_resource(resource))
        }
        ResourceTarget::ProviderScope {
            provider_id,
            workspace_id,
            resource_kind,
        } => StoredTarget::ProviderScope {
            provider_id: provider_id.as_str().to_owned(),
            workspace_id: id_text(*workspace_id),
            resource_kind: encode_kind(*resource_kind).to_owned(),
        },
    }
}

fn decode_target(stored: StoredTarget) -> Result<ResourceTarget, ApplicationError> {
    Ok(match stored {
        StoredTarget::CortexEntity(value) => ResourceTarget::CortexEntity(
            cortex_domain::EntityId::try_from(
                Uuid::parse_str(&value).map_err(|_| storage_error("invalid entity id"))?,
            )
            .map_err(ApplicationError::from)?,
        ),
        StoredTarget::ProviderResource(resource) => {
            ResourceTarget::ProviderResource(decode_resource(&resource)?)
        }
        StoredTarget::ProviderScope {
            provider_id,
            workspace_id,
            resource_kind,
        } => ResourceTarget::ProviderScope {
            provider_id: cortex_domain::ProviderId::new(provider_id)
                .map_err(ApplicationError::from)?,
            workspace_id: parse_workspace(&workspace_id)?,
            resource_kind: decode_kind(&resource_kind)?,
        },
    })
}

fn encode_resource(resource: &cortex_domain::ProviderResourceRef) -> StoredResource {
    StoredResource {
        workspace_id: id_text(resource.workspace_id()),
        provider_id: resource.provider_id().as_str().to_owned(),
        resource_id: resource.resource_id().as_str().to_owned(),
        resource_kind: encode_kind(resource.kind()).to_owned(),
    }
}

fn decode_resource(
    stored: &StoredResource,
) -> Result<cortex_domain::ProviderResourceRef, ApplicationError> {
    Ok(cortex_domain::ProviderResourceRef::new(
        parse_workspace(&stored.workspace_id)?,
        cortex_domain::ProviderId::new(&stored.provider_id).map_err(ApplicationError::from)?,
        cortex_domain::ProviderResourceId::new(&stored.resource_id)
            .map_err(ApplicationError::from)?,
        decode_kind(&stored.resource_kind)?,
    ))
}

fn encode_kind(kind: cortex_domain::ProviderResourceKind) -> &'static str {
    match kind {
        cortex_domain::ProviderResourceKind::Knowledge => "knowledge",
        cortex_domain::ProviderResourceKind::Task => "task",
    }
}

fn decode_kind(value: &str) -> Result<cortex_domain::ProviderResourceKind, ApplicationError> {
    match value {
        "knowledge" => Ok(cortex_domain::ProviderResourceKind::Knowledge),
        "task" => Ok(cortex_domain::ProviderResourceKind::Task),
        _ => Err(storage_error("invalid resource kind")),
    }
}

fn parse_principal(value: &str) -> Result<cortex_domain::PrincipalId, ApplicationError> {
    cortex_domain::PrincipalId::try_from(
        Uuid::parse_str(value).map_err(|_| storage_error("invalid principal id"))?,
    )
    .map_err(ApplicationError::from)
}

fn parse_workspace(value: &str) -> Result<WorkspaceId, ApplicationError> {
    WorkspaceId::try_from(
        Uuid::parse_str(value).map_err(|_| storage_error("invalid workspace id"))?,
    )
    .map_err(ApplicationError::from)
}
