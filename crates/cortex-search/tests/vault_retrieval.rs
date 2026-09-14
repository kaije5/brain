use std::num::NonZeroUsize;

use cortex_domain::{
    ContentHash, ObservedRevision, ProviderResourceKind, ProviderResourceRef, WorkspaceId,
};
use cortex_search::{
    ChunkProvenance, ChunkReference, DerivedVaultIndex, Embedding, IndexedChunk, VaultHit, hybrid,
    index_document, lexical, semantic,
};

fn workspace() -> WorkspaceId {
    WorkspaceId::new()
}

fn resource(workspace: WorkspaceId, name: &str, kind: ProviderResourceKind) -> ProviderResourceRef {
    ProviderResourceRef::new(
        workspace,
        cortex_domain::ProviderId::new("markdown-vault").expect("valid id"),
        cortex_domain::ProviderResourceId::new(name).expect("valid id"),
        kind,
    )
}

fn provenance() -> ChunkProvenance {
    ChunkProvenance::new(
        ContentHash::new([7; 32]),
        ObservedRevision::new("rev-abc").expect("bounded"),
    )
}

fn note_text() -> &'static str {
    "---\ntitle: Atlas Plan\ntags: [projects, planning]\n---\n\n# Atlas Plan\n\nThe migration needs zebra coordination.\n\n## Risks\n\nSee [[Risks]] for details.\n"
}

fn task_text() -> &'static str {
    "---\ntype: task\nbrain_id: 01926c8f-88f9-7d33-9a1b-2c7d33bd0a12\nstatus: todo\npriority: high\n---\n\nFinish the zebra report.\n"
}

fn unit_embedding(seed: f32, dimensions: usize) -> Embedding {
    // Deterministic unit vector along one axis so cosine ordering is exact.
    let mut values = vec![0.0; dimensions];
    values[0] = 1.0;
    values[dimensions - 1] = seed;
    Embedding::new("test-model", "1", values).expect("valid embedding")
}

#[test]
fn index_document_extracts_metadata_and_chunks() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let resource = resource(workspace, "atlas", ProviderResourceKind::Knowledge);

    let chunks = index_document(&mut index, &resource, note_text(), &provenance())
        .expect("indexing succeeds");
    assert!(chunks >= 1);

    let entry = index.document(&resource).expect("document entry present");
    assert_eq!(entry.title, "Atlas Plan");
    assert!(entry.tags.contains(&"projects".to_owned()));
    assert!(entry.headings.iter().any(|heading| heading == "Atlas Plan"));
    assert!(entry.links.iter().any(|link| link == "Risks"));
    assert_eq!(entry.task_status, None);

    // Every chunk carries the caller-observed provenance.
    for chunk in index.chunks_for_resource(&resource) {
        assert!(chunk.is_fresh(&provenance()));
    }
}

#[test]
fn task_documents_record_task_metadata() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let resource = resource(workspace, "task", ProviderResourceKind::Task);
    index_document(&mut index, &resource, task_text(), &provenance()).expect("indexing succeeds");

    let entry = index.document(&resource).expect("entry present");
    assert_eq!(entry.task_status.as_deref(), Some("todo"));
    assert_eq!(entry.task_priority.as_deref(), Some("high"));
}

#[test]
fn lexical_retrieval_matches_body_title_and_tags() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let atlas = resource(workspace, "atlas", ProviderResourceKind::Knowledge);
    let task = resource(workspace, "task", ProviderResourceKind::Task);
    index_document(&mut index, &atlas, note_text(), &provenance()).expect("indexes");
    index_document(&mut index, &task, task_text(), &provenance()).expect("indexes");

    // Body match.
    let limit = NonZeroUsize::new(10).expect("non-zero");
    let body_hits = lexical(&index, "zebra", limit).expect("lexical works");
    assert_eq!(body_hits.len(), 2, "note and task both mention zebra");

    // Title match.
    let title_hits = lexical(&index, "Atlas Plan", limit).expect("lexical works");
    assert_eq!(title_hits.len(), 1);
    assert_eq!(
        title_hits[0].reference.resource.resource_id().as_str(),
        "atlas"
    );

    // Tag match.
    let tag_hits = lexical(&index, "planning", limit).expect("lexical works");
    assert_eq!(tag_hits.len(), 1);

    // Blank queries are a typed validation error.
    assert!(lexical(&index, "  ", limit).is_err());
}

#[test]
fn lexical_hits_carry_chunk_references_and_bounded_snippets() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let resource = resource(workspace, "atlas", ProviderResourceKind::Knowledge);
    index_document(&mut index, &resource, note_text(), &provenance()).expect("indexes");

    let limit = NonZeroUsize::new(10).expect("non-zero");
    let hits = lexical(&index, "risks", limit).expect("works");
    assert_eq!(hits.len(), 1);
    let hit: &VaultHit = &hits[0];
    assert_eq!(hit.reference.resource.resource_id().as_str(), "atlas");
    assert!(hit.snippet.len() <= 240);
    assert_eq!(hit.lexical_rank, None, "ranks are assigned by fusion only");
    assert!(hit.fused_score.abs() < f64::EPSILON, "pre-fusion score is zero");
}

#[test]
fn semantic_retrieval_ranks_by_cosine_and_skips_unembedded_chunks() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let embedded = resource(workspace, "embedded", ProviderResourceKind::Knowledge);
    index
        .upsert_chunk(
            IndexedChunk::new(
                ChunkReference::new(embedded.clone(), NonZeroUsize::MIN),
                "embedded text".to_owned(),
                provenance(),
            )
            .expect("valid chunk"),
        )
        .expect("upsert");
    // Give the chunk an embedding along axis 0.
    let mut chunk = index
        .chunk(&ChunkReference::new(embedded.clone(), NonZeroUsize::MIN))
        .expect("present")
        .clone();
    chunk.embedding = Some(unit_embedding(0.0, 8));
    index.upsert_chunk(chunk).expect("upsert with embedding");

    // A second chunk without an embedding is invisible to the semantic leg.
    let plain = resource(workspace, "plain", ProviderResourceKind::Knowledge);
    index
        .upsert_chunk(
            IndexedChunk::new(
                ChunkReference::new(plain, NonZeroUsize::MIN),
                "plain text".to_owned(),
                provenance(),
            )
            .expect("valid chunk"),
        )
        .expect("upsert");

    let query = unit_embedding(0.0, 8);
    let hits =
        semantic(&index, &query, NonZeroUsize::new(10).expect("non-zero")).expect("semantic works");
    assert_eq!(hits.len(), 1, "chunks without embeddings are skipped");
    assert_eq!(
        hits[0].reference.resource.resource_id().as_str(),
        "embedded"
    );

    // Incompatible dimensions are a typed error.
    assert!(
        semantic(
            &index,
            &unit_embedding(0.0, 4),
            NonZeroUsize::new(10).expect("non-zero")
        )
        .is_err()
    );
}

#[test]
fn hybrid_fuses_lexical_and_semantic_legs() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let both = resource(workspace, "both", ProviderResourceKind::Knowledge);
    let only_lexical = resource(workspace, "lexical-only", ProviderResourceKind::Knowledge);
    let only_semantic = resource(workspace, "semantic-only", ProviderResourceKind::Knowledge);

    // "both": chunk matches the query lexically and carries a matching embedding.
    let mut chunk = IndexedChunk::new(
        ChunkReference::new(both.clone(), NonZeroUsize::MIN),
        "fusion target".to_owned(),
        provenance(),
    )
    .expect("valid chunk");
    chunk.embedding = Some(unit_embedding(0.0, 8));
    index.upsert_chunk(chunk).expect("upsert");
    // "lexical-only": matches lexically, no embedding.
    index
        .upsert_chunk(
            IndexedChunk::new(
                ChunkReference::new(only_lexical, NonZeroUsize::MIN),
                "fusion source".to_owned(),
                provenance(),
            )
            .expect("valid chunk"),
        )
        .expect("upsert");
    // "semantic-only": embedding along a different axis, no lexical match.
    let mut semantic_chunk = IndexedChunk::new(
        ChunkReference::new(only_semantic.clone(), NonZeroUsize::MIN),
        "unrelated words".to_owned(),
        provenance(),
    )
    .expect("valid chunk");
    semantic_chunk.embedding = Some(unit_embedding(1.0, 8));
    index.upsert_chunk(semantic_chunk).expect("upsert");

    let query_embedding = unit_embedding(0.0, 8);
    let outcome = hybrid(
        &index,
        "fusion",
        Some(&query_embedding),
        NonZeroUsize::new(10).expect("non-zero"),
    )
    .expect("hybrid works");
    assert!(!outcome.semantic_degraded);
    assert_eq!(outcome.hits.len(), 3);
    // The chunk appearing in both legs ranks first.
    assert_eq!(
        outcome.hits[0].reference.resource.resource_id().as_str(),
        "both"
    );
    assert!(outcome.hits[0].lexical_rank.is_some());
    assert!(outcome.hits[0].semantic_rank.is_some());
    assert!(outcome.hits[0].fused_score > outcome.hits[1].fused_score);
}

#[test]
fn hybrid_degrades_explicitly_without_embeddings() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    let resource = resource(workspace, "doc", ProviderResourceKind::Knowledge);
    index_document(&mut index, &resource, note_text(), &provenance()).expect("indexes");

    let outcome = hybrid(
        &index,
        "atlas",
        None,
        NonZeroUsize::new(10).expect("non-zero"),
    )
    .expect("degraded hybrid works");
    assert!(outcome.semantic_degraded);
    assert!(!outcome.hits.is_empty());
    for hit in &outcome.hits {
        assert!(hit.lexical_rank.is_some());
        assert!(hit.semantic_rank.is_none());
    }
}

#[test]
fn limit_bounds_results() {
    let workspace = workspace();
    let mut index = DerivedVaultIndex::new();
    for name in ["a", "b", "c"] {
        let resource = resource(workspace, name, ProviderResourceKind::Knowledge);
        index_document(
            &mut index,
            &resource,
            &format!("---\ntitle: {name}\n---\n\nshared body\n"),
            &provenance(),
        )
        .expect("indexes");
    }
    let hits = lexical(&index, "shared", NonZeroUsize::new(2).expect("non-zero")).expect("works");
    assert_eq!(hits.len(), 2);
}
