use std::num::NonZeroUsize;

use cortex_application::ApplicationError;
use cortex_domain::EntityId;

use crate::Embedding;

#[derive(Clone, Debug, PartialEq)]
pub struct VectorRecord {
    pub entity_id: EntityId,
    pub embedding: Embedding,
}

impl VectorRecord {
    #[must_use]
    pub const fn new(entity_id: EntityId, embedding: Embedding) -> Self {
        Self {
            entity_id,
            embedding,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SemanticCandidate {
    pub entity_id: EntityId,
    pub cosine_similarity: f64,
}

/// Computes deterministic cosine-ranked candidates after scanning at most the
/// configured number of stored vectors.
///
/// # Errors
/// Returns a validation error when a stored vector was produced by a different
/// model or has a different dimension than the query vector.
pub fn cosine_candidates(
    query: &Embedding,
    records: Vec<VectorRecord>,
    max_records: NonZeroUsize,
) -> Result<Vec<SemanticCandidate>, ApplicationError> {
    let query_norm = norm(query.values())?;
    let mut candidates = records
        .into_iter()
        .take(max_records.get())
        .map(|record| {
            validate_compatible(query, &record.embedding)?;
            let record_norm = norm(record.embedding.values())?;
            let dot = query
                .values()
                .iter()
                .zip(record.embedding.values())
                .map(|(left, right)| f64::from(*left) * f64::from(*right))
                .sum::<f64>();
            Ok(SemanticCandidate {
                entity_id: record.entity_id,
                cosine_similarity: dot / (query_norm * record_norm),
            })
        })
        .collect::<Result<Vec<_>, ApplicationError>>()?;
    candidates.sort_by(|left, right| {
        right
            .cosine_similarity
            .total_cmp(&left.cosine_similarity)
            .then_with(|| left.entity_id.cmp(&right.entity_id))
    });
    Ok(candidates)
}

fn validate_compatible(left: &Embedding, right: &Embedding) -> Result<(), ApplicationError> {
    if left.model_id() != right.model_id() || left.model_version() != right.model_version() {
        return Err(ApplicationError::Validation {
            field: "embedding_model",
        });
    }
    if left.dimensions() != right.dimensions() {
        return Err(ApplicationError::Validation {
            field: "embedding_dimensions",
        });
    }
    Ok(())
}

fn norm(values: &[f32]) -> Result<f64, ApplicationError> {
    let squared = values
        .iter()
        .map(|value| {
            let value = f64::from(*value);
            value * value
        })
        .sum::<f64>();
    if squared == 0.0 {
        return Err(ApplicationError::Validation {
            field: "embedding_vector",
        });
    }
    Ok(squared.sqrt())
}
