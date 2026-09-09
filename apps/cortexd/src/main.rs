#![forbid(unsafe_code)]

use std::sync::Arc;

use cortexd::{
    DaemonConfig, LocalDaemon, LocalSettings, ModelResolution, PlatformSecretStore, data_directory,
    default_database_path, resolve_default_model,
};
use tokio::sync::watch;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = data_directory();
    let settings = LocalSettings::load(&directory.join("cortexd.toml"))?;
    let database_path = settings
        .as_ref()
        .and_then(LocalSettings::database_override)
        .map_or_else(default_database_path, |name| directory.join(name));
    let mut config = DaemonConfig::from_local_settings(database_path, settings.as_ref())?;
    let transport = cortexd::ReqwestNimTransport::default();
    match resolve_default_model(settings.as_ref(), transport).await {
        ModelResolution::Disabled => {}
        ModelResolution::Configured { config: model, .. } => {
            config = config.with_model_config(model);
        }
        ModelResolution::Degraded { reason, secret } => {
            eprintln!("cortexd: model inference degraded: {reason}");
            if let Some(reference) = secret {
                config = config.with_inference_secret(reference);
            }
        }
    }
    let daemon =
        Arc::new(LocalDaemon::start_with_secret_store(config, &PlatformSecretStore).await?);
    let (shutdown_sender, shutdown) = watch::channel(false);
    let server = tokio::spawn(daemon.serve(shutdown));
    tokio::signal::ctrl_c().await?;
    shutdown_sender.send(true)?;
    server.await??;
    Ok(())
}
