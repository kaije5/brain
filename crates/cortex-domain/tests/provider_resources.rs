use cortex_domain::{
    ContentHash, DomainError, ObservedRevision, ProviderAuditMetadata, ProviderId,
    ProviderProvenance, ProviderResourceId, ProviderResourceKind, ProviderResourceRef,
    ResourceTarget, TaskId, WorkspaceId,
};
use uuid::Uuid;

#[test]
fn provider_identifiers_reject_blank_oversized_and_control_input() {
    assert!(matches!(
        ProviderId::new(" "),
        Err(DomainError::Validation {
            field: "provider_id",
            ..
        })
    ));
    assert!(matches!(
        ProviderResourceId::new("x".repeat(513)),
        Err(DomainError::Validation {
            field: "provider_resource_id",
            ..
        })
    ));
    assert!(matches!(
        ObservedRevision::new("rev\n2"),
        Err(DomainError::Validation {
            field: "observed_revision",
            ..
        })
    ));
}

#[test]
fn provider_provenance_keeps_stable_identity_separate_from_revision_and_hash() {
    let resource = ProviderResourceRef::new(
        WorkspaceId::new(),
        ProviderId::new("primary-vault").unwrap(),
        ProviderResourceId::new("01K4RESOURCE").unwrap(),
        ProviderResourceKind::Task,
    );
    let provenance = ProviderProvenance::new(
        resource.clone(),
        ObservedRevision::new("rev-7").unwrap(),
        ContentHash::new([0x2a; 32]),
    );

    assert_eq!(provenance.resource(), &resource);
    assert_eq!(provenance.observed_revision().as_str(), "rev-7");
    assert_eq!(provenance.content_hash().as_bytes(), &[0x2a; 32]);
}

#[test]
fn task_ids_require_uuid_v7_when_rehydrated() {
    assert!(TaskId::try_from(Uuid::nil()).is_err());
}

#[test]
fn resource_target_distinguishes_entity_provider_resource_and_scope() {
    let workspace_id = WorkspaceId::new();
    let entity_id = cortex_domain::EntityId::new();
    let resource = ProviderResourceRef::new(
        workspace_id,
        ProviderId::new("primary-vault").unwrap(),
        ProviderResourceId::new("01K4RESOURCE").unwrap(),
        ProviderResourceKind::Knowledge,
    );

    let entity = ResourceTarget::CortexEntity(entity_id);
    let provider_resource = ResourceTarget::ProviderResource(resource.clone());
    let scope = ResourceTarget::ProviderScope {
        provider_id: ProviderId::new("primary-vault").unwrap(),
        workspace_id,
        resource_kind: ProviderResourceKind::Knowledge,
    };

    assert_ne!(entity, provider_resource);
    assert_ne!(provider_resource, scope);
    assert_eq!(
        provider_resource,
        ResourceTarget::ProviderResource(resource)
    );
}

#[test]
fn resource_target_debug_output_stays_redacted() {
    let target = ResourceTarget::ProviderResource(ProviderResourceRef::new(
        WorkspaceId::new(),
        ProviderId::new("secret-vault-name").unwrap(),
        ProviderResourceId::new("01K4SECRETRESOURCE").unwrap(),
        ProviderResourceKind::Task,
    ));

    let rendered = format!("{target:?}");
    assert!(!rendered.contains("secret-vault-name"));
    assert!(!rendered.contains("01K4SECRETRESOURCE"));
}

#[test]
fn provider_audit_metadata_stays_redacted_and_option_bounded() {
    let metadata = ProviderAuditMetadata::new(
        Some(ObservedRevision::new("rev-3").unwrap()),
        Some(ContentHash::new([0x11; 32])),
        Some(ObservedRevision::new("rev-4").unwrap()),
        Some(ContentHash::new([0x22; 32])),
    );
    let rendered = format!("{metadata:?}");
    assert!(!rendered.contains("rev-3"));
    assert!(!rendered.contains("rev-4"));

    let empty = ProviderAuditMetadata::new(None, None, None, None);
    assert_eq!(empty.before_revision, None);
    assert_eq!(empty.after_hash, None);
}
