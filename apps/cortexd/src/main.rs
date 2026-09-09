#![forbid(unsafe_code)]

use std::sync::Arc;

use cortexd::{
    DaemonConfig, InferenceBearer, LocalDaemon, LocalSettings, ModelResolution,
    PlatformSecretStore, data_directory, default_database_path, resolve_default_model,
};
use tokio::sync::watch;

/// The default profile's opaque secret locator, when one is configured.
fn default_profile_secret(settings: Option<&LocalSettings>) -> Option<cortexd::SecretRef> {
    let settings = settings?;
    let profile_id = settings.default_profile_id()?;
    settings
        .provider_profiles()
        .ok()?
        .into_iter()
        .find(|profile| profile.id().as_str() == profile_id)
        .and_then(|profile| profile.secret_reference().cloned())
}

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
    // Composition root: resolve the default profile's credential once so
    // discovery and the configured provider authenticate against hosted
    // endpoints. The value never leaves this process.
    let credential = match default_profile_secret(settings.as_ref()) {
        Some(reference) => PlatformSecretStore.resolve_value(&reference).ok(),
        None => None,
    }
    .map(|value| value.as_str().to_owned());
    let bearer = credential.as_deref();
    match resolve_default_model(settings.as_ref(), transport, bearer).await {
        ModelResolution::Disabled => {}
        ModelResolution::Configured { config: model, .. } => {
            config = config
                .with_model_config(model)
                .with_inference_bearer(bearer.map(str::to_owned).map(InferenceBearer::new));
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
