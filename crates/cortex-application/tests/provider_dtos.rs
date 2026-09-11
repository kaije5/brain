use std::num::{NonZeroU32, NonZeroUsize};

use chrono::{TimeZone, Utc};
use cortex_application::{
    KnowledgeCreate, KnowledgeDelete, KnowledgeDocument, KnowledgeQuery, KnowledgeUpdate,
    ProviderError, ProviderFreshness, ProviderMutation, ProviderPage, ProviderRead, ProviderTask,
    ProviderTaskPriority, ProviderTaskStatus, TaskComplete, TaskCreate, TaskDelete, TaskQuery,
    TaskSchedulingMetadata, TaskUpdate,
};
use cortex_domain::{
    ContentHash, ObservedRevision, OperationId, ProviderId, ProviderProvenance, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, TaskId, WorkspaceId,
};

fn provenance(kind: ProviderResourceKind, revision: &str, hash_byte: u8) -> ProviderProvenance {
    ProviderProvenance::new(
        ProviderResourceRef::new(
            WorkspaceId::new(),
            ProviderId::new("primary").unwrap(),
            ProviderResourceId::new("resource-1").unwrap(),
            kind,
        ),
        ObservedRevision::new(revision).unwrap(),
        ContentHash::new([hash_byte; 32]),
    )
}

#[test]
fn provider_queries_reject_blank_oversized_and_excessive_limits() {
    assert!(KnowledgeQuery::new(WorkspaceId::new(), " ", NonZeroUsize::new(1).unwrap()).is_err());
    assert!(
        KnowledgeQuery::new(
            WorkspaceId::new(),
            "x".repeat(4097),
            NonZeroUsize::new(1).unwrap()
        )
        .is_err()
    );
    assert!(
        KnowledgeQuery::new(WorkspaceId::new(), "safe", NonZeroUsize::new(101).unwrap()).is_err()
    );
    assert!(
        TaskQuery::new(
            WorkspaceId::new(),
            Some("tasks"),
            NonZeroUsize::new(100).unwrap()
        )
        .is_ok()
    );
}

#[test]
fn provider_pages_reject_more_items_than_the_contract_limit() {
    assert!(ProviderPage::new((0..101).collect::<Vec<u8>>(), ProviderFreshness::Current).is_err());
}

#[test]
fn provider_errors_are_redacted_and_typed() {
    assert_eq!(format!("{:?}", ProviderError::Internal), "Internal");
    assert_eq!(
        ProviderRead::new(7_u8, ProviderFreshness::Stale).freshness(),
        ProviderFreshness::Stale
    );
}

#[test]
fn knowledge_dtos_keep_expected_revision_and_provider_neutral_content() {
    let current = provenance(ProviderResourceKind::Knowledge, "rev-2", 2);
    let document = KnowledgeDocument::new(current.clone(), "Runbook", "line one\nline two")
        .expect("valid knowledge document");
    let create = KnowledgeCreate::new(
        current.resource().workspace_id(),
        OperationId::new(),
        "Runbook",
        "",
    )
    .expect("empty body is valid");
    let update = KnowledgeUpdate::new(
        current.resource().clone(),
        OperationId::new(),
        ObservedRevision::new("rev-2").unwrap(),
        "Runbook 2",
        "updated",
    )
    .expect("valid update");
    let delete = KnowledgeDelete::new(
        current.resource().clone(),
        OperationId::new(),
        ObservedRevision::new("rev-2").unwrap(),
    )
    .expect("valid delete");

    assert_eq!(document.title(), "Runbook");
    assert_eq!(document.body(), "line one\nline two");
    assert_eq!(create.body(), "");
    assert_eq!(update.expected_revision().as_str(), "rev-2");
    assert_eq!(delete.expected_revision().as_str(), "rev-2");
}

#[test]
fn mutations_expose_created_updated_and_deleted_provenance_shapes() {
    let previous = provenance(ProviderResourceKind::Knowledge, "rev-1", 1);
    let current = ProviderProvenance::new(
        previous.resource().clone(),
        ObservedRevision::new("rev-2").unwrap(),
        ContentHash::new([2; 32]),
    );

    let created = ProviderMutation::created(previous.clone());
    let updated =
        ProviderMutation::updated(previous.clone(), current.clone()).expect("matching resources");
    let deleted = ProviderMutation::deleted(current.clone());

    assert!(created.previous().is_none());
    assert_eq!(created.current(), Some(&previous));
    assert_eq!(updated.previous(), Some(&previous));
    assert_eq!(updated.current(), Some(&current));
    assert_eq!(deleted.previous(), Some(&current));
    assert!(deleted.current().is_none());
}

#[test]
fn task_dtos_round_trip_identity_revision_and_all_scheduling_fields() {
    let task_provenance = provenance(ProviderResourceKind::Task, "rev-4", 4);
    let due_at = Utc.with_ymd_and_hms(2026, 9, 12, 10, 0, 0).unwrap();
    let deadline_at = Utc.with_ymd_and_hms(2026, 9, 12, 12, 0, 0).unwrap();
    let earliest_start = Utc.with_ymd_and_hms(2026, 9, 12, 8, 0, 0).unwrap();
    let scheduling = TaskSchedulingMetadata::new(
        Some(due_at),
        Some(deadline_at),
        NonZeroU32::new(90),
        Some(earliest_start),
        Some(true),
        Some("Cortex"),
        Some("office"),
    )
    .expect("valid scheduling");
    let task_id = TaskId::new();
    let task = ProviderTask::new(
        task_provenance.clone(),
        task_id,
        "Implement contracts",
        "",
        ProviderTaskStatus::InProgress,
        ProviderTaskPriority::High,
        scheduling.clone(),
    )
    .expect("valid task");
    let create = TaskCreate::new(
        task_provenance.resource().workspace_id(),
        OperationId::new(),
        task_id,
        "Implement contracts",
        "",
        ProviderTaskPriority::High,
        scheduling.clone(),
    )
    .expect("valid create");
    let update = TaskUpdate::new(
        task_provenance.resource().clone(),
        OperationId::new(),
        ObservedRevision::new("rev-4").unwrap(),
        "Implement contracts",
        "body",
        ProviderTaskStatus::InProgress,
        ProviderTaskPriority::Urgent,
        scheduling.clone(),
    )
    .expect("valid update");
    let complete = TaskComplete::new(
        task_provenance.resource().clone(),
        OperationId::new(),
        ObservedRevision::new("rev-4").unwrap(),
    )
    .expect("valid complete");
    let delete = TaskDelete::new(
        task_provenance.resource().clone(),
        OperationId::new(),
        ObservedRevision::new("rev-4").unwrap(),
    )
    .expect("valid delete");

    assert_eq!(task.task_id(), task_id);
    assert_eq!(create.task_id(), task_id);
    assert_eq!(update.expected_revision().as_str(), "rev-4");
    assert_eq!(complete.expected_revision().as_str(), "rev-4");
    assert_eq!(delete.expected_revision().as_str(), "rev-4");
    assert_eq!(scheduling.due_at(), Some(due_at));
    assert_eq!(scheduling.deadline_at(), Some(deadline_at));
    assert_eq!(scheduling.duration_minutes(), NonZeroU32::new(90));
    assert_eq!(scheduling.earliest_start(), Some(earliest_start));
    assert_eq!(scheduling.split(), Some(true));
    assert_eq!(scheduling.project(), Some("Cortex"));
    assert_eq!(scheduling.context(), Some("office"));
}

#[test]
fn mutations_reject_resource_kind_mismatches() {
    let knowledge = provenance(ProviderResourceKind::Knowledge, "rev-1", 1);
    assert!(
        TaskDelete::new(
            knowledge.resource().clone(),
            OperationId::new(),
            ObservedRevision::new("rev-1").unwrap(),
        )
        .is_err()
    );
}
