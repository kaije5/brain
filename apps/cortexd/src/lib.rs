#![deny(unsafe_code)]

mod client;
mod config;
mod ipc;
mod platform_secret_store;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_security;

pub use client::AuthenticatedIpcClient;
pub use config::DaemonConfig;
pub use ipc::{
    AuthenticatedLocalClient, DaemonError, DaemonRequest, DaemonResponse, LocalDaemon,
    PROTOCOL_VERSION, PairingChallenge, PairingResponse, ProvisionedLocalClient, WireResult,
};
pub use platform_secret_store::PlatformSecretStore;
