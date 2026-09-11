use sqlx::SqlitePool;

use cortex_application::ApplicationError;

/// Capability categories that may be probed for a discovered model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoredCapability {
    ToolCalling,
    StructuredOutput,
}

impl StoredCapability {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ToolCalling => "tool_calling",
            Self::StructuredOutput => "structured_output",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "tool_calling" => Some(Self::ToolCalling),
            "structured_output" => Some(Self::StructuredOutput),
            _ => None,
        }
    }
}

/// One durable provider profile row. The secret reference is an opaque
/// locator; credential values never reach storage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredProviderProfile {
    pub id: String,
    pub enabled: bool,
    pub secret_reference: Option<String>,
}

/// One recorded capability probe result for a discovered model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredCapabilityEvidence {
    pub model_id: String,
    pub capability: StoredCapability,
    /// RFC 3339 timestamp of the probe that produced this evidence.
    pub observed_at: String,
}

fn storage_error(context: &'static str) -> ApplicationError {
    ApplicationError::Storage(context.to_owned())
}

/// Durable store for provider/model profiles and the refreshable capability
/// catalog backing the runtime model router (ADR-021).
pub struct SqliteModelRoutingStore {
    pool: SqlitePool,
}

impl SqliteModelRoutingStore {
    #[must_use]
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Inserts or updates a provider profile without duplicating rows.
    ///
    /// # Errors
    /// Returns a validation error for invalid identifiers and a storage error
    /// when the write fails.
    pub async fn upsert_profile(
        &self,
        profile: &StoredProviderProfile,
    ) -> Result<(), ApplicationError> {
        if profile.id.trim().is_empty() || profile.id.len() > 256 {
            return Err(ApplicationError::Validation {
                field: "provider_profile_id",
            });
        }
        sqlx::query(
            "INSERT INTO model_provider_profile (id, enabled, secret_ref) VALUES (?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled, secret_ref = excluded.secret_ref",
        )
        .bind(&profile.id)
        .bind(profile.enabled)
        .bind(&profile.secret_reference)
        .execute(&self.pool)
        .await
        .map_err(|_| storage_error("provider profile write failed"))?;
        Ok(())
    }

    /// Lists all provider profiles ordered by identifier.
    ///
    /// # Errors
    /// Returns a storage error when the read fails.
    pub async fn list_profiles(&self) -> Result<Vec<StoredProviderProfile>, ApplicationError> {
        let rows = sqlx::query_as::<_, (String, bool, Option<String>)>(
            "SELECT id, enabled, secret_ref FROM model_provider_profile ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|_| storage_error("provider profile read failed"))?;
        Ok(rows
            .into_iter()
            .map(|(id, enabled, secret_reference)| StoredProviderProfile {
                id,
                enabled,
                secret_reference,
            })
            .collect())
    }

    /// Atomically replaces the capability catalog of one profile.
    ///
    /// # Errors
    /// Returns a storage error when the transaction fails.
    pub async fn replace_catalog(
        &self,
        profile_id: &str,
        evidence: Vec<StoredCapabilityEvidence>,
    ) -> Result<(), ApplicationError> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|_| storage_error("catalog write failed"))?;
        sqlx::query("DELETE FROM model_capability_evidence WHERE profile_id = ?")
            .bind(profile_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| storage_error("catalog write failed"))?;
        for entry in evidence {
            sqlx::query(
                "INSERT INTO model_capability_evidence (profile_id, model_id, capability, observed_at)
                 VALUES (?, ?, ?, ?)",
            )
            .bind(profile_id)
            .bind(&entry.model_id)
            .bind(entry.capability.as_str())
            .bind(&entry.observed_at)
            .execute(&mut *transaction)
            .await
            .map_err(|_| storage_error("catalog write failed"))?;
        }
        transaction
            .commit()
            .await
            .map_err(|_| storage_error("catalog write failed"))?;
        Ok(())
    }

    /// Loads the capability catalog of one profile ordered by model then capability.
    ///
    /// # Errors
    /// Returns a storage error when the read fails.
    pub async fn load_catalog(
        &self,
        profile_id: &str,
    ) -> Result<Vec<StoredCapabilityEvidence>, ApplicationError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT model_id, capability, observed_at FROM model_capability_evidence
             WHERE profile_id = ? ORDER BY model_id, capability",
        )
        .bind(profile_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|_| storage_error("catalog read failed"))?;
        let mut evidence = Vec::with_capacity(rows.len());
        for (model_id, capability, observed_at) in rows {
            let capability = StoredCapability::from_str(&capability)
                .ok_or(storage_error("catalog row held an unknown capability"))?;
            evidence.push(StoredCapabilityEvidence {
                model_id,
                capability,
                observed_at,
            });
        }
        Ok(evidence)
    }

    /// Records the latest routing decision as a typed `{profile_id, model_id}`
    /// selection. Only identifiers and a timestamp are stored; provider
    /// secrets and profile definitions never reach storage.
    ///
    /// # Errors
    /// Returns a validation error for invalid identifiers and a storage error
    /// when the write fails.
    pub async fn record_route(
        &self,
        profile_id: &str,
        model_id: &str,
        routed_at: &str,
    ) -> Result<(), ApplicationError> {
        if profile_id.trim().is_empty() || profile_id.len() > 256 {
            return Err(ApplicationError::Validation {
                field: "provider_profile_id",
            });
        }
        if model_id.trim().is_empty() || model_id.len() > 256 {
            return Err(ApplicationError::Validation { field: "model_id" });
        }
        sqlx::query(
            "INSERT INTO model_route_decision (id, profile_id, model_id, routed_at)
             VALUES (1, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                profile_id = excluded.profile_id,
                model_id = excluded.model_id,
                routed_at = excluded.routed_at",
        )
        .bind(profile_id)
        .bind(model_id)
        .bind(routed_at)
        .execute(&self.pool)
        .await
        .map_err(|_| storage_error("route decision write failed"))?;
        Ok(())
    }

    /// Loads the latest recorded routing decision, when one exists.
    ///
    /// # Errors
    /// Returns a storage error when the read fails.
    pub async fn load_route(&self) -> Result<Option<StoredRouteDecision>, ApplicationError> {
        let row = sqlx::query_as::<_, (String, String, String)>(
            "SELECT profile_id, model_id, routed_at FROM model_route_decision WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| storage_error("route decision read failed"))?;
        Ok(
            row.map(|(profile_id, model_id, routed_at)| StoredRouteDecision {
                profile_id,
                model_id,
                routed_at,
            }),
        )
    }
}

/// The latest durable routing decision: which profile served which model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredRouteDecision {
    pub profile_id: String,
    pub model_id: String,
    /// RFC 3339 timestamp of the resolution that produced this decision.
    pub routed_at: String,
}
