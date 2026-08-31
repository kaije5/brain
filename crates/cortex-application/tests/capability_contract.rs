use cortex_application::{
    AuditClassification, Capability, CapabilityCatalog, Idempotency, MutationResult,
};
use cortex_domain::{EntityId, Lifecycle, Revision};
use uuid::Uuid;

const EXPECTED_CATALOG: [(Capability, &str, &str, &str, bool, bool); 17] = [
    (
        Capability::NoteCreate,
        "cortex_note_create",
        "NoteCreateInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::NoteUpdate,
        "cortex_note_update",
        "NoteUpdateInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::NoteDelete,
        "cortex_note_delete",
        "NoteDeleteInput",
        "MutationResult",
        true,
        true,
    ),
    (
        Capability::NoteRestore,
        "cortex_note_restore",
        "NoteRestoreInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::NoteSearch,
        "cortex_note_search",
        "NoteSearchRequest",
        "NoteSearchResultList",
        false,
        false,
    ),
    (
        Capability::TaskCreate,
        "cortex_task_create",
        "TaskCreateInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::TaskComplete,
        "cortex_task_complete",
        "TaskCompleteInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::TaskUpdate,
        "cortex_task_update",
        "TaskUpdateInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::TaskDelete,
        "cortex_task_delete",
        "TaskDeleteInput",
        "MutationResult",
        true,
        true,
    ),
    (
        Capability::TaskRestore,
        "cortex_task_restore",
        "TaskRestoreInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::TaskList,
        "cortex_task_list",
        "TaskListRequest",
        "TaskListResult",
        false,
        false,
    ),
    (
        Capability::MemoryCreate,
        "cortex_memory_create",
        "MemoryCreateInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::MemoryCorrect,
        "cortex_memory_correct",
        "MemoryCorrectInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::MemoryDelete,
        "cortex_memory_delete",
        "MemoryDeleteInput",
        "MutationResult",
        true,
        true,
    ),
    (
        Capability::MemoryRestore,
        "cortex_memory_restore",
        "MemoryRestoreInput",
        "MutationResult",
        true,
        false,
    ),
    (
        Capability::MemorySearch,
        "cortex_memory_search",
        "MemorySearchRequest",
        "MemorySearchResultList",
        false,
        false,
    ),
    (
        Capability::KnowledgeRetrieve,
        "cortex_knowledge_search",
        "KnowledgeSearchRequest",
        "KnowledgeSearchResultList",
        false,
        false,
    ),
];

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

#[test]
fn catalog_is_the_complete_canonical_contract_for_every_capability() {
    assert_eq!(CapabilityCatalog::all().len(), EXPECTED_CATALOG.len());
    for (capability, mcp_name, input, output, mutates_state, destructive) in EXPECTED_CATALOG {
        let metadata = capability.metadata();
        assert_eq!(metadata.required_grant, capability);
        assert_eq!(metadata.mcp_name, mcp_name);
        assert_eq!(metadata.input_contract.name, input);
        assert_eq!(metadata.output_contract.name, output);
        assert_eq!(metadata.mutates_state, mutates_state);
        assert_eq!(metadata.destructive, destructive);
        assert_eq!(
            metadata.audit_classification,
            if mutates_state {
                AuditClassification::Mutation
            } else {
                AuditClassification::Decision
            }
        );
        assert_eq!(
            metadata.idempotency,
            if mutates_state {
                Idempotency::Required
            } else {
                Idempotency::NotApplicable
            }
        );
    }
}

#[test]
fn mutation_results_expose_the_canonical_entity_id_name() {
    let entity_id = EntityId::new();
    let result = MutationResult {
        entity_id,
        revision: Revision::initial(),
        lifecycle: Lifecycle::Active,
        audit_correlation_id: Uuid::now_v7(),
    };

    assert_eq!(result.entity_id, entity_id);
}
