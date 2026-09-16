//! SCRUM-130: retrieval fuses provenance-bearing vault chunks with
//! Cortex-owned AI memories into one deterministic result list.

use std::num::NonZeroUsize;

use cortex_domain::{
    ContentHash, EntityId, ObservedRevision, ProviderId, ProviderProvenance, ProviderResourceId,
    ProviderResourceKind, ProviderResourceRef, WorkspaceId,
};
use cortex_search::{
    ChunkProvenance, ChunkReference, DerivedVaultIndex, FusedLeg, IndexedChunk, SearchHit,
    fuse_with_memories, index_document,
};

fn task_resource(workspace_id: WorkspaceId, resource_id: &str) -> ProviderResourceRef {
    ProviderResourceRef::new(
        workspace_id,
        ProviderId::new("markdown-vault").expect("provider id"),
        ProviderResourceId::new(resource_id).expect("resource id"),
        ProviderResourceKind::Knowledge,
    )
}

fn index_note(
    index: &mut DerivedVaultIndex,
    workspace_id: WorkspaceId,
    resource_id: &str,
    text: &str,
) {
    let resource = task_resource(workspace_id, resource_id);
    let provenance = ChunkProvenance::new(
        ContentHash::new([1; 32]),
        ObservedRevision::new("rev-1").expect("revision"),
    );
    index_document(index, &resource, text, &provenance).expect("index document");
}

fn memory_hit(snippet: &str, fused_score: f64) -> SearchHit {
    SearchHit {
        entity_id: EntityId::new(),
        kind: cortex_search::EntityKind::Memory,
        snippet: snippet.to_owned(),
        lexical_rank: None,
        semantic_rank: None,
        fused_score,
        sources: Vec::new(),
        semantic_degraded: false,
    }
}

#[test]
fn fuses_vault_chunks_and_memories_deterministically() {
    let workspace_id = WorkspaceId::new();
    let mut index = DerivedVaultIndex::new();
    index_note(
        &mut index,
        workspace_id,
        "notes/quarterly.md",
        "# Quarterly\n\nThe launch retrospective is documented here.\n",
    );
    let outcome =
        cortex_search::hybrid(&index, "retrospective", None, NonZeroUsize::new(5).unwrap())
            .expect("vault retrieval");
    assert!(outcome.semantic_degraded, "no embeddings in this fixture");

    let memories = vec![memory_hit("retrospective decisions", 0.9)];
    let fused = fuse_with_memories(outcome, memories, NonZeroUsize::new(10).unwrap());

    assert_eq!(fused.len(), 2, "both legs are represented");
    assert!(
        fused[0].fused_score >= fused[1].fused_score,
        "results are ordered by fused score"
    );
    let has_vault = fused
        .iter()
        .any(|hit| matches!(&hit.leg, FusedLeg::VaultChunk(chunk) if !chunk.snippet.is_empty()));
    let has_memory = fused
        .iter()
        .any(|hit| matches!(&hit.leg, FusedLeg::Memory(memory) if memory.kind == cortex_search::EntityKind::Memory));
    assert!(
        has_vault,
        "vault chunk hit carries its snippet/provenance ref"
    );
    assert!(has_memory, "Cortex-owned memory hit is present");
}

#[test]
fn vault_chunk_ties_break_before_memories_and_order_is_stable() {
    let workspace_id = WorkspaceId::new();
    let mut index = DerivedVaultIndex::new();
    index_note(&mut index, workspace_id, "notes/a.md", "alpha note body\n");
    let outcome = cortex_search::hybrid(&index, "alpha", None, NonZeroUsize::new(5).unwrap())
        .expect("vault retrieval");
    let memories = vec![
        memory_hit("alpha memory one", 1.0),
        memory_hit("alpha memory two", 1.0),
    ];
    let fused = fuse_with_memories(outcome, memories.clone(), NonZeroUsize::new(10).unwrap());
    // Equal scores: vault chunk (key prefix 0) sorts before memories, and
    // memory ties break by entity id — stable across repeated calls with
    // identical inputs.
    let first = fused.clone();
    let second = fuse_with_memories(
        cortex_search::hybrid(&index, "alpha", None, NonZeroUsize::new(5).unwrap())
            .expect("vault retrieval"),
        memories,
        NonZeroUsize::new(10).unwrap(),
    );
    assert_eq!(first, second);
    assert!(matches!(fused[0].leg, FusedLeg::VaultChunk(_)));
}

#[test]
fn provenance_survives_fusion() {
    let workspace_id = WorkspaceId::new();
    let mut index = DerivedVaultIndex::new();
    let resource = task_resource(workspace_id, "notes/prov.md");
    let provenance = ChunkProvenance::new(
        ContentHash::new([9; 32]),
        ObservedRevision::new("rev-prov").expect("revision"),
    );
    index_document(
        &mut index,
        &resource,
        "provenance probe body\n",
        &provenance,
    )
    .expect("index document");
    let outcome = cortex_search::hybrid(&index, "provenance", None, NonZeroUsize::new(5).unwrap())
        .expect("vault retrieval");

    let fused = fuse_with_memories(outcome, Vec::new(), NonZeroUsize::new(10).unwrap());
    let FusedLeg::VaultChunk(hit) = &fused[0].leg else {
        panic!("expected vault chunk");
    };
    let indexed = index.chunk(&hit.reference).expect("indexed chunk");
    assert_eq!(indexed.provenance.content_hash.as_bytes(), &[9; 32]);
    assert_eq!(indexed.provenance.observed_revision.as_str(), "rev-prov");
    // Chunk reference resolves back to the originating resource.
    assert_eq!(&hit.reference.resource, &resource);
    assert!(hit.reference.chunk.get() >= 1);
    let _ = ChunkReference::new(resource.clone(), hit.reference.chunk);
}
