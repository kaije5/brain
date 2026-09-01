#![forbid(unsafe_code)]

mod embedding;
mod fts;
mod rank;
mod service;
mod vector;

pub use embedding::{Embedding, EmbeddingProvider};
pub use fts::{EntityKind, IndexedVector, SearchCandidate, SearchIndex};
pub use rank::{RankedEntity, reciprocal_rank_fusion};
pub use service::{HybridSearchService, SearchHit, SearchRequest};
pub use vector::{SemanticCandidate, VectorRecord, cosine_candidates};
