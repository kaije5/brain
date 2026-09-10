use std::{collections::BTreeSet, time::Duration};

use chrono::{DateTime, Utc};
use cortex_application::ApplicationError;
use cortex_inference::{
    DiscoveredModel, ModelCapability, ModelCatalog, ModelId, ModelRouter, ProviderProfile,
    ProviderProfileId, RoleRoutingPolicy,
};

fn profile(id: &str, enabled: bool) -> ProviderProfile {
    ProviderProfile::new(
        ProviderProfileId::new(id).expect("valid profile id"),
        enabled,
    )
    .expect("valid profile")
}

fn evidence_now() -> DateTime<Utc> {
    Utc::now()
}

fn discovered(
    id: &str,
    observed_at: DateTime<Utc>,
    capabilities: &[ModelCapability],
) -> DiscoveredModel {
    let mut model = DiscoveredModel::new(ModelId::new(id).expect("valid model id"));
    for capability in capabilities {
        model = model.with_evidence(*capability, observed_at);
    }
    model
}

fn agent_policy() -> RoleRoutingPolicy {
    RoleRoutingPolicy::agent_default(Duration::from_hours(24)).expect("valid default policy")
}

#[test]
fn model_ids_reject_blank_oversized_or_control_input() {
    assert!(ModelId::new("meta/llama-3.1-70b-instruct").is_ok());
    assert!(ModelId::new("").is_err());
    assert!(ModelId::new("   ").is_err());
    assert!(ModelId::new("bad\nid").is_err());
    assert!(ModelId::new("x".repeat(300)).is_err());
}

#[test]
fn provider_profile_ids_reject_blank_oversized_or_control_input() {
    assert!(ProviderProfileId::new("nim-dev").is_ok());
    assert!(ProviderProfileId::new("").is_err());
    assert!(ProviderProfileId::new("bad\u{0}id").is_err());
    assert!(ProviderProfileId::new("x".repeat(300)).is_err());
}

#[test]
fn router_selects_only_models_with_fresh_required_capability_evidence() {
    let now = evidence_now();
    let fresh = now - chrono::Duration::hours(1);
    let catalog = ModelCatalog::new(vec![discovered(
        "meta/llama-3.1-70b-instruct",
        fresh,
        &[
            ModelCapability::ToolCalling,
            ModelCapability::StructuredOutput,
        ],
    )]);

    let selection =
        ModelRouter::select(&agent_policy(), &[profile("nim-dev", true)], &catalog, now)
            .expect("eligible model exists");

    assert_eq!(selection.model_id.as_str(), "meta/llama-3.1-70b-instruct");
}

#[test]
fn router_skips_models_missing_required_capabilities() {
    let now = evidence_now();
    let fresh = now - chrono::Duration::hours(1);
    let catalog = ModelCatalog::new(vec![discovered(
        "text-only-model",
        fresh,
        &[ModelCapability::ToolCalling],
    )]);

    let result = ModelRouter::select(&agent_policy(), &[profile("nim-dev", true)], &catalog, now);

    assert_eq!(result, Err(ApplicationError::NoSuitableModel));
}

#[test]
fn router_treats_stale_evidence_as_ineligible() {
    let now = evidence_now();
    let stale = now - chrono::Duration::hours(25);
    let catalog = ModelCatalog::new(vec![discovered(
        "meta/llama-3.1-70b-instruct",
        stale,
        &[
            ModelCapability::ToolCalling,
            ModelCapability::StructuredOutput,
        ],
    )]);

    let result = ModelRouter::select(&agent_policy(), &[profile("nim-dev", true)], &catalog, now);

    assert_eq!(result, Err(ApplicationError::NoSuitableModel));
}

#[test]
fn router_skips_disabled_profiles_without_falling_back() {
    let now = evidence_now();
    let fresh = now - chrono::Duration::hours(1);
    let catalog = ModelCatalog::new(vec![discovered(
        "meta/llama-3.1-70b-instruct",
        fresh,
        &[
            ModelCapability::ToolCalling,
            ModelCapability::StructuredOutput,
        ],
    )]);

    let result = ModelRouter::select(&agent_policy(), &[profile("nim-dev", false)], &catalog, now);

    assert_eq!(result, Err(ApplicationError::NoSuitableModel));
}

#[test]
fn router_orders_by_preference_then_stable_model_identifier() {
    let now = evidence_now();
    let fresh = now - chrono::Duration::hours(1);
    let capabilities = vec![
        ModelCapability::ToolCalling,
        ModelCapability::StructuredOutput,
    ];
    let catalog = ModelCatalog::new(vec![
        discovered("zeta-model", fresh, &capabilities),
        discovered("alpha-model", fresh, &capabilities),
        discovered("preferred-model", fresh, &capabilities),
    ]);

    let policy = RoleRoutingPolicy::new(
        BTreeSet::from([
            ModelCapability::ToolCalling,
            ModelCapability::StructuredOutput,
        ]),
        vec![ModelId::new("preferred-model").expect("valid model id")],
        Duration::from_hours(24),
    )
    .expect("valid policy");

    let selection = ModelRouter::select(&policy, &[profile("nim-dev", true)], &catalog, now)
        .expect("eligible model exists");
    assert_eq!(selection.model_id.as_str(), "preferred-model");

    let without_preference = RoleRoutingPolicy::new(
        BTreeSet::from([
            ModelCapability::ToolCalling,
            ModelCapability::StructuredOutput,
        ]),
        Vec::new(),
        Duration::from_hours(24),
    )
    .expect("valid policy");
    let selection = ModelRouter::select(
        &without_preference,
        &[profile("nim-dev", true)],
        &catalog,
        now,
    )
    .expect("eligible model exists");
    assert_eq!(selection.model_id.as_str(), "alpha-model");
}

#[test]
fn router_is_deterministic_for_identical_inputs() {
    let now = evidence_now();
    let fresh = now - chrono::Duration::hours(1);
    let capabilities = vec![
        ModelCapability::ToolCalling,
        ModelCapability::StructuredOutput,
    ];
    let catalog = ModelCatalog::new(vec![
        discovered("b-model", fresh, &capabilities),
        discovered("a-model", fresh, &capabilities),
    ]);

    let first = ModelRouter::select(&agent_policy(), &[profile("nim-dev", true)], &catalog, now)
        .expect("eligible model exists");
    let second = ModelRouter::select(&agent_policy(), &[profile("nim-dev", true)], &catalog, now)
        .expect("eligible model exists");

    assert_eq!(first, second);
}

#[test]
fn catalog_refresh_replaces_prior_discovery_results() {
    let now = evidence_now();
    let fresh = now - chrono::Duration::hours(1);
    let mut catalog = ModelCatalog::new(vec![discovered(
        "retired-model",
        fresh,
        &[
            ModelCapability::ToolCalling,
            ModelCapability::StructuredOutput,
        ],
    )]);

    catalog.refresh(vec![discovered(
        "meta/llama-3.1-70b-instruct",
        fresh,
        &[
            ModelCapability::ToolCalling,
            ModelCapability::StructuredOutput,
        ],
    )]);

    let selection =
        ModelRouter::select(&agent_policy(), &[profile("nim-dev", true)], &catalog, now)
            .expect("refreshed model is eligible");
    assert_eq!(selection.model_id.as_str(), "meta/llama-3.1-70b-instruct");
}

#[test]
fn discovered_models_route_only_to_their_own_profile() {
    // SCRUM-82: catalog provenance. Two profiles discovering the same model id
    // must not let one profile inherit the other's evidence.
    let eligible_profile = profile("zeta", true);
    let ineligible_profile = profile("alpha", true);
    let now = Utc::now();
    let attributed = DiscoveredModel::new(ModelId::new("model-a").expect("id"))
        .with_evidence(ModelCapability::ToolCalling, now)
        .with_evidence(ModelCapability::StructuredOutput, now)
        .with_profile(eligible_profile.id().clone());
    let policy = RoleRoutingPolicy::agent_default(Duration::from_hours(1)).expect("policy");
    let catalog = ModelCatalog::new(vec![attributed]);

    let route = ModelRouter::select(
        &policy,
        &[eligible_profile.clone(), ineligible_profile],
        &catalog,
        now,
    )
    .expect("route");
    assert_eq!(route.profile_id, *eligible_profile.id());

    // Declared model sets constrain eligibility further: a profile that does
    // not list the discovered model never routes to it.
    let restrictive =
        profile("beta", true).with_declared_models(vec![ModelId::new("other").expect("id")]);
    assert!(
        ModelRouter::select(&policy, &[restrictive], &catalog, now).is_err(),
        "a declared-model allowlist must exclude unlisted models"
    );
}
