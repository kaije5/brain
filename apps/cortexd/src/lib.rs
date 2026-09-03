#![deny(unsafe_code)]

mod config;
mod ipc;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_security;

pub use config::DaemonConfig;
pub use ipc::{
    AuthenticatedLocalClient, DaemonError, DaemonRequest, DaemonResponse, LocalDaemon,
    PROTOCOL_VERSION, WireResult,
};
