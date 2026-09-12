//! Derived index records around provider references and chunks (SCRUM-111).

use std::num::NonZeroUsize;

use cortex_domain::{
    ContentHash, DomainError, ObservedRevision, ProviderId, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, WorkspaceId,
};
use cortex_search::{
    ChunkProvenance, ChunkReference, DerivedVaultIndex, DocumentIndexEntry, IndexedChunk,
    MAX_CHUNK_CHARS, chunk_body,
};

fn workspace() -> WorkspaceId {
    WorkspaceId::new()
}

fn resource(workspace: WorkspaceId, name: &str, kind: ProviderResourceKind) -> ProviderResourceRef {
    ProviderResourceRef::new(
        workspace,
        ProviderId::new("markdown-vault").expect("valid id"),
        ProviderResourceId::new(name).expect("valid id"),
        kind,
    )
}

fn provenance(seed: u8) -> ChunkProvenance {
    ChunkProvenance::new(
        ContentHash::new([seed; 32]),
        ObservedRevision::new(format!("rev-{seed}")).expect("bounded revision"),
    )
}

fn chunk(workspace: WorkspaceId, name: &str, ordinal: usize, text: &str, seed: u8) -> IndexedChunk {
    let reference = ChunkReference::new(
        resource(workspace, name, ProviderResourceKind::Knowledge),
        NonZeroUsize::new(ordinal).expect("non-zero"),
    );
    IndexedChunk::new(reference, text.to_owned(), provenance(seed)).expect("valid chunk")
}

#[test]
fn chunks_are_keyed_by_provider_reference_not_entity_rows() {
    let workspace = workspace();
    let index = DerivedVaultIndex::new();
    let reference = ChunkReference::new(
        resource(workspace, "doc-a", ProviderResourceKind::Knowledge),
        NonZeroUsize::MIN,
    );

    // The reference carries no EntityId: identity is provider + resource id
    // + chunk ordinal, exactly as the redesign requires.
    let _ = index;
    assert_eq!(reference.chunk, NonZeroUsize::MIN);
    assert_eq!(reference.resource.provider_id().as_str(), "markdown-vault");
}

#[test]
fn index_stores_and_retrieves_chunks_with_provenance() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let chunk = chunk(workspace, "doc-a", 1, "first chunk text", 1);

    assert!(index.upsert_chunk(chunk.clone()).expect("upsert"));
    let stored = index.chunk(chunk.reference()).expect("chunk present");
    assert_eq!(stored.text, "first chunk text");
    assert!(stored.is_fresh(&provenance(1)));
    assert!(!stored.is_fresh(&provenance(2)));
    assert_eq!(index.chunk_count(), 1);
}

#[test]
fn hash_gated_upsert_skips_unchanged_chunks() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let stable = chunk(workspace, "doc-a", 1, "stable text", 1);

    assert!(index.upsert_chunk(stable.clone()).expect("first insert"));
    // Same text + same provenance: the upsert is a no-op.
    assert!(!index.upsert_chunk(stable).expect("no-op upsert"));

    // Changed content (new provenance) does update.
    let changed = chunk(workspace, "doc-a", 1, "changed text", 2);
    assert!(index.upsert_chunk(changed).expect("changed insert"));
    let stored = index
        .chunk(&ChunkReference::new(
            resource(workspace, "doc-a", ProviderResourceKind::Knowledge),
            NonZeroUsize::MIN,
        ))
        .expect("chunk present");
    assert_eq!(stored.text, "changed text");
    assert!(stored.is_fresh(&provenance(2)));
}

#[test]
fn remove_resource_drops_all_chunks_and_the_document_entry() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    for ordinal in 1..=3 {
        index
            .upsert_chunk(chunk(workspace, "doc-a", ordinal, "text", 1))
            .expect("upsert");
    }
    index
        .upsert_chunk(chunk(workspace, "doc-b", 1, "other", 1))
        .expect("upsert other");
    index
        .upsert_document(DocumentIndexEntry {
            resource: resource(workspace, "doc-a", ProviderResourceKind::Knowledge),
            title: "Doc A".to_owned(),
            tags: vec![],
            headings: vec![],
            links: vec![],
            task_status: None,
            task_priority: None,
        })
        .expect("document upsert");

    let removed = index.remove_resource(&resource(
        workspace,
        "doc-a",
        ProviderResourceKind::Knowledge,
    ));
    assert_eq!(removed, 3);
    assert_eq!(index.chunk_count(), 1);
    assert!(
        index
            .document(&resource(
                workspace,
                "doc-a",
                ProviderResourceKind::Knowledge
            ))
            .is_none()
    );
    assert_eq!(index.document_count(), 0);
}

#[test]
fn chunker_splits_on_paragraph_boundaries_deterministically() {
    let body = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
    let chunks = chunk_body(body);
    assert_eq!(chunks.len(), 1, "small paragraphs coalesce");
    assert!(chunks[0].1.contains("First paragraph."));
    assert!(chunks[0].1.contains("Third paragraph."));

    // Deterministic: same input → same output.
    assert_eq!(chunk_body(body), chunks);
}

#[test]
fn oversized_paragraphs_become_their_own_bounded_chunks() {
    let long = "x".repeat(MAX_CHUNK_CHARS + 500);
    let body = format!("intro\n\n{long}\n\ntail");
    let chunks = chunk_body(&body);
    assert!(chunks.len() >= 3);
    assert_eq!(chunks[0].1, "intro");
    assert!(chunks[1].1.chars().count() <= MAX_CHUNK_CHARS);
    assert_eq!(chunks.last().expect("tail").1, "tail");
}

#[test]
fn chunk_text_bounds_are_enforced() {
    let workspace = workspace();
    let oversized = "x".repeat(MAX_CHUNK_CHARS * 4 + 1);
    let result = IndexedChunk::new(
        ChunkReference::new(
            resource(workspace, "doc-a", ProviderResourceKind::Knowledge),
            NonZeroUsize::MIN,
        ),
        oversized,
        provenance(1),
    );
    assert!(matches!(
        result,
        Err(DomainError::Validation {
            field: "chunk_text",
            ..
        })
    ));
}

#[test]
fn document_entries_record_metadata_keyed_by_resource() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let resource = resource(workspace, "task-1", ProviderResourceKind::Task);
    index
        .upsert_document(DocumentIndexEntry {
            resource: resource.clone(),
            title: "Report".to_owned(),
            tags: vec!["school".to_owned()],
            headings: vec!["Notes".to_owned()],
            links: vec!["atlas".to_owned()],
            task_status: Some("todo".to_owned()),
            task_priority: Some("high".to_owned()),
        })
        .expect("upsert");

    let entry = index.document(&resource).expect("entry present");
    assert_eq!(entry.title, "Report");
    assert_eq!(entry.task_status.as_deref(), Some("todo"));
    assert_eq!(entry.task_priority.as_deref(), Some("high"));
    assert_eq!(index.documents().count(), 1);
}
