#![deny(unsafe_code)]

mod client;
mod config;
mod governed;
mod ipc;
mod platform_secret_store;
mod settings;
mod vault;
mod vault_provider;
mod vault_watcher;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_security;

pub use client::AuthenticatedIpcClient;
pub use config::{DaemonConfig, InferenceBearer, PromptConfig};
pub use cortex_application::SecretRef;
pub use cortex_inference::ReqwestOpenAiTransport;
pub use governed::GovernedVaultProvider;
pub use ipc::{
    AuthenticatedLocalClient, DaemonError, DaemonRequest, DaemonResponse, LocalDaemon,
    PROTOCOL_VERSION, PairingChallenge, PairingResponse, ProvisionedLocalClient, WireResult,
};
pub use platform_secret_store::PlatformSecretStore;
pub use settings::{
    BrainPromptSource, LocalSettings, ModelResolution, SettingsError, data_directory,
    data_directory_for, default_database_path, default_database_path_for, resolve_default_model,
    resolve_default_model_with_lookup,
};
pub use vault::{
    InMemoryVaultProvider, VaultConfigError, VaultExclusion, VaultProviderConfig,
    VaultProviderMode, VaultScope,
};
pub use vault_provider::{ConfinedPath, MarkdownVaultProvider, VaultPathError, VaultScanError};
pub use vault_watcher::{
    AppliedEvent, CoalescedEvent, DEFAULT_DEBOUNCE_MILLIS, EventQueue, ReconciliationError,
    ReconciliationReport, VaultEvent, VaultReconciler, apply_event, rebuild_vault, reconcile_vault,
};
