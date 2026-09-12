//! Adversarial round-trip fixture coverage (format spec §9; SCRUM-103).
//!
//! Every valid fixture asserts both directions of §7 promise 5:
//! `parse(serialize(doc)) == doc` (the raw text is carried verbatim) and
//! `serialize(parse(file)) == file` for unmutated files. Every invalid
//! fixture fails with exactly the typed error the spec assigns to its class.
//! Bound-exceeding cases are generated inline so oversized files never enter
//! git.

use cortex_application::{ProviderTaskPriority, ProviderTaskStatus};
use cortex_vault::{ParsedTask, VaultFormatError, parse_document, parse_task, rewrite_task_file};

const VALID_FIXTURES: &[(&str, &str)] = &[
    (
        "ordinary-document.md",
        include_str!("fixtures/valid/ordinary-document.md"),
    ),
    (
        "body-only-document.md",
        include_str!("fixtures/valid/body-only-document.md"),
    ),
    ("full-task.md", include_str!("fixtures/valid/full-task.md")),
    (
        "minimal-task.md",
        include_str!("fixtures/valid/minimal-task.md"),
    ),
    (
        "unicode-document.md",
        include_str!("fixtures/valid/unicode-document.md"),
    ),
    (
        "unknown-interleaved.md",
        include_str!("fixtures/valid/unknown-interleaved.md"),
    ),
    (
        "foreign-task.md",
        include_str!("fixtures/valid/foreign-task.md"),
    ),
    (
        "hyphenated-brainid-task.md",
        include_str!("fixtures/valid/hyphenated-brainid-task.md"),
    ),
];

const INVALID_FIXTURES: &[(&str, &str, VaultFormatError)] = &[
    (
        "unterminated-frontmatter.md",
        include_str!("fixtures/invalid/unterminated-frontmatter.md"),
        VaultFormatError::MalformedFrontmatter,
    ),
    (
        "duplicate-title.md",
        include_str!("fixtures/invalid/duplicate-title.md"),
        VaultFormatError::DuplicateProperty {
            field: "frontmatter",
        },
    ),
    (
        "duplicate-brain-id.md",
        include_str!("fixtures/invalid/duplicate-brain-id.md"),
        VaultFormatError::DuplicateProperty { field: "brain_id" },
    ),
    (
        "non-iso-date.md",
        include_str!("fixtures/invalid/non-iso-date.md"),
        VaultFormatError::InvalidProperty { field: "due" },
    ),
    (
        "invalid-status.md",
        include_str!("fixtures/invalid/invalid-status.md"),
        VaultFormatError::InvalidProperty { field: "status" },
    ),
    (
        "missing-brain-id.md",
        include_str!("fixtures/invalid/missing-brain-id.md"),
        VaultFormatError::MissingProperty { field: "brain_id" },
    ),
    (
        "bad-boolean.md",
        include_str!("fixtures/invalid/bad-boolean.md"),
        VaultFormatError::InvalidProperty { field: "split" },
    ),
];

fn parse_fixture(raw: &str) -> ParsedTask {
    let document = parse_document(raw).expect("valid fixture parses");
    parse_task(&document, "fixture").expect("valid fixture is a task")
}

#[test]
fn valid_fixtures_round_trip_byte_for_byte() {
    for (name, raw) in VALID_FIXTURES {
        let document = parse_document(raw)
            .unwrap_or_else(|error| panic!("fixture {name} must parse, got {error:?}"));
        // Promise 5, direction 2: serialize(parse(file)) == file.
        assert_eq!(document.raw(), *raw, "fixture {name} must round-trip");
    }
}

#[test]
fn task_fixtures_survive_a_managed_rewrite_and_round_trip_again() {
    for (name, raw) in VALID_FIXTURES {
        let document = parse_document(raw).expect("fixture parses");
        // Foreign fixtures without a brain_id are not Brain-managed tasks.
        let Ok(task) = parse_task(&document, "fixture") else {
            continue;
        };
        let rewritten = rewrite_task_file(&document, &task)
            .unwrap_or_else(|error| panic!("fixture {name} must rewrite, got {error:?}"));

        // Promise 5, direction 1: parse(serialize(doc)) == doc.
        let reparsed = parse_document(&rewritten)
            .unwrap_or_else(|error| panic!("rewritten {name} must parse, got {error:?}"));
        let reparsed_task = parse_task(&reparsed, "fixture")
            .unwrap_or_else(|error| panic!("rewritten {name} must parse as task, got {error:?}"));

        assert_eq!(reparsed_task.task_id(), task.task_id(), "{name}");
        assert_eq!(reparsed_task.status(), task.status(), "{name}");
        assert_eq!(reparsed_task.priority(), task.priority(), "{name}");
        assert_eq!(reparsed_task.scheduling(), task.scheduling(), "{name}");
        // A Brain write normalizes CRLF to LF (format spec §2), so bodies
        // are compared after that documented normalization.
        assert_eq!(
            reparsed_task.body(),
            cortex_vault::normalize_line_endings(task.body()),
            "{name}"
        );

        // And the twice-rewritten file is stable (idempotent serialization).
        let rewritten_again = rewrite_task_file(&reparsed, &reparsed_task).expect("rewrites");
        assert_eq!(
            rewritten_again, rewritten,
            "{name} serialization must be idempotent"
        );
    }
}

#[test]
fn crlf_fixture_is_accepted_and_normalized_only_on_rewrite() {
    // Built at runtime: git normalizes CRLF in committed text files, so the
    // CRLF variant cannot be a checked-in fixture.
    let raw = fixture("minimal-task.md").replace('\n', "\r\n");
    assert!(raw.contains("\r\n"), "fixture is authored with CRLF");
    let document = parse_document(&raw).expect("CRLF accepted on read");
    let task = parse_task(&document, "crlf").expect("valid task");
    assert_eq!(task.status(), ProviderTaskStatus::Todo);

    let rewritten = rewrite_task_file(&document, &task).expect("rewrites");
    assert!(!rewritten.contains("\r\n"), "Brain writes normalize to LF");
    let reparsed = parse_document(&rewritten).expect("normalized file parses");
    assert_eq!(reparsed_task_values(&reparsed).0, task.status());
}

fn reparsed_task_values(
    document: &cortex_vault::ParsedDocument,
) -> (ProviderTaskStatus, ProviderTaskPriority) {
    let task = parse_task(document, "crlf").expect("parses");
    (task.status(), task.priority())
}

#[test]
fn unknown_interleaved_fixture_preserves_every_unknown_on_rewrite() {
    let raw = fixture("unknown-interleaved.md");
    let document = parse_document(raw).expect("parses");
    let task = parse_task(&document, "fixture").expect("valid");
    let rewritten = rewrite_task_file(&document, &task).expect("rewrites");

    assert!(rewritten.contains("plugin_a: on\n"));
    assert!(rewritten.contains("custom_between: yes\n"));
    assert!(rewritten.contains("plugin_b:\n  - x\n  - y\n"));
    assert!(rewritten.contains("status: todo\n"));
    // The brain_id spelling of the source file is preserved on rewrite.
    assert!(rewritten.contains("brain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a14\n"));
}

#[test]
fn unicode_fixture_survives_round_trips_exactly() {
    let raw = fixture("unicode-document.md");
    let document = parse_document(raw).expect("parses");
    assert_eq!(document.title("fallback"), "学习笔记 📚");
    assert!(document.tags().contains(&"café".to_owned()));
    assert!(document.tags().contains(&"日本語".to_owned()));
    assert_eq!(document.raw(), raw);
    // The unknown accented property is preserved verbatim.
    let block = document.frontmatter().expect("frontmatter present");
    assert_eq!(block.scalar("métadonnée"), Some("accentué"));
}

#[test]
fn renamed_task_fixture_keeps_identity() {
    let raw = fixture("full-task.md");
    let original = parse_fixture(raw);
    // A rename is only a different file stem; content is unchanged.
    let renamed = parse_document(raw).expect("parses");
    let renamed_task = parse_task(
        &renamed,
        "01926c8f-88f9-7d33-9a1b-2c7d33bd0a12-finish-anatomy",
    )
    .expect("valid");
    assert_eq!(original.task_id(), renamed_task.task_id());
    assert_eq!(original.resource_id(), renamed_task.resource_id());
}

#[test]
fn hyphenated_and_compact_brain_id_fixtures_are_one_identity() {
    let compact = parse_fixture(fixture("full-task.md"));
    let hyphenated = parse_fixture(fixture("hyphenated-brainid-task.md"));
    assert_eq!(compact.task_id(), hyphenated.task_id());
    assert_eq!(compact.resource_id(), hyphenated.resource_id());
}

#[test]
fn invalid_fixtures_fail_with_exactly_their_typed_error() {
    for (name, raw, expected) in INVALID_FIXTURES {
        let document = parse_document(raw);
        let result = match document {
            Ok(document) => parse_task(&document, "fixture").map(|_| ()),
            Err(error) => Err(error),
        };
        assert_eq!(
            result,
            Err(*expected),
            "fixture {name} must fail with exactly the spec's typed error"
        );
    }
}

fn fixture(name: &str) -> &'static str {
    match VALID_FIXTURES
        .iter()
        .find(|(fixture_name, _)| *fixture_name == name)
    {
        Some((_, raw)) => raw,
        None => panic!("fixture {name} must be registered"),
    }
}

#[test]
fn foreign_task_fixture_has_no_identity_until_adopted() {
    let raw = fixture("foreign-task.md");
    let document = parse_document(raw).expect("parses");
    assert!(parse_task(&document, "foreign").is_err());
}

#[test]
fn duplicate_brain_id_across_two_files_is_a_typed_duplicate_identity() {
    let first = parse_fixture(fixture("full-task.md"));
    let second = parse_fixture(fixture("hyphenated-brainid-task.md"));
    // Same identity spelled differently in two files.
    assert!(cortex_vault::assert_unique_identities(&[first.clone(), second]).is_err());

    let distinct = parse_fixture(fixture("minimal-task.md"));
    assert!(cortex_vault::assert_unique_identities(&[first.clone(), distinct]).is_ok());
    assert!(cortex_vault::assert_unique_identities(&[first.clone(), first]).is_err());
}

#[test]
fn generated_bound_fixtures_exceed_exactly_one_bound() {
    // Oversized title value.
    let oversized_title = format!("---\ntitle: {}\n---\nbody\n", "x".repeat(257));
    assert!(matches!(
        parse_document(&oversized_title),
        Err(VaultFormatError::InvalidProperty { field: "title" })
    ));

    // Oversized single frontmatter value.
    let oversized_value = format!("---\ncustom: {}\n---\nbody\n", "x".repeat(1025));
    assert!(matches!(
        parse_document(&oversized_value),
        Err(VaultFormatError::TooLarge)
    ));

    // Oversized inline tag.
    let oversized_tag = format!("body with #{}\n", "t".repeat(65));
    assert!(matches!(
        parse_document(&oversized_tag),
        Err(VaultFormatError::TooLarge)
    ));

    // Oversized heading text.
    let oversized_heading = format!("# {}\n", "h".repeat(513));
    assert!(matches!(
        parse_document(&oversized_heading),
        Err(VaultFormatError::TooLarge)
    ));

    // Too many headings.
    let mut many_headings = String::new();
    for index in 0..513 {
        many_headings.push_str("# h");
        many_headings.push_str(&index.to_string());
        many_headings.push('\n');
    }
    assert!(matches!(
        parse_document(&many_headings),
        Err(VaultFormatError::TooLarge)
    ));

    // Oversized wikilink target.
    let oversized_wikilink = format!("[[{}]]\n", "w".repeat(513));
    assert!(matches!(
        parse_document(&oversized_wikilink),
        Err(VaultFormatError::InvalidProperty { field: "wikilink" })
    ));

    // Oversized block ID.
    let oversized_block_id = format!("text ^{}\n", "b".repeat(65));
    assert!(matches!(
        parse_document(&oversized_block_id),
        Err(VaultFormatError::InvalidProperty { field: "block_id" })
    ));

    // File beyond the 1 MiB parse bound.
    let oversized_file = format!("body\n{}", "x".repeat(1024 * 1024 + 1));
    assert!(matches!(
        parse_document(&oversized_file),
        Err(VaultFormatError::TooLarge)
    ));

    // Frontmatter beyond the 8 KiB block bound.
    let oversized_frontmatter = format!(
        "---\n{}\n---\nbody\n",
        (0..400)
            .map(|index| format!("key{index}: value value value value value value value value"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(matches!(
        parse_document(&oversized_frontmatter),
        Err(VaultFormatError::TooLarge)
    ));
}
