use cortex_vault::{VaultFormatError, parse_document};

#[test]
fn parses_title_headings_body_tags_wikilinks_block_ids_and_links() {
    let raw = "---\ntitle: Architecture Notes\ntags: [rust, vault]\n---\n\n# Overview\n\nSee [[Design Doc|the design]] and [spec](https://example.com/spec.md).\n\n## Details ^sec-1\n\nMixed inline `#notatag` and real #inline-tag text.\n";
    let document = parse_document(raw).expect("valid document");

    assert_eq!(document.title("fallback"), "Architecture Notes");
    assert_eq!(document.headings(), ["Overview", "Details"]);
    assert_eq!(document.tags(), ["rust", "vault", "inline-tag"]);
    assert_eq!(document.wikilinks().len(), 1);
    assert_eq!(document.wikilinks()[0].target, "Design Doc");
    assert_eq!(document.wikilinks()[0].label.as_deref(), Some("the design"));
    assert_eq!(document.block_ids(), ["sec-1"]);
    assert_eq!(document.links().len(), 1);
    assert_eq!(document.links()[0].target, "https://example.com/spec.md");
    assert!(document.body().contains("# Overview"));
}

#[test]
fn title_falls_back_to_first_heading_then_file_stem() {
    let with_heading = parse_document("# First Heading\n\nbody\n").expect("valid");
    assert_eq!(with_heading.title("stem.txt"), "First Heading");

    let plain = parse_document("just a body\n").expect("valid");
    assert_eq!(plain.title("my-note"), "my-note");
}

#[test]
fn documents_without_frontmatter_are_valid() {
    let document = parse_document("# Only a heading\n\nSome body text.\n").expect("valid");
    assert!(document.frontmatter().is_none());
    assert_eq!(document.headings(), ["Only a heading"]);
    assert_eq!(document.raw(), "# Only a heading\n\nSome body text.\n");
}

#[test]
fn wikilinks_without_labels_and_multiple_occurrences_are_supported() {
    let raw = "A [[Alpha]] then [[Beta|b label]] then [[Alpha]] again.\n";
    let document = parse_document(raw).expect("valid");
    let targets: Vec<&str> = document
        .wikilinks()
        .iter()
        .map(|link| link.target.as_str())
        .collect();
    assert_eq!(targets, ["Alpha", "Beta", "Alpha"]);
    assert_eq!(document.wikilinks()[1].label.as_deref(), Some("b label"));
}

#[test]
fn inline_scanning_ignores_code_spans_and_fenced_blocks() {
    let raw = "Real #tag one `#codespan` text.\n\n```rust\n#ignored_heading\n[[ignored]]\n^ignored-id\n```\n\n~~~\n#also-ignored\n~~~\n\n[[real-link]]\n";
    let document = parse_document(raw).expect("valid");
    assert_eq!(document.tags(), ["tag"]);
    assert!(document.headings().is_empty());
    assert_eq!(document.wikilinks().len(), 1);
    assert_eq!(document.wikilinks()[0].target, "real-link");
    assert!(document.block_ids().is_empty());
}

#[test]
fn unicode_content_is_preserved_exactly() {
    let raw = "---\ntitle: 学习笔记 📚\n---\n\n# 学习笔记 📚\n\nEmoji 🚀 and combining é compare.\n[[日本語ノート]]\n";
    let document = parse_document(raw).expect("valid");
    assert_eq!(document.title("fallback"), "学习笔记 📚");
    assert_eq!(document.headings(), ["学习笔记 📚"]);
    assert_eq!(document.wikilinks()[0].target, "日本語ノート");
    assert_eq!(document.raw(), raw);
}

#[test]
fn crlf_input_is_accepted_and_normalizable() {
    let raw = "---\r\ntitle: Windows\r\n---\r\n\r\n# Body\r\n";
    let document = parse_document(raw).expect("CRLF is valid on read");
    assert_eq!(document.title("fallback"), "Windows");
    assert_eq!(document.headings(), ["Body"]);

    let normalized = cortex_vault::normalize_line_endings(raw);
    assert!(!normalized.contains('\r'));
    assert!(parse_document(&normalized).is_ok());
}

#[test]
fn malformed_frontmatter_is_a_typed_error() {
    let unterminated = parse_document("---\ntitle: broken\n\nbody without close\n");
    assert!(matches!(
        unterminated,
        Err(VaultFormatError::MalformedFrontmatter)
    ));

    let duplicate = parse_document("---\ntitle: one\ntitle: two\n---\nbody\n");
    assert!(matches!(
        duplicate,
        Err(VaultFormatError::DuplicateProperty {
            field: "frontmatter"
        })
    ));

    let oversized_title = parse_document(&format!("---\ntitle: {}\n---\nbody\n", "x".repeat(300)));
    assert!(matches!(
        oversized_title,
        Err(VaultFormatError::InvalidProperty { field: "title" })
    ));
}

#[test]
fn oversized_files_and_sequences_exceed_bounds() {
    let large = parse_document(&format!("body\n{}", "x".repeat(1024 * 1024 + 1)));
    assert!(matches!(large, Err(VaultFormatError::TooLarge)));

    let many_tags = parse_document(&format!(
        "---\ntags: [{}]\n---\nbody\n",
        (0..65)
            .map(|index| format!("t{index}"))
            .collect::<Vec<_>>()
            .join(", ")
    ));
    assert!(matches!(many_tags, Err(VaultFormatError::TooLarge)));
}

#[test]
fn invalid_utf8_is_a_typed_encoding_error() {
    let bytes = [0x23, 0x20, 0x68, 0x69, 0xFF, 0xFE];
    assert!(matches!(
        cortex_vault::parse_document_bytes(&bytes),
        Err(VaultFormatError::InvalidEncoding)
    ));
}
