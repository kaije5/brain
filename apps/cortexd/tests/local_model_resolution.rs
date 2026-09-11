use std::time::Duration;

use cortex_inference::{ProviderError, ProviderFailureCategory};
use cortexd::{LocalSettings, ModelResolution, resolve_default_model};
use serde_json::{Value, json};
use tempfile::TempDir;

const VALID_SETTINGS: &str = r#"
[models]
default_profile = "local"

[models.profiles.local]
base_url = "http://127.0.0.1:8000/v1/"
secret_ref = "keyring:cortexd/local"
"#;

fn settings_from(contents: &str) -> LocalSettings {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = directory.path().join("cortexd.toml");
    std::fs::write(&path, contents).expect("settings file should be writable");
    LocalSettings::load(&path)
        .expect("valid settings")
        .expect("file present")
}

/// Fake NIM transport: discovery lists two models, every capability probe
/// succeeds. Discovery records the bearer it received for auth assertions.
#[derive(Clone, Default)]
struct FakeNimTransport {
    unavailable: bool,
    discovery_bearer: std::sync::Arc<std::sync::Mutex<Vec<Option<String>>>>,
}

impl cortex_inference::NimTransport for FakeNimTransport {
    async fn get_json(
        &self,
        _endpoint: &str,
        bearer: Option<&str>,
        _timeout: Duration,
        _max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        if let Ok(mut seen) = self.discovery_bearer.lock() {
            seen.push(bearer.map(str::to_owned));
        }
        if self.unavailable {
            return Err(ProviderError::from_category(
                ProviderFailureCategory::Unavailable,
            ));
        }
        Ok(
            serde_json::to_vec(&json!({"data": [{"id": "model-b"}, {"id": "model-a"}]}))
                .expect("static JSON"),
        )
    }

    async fn post_json(
        &self,
        _endpoint: &str,
        _bearer: Option<&str>,
        _body: Value,
        _timeout: Duration,
        _max_response_bytes: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        if self.unavailable {
            return Err(ProviderError::from_category(
                ProviderFailureCategory::Unavailable,
            ));
        }
        Ok(b"{}".to_vec())
    }
}

#[tokio::test]
async fn default_profile_resolves_through_the_runtime_router() {
    let settings = settings_from(VALID_SETTINGS);
    let resolution =
        resolve_default_model(Some(&settings), FakeNimTransport::default(), None).await;
    match resolution {
        ModelResolution::Configured { config, route, .. } => {
            assert_eq!(route.profile_id.as_str(), "local");
            assert_eq!(route.model_id.as_str(), "model-a");
            assert_eq!(config.model(), "model-a");
            assert_eq!(
                config
                    .secret_reference()
                    .expect("secret reference")
                    .as_str(),
                "keyring:cortexd/local"
            );
        }
        other => panic!("expected a configured route, got {other:?}"),
    }
}

#[tokio::test]
async fn resolved_bearer_authenticates_discovery() {
    let settings = settings_from(VALID_SETTINGS);
    let transport = FakeNimTransport::default();
    let seen = transport.discovery_bearer.clone();
    let resolution =
        resolve_default_model(Some(&settings), transport.clone(), Some("nvapi-test-key")).await;
    assert!(matches!(resolution, ModelResolution::Configured { .. }));
    let seen = seen.lock().expect("bearer log");
    assert!(
        seen.iter()
            .all(|bearer| bearer.as_deref() == Some("nvapi-test-key")),
        "every discovery request must carry the resolved bearer, got {seen:?}"
    );
}

#[tokio::test]
async fn unavailable_provider_is_an_explicit_degraded_state() {
    let settings = settings_from(VALID_SETTINGS);
    let resolution = resolve_default_model(
        Some(&settings),
        FakeNimTransport {
            unavailable: true,
            ..FakeNimTransport::default()
        },
        None,
    )
    .await;
    assert!(
        matches!(resolution, ModelResolution::Degraded { .. }),
        "provider failure must surface as an explicit degraded state, got {resolution:?}"
    );
}

#[tokio::test]
async fn unknown_default_profile_is_explicitly_degraded() {
    let settings = settings_from(
        r#"
[models]
default_profile = "missing"

[models.profiles.local]
base_url = "http://127.0.0.1:8000/v1/"
"#,
    );
    let resolution =
        resolve_default_model(Some(&settings), FakeNimTransport::default(), None).await;
    assert!(matches!(resolution, ModelResolution::Degraded { .. }));
}

#[tokio::test]
async fn no_default_profile_leaves_inference_disabled() {
    let settings = settings_from("[daemon]\n");
    let resolution =
        resolve_default_model(Some(&settings), FakeNimTransport::default(), None).await;
    assert!(matches!(resolution, ModelResolution::Disabled));
}

#[test]
fn daemon_settings_apply_endpoint_override_on_first_initialization() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let path = directory.path().join("cortexd.toml");
    std::fs::write(&path, "[daemon]\nendpoint = \"cortexd-lab\"\n").expect("writable");
    let settings = LocalSettings::load(&path)
        .expect("valid settings")
        .expect("file present");
    let config = cortexd::DaemonConfig::from_local_settings(
        directory.path().join("cortex.db"),
        Some(&settings),
    )
    .expect("valid daemon config");
    assert_eq!(config.endpoint_name(), "cortexd-lab");
}

#[test]
fn daemon_defaults_apply_without_a_settings_file() {
    let directory = TempDir::new().expect("temporary directory should be available");
    let config =
        cortexd::DaemonConfig::from_local_settings(directory.path().join("cortex.db"), None)
            .expect("valid daemon config");
    assert!(config.endpoint_name().starts_with("cortexd-"));
}
