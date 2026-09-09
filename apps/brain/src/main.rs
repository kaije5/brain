#![forbid(unsafe_code)]

use brain::{
    Cli, CliEnvelope, Command, ConfigCommand, DaemonClient, LocalOpError, PlatformSecretWriter,
    SecretCommand, command_request, default_database_path, import_secret, init_config, render_json,
    render_text,
};
use clap::Parser;
use cortexd::WireResult;
use serde_json::{Value, json};

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
            "{\"ok\":false,\"data\":null,\"error\":{\"code\":\"render_failed\"}}
"
            .to_owned()
        }),
        brain::Output::Text => render_text(&envelope),
    };
    print!("{text}");
    if exit != 0 {
        std::process::exit(exit);
    }
}

enum RunError {
    Client(brain::ClientError),
    Local(LocalOpError),
    MissingSecret,
}

impl RunError {
    fn code(&self) -> &str {
        match self {
            Self::Client(error) => error.code(),
            Self::Local(LocalOpError::ConfigAlreadyExists) => "config_already_exists",
            Self::Local(LocalOpError::InvalidProfile) => "invalid_profile",
            Self::Local(LocalOpError::MissingSecret) | Self::MissingSecret => "missing_secret",
            Self::Local(_) => "secret_store_unavailable",
        }
    }
}

async fn run(cli: &Cli) -> Result<Value, RunError> {
    match &cli.command {
        Command::Config(ConfigCommand::Init) => {
            let path = init_config(&default_database_path()).map_err(RunError::Local)?;
            Ok(json!({"config_path": path.to_string_lossy()}))
        }
        Command::Secret(SecretCommand::Import { profile }) => {
            let mut secret = String::new();
            std::io::stdin()
                .read_line(&mut secret)
                .map_err(|_| RunError::MissingSecret)?;
            let secret = secret.trim().to_owned();
            if secret.is_empty() {
                return Err(RunError::MissingSecret);
            }
            let secret_ref = import_secret(&PlatformSecretWriter, profile, secret.as_bytes())
                .map_err(RunError::Local)?;
            drop(secret);
            Ok(json!({"secret_ref": secret_ref, "profile": profile}))
        }
        _ => {
            let command = command_request(cli)
                .map_err(|_| RunError::Client(brain::ClientError::InvalidInput))?;
            let response = DaemonClient::from_environment()
                .map_err(RunError::Client)?
                .request(command)
                .await
                .map_err(RunError::Client)?;
            match response.result {
                WireResult::Success { value } => Ok(value),
                WireResult::Error { code } => {
                    Err(RunError::Client(brain::ClientError::Daemon(code)))
                }
            }
        }
    }
}
