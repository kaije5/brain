#![forbid(unsafe_code)]

use std::{path::PathBuf, sync::Arc};

use cortexd::{DaemonConfig, LocalDaemon};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_path = std::env::var_os("CORTEX_DATABASE")
        .map(PathBuf::from)
        .ok_or("CORTEX_DATABASE must name a local SQLite file")?;
    let daemon =
        Arc::new(LocalDaemon::start(DaemonConfig::from_database_path(database_path)?).await?);
    let (shutdown_sender, shutdown) = watch::channel(false);
    let server = tokio::spawn(daemon.serve(shutdown));
    tokio::signal::ctrl_c().await?;
    shutdown_sender.send(true)?;
    server.await??;
    Ok(())
}
