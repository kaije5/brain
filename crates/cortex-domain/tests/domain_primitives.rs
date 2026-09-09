use cortex_domain::{
    AuditEventId, DomainError, EntityId, Lifecycle, OperationId, PrincipalId, Revision, WorkspaceId,
};

#[test]
fn deletion_is_idempotent_only_at_the_operation_layer_not_the_entity_layer() {
    assert_eq!(Lifecycle::Active.delete().unwrap(), Lifecycle::Deleted);
    assert_eq!(
        Lifecycle::Deleted.delete(),
        Err(DomainError::AlreadyDeleted)
    );
}

#[test]
fn revision_increments_without_wraparound() {
    assert_eq!(Revision::initial().next().unwrap().get(), 2);
}

#[test]
fn entity_ids_are_opaque_unique_values() {
    assert_ne!(EntityId::new(), EntityId::new());
}

#[test]
fn each_domain_id_type_generates_its_own_value() {
    let _workspace = WorkspaceId::new();
    let _principal = PrincipalId::new();
    let _operation = OperationId::new();
    let _audit_event = AuditEventId::new();
}

#[test]
fn validation_errors_retain_the_invalid_field() {
    assert_eq!(
        DomainError::validation("title", "must not be empty"),
        DomainError::Validation {
            field: "title",
            reason: "must not be empty".to_owned(),
        }
    );
}
