#![forbid(unsafe_code)]

mod embedding;
mod rank;
mod service;
mod vector;

pub use cortex_application::{EntityKind, IndexedVector, SearchCandidate, SearchIndex};
pub use embedding::{Embedding, EmbeddingProvider};
pub use rank::{RankedEntity, reciprocal_rank_fusion};
pub use service::{HybridSearchService, SearchHit, SearchRequest};
pub use vector::{SemanticCandidate, VectorRecord, cosine_candidates};
