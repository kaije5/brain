use cortex_domain::{
    ConflictSet, DomainError, EntityId, MemoryAssertion, MemoryAssertionInput, MemoryStatus,
    SourceRef, WorkspaceId,
};

fn input(workspace_id: WorkspaceId, object: &str) -> MemoryAssertionInput {
    MemoryAssertionInput {
        workspace_id,
        statement: format!("Cortex uses {object} as its local AI."),
        normalized_subject: "cortex".to_owned(),
        normalized_predicate: "local_ai".to_owned(),
        normalized_object: object.to_lowercase(),
        sources: vec![SourceRef {
            source_id: EntityId::new(),
        }],
    }
}

#[test]
fn create_rejects_assertions_without_provenance() {
    let mut without_sources = input(WorkspaceId::new(), "Nemotron");
    without_sources.sources.clear();

    assert!(matches!(
        MemoryAssertion::create(without_sources),
        Err(DomainError::Validation {
            field: "sources",
            ..
        })
    ));
}

#[test]
fn correction_keeps_original_active_and_links_successor() {
    let workspace_id = WorkspaceId::new();
    let original = MemoryAssertion::create(input(workspace_id, "OldModel")).unwrap();
    let corrected = original.correct(input(workspace_id, "Nemotron")).unwrap();

    assert_eq!(corrected.supersedes(), Some(original.id()));
    assert_eq!(original.status(), MemoryStatus::Active);
    assert_eq!(corrected.status(), MemoryStatus::Active);
}

#[test]
fn forgetting_hides_an_assertion_from_active_conflicts() {
    let workspace_id = WorkspaceId::new();
    let first = MemoryAssertion::create(input(workspace_id, "Nemotron")).unwrap();
    let forgotten = first.forget().unwrap();
    let second = MemoryAssertion::create(input(workspace_id, "Llama")).unwrap();

    assert!(ConflictSet::from_assertions(&[forgotten, second]).is_empty());
}

#[test]
fn conflict_sets_only_group_distinct_active_values_for_the_same_fact() {
    let workspace_id = WorkspaceId::new();
    let first = MemoryAssertion::create(input(workspace_id, "Nemotron")).unwrap();
    let second = MemoryAssertion::create(input(workspace_id, "Llama")).unwrap();
    let duplicate = MemoryAssertion::create(input(workspace_id, "Llama")).unwrap();

    let conflicts = ConflictSet::from_assertions(&[first, second, duplicate]);

    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].assertions().len(), 3);
}
