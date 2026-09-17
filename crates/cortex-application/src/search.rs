use std::num::NonZeroUsize;

use cortex_domain::{EntityId, PrincipalId, SourceRef, WorkspaceId};

use crate::ApplicationError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityKind {
    Memory,
    Source,
}

impl EntityKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Source => "source",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "memory" => Some(Self::Memory),
            "source" => Some(Self::Source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SearchCandidate {
    pub entity_id: EntityId,
    pub kind: EntityKind,
    pub snippet: String,
    pub sources: Vec<SourceRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IndexedVector {
    pub candidate: SearchCandidate,
    pub embedding: Embedding,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    model_id: String,
    model_version: String,
    values: Vec<f32>,
}

impl Embedding {
    /// Builds a finite, non-empty, non-zero embedding tagged with its producing model.
    ///
    /// # Errors
    /// Returns a validation error for blank model metadata or an invalid vector.
    pub fn new(
        model_id: impl Into<String>,
        model_version: impl Into<String>,
        values: Vec<f32>,
    ) -> Result<Self, ApplicationError> {
        let model_id = model_id.into();
        let model_version = model_version.into();
        if model_id.trim().is_empty() || model_version.trim().is_empty() {
            return Err(ApplicationError::Validation {
                field: "embedding_model",
            });
        }
        if values.is_empty()
            || values.iter().any(|value| !value.is_finite())
            || values.iter().all(|value| *value == 0.0)
        {
            return Err(ApplicationError::Validation {
                field: "embedding_vector",
            });
        }
        Ok(Self {
            model_id,
            model_version,
            values,
        })
    }

    /// Decodes an exact fixed-length little-endian `f32` vector.
    ///
    /// # Errors
    /// Returns a validation error when dimensions do not exactly match the
    /// payload or decoded values violate embedding invariants.
    pub fn from_le_bytes(
        model_id: impl Into<String>,
        model_version: impl Into<String>,
        dimensions: usize,
        bytes: &[u8],
    ) -> Result<Self, ApplicationError> {
        let expected_len =
            dimensions
                .checked_mul(size_of::<f32>())
                .ok_or(ApplicationError::Validation {
                    field: "embedding_dimensions",
                })?;
        if dimensions == 0 || bytes.len() != expected_len {
            return Err(ApplicationError::Validation {
                field: "embedding_dimensions",
            });
        }
        let values = bytes
            .chunks_exact(size_of::<f32>())
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect();
        Self::new(model_id, model_version, values)
    }

    #[must_use]
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    #[must_use]
    pub fn model_version(&self) -> &str {
        &self.model_version
    }

    #[must_use]
    pub fn dimensions(&self) -> usize {
        self.values.len()
    }

    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    #[must_use]
    pub fn to_le_bytes(&self) -> Vec<u8> {
        self.values
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect()
    }
}

#[allow(async_fn_in_trait)]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Embedding, ApplicationError>;
}

#[allow(async_fn_in_trait)]
pub trait SearchIndex: Send + Sync {
    async fn is_authorized(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
    ) -> Result<bool, ApplicationError>;

    async fn lexical_candidates(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &str,
        limit: NonZeroUsize,
    ) -> Result<Vec<SearchCandidate>, ApplicationError>;

    async fn semantic_records(
        &self,
        workspace_id: WorkspaceId,
        principal_id: PrincipalId,
        query: &Embedding,
        max_records: NonZeroUsize,
    ) -> Result<Vec<IndexedVector>, ApplicationError>;
}
