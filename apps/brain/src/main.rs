#![forbid(unsafe_code)]

use brain::{Cli, CliEnvelope, DaemonClient, command_request, render_json, render_text};
use clap::Parser;
use cortexd::WireResult;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = run(&cli).await;
    let (envelope, exit) = match result {
        Ok(value) => (CliEnvelope::success(value), 0),
        Err(error) => (CliEnvelope::error(error.code()), 1),
    };
    let text = match cli.output {
        brain::Output::Json => render_json(&envelope).unwrap_or_else(|_| {
            "{\"ok\":false,\"data\":null,\"error\":{\"code\":\"render_failed\"}}\n".to_owned()
        }),
        brain::Output::Text => render_text(&envelope),
    };
    print!("{text}");
    if exit != 0 {
        std::process::exit(exit);
    }
}

async fn run(cli: &Cli) -> Result<serde_json::Value, brain::ClientError> {
    let command = command_request(cli).map_err(|_| brain::ClientError::InvalidInput)?;
    let response = DaemonClient::from_environment()?.request(command).await?;
    match response.result {
        WireResult::Success { value } => Ok(value),
        WireResult::Error { code } => Err(brain::ClientError::Daemon(code)),
    }
}
