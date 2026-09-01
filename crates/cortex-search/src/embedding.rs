use cortex_application::ApplicationError;

#[derive(Clone, Debug, PartialEq)]
pub struct Embedding {
    model_id: String,
    model_version: String,
    values: Vec<f32>,
}

#[allow(async_fn_in_trait)]
pub trait EmbeddingProvider: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Embedding, ApplicationError>;
}

impl Embedding {
    /// Builds a finite, non-empty embedding tagged with its producing model.
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
        if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
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

    /// Decodes a fixed-length little-endian `f32` vector.
    ///
    /// # Errors
    /// Returns a validation error when the declared dimension does not exactly
    /// match the blob length or decoded values are invalid.
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
