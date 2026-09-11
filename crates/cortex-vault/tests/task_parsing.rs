use cortex_application::{ProviderTaskPriority, ProviderTaskStatus};
use cortex_vault::{parse_document, parse_task, serialize_task_frontmatter};

fn full_task_file() -> String {
    "---\ntype: task\nbrain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a12\nstatus: in_progress\npriority: high\ndue: 2026-09-17\ndeadline: 2026-09-20T18:00:00Z\nduration_minutes: 60\nearliest_start: 2026-09-12\nsplit: false\nproject: school\ncontext: homework\ntags: [school]\nunknown_prop: keep me\n---\n\nHuman-readable context and notes.\n"
        .replace(
            "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12",
            "01926c8f88f97d339a1b2c7d33bd0a12",
        )
}

#[test]
fn full_task_parses_into_contract_fields() {
    let document = parse_document(&full_task_file()).expect("valid task file");
    let task = parse_task(&document, "01K4-finish-anatomy").expect("valid task");

    assert_eq!(task.status(), ProviderTaskStatus::InProgress);
    assert_eq!(task.priority(), ProviderTaskPriority::High);

    let scheduling = task.scheduling();
    assert_eq!(
        scheduling.due_at().map(|due| due.to_rfc3339()),
        Some("2026-09-17T00:00:00+00:00".to_owned())
    );
    assert!(scheduling.deadline_at().is_some());
    assert_eq!(
        scheduling.duration_minutes().map(std::num::NonZeroU32::get),
        Some(60)
    );
    assert_eq!(
        scheduling.earliest_start().map(|start| start.to_rfc3339()),
        Some("2026-09-12T00:00:00+00:00".to_owned())
    );
    assert_eq!(scheduling.split(), Some(false));
    assert_eq!(scheduling.project(), Some("school"));
    assert_eq!(scheduling.context(), Some("homework"));

    assert_eq!(task.title(), "01K4-finish-anatomy");
    assert!(task.body().contains("Human-readable context"));
}

#[test]
fn minimal_task_uses_missing_optional_absence() {
    let raw = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\npriority: normal\n---\n\nBody only.\n";
    let document = parse_document(raw).expect("valid");
    let task = parse_task(&document, "minimal").expect("valid task");
    assert_eq!(task.status(), ProviderTaskStatus::Todo);
    assert_eq!(task.priority(), ProviderTaskPriority::Normal);
    assert_eq!(task.scheduling().due_at(), None);
    assert_eq!(task.scheduling().duration_minutes(), None);
    assert_eq!(task.scheduling().split(), None);
    assert_eq!(task.scheduling().project(), None);
    assert_eq!(task.title(), "minimal");
}

#[test]
fn missing_required_properties_are_typed_errors() {
    for raw in [
        "---\ntype: task\nstatus: todo\npriority: normal\n---\nbody\n", // no brain_id
        "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\npriority: normal\n---\nbody\n", // no status
        "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\n---\nbody\n", // no priority
    ] {
        let document = parse_document(raw).expect("parses");
        let task = parse_task(&document, "stem");
        assert!(matches!(
            task,
            Err(cortex_vault::VaultFormatError::MissingProperty { .. })
        ));
    }
}

#[test]
fn invalid_values_are_typed_errors() {
    let invalid_status = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: finished\npriority: normal\n---\n";
    let document = parse_document(invalid_status).expect("parses");
    assert!(matches!(
        parse_task(&document, "s"),
        Err(cortex_vault::VaultFormatError::InvalidProperty { field: "status" })
    ));

    let invalid_priority = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\npriority: critical\n---\n";
    let document = parse_document(invalid_priority).expect("parses");
    assert!(matches!(
        parse_task(&document, "s"),
        Err(cortex_vault::VaultFormatError::InvalidProperty { field: "priority" })
    ));

    let invalid_brain_id =
        "---\ntype: task\nbrain_id: not-a-uuid\nstatus: todo\npriority: normal\n---\n";
    let document = parse_document(invalid_brain_id).expect("parses");
    assert!(matches!(
        parse_task(&document, "s"),
        Err(cortex_vault::VaultFormatError::InvalidProperty { field: "brain_id" })
    ));

    let bad_date = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\npriority: normal\ndue: soon\n---\n";
    let document = parse_document(bad_date).expect("parses");
    assert!(matches!(
        parse_task(&document, "s"),
        Err(cortex_vault::VaultFormatError::InvalidProperty { field: "due" })
    ));

    let duration_over = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\npriority: normal\nduration_minutes: 1441\n---\n";
    let document = parse_document(duration_over).expect("parses");
    assert!(matches!(
        parse_task(&document, "s"),
        Err(cortex_vault::VaultFormatError::InvalidProperty {
            field: "duration_minutes"
        })
    ));

    let duration_zero = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\npriority: normal\nduration_minutes: 0\n---\n";
    let document = parse_document(duration_zero).expect("parses");
    assert!(matches!(
        parse_task(&document, "s"),
        Err(cortex_vault::VaultFormatError::InvalidProperty {
            field: "duration_minutes"
        })
    ));
}

#[test]
fn duplicate_managed_keys_are_rejected() {
    let raw = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a12\nstatus: todo\npriority: normal\n---\nbody\n";
    let document = parse_document(raw).expect("document parses");
    assert!(matches!(
        parse_task(&document, "s"),
        Err(cortex_vault::VaultFormatError::DuplicateProperty { field: "brain_id" })
    ));
}

#[test]
fn non_task_files_are_rejected() {
    let raw = "---\ntitle: just a note\n---\nbody\n";
    let document = parse_document(raw).expect("parses");
    assert!(matches!(
        parse_task(&document, "note"),
        Err(cortex_vault::VaultFormatError::InvalidProperty { field: "type" })
    ));
}

#[test]
fn identity_comes_from_brain_id_not_filename() {
    let raw = "---\ntype: task\nbrain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a12\nstatus: todo\npriority: normal\n---\nbody\n";
    let document = parse_document(raw).expect("parses");
    let task = parse_task(&document, "totally-different-filename").expect("valid");
    // The hyphenated and compact spellings are the same identity.
    let other = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a12\nstatus: todo\npriority: normal\n---\nbody\n";
    let other_document = parse_document(other).expect("parses");
    let other_task = parse_task(&other_document, "another-name").expect("valid");

    assert_eq!(task.task_id(), other_task.task_id());
    assert_eq!(task.resource_id(), other_task.resource_id());
    assert_eq!(task.brain_id(), "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12");
}

#[test]
fn serialization_is_in_canonical_order_and_round_trips() {
    let raw = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\npriority: urgent\ndue: 2026-09-17\n---\nbody\n";
    let document = parse_document(raw).expect("parses");
    let task = parse_task(&document, "s").expect("valid");

    let frontmatter = serialize_task_frontmatter(&task, None).expect("serializes");
    let lines: Vec<&str> = frontmatter.lines().collect();
    assert_eq!(
        lines,
        [
            "type: task",
            "brain_id: 01926c8f88f97d339a1b2c7d33bd0a13",
            "status: todo",
            "priority: urgent",
            "due: 2026-09-17",
        ]
    );

    // Parse(serialize) yields the same contract values.
    let reparsed = parse_document(&format!("---\n{frontmatter}---\nbody\n")).expect("parses");
    let reparsed_task = parse_task(&reparsed, "s").expect("valid");
    assert_eq!(task, reparsed_task);
}

#[test]
fn rewrite_preserves_unknown_properties_and_body() {
    let raw = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a13\nstatus: todo\npriority: normal\nunknown_prop:   keep exactly\n---\n\nOriginal body prose.\n";
    let document = parse_document(raw).expect("parses");
    let mut task = parse_task(&document, "s").expect("valid");
    assert_eq!(task.status(), ProviderTaskStatus::Todo);

    // Simulate a status mutation: same identity, new status.
    let updated =
        cortex_vault::parse_document(&raw.replace("status: todo", "status: done")).expect("parses");
    task = parse_task(&updated, "s").expect("valid");
    assert_eq!(task.status(), ProviderTaskStatus::Completed);

    let rewritten = serialize_task_frontmatter(&task, document.frontmatter()).expect("rewrites");
    assert!(rewritten.contains("status: done\n"));
    assert!(rewritten.contains("unknown_prop:   keep exactly\n"));
    assert!(rewritten.contains("brain_id: 01926c8f88f97d339a1b2c7d33bd0a13\n"));
    assert!(!rewritten.contains("status: todo"));
    // The body prose is never touched by frontmatter rewriting.
    assert_eq!(task.body(), "\nOriginal body prose.\n");
}
