use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use uuid::Uuid;

use cortexd::PROTOCOL_VERSION;

#[derive(Clone, Debug, Parser)]
#[command(name = "brain", about = "Cortex local command client")]
pub struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = Output::Text)]
    pub output: Output,
    /// Absent subcommand opens the interactive full-screen session.
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Output {
    Text,
    Json,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    Status,
    Doctor,
    Logs,
    #[command(subcommand)]
    Note(NoteCommand),
    #[command(subcommand)]
    Task(TaskCommand),
    Remember(RememberArgs),
    #[command(subcommand)]
    Memory(MemoryCommand),
    #[command(subcommand)]
    Remote(RemoteCommand),
    #[command(subcommand)]
    Config(ConfigCommand),
    #[command(subcommand)]
    Secret(SecretCommand),
    Ask {
        prompt: String,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum NoteCommand {
    Create { title: String, content: String },
    Search(SearchArgs),
}

#[derive(Clone, Debug, Subcommand)]
pub enum TaskCommand {
    Add {
        title: String,
        #[arg(long)]
        due: Option<String>,
    },
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    Complete(EntityArgs),
}

#[derive(Clone, Debug, Subcommand)]
pub enum MemoryCommand {
    Search(SearchArgs),
}

#[derive(Clone, Debug, Subcommand)]
pub enum ConfigCommand {
    /// Writes the documented `cortexd.toml` template beside the database.
    Init,
}

#[derive(Clone, Debug, Subcommand)]
pub enum SecretCommand {
    /// Imports a provider credential from stdin into the OS keyring.
    Import {
        #[arg(long)]
        profile: String,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub enum RemoteCommand {
    /// Enrolls one OIDC subject through the authenticated local owner daemon.
    Enroll(RemoteEnrollArgs),
}

#[derive(Clone, Debug, Args)]
pub struct RemoteEnrollArgs {
    #[arg(long)]
    pub subject: String,
    #[arg(long = "grant", required = true)]
    pub grants: Vec<String>,
}

#[derive(Clone, Debug, Args)]
pub struct SearchArgs {
    pub query: String,
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

#[derive(Clone, Debug, Args)]
pub struct EntityArgs {
    pub entity_id: Uuid,
    #[arg(long)]
    pub revision: u64,
}

#[derive(Clone, Debug, Args)]
pub struct RememberArgs {
    pub statement: String,
    #[arg(long)]
    pub subject: String,
    #[arg(long)]
    pub predicate: String,
    #[arg(long)]
    pub object: String,
    #[arg(long = "source", required = true)]
    pub sources: Vec<Uuid>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRequest {
    pub request_id: Uuid,
    pub operation_id: Uuid,
    pub capability: String,
    pub payload: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum CliCommandError {
    #[error("invalid command input")]
    InvalidInput,
}

/// Converts locally parsed user input to the daemon's typed, bounded wire payload.
///
/// # Errors
///
/// Returns an error when a user-supplied limit or due date is invalid.
pub fn command_request(cli: &Cli) -> Result<CommandRequest, CliCommandError> {
    let Some(command) = &cli.command else {
        return Err(CliCommandError::InvalidInput);
    };
    let (capability, payload, _mutation) = match command {
        Command::Status => ("cortex_daemon_status", json!({}), false),
        Command::Doctor => ("cortex_daemon_doctor", json!({}), false),
        Command::Logs => ("cortex_daemon_logs", json!({}), false),
        Command::Note(NoteCommand::Create { title, content }) => (
            "cortex_note_create",
            json!({"title": title, "content": content}),
            true,
        ),
        Command::Note(NoteCommand::Search(input)) => search("cortex_note_search", input)?,
        Command::Task(TaskCommand::Add { title, due }) => (
            "cortex_task_create",
            json!({"title": title, "due_at": due.as_deref().map(normalize_due).transpose()?}),
            true,
        ),
        Command::Task(TaskCommand::List { limit }) => task_list(*limit)?,
        Command::Task(TaskCommand::Complete(input)) => (
            "cortex_task_complete",
            json!({"entity_id": input.entity_id, "expected_revision": input.revision}),
            true,
        ),
        Command::Remember(input) => (
            "cortex_memory_create",
            json!({"statement": input.statement, "normalized_subject": input.subject, "normalized_predicate": input.predicate, "normalized_object": input.object, "sources": input.sources.iter().map(|source_id| json!({"source_id":source_id})).collect::<Vec<_>>() }),
            true,
        ),
        Command::Memory(MemoryCommand::Search(input)) => search("cortex_memory_search", input)?,
        Command::Remote(RemoteCommand::Enroll(input)) => (
            "cortex_remote_enroll",
            json!({"subject": input.subject, "grants": input.grants}),
            true,
        ),
        Command::Ask { prompt } => ("cortex_agent_run", json!({"prompt":prompt}), false),
        // `config` and `secret` are local client operations handled by the
        // binary before any daemon request is built.
        Command::Config(_) | Command::Secret(_) => {
            return Err(CliCommandError::InvalidInput);
        }
    };
    let operation_id = Uuid::now_v7();
    Ok(CommandRequest {
        request_id: Uuid::now_v7(),
        operation_id,
        capability: capability.to_owned(),
        payload,
    })
}

fn search<'a>(
    capability: &'a str,
    input: &SearchArgs,
) -> Result<(&'a str, Value, bool), CliCommandError> {
    bounded(capability, &input.query, input.limit)
}
fn bounded<'a>(
    capability: &'a str,
    query: &str,
    limit: usize,
) -> Result<(&'a str, Value, bool), CliCommandError> {
    if limit == 0 || limit > 100 {
        return Err(CliCommandError::InvalidInput);
    }
    Ok((capability, json!({"query":query,"limit":limit}), false))
}

fn task_list(limit: usize) -> Result<(&'static str, Value, bool), CliCommandError> {
    if limit == 0 || limit > 100 {
        return Err(CliCommandError::InvalidInput);
    }
    Ok(("cortex_task_list", json!({"limit":limit}), false))
}

fn normalize_due(value: &str) -> Result<String, CliCommandError> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|date| date.and_utc().to_rfc3339())
        .ok_or(CliCommandError::InvalidInput)
}

impl CommandRequest {
    #[must_use]
    pub fn into_daemon_request(self, principal_id: Uuid) -> cortexd::DaemonRequest {
        cortexd::DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: self.request_id,
            principal_id,
            operation_id: self.operation_id,
            capability: self.capability,
            payload: self.payload,
        }
    }
}
