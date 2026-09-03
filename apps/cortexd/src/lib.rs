#![forbid(unsafe_code)]

mod config;
mod ipc;

pub use config::DaemonConfig;
pub use ipc::{
    AuthenticatedLocalClient, DaemonError, DaemonRequest, DaemonResponse, LocalDaemon,
    PROTOCOL_VERSION,
};
