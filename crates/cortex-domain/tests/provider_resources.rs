use cortex_domain::{
    ContentHash, DomainError, ObservedRevision, ProviderId, ProviderProvenance, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, TaskId, WorkspaceId,
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
