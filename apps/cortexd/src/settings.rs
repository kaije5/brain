use std::{collections::BTreeMap, fs, path::Path, time::Duration};

use chrono::Utc;
use cortex_application::{ApplicationError, SecretRef};
use cortex_inference::{
    ApiMode, AuthStrategy, DiscoveredModel, ModelCatalog, ModelId, ModelRouter,
    OpenAiCompatibleConfig, OpenAiDiscoveryConfig, OpenAiModelDiscovery, OpenAiTransport,
    ProfileTimeouts, ProviderLimits, ProviderProfile, ProviderProfileId, ProviderQuirks,
    RoleRoutingPolicy, RoutedModel,
};
use serde::Deserialize;

use crate::vault::{
    VaultConfigError, VaultExclusion, VaultProviderConfig, VaultProviderMode, VaultScope,
};

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
    fn from(error: ApplicationError) -> Self {
        let field = match error {
            ApplicationError::Validation { field } => field,
            _ => "secret_ref",
        };
        Self::Invalid { field }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    daemon: Option<DaemonSection>,
    models: Option<ModelsSection>,
    vault: Option<VaultSection>,
}

/// The `[vault]` section: the daemon-owned, non-secret declaration of the
/// authoritative Markdown vault provider. The root is a local filesystem
/// path and stays a cortexd composition input; scopes, exclusions, and the
/// provider mode are validated through the typed vault configuration.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VaultSection {
    provider_id: String,
    root: String,
    mode: ConfigVaultMode,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    exclusions: Vec<String>,
}

/// Explicit provider mode; there is no implicit default so a deployment must
/// state its read/write posture.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum ConfigVaultMode {
    ReadOnly,
    ReadWrite,
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
    profiles: Option<BTreeMap<String, ModelProfileEntry>>,
}

/// Wire protocol spoken by a profile. Only the OpenAI-compatible completions
/// mode exists (SCRUM-82); unknown names are rejected at parse time.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum ConfigApiMode {
    #[default]
    #[serde(rename = "openai_completions")]
    OpenAiCompletions,
}

/// Typed auth strategy. `secret_ref` requires the profile's `secret_ref`
/// locator; a raw credential key is structurally impossible because unknown
/// fields are rejected.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
enum ConfigAuthType {
    #[default]
    None,
    SecretRef,
}

/// Typed, narrow OpenAI-compatibility quirks; no free-form escape hatch.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
struct ConfigQuirks {
    #[serde(default)]
    omit_tool_choice: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelProfileEntry {
    base_url: String,
    #[serde(default = "enabled_by_default")]
    enabled: bool,
    secret_ref: Option<String>,
    #[serde(default)]
    api_mode: ConfigApiMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    auth_type: Option<ConfigAuthType>,
    #[serde(default)]
    connect_timeout_ms: Option<u64>,
    #[serde(default)]
    request_timeout_ms: Option<u64>,
    #[serde(default)]
    stale_stream_timeout_ms: Option<u64>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default)]
    quirks: ConfigQuirks,
}

fn enabled_by_default() -> bool {
    true
}

fn settings_error_from_vault(error: VaultConfigError) -> SettingsError {
    let field = match error {
        VaultConfigError::Invalid { field } => field,
        VaultConfigError::RootInaccessible => "vault_root",
    };
    SettingsError::Invalid { field }
}

/// Per-profile timeouts; unset fields keep the historical 5 s default so
/// existing single-profile configurations behave exactly as before.
///
/// # Errors
/// Returns [`SettingsError::Invalid`] when any declared timeout is zero.
fn profile_timeouts(entry: &ModelProfileEntry) -> Result<ProfileTimeouts, SettingsError> {
    let defaults = ProfileTimeouts::default();
    let connect = entry
        .connect_timeout_ms
        .map_or(defaults.connect(), Duration::from_millis);
    let request = entry
        .request_timeout_ms
        .map_or(defaults.request(), Duration::from_millis);
    let stale_stream = entry
        .stale_stream_timeout_ms
        .map_or(defaults.stale_stream(), Duration::from_millis);
    ProfileTimeouts::new(connect, request, stale_stream)
        .map_err(|_| SettingsError::Invalid { field: "timeouts" })
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
    vault: Option<VaultSection>,
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
                profiles: None,
            }),
            vault: file.vault,
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

# One table per provider profile, keyed by profile id. Optional typed keys:
# api_mode ("openai_completions"), auth_type ("none" | "secret_ref"), per-phase
# timeouts in milliseconds (connect_timeout_ms, request_timeout_ms,
# stale_stream_timeout_ms), a declared model allowlist ("models"), and typed
# "quirks" (omit_tool_choice). Unknown keys — including any raw credential
# field — are rejected at startup.
#[models.profiles.nim]
#base_url = "https://integrate.api.nvidia.com/v1"
#enabled = true
#secret_ref = "keyring:cortexd/nim"
#auth_type = "secret_ref"
#request_timeout_ms = 5000

#[vault]
# The authoritative local Markdown vault provider (v0.2). Declares exactly
# one local root, the allowed logical scopes, bounded vault-relative
# exclusions, and an explicit provider mode. No credentials or sync
# settings belong here. The root must exist and be a directory at startup.
#provider_id = "markdown-vault"
#root = "C:\\Users\\me\\Documents\\Vault"
#mode = "read_only"
#scopes = ["knowledge", "task"]
#exclusions = [".obsidian", "archive"]
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

    /// Builds the durable routing profiles declared in the file. Secret
    /// material never appears here, only opaque `SecretRef` locators.
    ///
    /// # Errors
    /// Returns [`SettingsError::Invalid`] when a profile id, secret
    /// reference, auth strategy, timeout, or declared model fails validation
    /// — including an `auth_type = "secret_ref"` profile without a
    /// `secret_ref`, and a keyless profile that declares one anyway.
    pub fn provider_profiles(&self) -> Result<Vec<ProviderProfile>, SettingsError> {
        let mut profiles = Vec::new();
        for (id, entry) in self.models.profiles.iter().flatten() {
            let profile_id = ProviderProfileId::new(id)?;
            let secret_reference = entry
                .secret_ref
                .as_deref()
                .map(SecretRef::new)
                .transpose()?;
            // The declared auth strategy and the presence of a secret locator
            // must agree. An unset `auth_type` keeps legacy configurations
            // working: a `secret_ref` implies `secret_ref` auth, no locator
            // implies keyless. A pasted credential has no representable field.
            let wants_secret = match entry.auth_type {
                // Unset keeps legacy configs working: infer from the locator.
                None => entry.secret_ref.is_some(),
                Some(ConfigAuthType::SecretRef) => true,
                Some(ConfigAuthType::None) => false,
            };
            let auth = match (wants_secret, entry.secret_ref.is_some()) {
                (true, true) => AuthStrategy::SecretRef,
                (false, false) => AuthStrategy::None,
                // Explicit strategy contradicting the locator presence.
                (true, false) | (false, true) => {
                    return Err(SettingsError::Invalid { field: "auth_type" });
                }
            };
            let timeouts = profile_timeouts(entry)?;
            let declared_models = entry
                .models
                .iter()
                .map(|model| ModelId::new(model).map_err(SettingsError::from))
                .collect::<Result<Vec<_>, _>>()?;
            let quirks =
                ProviderQuirks::default().with_omit_tool_choice(entry.quirks.omit_tool_choice);
            profiles.push(
                ProviderProfile::new(profile_id, entry.enabled)?
                    .with_secret_reference(secret_reference)
                    .with_api_mode(match entry.api_mode {
                        ConfigApiMode::OpenAiCompletions => ApiMode::OpenAiCompletions,
                    })
                    .with_auth_strategy(auth)
                    .with_timeouts(timeouts)
                    .with_quirks(quirks)
                    .with_declared_models(declared_models),
            );
        }
        Ok(profiles)
    }

    /// Builds the validated vault provider configuration from the `[vault]`
    /// section, or `None` when the section is absent (no vault provider is
    /// configured). The declared root is a local filesystem path and is only
    /// interpreted by the daemon composition, never by domain contracts.
    ///
    /// # Errors
    /// Returns [`SettingsError::Invalid`] when any declared value fails
    /// validation — including unknown scopes, absolute or traversing
    /// exclusions, and contradictory bounds.
    pub fn vault_config(&self) -> Result<Option<VaultProviderConfig>, SettingsError> {
        let Some(section) = self.vault.as_ref() else {
            return Ok(None);
        };
        let mode = match section.mode {
            ConfigVaultMode::ReadOnly => VaultProviderMode::ReadOnly,
            ConfigVaultMode::ReadWrite => VaultProviderMode::ReadWrite,
        };
        let mut scopes = std::collections::BTreeSet::new();
        for scope in &section.scopes {
            let scope = VaultScope::new(scope).map_err(settings_error_from_vault)?;
            if !scopes.insert(scope) {
                return Err(SettingsError::Invalid { field: "scopes" });
            }
        }
        let mut exclusions = std::collections::BTreeSet::new();
        for exclusion in &section.exclusions {
            let exclusion = VaultExclusion::new(exclusion).map_err(settings_error_from_vault)?;
            if !exclusions.insert(exclusion) {
                return Err(SettingsError::Invalid {
                    field: "exclusions",
                });
            }
        }
        VaultProviderConfig::new(
            &section.provider_id,
            std::path::PathBuf::from(&section.root),
            mode,
            scopes,
            exclusions,
        )
        .map(Some)
        .map_err(settings_error_from_vault)
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
                .map_or_else(std::env::temp_dir, |home| home.join(".local").join("share")),
        };
        base.join("cortex")
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
/// router: fresh OpenAI-compatible discovery on every enabled profile builds the capability
/// catalog (each with its own declared endpoint, auth strategy, and timeouts),
/// then the deterministic agent-role policy selects the
/// `{profile_id, model_id}` route. The bearer credential is resolved once by
/// the daemon composition root (from the profile's `SecretRef`) and crosses
/// only the transport boundary — and only to profiles whose auth strategy is
/// `secret_ref`.
///
/// Discovery and probes talk to the configured endpoints; failures surface as
/// [`ModelResolution::Degraded`] rather than blocking deterministic operation.
/// There is no fallback: the routed selection is the typed decision.
pub async fn resolve_default_model<T: OpenAiTransport + Clone>(
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
    let Some(default_profile) = profiles
        .iter()
        .find(|profile| profile.id().as_str() == profile_id && profile.enabled())
    else {
        return ModelResolution::Degraded {
            reason: "default_profile_missing_or_disabled",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };

    let discovery = discover_enabled_profiles(
        settings,
        &profiles,
        default_profile.id(),
        &transport,
        bearer,
    )
    .await;
    let Some(discovered) = discovery else {
        return ModelResolution::Degraded {
            reason: "provider_unavailable",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };

    let Ok(policy) = RoleRoutingPolicy::agent_default(MAX_EVIDENCE_AGE) else {
        return ModelResolution::Degraded {
            reason: "invalid_routing_policy",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let catalog = ModelCatalog::new(discovered);
    let Ok(route) = ModelRouter::select(&policy, &profiles, &catalog, Utc::now()) else {
        return ModelResolution::Degraded {
            reason: "no_eligible_model",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let Some(routed_profile) = profiles
        .iter()
        .find(|profile| profile.id() == &route.profile_id)
    else {
        return ModelResolution::Degraded {
            reason: "routed_profile_missing",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let Some(routed_base_url) = settings.endpoint_for(route.profile_id.as_str()) else {
        return ModelResolution::Degraded {
            reason: "routed_profile_missing",
            secret: routed_profile.secret_reference().cloned(),
        };
    };
    let routed_secret = routed_profile.secret_reference().cloned();
    let Ok(limits) = ProviderLimits::new(
        MODEL_RESPONSE_BYTES,
        MODEL_EMBEDDING_INPUT_BYTES,
        MODEL_EMBEDDING_DIMENSIONS,
    ) else {
        return ModelResolution::Degraded {
            reason: "invalid_provider_limits",
            secret: profile_and_secret(&profiles, &profile_id),
        };
    };
    let models = catalog_model_ids(catalog.models());
    match OpenAiCompatibleConfig::new(
        routed_base_url,
        route.model_id.as_str(),
        routed_secret.clone(),
        routed_profile.timeouts().request(),
        limits,
    )
    .map(|config| config.with_quirks(routed_profile.quirks()))
    {
        Ok(config) => ModelResolution::Configured {
            config,
            route,
            models,
        },
        Err(_) => ModelResolution::Degraded {
            reason: "invalid_model_config",
            secret: routed_secret,
        },
    }
}

/// Bounded discovery across every enabled profile, each with its own
/// endpoint, auth strategy, and request timeout. Discoveries are attributed
/// to their profile so the router can never pair a model with another
/// profile's evidence. Returns `None` when the default profile's discovery
/// fails; other profiles degrade best-effort so one unreachable endpoint
/// cannot hide an eligible alternative.
async fn discover_enabled_profiles<T: OpenAiTransport + Clone>(
    settings: &LocalSettings,
    profiles: &[ProviderProfile],
    default_profile_id: &ProviderProfileId,
    transport: &T,
    bearer: Option<&str>,
) -> Option<Vec<DiscoveredModel>> {
    let mut discovered = Vec::new();
    for profile in profiles.iter().filter(|profile| profile.enabled()) {
        let base_url = settings.endpoint_for(profile.id().as_str())?;
        let Ok(discovery_config) = OpenAiDiscoveryConfig::new(
            base_url,
            profile.secret_reference().cloned(),
            profile.timeouts().request(),
        ) else {
            if profile.id() == default_profile_id {
                return None;
            }
            continue;
        };
        // Credentials cross the transport boundary only for profiles whose
        // typed auth strategy references secret material.
        let profile_bearer = match profile.auth_strategy() {
            AuthStrategy::SecretRef => bearer,
            AuthStrategy::None => None,
        };
        match OpenAiModelDiscovery::new(discovery_config, transport.clone())
            .refresh(profile_bearer)
            .await
        {
            Ok(catalog) => discovered.extend(
                catalog
                    .models()
                    .iter()
                    .cloned()
                    .map(|model| model.with_profile(profile.id().clone())),
            ),
            Err(_) if profile.id() == default_profile_id => return None,
            Err(_) => {}
        }
    }
    Some(discovered)
}

fn catalog_model_ids(models: &[DiscoveredModel]) -> Vec<String> {
    models
        .iter()
        .map(|model| model.model_id().as_str().to_owned())
        .collect()
}
