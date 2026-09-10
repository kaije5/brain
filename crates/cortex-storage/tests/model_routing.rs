use cortex_application::ApplicationError;
use cortex_storage::{
    SqliteDatabase, StoredCapability, StoredCapabilityEvidence, StoredProviderProfile,
};
use tempfile::TempDir;

fn debug_error(error: &ApplicationError) -> String {
    format!("{error:?}")
}

async fn database() -> (TempDir, SqliteDatabase) {
    let temp = tempfile::TempDir::new().expect("temp directory");
    let database = SqliteDatabase::connect_and_migrate(temp.path().join("cortex.db"))
        .await
        .expect("migrated database");
    (temp, database)
}

async fn ensure_profile(database: &SqliteDatabase, id: &str) {
    database
        .model_routing_store()
        .upsert_profile(&StoredProviderProfile {
            id: id.to_owned(),
            enabled: true,
            secret_reference: None,
        })
        .await
        .expect("profile created");
}

#[tokio::test]
async fn provider_profiles_round_trip_without_secret_material_expansion() -> Result<(), String> {
    let (_temp, database) = database().await;
    let store = database.model_routing_store();

    store
        .upsert_profile(&StoredProviderProfile {
            id: "nim-dev".to_owned(),
            enabled: true,
            secret_reference: Some("nvidia/api-catalog/dev".to_owned()),
        })
        .await
        .map_err(|error| debug_error(&error))?;

    let profiles = store
        .list_profiles()
        .await
        .map_err(|error| debug_error(&error))?;
    assert_eq!(
        profiles,
        vec![StoredProviderProfile {
            id: "nim-dev".to_owned(),
            enabled: true,
            secret_reference: Some("nvidia/api-catalog/dev".to_owned()),
        }]
    );

    store
        .upsert_profile(&StoredProviderProfile {
            id: "nim-dev".to_owned(),
            enabled: false,
            secret_reference: None,
        })
        .await
        .map_err(|error| debug_error(&error))?;
    let profiles = store
        .list_profiles()
        .await
        .map_err(|error| debug_error(&error))?;
    assert_eq!(profiles.len(), 1, "upsert updates rather than duplicates");
    assert!(!profiles[0].enabled);
    assert!(profiles[0].secret_reference.is_none());
    Ok(())
}

#[tokio::test]
async fn catalog_refresh_replaces_all_prior_evidence_for_a_profile() -> Result<(), String> {
    let (_temp, database) = database().await;
    let store = database.model_routing_store();
    ensure_profile(&database, "nim-dev").await;

    let first_refresh = vec![StoredCapabilityEvidence {
        model_id: "retired-model".to_owned(),
        capability: StoredCapability::ToolCalling,
        observed_at: "2026-09-08T00:00:00Z".to_owned(),
    }];
    store
        .replace_catalog("nim-dev", first_refresh)
        .await
        .map_err(|error| debug_error(&error))?;

    let second_refresh = vec![
        StoredCapabilityEvidence {
            model_id: "meta/llama-3.1-70b-instruct".to_owned(),
            capability: StoredCapability::ToolCalling,
            observed_at: "2026-09-09T00:00:00Z".to_owned(),
        },
        StoredCapabilityEvidence {
            model_id: "meta/llama-3.1-70b-instruct".to_owned(),
            capability: StoredCapability::StructuredOutput,
            observed_at: "2026-09-09T00:00:00Z".to_owned(),
        },
    ];
    store
        .replace_catalog("nim-dev", second_refresh.clone())
        .await
        .map_err(|error| debug_error(&error))?;

    let mut evidence = store
        .load_catalog("nim-dev")
        .await
        .map_err(|error| debug_error(&error))?;
    evidence.sort_by_key(|entry| (entry.model_id.clone(), format!("{:?}", entry.capability)));
    let mut expected = second_refresh;
    expected.sort_by_key(|entry| (entry.model_id.clone(), format!("{:?}", entry.capability)));
    assert_eq!(evidence, expected, "refresh replaces prior evidence");
    Ok(())
}

#[tokio::test]
async fn catalog_evidence_is_scoped_per_profile() -> Result<(), String> {
    let (_temp, database) = database().await;
    let store = database.model_routing_store();
    ensure_profile(&database, "nim-dev").await;
    let evidence = vec![StoredCapabilityEvidence {
        model_id: "model-a".to_owned(),
        capability: StoredCapability::StructuredOutput,
        observed_at: "2026-09-09T00:00:00Z".to_owned(),
    }];
    store
        .replace_catalog("nim-dev", evidence)
        .await
        .map_err(|error| debug_error(&error))?;

    let other = store
        .load_catalog("other-profile")
        .await
        .map_err(|error| debug_error(&error))?;
    assert!(other.is_empty(), "evidence must not leak across profiles");
    Ok(())
}

#[tokio::test]
async fn blank_profile_ids_are_rejected_at_the_store_boundary() -> Result<(), String> {
    let (_temp, database) = database().await;
    let store = database.model_routing_store();
    let result = store
        .upsert_profile(&StoredProviderProfile {
            id: "  ".to_owned(),
            enabled: true,
            secret_reference: None,
        })
        .await;
    assert!(matches!(&result, Err(ApplicationError::Validation { .. })));
    Ok(())
}

#[tokio::test]
async fn route_decision_round_trips_and_replaces_the_previous_selection() -> Result<(), String> {
    let (_temp, database) = database().await;
    let store = database.model_routing_store();
    ensure_profile(&database, "alpha").await;
    ensure_profile(&database, "beta").await;

    assert!(store.load_route().await.is_ok_and(|route| route.is_none()));

    store
        .record_route("beta", "model-a", "2026-09-10T12:00:00Z")
        .await
        .map_err(|error| debug_error(&error))?;
    let decision = store
        .load_route()
        .await
        .map_err(|error| debug_error(&error))?
        .expect("decision recorded");
    assert_eq!(decision.profile_id, "beta");
    assert_eq!(decision.model_id, "model-a");
    assert_eq!(decision.routed_at, "2026-09-10T12:00:00Z");

    // The latest resolution replaces the previous decision, identifiers only.
    store
        .record_route("alpha", "model-b", "2026-09-10T13:00:00Z")
        .await
        .map_err(|error| debug_error(&error))?;
    let decision = store
        .load_route()
        .await
        .map_err(|error| debug_error(&error))?
        .expect("decision replaced");
    assert_eq!(decision.profile_id, "alpha");
    assert_eq!(decision.model_id, "model-b");

    // Invalid identifiers are rejected before any write.
    assert!(
        store
            .record_route("", "model-a", "2026-09-10T14:00:00Z")
            .await
            .is_err()
    );
    Ok(())
}
