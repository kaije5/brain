use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use chrono::Utc;
use cortex_application::{ApplicationError, SecretRef};
use cortex_inference::{
    ModelCatalog, ModelRouter, NimConfig, NimDiscovery, NimTransport, OpenAiCompatibleConfig,
    ProviderLimits, ProviderProfile, ProviderProfileId, RoleRoutingPolicy, RoutedModel,
};
use serde::Deserialize;

/// Redacted failure category for the local settings file. The error never
/// embeds file contents, because those must never include credentials.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SettingsError {
    /// The file exists but is unreadable, unparsable, or holds unsupported keys.
    #[error("invalid local settings")]
    Invalid {
        /// Coarse field name for diagnostics; never a value.
        field: &'static str,
    },
}

impl From<ApplicationError> for SettingsError {
    fn from(_: ApplicationError) -> Self {
        Self::Invalid {
            field: "secret_ref",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    daemon: Option<DaemonSection>,
    models: Option<ModelsSection>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DaemonSection {
    database: Option<String>,
    endpoint: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelsSection {
    default_profile: Option<String>,
    model: Option<String>,
    profiles: Option<BTreeMap<String, ModelProfileEntry>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelProfileEntry {
    base_url: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    secret_ref: Option<String>,
}

fn enabled_by_default() -> bool {
    true
}

/// Non-secret local settings loaded from `cortexd.toml` beside the database.
///
/// The schema deliberately has no field that could hold a raw credential:
/// unknown keys are rejected, so a pasted API key fails startup instead of
/// landing in a tracked or world-readable file.
#[derive(Clone, Debug)]
pub struct LocalSettings {
    daemon: DaemonSection,
    models: ModelsSection,
}

impl LocalSettings {
    /// Loads the local settings file, returning documented defaults when the
    /// file is absent.
    ///
    /// # Errors
    /// Returns [`SettingsError::Invalid`] when the file exists but cannot be
    /// read or parsed, or when it contains unsupported (potentially secret)
    /// keys.
    pub fn load(path: &Path) -> Result<Option<Self>, SettingsError> {
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(path).map_err(|_| SettingsError::Invalid { field: "file" })?;
        let file: SettingsFile =
            toml::from_slice(&bytes).map_err(|_| SettingsError::Invalid { field: "file" })?;
        Ok(Some(Self {
            daemon: file.daemon.unwrap_or(DaemonSection {
                database: None,
                endpoint: None,
            }),
            models: file.models.unwrap_or(ModelsSection {
                default_profile: None,
                model: None,
                profiles: None,
            }),
        }))
    }

    /// Commented template documenting every supported non-secret key.
    /// `brain config init` writes this file verbatim.
    #[must_use]
    pub fn template() -> &'static str {
        r#"# cortexd.toml - non-secret local Cortex settings.
# Loaded by cortexd at startup from this data directory. Absent file means
# defaults. Credentials never belong here: import provider API keys with
# `brain secret import` and reference them with `secret_ref` locators.

[daemon]
# SQLite database file name, relative to this directory. Default: cortex.db
#database = "cortex.db"
# Local IPC endpoint name. Default: generated per workspace.
#endpoint = "cortexd-local"

[models]
# Provider profile used for agent/chat inference. Resolved at runtime through
# the capability-aware model router; a missing or ineligible profile leaves
# the daemon explicitly degraded (no silent fallback).
#default_profile = "nim"

# One table per provider profile, keyed by profile id.
#[models.profiles.nim]
#base_url = "https://integrate.api.nvidia.com/v1"
#enabled = true
#secret_ref = "keyring:cortexd/nim"
"#
    }

    /// Daemon endpoint name override from `[daemon] endpoint`.
    #[must_use]
    pub fn endpoint_override(&self) -> Option<&str> {
        self.daemon.endpoint.as_deref()
    }

    /// Database file name override from `[daemon] database`, relative to the
    /// data directory holding this file.
    #[must_use]
    pub fn database_override(&self) -> Option<&str> {
        self.daemon.database.as_deref()
    }

    /// Default provider profile id from `[models] default_profile`.
    #[must_use]
    pub fn default_profile_id(&self) -> Option<&str> {
        self.models.default_profile.as_deref()
    }

    /// Pinned model id from `[models] model`, overriding router selection
    /// when the pinned model appears in the discovered catalog.
    #[must_use]
    pub fn pinned_model(&self) -> Option<&str> {
        self.models.model.as_deref()
    }

    /// Builds the durable routing profiles declared in the file. Secret
    /// material never appears here, only opaque `SecretRef` locators.
    ///
    /// # Errors
    /// Returns [`SettingsError::Invalid`] when a profile id or secret
    /// reference fails validation.
    pub fn provider_profiles(&self) -> Result<Vec<ProviderProfile>, SettingsError> {
        let mut profiles = Vec::new();
        for (id, entry) in self.models.profiles.iter().flatten() {
            let profile_id = ProviderProfileId::new(id)?;
            let secret_reference = entry
                .secret_ref
                .as_deref()
                .map(SecretRef::new)
                .transpose()?;
            profiles.push(
                ProviderProfile::new(profile_id, entry.enabled)?
                    .with_secret_reference(secret_reference),
            );
        }
        Ok(profiles)
    }

    /// Base endpoint declared for one profile id.
    #[must_use]
    pub fn endpoint_for(&self, profile_id: &str) -> Option<&str> {
        self.models
            .profiles
            .as_ref()?
            .get(profile_id)
            .map(|entry| entry.base_url.as_str())
    }
}

/// Data directory holding `cortex.db` and `cortexd.toml`. `CORTEX_DATABASE`
/// is the single documented override for the database location.
#[must_use]
pub fn data_directory() -> std::path::PathBuf {
    data_directory_for(std::env::var_os("CORTEX_DATABASE").as_deref())
}

/// Pure form of [`data_directory`] for tests and tools with an injected
/// override value.
#[must_use]
pub fn data_directory_for(override_value: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    override_value
        .map(std::path::PathBuf::from)
        .and_then(|path| path.parent().map(std::path::Path::to_path_buf))
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(default_data_directory)
}

/// Database location: the `CORTEX_DATABASE` override when set, otherwise the
/// platform data directory's `cortex.db`.
#[must_use]
pub fn default_database_path() -> std::path::PathBuf {
    default_database_path_for(std::env::var_os("CORTEX_DATABASE").as_deref())
}

/// Pure form of [`default_database_path`] for tests and tools with an
/// injected override value.
#[must_use]
pub fn default_database_path_for(override_value: Option<&std::ffi::OsStr>) -> std::path::PathBuf {
    override_value.map_or_else(
        || default_data_directory().join("cortex.db"),
        std::path::PathBuf::from,
    )
}

fn default_data_directory() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        match std::env::var_os("LOCALAPPDATA") {
            Some(value) => std::path::PathBuf::from(value),
            None => std::env::temp_dir(),
        }
        .join("cortex")
    }
    #[cfg(not(windows))]
    {
        let base = match std::env::var_os("XDG_DATA_HOME") {
            Some(value) => std::path::PathBuf::from(value),
            None => std::env::home_dir()
                .map(|home| home.join(".local").join("share"))
                .unwrap_or_else(std::env::temp_dir),
        };
        base.join("cortex")
    }
}

/// Narrows the catalog to the pinned model when one is configured.
/// `Err(())` means the pin names a model the profile never discovered.
fn apply_pinned_model(catalog: ModelCatalog, pinned: Option<&str>) -> Result<ModelCatalog, ()> {
    match pinned {
        Some(pinned) => catalog
            .models()
            .iter()
            .find(|model| model.model_id().as_str() == pinned)
            .cloned()
            .map(|model| ModelCatalog::new(vec![model]))
            .ok_or(()),
        None => Ok(catalog),
    }
}

/// Outcome of resolving the configured default model profile through the
/// runtime model router. Every non-configured outcome is explicit: there is
/// no silent provider fallback.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)] // `Configured` carries the validated provider config; the degraded path stays value-sized in practice.
pub enum ModelResolution {
    /// No `[models] default_profile` is configured; deterministic
    /// capabilities run without inference.
    Disabled,
    /// The router selected an eligible model on the configured profile.
    Configured {
        /// Validated loopback provider configuration for the routed model.
        config: OpenAiCompatibleConfig,
        /// The durable routing decision, for diagnostics only.
        route: RoutedModel,
        /// Every model id discovered on the profile, for client selection.
        models: Vec<String>,
    },
    /// A configured profile could not produce an eligible model. The daemon
    /// starts and reports the degraded state; it never falls back silently.
    Degraded {
        /// Coarse, value-free reason for diagnostics.
        reason: &'static str,
        /// The configured profile's secret locator, when one was declared.
        /// Callers still verify it at composition time even without a model.
        secret: Option<SecretRef>,
    },
}

const MODEL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_EVIDENCE_AGE: Duration = Duration::from_hours(24);

const MODEL_RESPONSE_BYTES: usize = 64 * 1024;
const MODEL_EMBEDDING_INPUT_BYTES: usize = 32 * 1024;
const MODEL_EMBEDDING_DIMENSIONS: usize = 4096;

fn profile_and_secret(profiles: &[ProviderProfile], profile_id: &str) -> Option<SecretRef> {
    profiles
        .iter()
        .find(|profile| profile.id().as_str() == profile_id)
        .and_then(|profile| profile.secret_reference())
        .cloned()
}

/// Resolves the settings' default model profile through the SCRUM-41 runtime
/// router: fresh NIM discovery builds the capability catalog, then the
/// deterministic agent-role policy selects the model. The bearer credential
/// is resolved once by the daemon composition root (from the profile's
/// `SecretRef`) and crosses only the transport boundary.
///
/// Discovery and probes talk to the configured endpoint; failures surface as
/// [`ModelResolution::Degraded`] rather than blocking deterministic operation.
pub async fn resolve_default_model<T: NimTransport>(
    settings: Option<&LocalSettings>,
    transport: T,
    bearer: Option<&str>,
) -> ModelResolution {
    let Some(settings) = settings else {
        return ModelResolution::Disabled;
    };
    let Some(profile_id) = settings.default_profile_id().map(str::to_owned) else {
        return ModelResolution::Disabled;
    };
    let Ok(profiles) = settings.provider_profiles() else {
        return ModelResolution::Degraded {
            reason: "invalid_profiles",
            secret: None,
        };
    };
    let Some(profile) = profiles
        .iter()
        .find(|profile| profile.id().as_str() == profile_id && profile.enabled())
    else {
        return ModelResolution::Degraded {
            reason: "default_profile_missing_or_disabled",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let base_url = settings.endpoint_for(&profile_id).unwrap_or_default();
    let Ok(discovery_config) =
        NimConfig::new(base_url, profile.secret_reference().cloned(), MODEL_TIMEOUT)
    else {
        return ModelResolution::Degraded {
            reason: "invalid_endpoint",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let catalog = NimDiscovery::new(discovery_config, transport)
        .refresh(bearer)
        .await;
    let Ok(catalog) = catalog else {
        return ModelResolution::Degraded {
            reason: "provider_unavailable",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let all_models: Vec<String> = catalog
        .models()
        .iter()
        .map(|model| model.model_id().as_str().to_owned())
        .collect();
    let Ok(catalog) = apply_pinned_model(catalog, settings.pinned_model()) else {
        return ModelResolution::Degraded {
            reason: "pinned_model_unavailable",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let Ok(policy) = RoleRoutingPolicy::agent_default(MAX_EVIDENCE_AGE) else {
        return ModelResolution::Degraded {
            reason: "invalid_routing_policy",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let Ok(route) = ModelRouter::select(&policy, &profiles, &catalog, Utc::now()) else {
        return ModelResolution::Degraded {
            reason: "no_eligible_model",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let Some(routed_base_url) = settings.endpoint_for(route.profile_id.as_str()) else {
        return ModelResolution::Degraded {
            reason: "routed_profile_missing",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let routed_secret = profiles
        .iter()
        .find(|profile| profile.id() == &route.profile_id)
        .and_then(|profile| profile.secret_reference())
        .cloned();
    build_routed_provider(
        routed_base_url,
        route.model_id.as_str().to_owned(),
        routed_secret,
        all_models,
        route,
    )
}

/// Builds the validated provider configuration for one routed model.
fn build_routed_provider(
    base_url: &str,
    model_id: String,
    secret: Option<SecretRef>,
    discovered: Vec<String>,
    route: cortex_inference::RoutedModel,
) -> ModelResolution {
    let Ok(limits) = ProviderLimits::new(
        MODEL_RESPONSE_BYTES,
        MODEL_EMBEDDING_INPUT_BYTES,
        MODEL_EMBEDDING_DIMENSIONS,
    ) else {
        return ModelResolution::Degraded {
            reason: "invalid_provider_limits",
            secret,
        };
    };
    match OpenAiCompatibleConfig::new(base_url, model_id, secret.clone(), MODEL_TIMEOUT, limits) {
        Ok(config) => ModelResolution::Configured {
            config,
            route,
            models: discovered,
        },
        Err(_) => ModelResolution::Degraded {
            reason: "invalid_model_config",
            secret,
        },
    }
}
