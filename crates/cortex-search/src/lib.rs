#![forbid(unsafe_code)]

mod embedding;
mod fused;
mod index;
mod rank;
mod service;
mod vault_retrieval;
mod vector;

pub use cortex_application::{EntityKind, IndexedVector, SearchCandidate, SearchIndex};
pub use embedding::{Embedding, EmbeddingProvider};
pub use fused::{FusedHit, FusedLeg, fuse_with_memories};
pub use index::{
    ChunkProvenance, ChunkReference, DerivedVaultIndex, DocumentIndexEntry, IndexedChunk,
    MAX_CHUNK_CHARS, MAX_CHUNKS_PER_DOCUMENT, chunk_body,
};
pub use rank::{RankedEntity, reciprocal_rank_fusion};
pub use service::{HybridSearchService, SearchHit, SearchRequest};
pub use vault_retrieval::{RetrievalOutcome, VaultHit, hybrid, index_document, lexical, semantic};
pub use vector::{SemanticCandidate, VectorRecord, cosine_candidates};
