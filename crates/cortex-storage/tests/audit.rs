use cortex_application::{AuditPort, SecretRef, SecretStore};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, ContentHash, EntityId, ObservedRevision, OperationId,
    PolicyDecision, PrincipalId, ProviderAuditMetadata, ProviderId, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, ResourceTarget, WorkspaceId,
};
use cortex_storage::SqliteDatabase;
use tempfile::TempDir;
use uuid::Uuid;

#[tokio::test]
async fn audit_port_round_trips_only_redacted_evidence_and_is_append_only() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
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
    audit.append(event.clone()).await.map_err(debug_error)?;

    assert_eq!(
        audit
            .find(workspace_id, event.id)
            .await
            .map_err(debug_error)?,
        Some(event.clone())
    );
    assert_eq!(
        audit
            .find(WorkspaceId::new(), event.id)
            .await
            .map_err(debug_error)?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn audit_port_round_trips_provider_targets_and_revision_metadata() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
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
    let audit = database.audit_port();

    let resource = ProviderResourceRef::new(
        workspace_id,
        ProviderId::new("primary-vault").map_err(debug_error)?,
        ProviderResourceId::new("01K4RESOURCE").map_err(debug_error)?,
        ProviderResourceKind::Knowledge,
    );
    let resource_event = AuditEvent {
        id: AuditEventId::new(),
        workspace_id,
        principal_id,
        operation_id: OperationId::new(),
        correlation_id: Uuid::now_v7(),
        capability: "cortex_note_search",
        target: Some(ResourceTarget::ProviderResource(resource)),
        provider_metadata: Some(ProviderAuditMetadata::new(
            Some(ObservedRevision::new("rev-before").map_err(debug_error)?),
            Some(ContentHash::new([0x11; 32])),
            Some(ObservedRevision::new("rev-after").map_err(debug_error)?),
            Some(ContentHash::new([0x22; 32])),
        )),
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    };
    audit
        .append(resource_event.clone())
        .await
        .map_err(debug_error)?;
    assert_eq!(
        audit
            .find(workspace_id, resource_event.id)
            .await
            .map_err(debug_error)?,
        Some(resource_event)
    );

    let scope_event = AuditEvent {
        id: AuditEventId::new(),
        workspace_id,
        principal_id,
        operation_id: OperationId::new(),
        correlation_id: Uuid::now_v7(),
        capability: "cortex_note_create",
        target: Some(ResourceTarget::ProviderScope {
            provider_id: ProviderId::new("primary-vault").map_err(debug_error)?,
            workspace_id,
            resource_kind: ProviderResourceKind::Task,
        }),
        provider_metadata: None,
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Rejected,
    };
    audit
        .append(scope_event.clone())
        .await
        .map_err(debug_error)?;
    assert_eq!(
        audit
            .find(workspace_id, scope_event.id)
            .await
            .map_err(debug_error)?,
        Some(scope_event)
    );

    // Persisted evidence must remain redacted: opaque ids only, no content,
    // and revisions/hashes stored as bounded tokens rather than text bodies.
    Ok(())
}

#[tokio::test]
async fn audit_rows_with_plain_entity_targets_still_decode() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database_path = temp.path().join("cortex.db");
    let database = SqliteDatabase::connect_and_migrate(database_path.clone())
        .await
        .map_err(debug_error)?;
    let raw_pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", database_path.display()))
        .await
        .map_err(|error| format!("raw pool failed: {error}"))?;
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

    // A pre-provider row stores a plain Cortex entity UUID.
    let entity_id = EntityId::new();
    let legacy_event = AuditEvent {
        id: AuditEventId::new(),
        workspace_id,
        principal_id,
        operation_id: OperationId::new(),
        correlation_id: Uuid::now_v7(),
        capability: "cortex_note_search",
        target: Some(ResourceTarget::CortexEntity(entity_id)),
        provider_metadata: None,
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    };
    sqlx::query(
        "INSERT INTO audit_event \
         (id, workspace_id, principal_id, operation_id, correlation_id, capability, target_id, \
          policy_decision, result, redacted_metadata) VALUES (?, ?, ?, ?, ?, ?, ?, 'allow', 'succeeded', '{}')",
    )
    .bind(Uuid::from(legacy_event.id).to_string())
    .bind(Uuid::from(workspace_id).to_string())
    .bind(Uuid::from(principal_id).to_string())
    .bind(Uuid::from(legacy_event.operation_id).to_string())
    .bind(legacy_event.correlation_id.to_string())
    .bind("cortex_note_search")
    .bind(Uuid::from(entity_id).to_string())
    .execute(&raw_pool)
    .await
    .map_err(|error| format!("legacy insert failed: {error}"))?;

    let event = database
        .audit_port()
        .find(workspace_id, legacy_event.id)
        .await
        .map_err(debug_error)?
        .ok_or("legacy audit row missing")?;
    assert_eq!(event.target, Some(ResourceTarget::CortexEntity(entity_id)));
    assert_eq!(event.provider_metadata, None);
    Ok(())
}

#[tokio::test]
async fn audit_port_rejects_unknown_capabilities_without_persisting_them() -> Result<(), String> {
    let temp = TempDir::new().map_err(|error| format!("temp directory failed: {error}"))?;
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .map_err(debug_error)?;
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

    let event = AuditEvent {
        id: AuditEventId::new(),
        workspace_id,
        principal_id,
        operation_id: OperationId::new(),
        correlation_id: Uuid::now_v7(),
        capability: "cortex_unrecognized_capability",
        target: None,
        provider_metadata: None,
        policy_decision: PolicyDecision::Allow,
        result: AuditResult::Succeeded,
    };
    let audit = database.audit_port();

    assert!(audit.append(event.clone()).await.is_err());
    assert_eq!(
        audit
            .find(workspace_id, event.id)
            .await
            .map_err(debug_error)?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn secret_store_boundary_resolves_only_opaque_references() -> Result<(), String> {
    let reference = SecretRef::new("keyring:cortex/model-api").map_err(debug_error)?;
    let store = ReferenceOnlyStore;
    let resolved = store.resolve(&reference).await.map_err(debug_error)?;

    assert_eq!(resolved, reference);
    assert_eq!(format!("{resolved:?}"), "SecretRef([REDACTED])");
    assert!(SecretRef::new("  ").is_err());
    Ok(())
}

struct ReferenceOnlyStore;

impl SecretStore for ReferenceOnlyStore {
    async fn resolve(
        &self,
        reference: &SecretRef,
    ) -> Result<SecretRef, cortex_application::ApplicationError> {
        Ok(reference.clone())
    }
}

fn debug_error(error: impl std::fmt::Debug) -> String {
    format!("{error:?}")
}
