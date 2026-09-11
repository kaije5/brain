//! Identity and unknown-content preservation across renames and edits
//! (format spec §3, §6.2, §7; SCRUM-105).

use cortex_vault::{ParsedTask, VaultFormatError, parse_document, parse_task, rewrite_task_file};

const ORIGINAL: &str = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a12\nstatus: todo\npriority: normal\nuser_note:   keep my spacing\nsync_meta: [a, b]\n---\n\nOriginal prose with [[links]] and details.\n";

fn parse_task_file(raw: &str, stem: &str) -> ParsedTask {
    let document = parse_document(raw).expect("task file parses");
    parse_task(&document, stem).expect("valid task")
}

#[test]
fn rename_and_move_never_change_task_identity() {
    // The same content parsed under different file names and a different
    // vault location yields the identical stable identity.
    let under_tasks = parse_task_file(ORIGINAL, "01926c8f88f97d339a1b2c7d33bd0a12-call-duo");
    let after_rename = parse_task_file(ORIGINAL, "something-else-entirely");
    let after_move = parse_task_file(ORIGINAL, "nested-location-name");

    assert_eq!(under_tasks.task_id(), after_rename.task_id());
    assert_eq!(under_tasks.task_id(), after_move.task_id());
    assert_eq!(under_tasks.resource_id(), after_move.resource_id());
    assert_eq!(under_tasks.brain_id(), after_move.brain_id());
}

#[test]
fn status_edit_preserves_unknowns_and_body_byte_for_byte() {
    // Externally, the user edits the unknown property and the status; Brain
    // then rewrites the file from that exact state.
    let externally_edited = ORIGINAL
        .replace("keep my spacing", "user changed this")
        .replace("status: todo", "status: in_progress");
    let edited_document = parse_document(&externally_edited).expect("parses");
    let mutated = parse_task(&edited_document, "task").expect("valid");

    let rewritten = rewrite_task_file(&edited_document, &mutated).expect("rewrites");

    // The recomposed file keeps the externally edited unknown property with
    // its exact spacing, changes only the managed key, and keeps the body.
    assert!(rewritten.contains("user_note:   user changed this\n"));
    assert!(rewritten.contains("sync_meta: [a, b]\n"));
    assert!(rewritten.contains("status: in_progress\n"));
    assert!(!rewritten.contains("status: todo"));
    assert!(rewritten.ends_with("\nOriginal prose with [[links]] and details.\n"));
    assert!(rewritten.contains("brain_id: 01926c8f88f97d339a1b2c7d33bd0a12\n"));
}

#[test]
fn recomposed_file_round_trips_through_parse_again() {
    let document = parse_document(ORIGINAL).expect("parses");
    let task = parse_task_file(ORIGINAL, "task");
    let rewritten = rewrite_task_file(&document, &task).expect("rewrites");
    let reparsed = parse_document(&rewritten).expect("recomposed file parses");
    let reparsed_task = parse_task(&reparsed, "any-name").expect("valid");

    assert_eq!(reparsed_task.task_id(), task.task_id());
    assert_eq!(reparsed_task.status(), task.status());
    assert_eq!(reparsed_task.priority(), task.priority());
    assert_eq!(reparsed_task.scheduling(), task.scheduling());
    assert_eq!(
        reparsed_task.body(),
        "\nOriginal prose with [[links]] and details.\n"
    );
}

#[test]
fn rewrite_never_replaces_a_different_task_identity() {
    let document = parse_document(ORIGINAL).expect("parses");
    // A task parsed from a *different* file must never be written over this
    // file's identity.
    let other = ORIGINAL.replace(
        "01926c8f88f97d339a1b2c7d33bd0a12",
        "01926c8f88f97d339a1b2c7d33bd0a13",
    );
    let other_task = parse_task_file(&other, "other");

    assert!(matches!(
        rewrite_task_file(&document, &other_task),
        Err(VaultFormatError::IdentityConflict)
    ));
}

#[test]
fn adopting_a_file_without_brain_id_sets_identity_once() {
    // A foreign task file without brain_id has no identity yet; the first
    // Brain write assigns one, and the assignment sticks.
    let foreign = "---\ntype: task\nstatus: todo\npriority: low\n---\n\nForeign task body.\n";
    let document = parse_document(foreign).expect("parses");
    assert!(parse_task(&document, "foreign").is_err());

    let assigned = "---\ntype: task\nbrain_id: 01926c8f88f97d339a1b2c7d33bd0a12\nstatus: todo\npriority: low\n---\n\nForeign task body.\n";
    let assigned_document = parse_document(assigned).expect("parses");
    let task = parse_task(&assigned_document, "foreign").expect("valid");
    let rewritten = rewrite_task_file(&assigned_document, &task).expect("rewrites");
    assert!(rewritten.contains("brain_id: 01926c8f88f97d339a1b2c7d33bd0a12\n"));
    assert!(rewritten.contains("Foreign task body.\n"));
}

#[test]
fn compact_and_hyphenated_brain_id_spellings_are_one_task() {
    let compact = ORIGINAL.replace(
        "01926c8f88f97d339a1b2c7d33bd0a12",
        "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12",
    );
    let compact_task = parse_task_file(&compact, "compact");
    let hyphenated_task = parse_task_file(ORIGINAL, "hyphenated");
    assert_eq!(compact_task.task_id(), hyphenated_task.task_id());

    // A rewrite of the compact-spelled file keeps the user's spelling.
    let compact_document = parse_document(&compact).expect("parses");
    let rewritten = rewrite_task_file(&compact_document, &compact_task).expect("rewrites");
    assert!(rewritten.contains("brain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a12\n"));

    // And the two files are the same task for identity-conflict purposes:
    // rewriting the compact file with the hyphenated parse is allowed.
    assert!(rewrite_task_file(&compact_document, &hyphenated_task).is_ok());
}

#[test]
fn identity_conflict_is_a_distinct_typed_error() {
    let document = parse_document(ORIGINAL).expect("parses");
    let other_task = parse_task_file(
        &ORIGINAL.replace(
            "01926c8f88f97d339a1b2c7d33bd0a12",
            "01926c8f88f97d339a1b2c7d33bd0a14",
        ),
        "other",
    );
    match rewrite_task_file(&document, &other_task) {
        Err(VaultFormatError::IdentityConflict) => {}
        other => panic!("expected IdentityConflict, got {other:?}"),
    }
}
