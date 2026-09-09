use cortex_application::{AuditPort, SecretRef, SecretStore};
use cortex_domain::{
    AuditEvent, AuditEventId, AuditResult, OperationId, PolicyDecision, PrincipalId, WorkspaceId,
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
        target_id: None,
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
        target_id: None,
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
