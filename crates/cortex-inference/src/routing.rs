use std::{collections::BTreeMap, time::Duration};

use chrono::{DateTime, Utc};
use cortex_application::{ApplicationError, SecretRef};

const MAX_ID_BYTES: usize = 256;

fn validated_id(value: &str, field: &'static str) -> Result<(), ApplicationError> {
    if value.trim().is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control)
    {
        return Err(ApplicationError::Validation { field });
    }
    Ok(())
}

/// Stable identifier for one durable provider profile.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProviderProfileId(String);

impl ProviderProfileId {
    /// # Errors
    /// Returns a validation error for blank, oversized, or control-character identifiers.
    pub fn new(value: impl Into<String>) -> Result<Self, ApplicationError> {
        let value = value.into();
        validated_id(&value, "provider_profile_id")?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Provider-neutral validated model identifier discovered from a provider.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ModelId(String);

impl ModelId {
    /// # Errors
    /// Returns a validation error for blank, oversized, or control-character identifiers.
    pub fn new(value: impl Into<String>) -> Result<Self, ApplicationError> {
        let value = value.into();
        validated_id(&value, "model_id")?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Normalized capability a discovered model may demonstrate through probing.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ModelCapability {
    ToolCalling,
    StructuredOutput,
}

/// Durable, secret-free description of one configured provider endpoint.
#[derive(Clone, Debug)]
pub struct ProviderProfile {
    id: ProviderProfileId,
    enabled: bool,
    /// Opaque credential locator; resolved only by the platform secret store.
    secret_reference: Option<SecretRef>,
}

impl ProviderProfile {
    /// # Errors
    /// Returns a validation error when the embedded profile id is invalid.
    pub fn new(
        id: ProviderProfileId,
        enabled: bool,
    ) -> Result<Self, ApplicationError> {
        Ok(Self {
            id,
            enabled,
            secret_reference: None,
        })
    }

    #[must_use]
    pub fn with_secret_reference(mut self, reference: Option<SecretRef>) -> Self {
        self.secret_reference = reference;
        self
    }

    #[must_use]
    pub const fn id(&self) -> &ProviderProfileId {
        &self.id
    }

    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    #[must_use]
    pub const fn secret_reference(&self) -> Option<&SecretRef> {
        self.secret_reference.as_ref()
    }
}

/// One discovered model with timestamped capability evidence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredModel {
    model_id: ModelId,
    evidence: BTreeMap<ModelCapability, DateTime<Utc>>,
}

impl DiscoveredModel {
    #[must_use]
    pub fn new(model_id: ModelId) -> Self {
        Self {
            model_id,
            evidence: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_evidence(mut self, capability: ModelCapability, observed_at: DateTime<Utc>) -> Self {
        let observed = self
            .evidence
            .get(&capability)
            .copied()
            .map_or(observed_at, |existing| existing.max(observed_at));
        self.evidence.insert(capability, observed);
        self
    }

    #[must_use]
    pub const fn model_id(&self) -> &ModelId {
        &self.model_id
    }

    /// Whether the model carried evidence for `capability` within `max_age` of `now`.
    #[must_use]
    pub fn has_fresh_evidence(
        &self,
        capability: ModelCapability,
        now: DateTime<Utc>,
        max_age: Duration,
    ) -> bool {
        self.evidence.get(&capability).is_some_and(|observed_at| {
            let age = now.signed_duration_since(*observed_at);
            age >= chrono::Duration::zero() && age <= chrono::Duration::from_std(max_age).unwrap_or(
                chrono::Duration::hours(24 * 365 * 100),
            )
        })
    }
}

/// Role-scoped routing policy: required capabilities, preference order, and
/// the maximum age of accepted capability evidence.
#[derive(Clone, Debug)]
pub struct RoleRoutingPolicy {
    required: std::collections::BTreeSet<ModelCapability>,
    preference: Vec<ModelId>,
    max_evidence_age: Duration,
}

impl RoleRoutingPolicy {
    /// # Errors
    /// Returns a validation error when the evidence window is zero.
    pub fn new(
        required: std::collections::BTreeSet<ModelCapability>,
        preference: Vec<ModelId>,
        max_evidence_age: Duration,
    ) -> Result<Self, ApplicationError> {
        if max_evidence_age.is_zero() {
            return Err(ApplicationError::Validation {
                field: "max_evidence_age",
            });
        }
        Ok(Self {
            required,
            preference,
            max_evidence_age,
        })
    }

    /// Sprint 1 agent policy per ADR-023: tool calling plus structured output.
    ///
    /// # Errors
    /// Returns a validation error when the evidence window is zero.
    pub fn agent_default(max_evidence_age: Duration) -> Result<Self, ApplicationError> {
        Self::new(
            std::collections::BTreeSet::from([
                ModelCapability::ToolCalling,
                ModelCapability::StructuredOutput,
            ]),
            Vec::new(),
            max_evidence_age,
        )
    }

    #[must_use]
    pub fn required(&self) -> &std::collections::BTreeSet<ModelCapability> {
        &self.required
    }
}

/// Refreshable capability catalog: normalized discovery results retained with
/// evidence timestamps. Stale evidence is never eligibility.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModelCatalog {
    models: Vec<DiscoveredModel>,
}

impl ModelCatalog {
    #[must_use]
    pub fn new(models: Vec<DiscoveredModel>) -> Self {
        Self { models }
    }

    /// Replaces the catalog with fresh discovery results; prior entries never
    /// survive a refresh because availability may have changed.
    pub fn refresh(&mut self, models: Vec<DiscoveredModel>) {
        self.models = models;
    }

    #[must_use]
    pub fn models(&self) -> &[DiscoveredModel] {
        &self.models
    }

    fn eligible<'a>(
        &'a self,
        policy: &'a RoleRoutingPolicy,
        now: DateTime<Utc>,
    ) -> impl Iterator<Item = &'a DiscoveredModel> + use<'a> {
        self.models.iter().filter(move |model| {
            policy
                .required
                .iter()
                .all(|capability| model.has_fresh_evidence(*capability, now, policy.max_evidence_age))
        })
    }
}

/// The deterministic result of routing one role to a concrete model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RoutedModel {
    pub profile_id: ProviderProfileId,
    pub model_id: ModelId,
}

/// Capability-aware, deterministic runtime model router (ADR-021).
///
/// Selection is a pure function of enabled profiles, the capability catalog,
/// the role policy, and the evaluation instant. There is no availability
/// probing and no silent fallback: no eligible model yields
/// [`ApplicationError::NoSuitableModel`].
pub struct ModelRouter;

impl ModelRouter {
    /// Selects the model for a role from enabled profiles and catalog evidence.
    ///
    /// # Errors
    /// Returns [`ApplicationError::NoSuitableModel`] as the explicit degraded
    /// state when no enabled provider exposes an eligible model.
    #[must_use = "routing has no side effects; the selection result must be used"]
    pub fn select(
        policy: &RoleRoutingPolicy,
        profiles: &[ProviderProfile],
        catalog: &ModelCatalog,
        now: DateTime<Utc>,
    ) -> Result<RoutedModel, ApplicationError> {
        let mut enabled: Vec<&ProviderProfile> =
            profiles.iter().filter(|profile| profile.enabled).collect();
        enabled.sort_by(|left, right| left.id().as_str().cmp(right.id().as_str()));

        let mut candidates: Vec<(&ProviderProfile, &DiscoveredModel)> = enabled
            .iter()
            .flat_map(|profile| {
                catalog
                    .eligible(policy, now)
                    .map(move |model| (*profile, model))
            })
            .collect();

        candidates.sort_by(|(profile_left, model_left), (profile_right, model_right)| {
            let preference =
                |model: &DiscoveredModel| policy.preference.iter().position(|id| id == model.model_id());
            let rank = |model: &DiscoveredModel| {
                preference(model).unwrap_or(usize::MAX)
            };
            rank(model_left)
                .cmp(&rank(model_right))
                .then_with(|| model_left.model_id().as_str().cmp(model_right.model_id().as_str()))
                .then_with(|| {
                    profile_left
                        .id()
                        .as_str()
                        .cmp(profile_right.id().as_str())
                })
        });

        candidates
            .into_iter()
            .next()
            .map(|(profile, model)| RoutedModel {
                profile_id: profile.id().clone(),
                model_id: model.model_id().clone(),
            })
            .ok_or(ApplicationError::NoSuitableModel)
    }
}
