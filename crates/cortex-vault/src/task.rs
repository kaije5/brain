//! Brain-managed task files (format spec §6): frontmatter → provider-neutral
//! task contract fields, and back.
//!
//! [`ParsedTask`] is deliberately context-free: the vault format layer knows
//! nothing about workspace or provider, so it produces the contract's
//! component types and the original `brain_id` text. The vault provider
//! (SCRUM-91) assembles the full `ProviderTask` with workspace/provider
//! provenance around it.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use cortex_application::{ProviderTaskPriority, ProviderTaskStatus, TaskSchedulingMetadata};
use cortex_domain::{ProviderResourceId, TaskId};

use crate::document::ParsedDocument;
use crate::error::VaultFormatError;
use crate::frontmatter::FrontmatterBlock;

/// The managed task frontmatter keys in canonical §6.1 order.
pub const TASK_MANAGED_KEYS: [&str; 11] = [
    "type",
    "brain_id",
    "status",
    "priority",
    "due",
    "deadline",
    "duration_minutes",
    "earliest_start",
    "split",
    "project",
    "context",
];

const MAX_PROJECT_CONTEXT_BYTES: usize = 256;
const MAX_DURATION_MINUTES: u32 = 1440;

/// A parsed Brain-managed task file: contract component fields plus the
/// original `brain_id` text. Identity comes exclusively from `brain_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedTask {
    brain_id: String,
    task_id: TaskId,
    resource_id: ProviderResourceId,
    status: ProviderTaskStatus,
    priority: ProviderTaskPriority,
    scheduling: TaskSchedulingMetadata,
    resolved_title: String,
    body: String,
}

impl ParsedTask {
    /// The `brain_id` exactly as written in the file.
    #[must_use]
    pub fn brain_id(&self) -> &str {
        &self.brain_id
    }

    /// The stable task identity derived from `brain_id`.
    #[must_use]
    pub const fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    /// The stable provider resource identity derived from `brain_id`.
    #[must_use]
    pub const fn resource_id(&self) -> &ProviderResourceId {
        &self.resource_id
    }

    #[must_use]
    pub const fn status(&self) -> ProviderTaskStatus {
        self.status
    }

    #[must_use]
    pub const fn priority(&self) -> ProviderTaskPriority {
        self.priority
    }

    #[must_use]
    pub const fn scheduling(&self) -> &TaskSchedulingMetadata {
        &self.scheduling
    }

    /// The resolved title (frontmatter `title`, first heading, else the
    /// file-stem fallback captured at parse time).
    #[must_use]
    pub fn title(&self) -> &str {
        &self.resolved_title
    }

    /// The verbatim body (human-readable context; Brain never edits it).
    #[must_use]
    pub fn body(&self) -> &str {
        &self.body
    }
}

/// Parses a Brain-managed task file into contract component fields.
///
/// # Errors
/// Returns typed [`VaultFormatError`] values: document-level format errors,
/// `DuplicateProperty` for repeated managed keys, `MissingProperty` for
/// absent `brain_id`/`status`/`priority`, and `InvalidProperty` for values
/// outside the spec §6 types and bounds.
pub fn parse_task(
    document: &ParsedDocument,
    file_stem: &str,
) -> Result<ParsedTask, VaultFormatError> {
    let block = document
        .frontmatter()
        .ok_or(VaultFormatError::MissingProperty { field: "brain_id" })?;

    // Repeated managed keys cannot be rewritten without guessing intent.
    for key in TASK_MANAGED_KEYS {
        if block
            .properties()
            .iter()
            .filter(|property| property.key == key)
            .count()
            > 1
        {
            return Err(VaultFormatError::DuplicateProperty { field: key });
        }
    }

    // `type: task` marks a Brain-managed task file.
    match block.scalar("type") {
        Some("task") => {}
        _ => {
            return Err(VaultFormatError::InvalidProperty { field: "type" });
        }
    }

    let brain_id = block
        .scalar("brain_id")
        .ok_or(VaultFormatError::MissingProperty { field: "brain_id" })?;
    let task_id = parse_brain_id(brain_id)?;
    // Identity is canonical: hyphenated and compact spellings of the same
    // UUID are the same task (format spec §6.2). The raw `brain_id` text is
    // preserved for rewrites so files are not reformatted needlessly.
    let canonical_id = uuid::Uuid::from(task_id).hyphenated().to_string();

    let status = match block
        .scalar("status")
        .ok_or(VaultFormatError::MissingProperty { field: "status" })?
    {
        "todo" => ProviderTaskStatus::Todo,
        "in_progress" => ProviderTaskStatus::InProgress,
        "done" => ProviderTaskStatus::Completed,
        "cancelled" => ProviderTaskStatus::Cancelled,
        _ => return Err(VaultFormatError::InvalidProperty { field: "status" }),
    };

    let priority = match block
        .scalar("priority")
        .ok_or(VaultFormatError::MissingProperty { field: "priority" })?
    {
        "low" => ProviderTaskPriority::Low,
        "normal" => ProviderTaskPriority::Normal,
        "high" => ProviderTaskPriority::High,
        "urgent" => ProviderTaskPriority::Urgent,
        _ => return Err(VaultFormatError::InvalidProperty { field: "priority" }),
    };

    let due = optional_instant(block, "due")?;
    let deadline = optional_instant(block, "deadline")?;
    let earliest_start = optional_instant(block, "earliest_start")?;
    let duration_minutes = optional_duration(block)?;
    let split = optional_bool(block, "split")?;
    let project = optional_label(block, "project")?;
    let context = optional_label(block, "context")?;

    let scheduling = TaskSchedulingMetadata::new(
        due,
        deadline,
        duration_minutes,
        earliest_start,
        split,
        project,
        context,
    )
    .map_err(|_| VaultFormatError::InvalidProperty {
        field: "scheduling",
    })?;

    Ok(ParsedTask {
        brain_id: brain_id.to_owned(),
        task_id,
        resource_id: ProviderResourceId::new(&canonical_id)
            .map_err(|_| VaultFormatError::InvalidProperty { field: "brain_id" })?,
        status,
        priority,
        scheduling,
        resolved_title: document.title(file_stem).to_string(),
        body: document.body().to_owned(),
    })
}

/// Serializes the canonical §6.1 frontmatter for a task, optionally
/// preserving the unknown properties of an existing block.
///
/// # Errors
/// Returns typed [`VaultFormatError`] values when the existing block carries
/// duplicate managed keys.
pub fn serialize_task_frontmatter(
    task: &ParsedTask,
    existing: Option<&FrontmatterBlock>,
) -> Result<String, VaultFormatError> {
    let mut managed: Vec<(&str, String)> = vec![
        ("type", "task".to_owned()),
        ("brain_id", task.brain_id.clone()),
        ("status", status_text(task.status).to_owned()),
        ("priority", priority_text(task.priority).to_owned()),
    ];
    let scheduling = task.scheduling();
    if let Some(due) = scheduling.due_at() {
        managed.push(("due", format_instant(due)));
    }
    if let Some(deadline) = scheduling.deadline_at() {
        managed.push(("deadline", format_instant(deadline)));
    }
    if let Some(duration) = scheduling.duration_minutes() {
        managed.push(("duration_minutes", duration.get().to_string()));
    }
    if let Some(start) = scheduling.earliest_start() {
        managed.push(("earliest_start", format_instant(start)));
    }
    if let Some(split) = scheduling.split() {
        managed.push(("split", bool_text(split).to_owned()));
    }
    if let Some(project) = scheduling.project() {
        managed.push(("project", format_scalar(project)));
    }
    if let Some(context) = scheduling.context() {
        managed.push(("context", format_scalar(context)));
    }
    if let Some(block) = existing {
        return block.with_managed_properties(&managed);
    }
    let mut output = String::new();
    for (key, value) in managed {
        output.push_str(key);
        output.push_str(": ");
        output.push_str(&value);
        output.push('\n');
    }
    Ok(output)
}

/// Recomposes a full task file from the rewritten task and the existing
/// document: canonical managed frontmatter, the existing document's unknown
/// properties preserved byte-for-byte, and the body verbatim (format spec
/// §7). Renames and file moves never participate — identity is `brain_id`.
///
/// # Errors
/// Returns [`VaultFormatError::IdentityConflict`] when the existing file
/// carries a `brain_id` that parses to a *different* task identity: Brain
/// never overwrites one task's identity with another's.
pub fn rewrite_task_file(
    existing: &ParsedDocument,
    task: &ParsedTask,
) -> Result<String, VaultFormatError> {
    if let Some(block) = existing.frontmatter()
        && let Some(existing_id) = block.scalar("brain_id")
    {
        let existing_task_id = parse_brain_id(existing_id)?;
        if existing_task_id != *task.task_id() {
            return Err(VaultFormatError::IdentityConflict);
        }
    }
    let frontmatter = serialize_task_frontmatter(task, existing.frontmatter())?;
    let mut output = String::with_capacity(frontmatter.len() + task.body().len() + 8);
    output.push_str(
        "---
",
    );
    output.push_str(&frontmatter);
    output.push_str(
        "---
",
    );
    output.push_str(existing.body());
    Ok(output)
}

fn parse_brain_id(brain_id: &str) -> Result<TaskId, VaultFormatError> {
    let normalized = brain_id.replace('-', "");
    let uuid = uuid::Uuid::parse_str(&normalized)
        .or_else(|_| uuid::Uuid::parse_str(brain_id))
        .map_err(|_| VaultFormatError::InvalidProperty { field: "brain_id" })?;
    TaskId::try_from(uuid).map_err(|_| VaultFormatError::InvalidProperty { field: "brain_id" })
}

fn optional_instant(
    block: &FrontmatterBlock,
    key: &'static str,
) -> Result<Option<DateTime<Utc>>, VaultFormatError> {
    let Some(value) = block.scalar(key) else {
        return Ok(None);
    };
    // Date-only form: midnight UTC.
    if let Ok(date) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok(Some(
            date.and_hms_opt(0, 0, 0)
                .expect("midnight is valid")
                .and_utc(),
        ));
    }
    // Offset-free instant: interpreted as UTC.
    if let Ok(naive) = NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S") {
        return Ok(Some(naive.and_utc()));
    }
    // Full RFC 3339 with offset.
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Ok(Some(parsed.with_timezone(&Utc)));
    }
    Err(VaultFormatError::InvalidProperty { field: key })
}

fn optional_duration(
    block: &FrontmatterBlock,
) -> Result<Option<std::num::NonZeroU32>, VaultFormatError> {
    let Some(value) = block.scalar("duration_minutes") else {
        return Ok(None);
    };
    let minutes: u32 = value
        .parse()
        .map_err(|_| VaultFormatError::InvalidProperty {
            field: "duration_minutes",
        })?;
    if minutes == 0 || minutes > MAX_DURATION_MINUTES {
        return Err(VaultFormatError::InvalidProperty {
            field: "duration_minutes",
        });
    }
    Ok(Some(
        std::num::NonZeroU32::new(minutes).expect("non-zero checked above"),
    ))
}

fn optional_bool(
    block: &FrontmatterBlock,
    key: &'static str,
) -> Result<Option<bool>, VaultFormatError> {
    match block.scalar(key) {
        None => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(VaultFormatError::InvalidProperty { field: key }),
    }
}

fn optional_label<'a>(
    block: &'a FrontmatterBlock,
    key: &'static str,
) -> Result<Option<&'a str>, VaultFormatError> {
    match block.scalar(key) {
        None => Ok(None),
        Some(value) if value.is_empty() || value.chars().any(char::is_control) => {
            Err(VaultFormatError::InvalidProperty { field: key })
        }
        Some(value) if value.len() > MAX_PROJECT_CONTEXT_BYTES => {
            Err(VaultFormatError::InvalidProperty { field: key })
        }
        Some(value) => Ok(Some(value)),
    }
}

fn status_text(status: ProviderTaskStatus) -> &'static str {
    match status {
        ProviderTaskStatus::Todo => "todo",
        ProviderTaskStatus::InProgress => "in_progress",
        ProviderTaskStatus::Completed => "done",
        ProviderTaskStatus::Cancelled => "cancelled",
    }
}

fn priority_text(priority: ProviderTaskPriority) -> &'static str {
    match priority {
        ProviderTaskPriority::Low => "low",
        ProviderTaskPriority::Normal => "normal",
        ProviderTaskPriority::High => "high",
        ProviderTaskPriority::Urgent => "urgent",
    }
}

const fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

/// Date-only output when the instant is midnight UTC, RFC 3339 otherwise.
fn format_instant(instant: DateTime<Utc>) -> String {
    if instant.time() == chrono::NaiveTime::MIN {
        instant.format("%Y-%m-%d").to_string()
    } else {
        instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }
}

/// Quotes a scalar when it could otherwise be misread as another YAML type.
fn format_scalar(value: &str) -> String {
    let needs_quotes = value.parse::<f64>().is_ok()
        || matches!(value, "true" | "false" | "null" | "~")
        || value.starts_with('[')
        || value.starts_with('{')
        || value.starts_with('\'')
        || value.starts_with('"')
        || value.starts_with('#')
        || value.starts_with(' ')
        || value.ends_with(' ');
    if needs_quotes {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_owned()
    }
}
