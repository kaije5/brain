use cortex_application::{AuditClassification, Capability, CapabilityCatalog, Idempotency};

#[test]
fn delete_capability_metadata_is_destructive_and_audited() {
    let metadata = Capability::TaskDelete.metadata();

    assert!(metadata.destructive);
    assert!(metadata.mutates_state);
    assert_eq!(metadata.audit_classification, AuditClassification::Mutation);
    assert_eq!(metadata.idempotency, Idempotency::Required);
}

#[test]
fn catalog_exposes_a_unique_mcp_name_for_every_capability() {
    let names = CapabilityCatalog::all()
        .iter()
        .map(|capability| capability.metadata().mcp_name)
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(names.len(), CapabilityCatalog::all().len());
    assert!(names.contains("cortex_memory_delete"));
}
