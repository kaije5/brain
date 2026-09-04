#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, time::Duration};

use cortex_mcp::McpPrincipal as LocalMcpPrincipal;
use cortex_mcp_gateway::{
    GatewayConfig, GatewayError, GatewayTransport, PrincipalRegistry, RetryPolicy,
    RustlsTunnelConnector, TunnelClient, bind_loopback,
};
use cortexd::AuthenticatedIpcClient;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = required_path("CORTEX_GATEWAY_CONFIG")?;
    let database_path = required_path("CORTEX_DATABASE")?;
    let config_text =
        fs::read_to_string(config_path).map_err(|_| GatewayError::InvalidConfiguration)?;
    let config = GatewayConfig::parse(&config_text)?;
    let resolver = config.identity_resolver()?;
    let ipc = AuthenticatedIpcClient::from_database_path(&database_path)
        .map_err(|_| GatewayError::LocalTransportUnavailable)?;
    if !config.paired_principal_ids().contains(&ipc.principal_id()) {
        return Err(GatewayError::InvalidConfiguration.into());
    }
    let mut registry = PrincipalRegistry::new();
    registry.insert(ipc.principal_id(), LocalMcpPrincipal::from_ipc(ipc));

    let cancellation = CancellationToken::new();
    let listener = bind_loopback(&config).await?;
    let local_addr = listener
        .local_addr()
        .map_err(|_| GatewayError::LocalTransportUnavailable)?;
    let transport = GatewayTransport::new(resolver, registry, cancellation.clone());
    let tunnel = TunnelClient::new(
        config.relay().clone(),
        local_addr,
        RustlsTunnelConnector::with_webpki_roots(),
        RetryPolicy::new(Duration::from_millis(250), Duration::from_secs(30))?,
    )?;

    let result = tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            signal.map_err(|_| GatewayError::LocalTransportUnavailable)
        }
        result = transport.serve(listener, cancellation.clone()) => result,
        result = tunnel.run(cancellation.clone()) => result,
    };
    cancellation.cancel();
    result.map_err(Into::into)
}

fn required_path(name: &str) -> Result<PathBuf, GatewayError> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(GatewayError::InvalidConfiguration)
}
