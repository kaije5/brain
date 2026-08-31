use cortex_domain::{
    DomainError, Lifecycle, Note, NoteInput, Source, SourceInput, Task, TaskInput, TaskStatus,
    WorkspaceId,
};

#[test]
fn create_rejects_a_task_without_a_title() {
    let input = TaskInput {
        workspace_id: WorkspaceId::new(),
        title: "   ".to_owned(),
        due_at: None,
    };

    assert!(matches!(
        Task::create(input),
        Err(DomainError::Validation { field: "title", .. })
    ));
}

#[test]
fn completing_an_open_task_returns_a_completed_successor() {
    let task = Task::create(TaskInput {
        workspace_id: WorkspaceId::new(),
        title: "Finish Cortex architecture".to_owned(),
        due_at: None,
    })
    .unwrap();

    let completed = task.complete().unwrap();

    assert_eq!(task.status(), TaskStatus::Open);
    assert_eq!(completed.status(), TaskStatus::Completed);
}

#[test]
fn completing_a_completed_task_is_rejected() {
    let task = Task::create(TaskInput {
        workspace_id: WorkspaceId::new(),
        title: "Finish Cortex architecture".to_owned(),
        due_at: None,
    })
    .unwrap()
    .complete()
    .unwrap();

    assert!(matches!(
        task.complete(),
        Err(DomainError::Validation {
            field: "status",
            ..
        })
    ));
}

#[test]
fn note_content_is_separate_from_memory_assertions() {
    let note = Note::create(NoteInput {
        workspace_id: WorkspaceId::new(),
        title: "Architecture".to_owned(),
        content: "Long-form design evidence".to_owned(),
    })
    .unwrap();

    assert_eq!(note.content(), "Long-form design evidence");
    assert_eq!(note.lifecycle(), Lifecycle::Active);
}

#[test]
fn source_records_immutable_evidence_separately_from_an_assertion() {
    let source = Source::create(SourceInput {
        workspace_id: WorkspaceId::new(),
        reference: "note:architecture".to_owned(),
    })
    .unwrap();

    assert_eq!(source.reference(), "note:architecture");
    assert_eq!(source.lifecycle(), Lifecycle::Active);
}
