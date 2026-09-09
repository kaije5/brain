use cortex_domain::{
    DomainError, EntityId, Lifecycle, MemoryAssertion, MemoryStatus, Note, Revision, Source,
    SourceRef, Task, TaskStatus, WorkspaceId,
};
use uuid::Uuid;

#[test]
fn persisted_ids_reject_nil_and_non_v7_uuids() {
    assert!(matches!(
        EntityId::try_from(Uuid::nil()),
        Err(DomainError::Validation { field: "id", .. })
    ));
    assert!(matches!(
        EntityId::try_from(Uuid::from_u128(0x67e5_5044_10b1_426f_9247_bb68_0e5f_e0c8)),
        Err(DomainError::Validation { field: "id", .. })
    ));
}

#[test]
fn persisted_ids_round_trip_without_exposing_their_representation() -> Result<(), String> {
    let original = EntityId::new();
    let persisted = Uuid::from(original);
    let restored = EntityId::try_from(persisted)
        .map_err(|error| format!("valid persisted id rejected: {error:?}"))?;

    assert_eq!(restored, original);
    Ok(())
}

#[test]
fn persisted_revision_rejects_zero() {
    assert!(matches!(
        Revision::rehydrate(0),
        Err(DomainError::Validation {
            field: "revision",
            ..
        })
    ));
}

#[test]
fn note_rehydration_revalidates_text() {
    let result = Note::rehydrate(
        EntityId::new(),
        WorkspaceId::new(),
        "persisted title".to_owned(),
        "  ".to_owned(),
        Revision::initial(),
        Lifecycle::Active,
    );

    assert!(matches!(
        result,
        Err(DomainError::Validation {
            field: "content",
            ..
        })
    ));
}

#[test]
fn task_rehydration_revalidates_text() {
    let result = Task::rehydrate(
        EntityId::new(),
        WorkspaceId::new(),
        String::new(),
        None,
        TaskStatus::Open,
        Revision::initial(),
        Lifecycle::Active,
    );

    assert!(matches!(
        result,
        Err(DomainError::Validation { field: "title", .. })
    ));
}

#[test]
fn source_rehydration_revalidates_reference() {
    let result = Source::rehydrate(
        EntityId::new(),
        WorkspaceId::new(),
        "\n".to_owned(),
        Revision::initial(),
        Lifecycle::Active,
    );

    assert!(matches!(
        result,
        Err(DomainError::Validation {
            field: "reference",
            ..
        })
    ));
}

#[test]
fn memory_rehydration_revalidates_provenance() {
    let result = MemoryAssertion::rehydrate(
        EntityId::new(),
        WorkspaceId::new(),
        "Cortex is local first".to_owned(),
        "cortex".to_owned(),
        "architecture".to_owned(),
        "local-first".to_owned(),
        Vec::<SourceRef>::new(),
        None,
        MemoryStatus::Active,
        Revision::initial(),
        Lifecycle::Active,
    );

    assert!(matches!(
        result,
        Err(DomainError::Validation {
            field: "sources",
            ..
        })
    ));
}
