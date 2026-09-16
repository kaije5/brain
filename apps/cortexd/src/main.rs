#![forbid(unsafe_code)]

use std::sync::Arc;

use cortexd::{
    DaemonConfig, LocalDaemon, LocalSettings, ModelResolution, PlatformSecretStore, data_directory,
    default_database_path, resolve_default_model,
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
    // SCRUM-82: fail fast on structurally invalid provider profile
    // configuration. A runtime-unreachable provider degrades explicitly
    // later; a profile that fails validation is a startup error.
    if let Some(Err(cortexd::SettingsError::Invalid { field })) =
        settings.as_ref().map(LocalSettings::provider_profiles)
    {
        return Err(format!(
            "invalid provider profile configuration in cortexd.toml (field: {field})"
        )
        .into());
    }
    let database_path = settings
        .as_ref()
        .and_then(LocalSettings::database_override)
        .map_or_else(default_database_path, |name| directory.join(name));
    let config = DaemonConfig::from_local_settings(database_path, settings.as_ref())?;
    let daemon = Arc::new(LocalDaemon::start(config).await?);
    let (shutdown_sender, shutdown) = watch::channel(false);
    let server = tokio::spawn(daemon.clone().serve(shutdown));

    // Composition root (SCRUM-76): model resolution runs in the background so
    // the IPC server accepts requests immediately; deterministic capabilities
    // work while the provider catalog is still being probed, and inference
    // upgrades in place once resolution completes. The credential is resolved
    // once here and never leaves this process.
    let resolve_settings = settings.clone();
    let resolve_daemon = daemon.clone();
    tokio::spawn(async move {
        let credential = match default_profile_secret(resolve_settings.as_ref()) {
            Some(reference) => PlatformSecretStore.resolve_value(&reference).ok(),
            None => None,
        }
        .map(|value| value.as_str().to_owned());
        let bearer = credential.as_deref();
        match resolve_default_model(
            resolve_settings.as_ref(),
            cortexd::ReqwestOpenAiTransport::default(),
            bearer,
        )
        .await
        {
            ModelResolution::Disabled => {}
            ModelResolution::Configured {
                config,
                models,
                route,
            } => {
                resolve_daemon.install_resolved_model(config, bearer.map(str::to_owned), models);
                // SCRUM-82: persist the typed {profile_id, model_id} decision
                // for diagnostics; identifiers only, never secrets.
                if let Err(error) = resolve_daemon
                    .record_route(route.profile_id.as_str(), route.model_id.as_str())
                    .await
                {
                    eprintln!("cortexd: routing decision not persisted: {error:?}");
                }
            }
            ModelResolution::Degraded { reason, .. } => {
                eprintln!("cortexd: model inference degraded: {reason}");
            }
        }
    });

    tokio::signal::ctrl_c().await?;
    shutdown_sender.send(true)?;
    server.await??;
    Ok(())
}
