#![deny(unsafe_code)]

mod cli;
mod client;
mod local_ops;
mod render;
pub mod tui;

pub use cli::{
    Cli, Command, CommandRequest, ConfigCommand, Output, RemoteCommand, SecretCommand, TaskCommand,
    command_request,
};
pub use client::{ClientError, DaemonClient};
pub use local_ops::{
    LocalOpError, PlatformSecretWriter, SecretWriter, data_directory, data_directory_for,
    default_database_path, default_database_path_for, import_secret, init_config,
};
pub use render::{CliEnvelope, render_json, render_text};
