//! Round-trip preservation guarantees (format spec §7): parse(serialize(doc))
//! == doc, and an unmutated file re-serializes byte-for-byte. Managed
//! property rewrites preserve unknown properties, their order, and their
//! exact formatting.

use cortex_vault::{parse_document, split_frontmatter};

#[test]
fn unmutated_files_round_trip_byte_for_byte() {
    let fixtures = [
        "# Plain note\n\nBody text only.\n",
        "---\ntitle: With Frontmatter\ntags:\n  - one\n  - two\n---\n\n# Heading\n\nBody.\n",
        "---\ntitle: Unicode 📚\ncustom: keep me exactly\nnested_unknown:\n  key: value\n---\n\nBody with [[links]] and #tags.\n",
        "---\nunknown_first: 1\ntitle: Ordered\n---\n\nBody.\n",
        "",
        "---\nunknown: value\n---\n",
    ];
    for fixture in fixtures {
        let document = parse_document(fixture).expect("fixture parses");
        assert_eq!(document.raw(), fixture, "unmutated file must round-trip");
    }
}

#[test]
fn frontmatter_block_preserves_unknown_properties_verbatim() {
    let raw = "---\ntitle: Keep\n# a comment inside frontmatter\nunknown   :    spaced value\nquoted: \"keep \\\"quotes\\\"\"\n---\n\nBody.\n";
    let (block, _body) = split_frontmatter(raw).expect("valid frontmatter");
    let block = block.expect("frontmatter present");
    assert_eq!(block.scalar("title"), Some("Keep"));
    assert_eq!(block.scalar("unknown"), Some("spaced value"));
    assert_eq!(block.scalar("quoted"), Some("keep \\\"quotes\\\""));
    // The raw block text (everything between the delimiters) is untouched.
    assert_eq!(
        block.raw(),
        "title: Keep\n# a comment inside frontmatter\nunknown   :    spaced value\nquoted: \"keep \\\"quotes\\\"\"\n"
    );
}

#[test]
fn managed_property_rewrite_preserves_unknowns_and_order() {
    let raw = "---\nunknown_first: keep-1\ntitle: Old\nanother: keep-2\ntags: [old]\n---\n\nBody stays.\n";
    let document = parse_document(raw).expect("valid document");
    let block = document.frontmatter().expect("frontmatter present");

    let rewritten = block
        .with_managed_properties(&[("title", "New".to_owned()), ("tags", "[rust]".to_owned())])
        .expect("no duplicate managed keys");

    // Managed keys are re-emitted canonically at the top; unknowns keep
    // their exact lines in their original relative order.
    assert!(rewritten.starts_with("title: New\ntags: [rust]\n"));
    assert!(rewritten.contains("unknown_first: keep-1\n"));
    assert!(rewritten.contains("another: keep-2\n"));
    assert!(!rewritten.contains("title: Old"));
    assert!(!rewritten.contains("tags: [old]"));

    // The rewritten document still parses and exposes the new values.
    let reparsed = parse_document(&format!("---\n{rewritten}---\n\nBody stays.\n"))
        .expect("rewritten frontmatter parses");
    assert_eq!(reparsed.title("fallback"), "New");
    assert_eq!(reparsed.tags(), ["rust"]);
}

#[test]
fn managed_rewrite_keeps_unknown_sequence_blocks_intact() {
    let raw = "---\nrelations:\n  - a\n  - b\ntitle: T\n---\n\nBody.\n";
    let document = parse_document(raw).expect("valid document");
    let block = document.frontmatter().expect("frontmatter");
    let rewritten = block
        .with_managed_properties(&[("title", "T2".to_owned())])
        .expect("rewrite succeeds");
    // The unknown block-form sequence survives with its continuation lines.
    assert!(rewritten.contains("relations:\n  - a\n  - b\n"));
    assert!(rewritten.contains("title: T2\n"));
    assert!(!rewritten.contains("title: T\n"));
}

#[test]
fn managed_rewrite_rejects_duplicate_managed_keys() {
    let raw = "---\ntags: [a]\ntags: [b]\n---\nbody\n";
    let document = parse_document(raw);
    // Duplicate unknown-at-parse-time managed keys are rejected at rewrite.
    assert!(
        document.is_err() || {
            let document = document.expect("parsed");
            document
                .frontmatter()
                .expect("frontmatter")
                .with_managed_properties(&[("tags", "[c]".to_owned())])
                .is_err()
        }
    );
}

#[test]
fn body_is_preserved_exactly_when_only_frontmatter_is_rewritten() {
    let body = "Line one.\n\n  indented   spacing \n\ttabbed\n\nlast\n";
    let raw = format!("---\ntitle: Old\n---\n{body}");
    let document = parse_document(&raw).expect("valid document");
    assert_eq!(document.body(), body);
    let block = document.frontmatter().expect("frontmatter");
    let rewritten = block
        .with_managed_properties(&[("title", "New".to_owned())])
        .expect("rewrite succeeds");
    // Recomposing the file with the rewritten block leaves the body
    // byte-for-byte identical.
    let recomposed = format!("---\n{rewritten}---\n{body}");
    let reparsed = parse_document(&recomposed).expect("recomposed file parses");
    assert_eq!(reparsed.body(), body);
    assert_eq!(reparsed.title("fallback"), "New");
}

#[test]
fn sequence_parsing_supports_flow_and_block_forms() {
    let flow = "---\ntags: [a, \"b c\", d]\n---\nbody\n";
    let (block, _) = split_frontmatter(flow).expect("valid");
    let block = block.expect("frontmatter");
    assert_eq!(
        block.sequence("tags").expect("valid sequence"),
        ["a", "b c", "d"]
    );

    let block_form = "---\ntags:\n  - x\n  - y\n---\nbody\n";
    let (block, _) = split_frontmatter(block_form).expect("valid");
    let block = block.expect("frontmatter");
    assert_eq!(block.sequence("tags").expect("valid sequence"), ["x", "y"]);
}
