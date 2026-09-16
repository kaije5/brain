use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct CliErrorView {
    pub code: String,
}
#[derive(Debug, Serialize)]
pub struct CliEnvelope<T> {
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<CliErrorView>,
}
impl<T> CliEnvelope<T> {
    #[must_use]
    pub fn success(data: T) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }
    #[must_use]
    pub fn error(code: &str) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(CliErrorView {
                code: code.to_owned(),
            }),
        }
    }
}
/// Renders the stable, machine-readable CLI result envelope.
///
/// # Errors
///
/// Returns an error only when the supplied result cannot be represented as JSON.
pub fn render_json<T: Serialize>(value: &CliEnvelope<T>) -> Result<String, serde_json::Error> {
    serde_json::to_string(value).map(|text| format!("{text}\n"))
}
#[must_use]
pub fn render_text(value: &CliEnvelope<Value>) -> String {
    if let Some(error) = &value.error {
        return format!("error: {} ({})\n", error.code, error_hint(&error.code));
    }
    value
        .data
        .as_ref()
        .map_or_else(|| "\n".to_owned(), render_value)
}
fn render_value(value: &Value) -> String {
    match value {
        Value::Array(values) => values.iter().map(render_hit).collect(),
        // Vault task lists are freshness-tagged envelopes of typed rows.
        Value::Object(object) if object.contains_key("tasks") => render_task_list(value),
        _ => format!("{value}\n"),
    }
}

/// Human rendering of a vault task list: one row per task with its status,
/// opaque revision prefix, and an explicit degraded-state warning when the
/// freshness tag reports a stale index.
fn render_task_list(value: &Value) -> String {
    let mut rendered = String::new();
    if value.get("freshness").and_then(Value::as_str) == Some("stale") {
        rendered
            .push_str("warning: vault index is stale; results may lag recent edits (degraded)\n");
    }
    if let Some(tasks) = value.get("tasks").and_then(Value::as_array) {
        for task in tasks {
            let title = task.get("title").and_then(Value::as_str).unwrap_or("");
            let status = task
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let revision = task
                .get("revision")
                .and_then(Value::as_str)
                .map(|revision| format!(" (rev {})", revision.get(..8).unwrap_or(revision)))
                .unwrap_or_default();
            let due = task
                .get("due_at")
                .and_then(Value::as_str)
                .map(|due| format!(" due {due}"))
                .unwrap_or_default();
            let _ = writeln!(rendered, "[{status}]{due} {title}{revision}");
        }
    }
    rendered
}

/// The safe recovery hint for a typed wire error code.
#[must_use]
pub fn error_hint(code: &str) -> &'static str {
    match code {
        "conflict" => {
            "the resource changed since you last observed it; refresh the revision and retry"
        }
        "not_found" => "the addressed resource does not exist",
        "permission_denied" => "your principal lacks the capability grant for this operation",
        "rate_limited" => "the provider throttled the request; retry later",
        _ => "the operation failed; see the code for the category",
    }
}
fn render_hit(value: &Value) -> String {
    let snippet = value
        .get("snippet")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source =
        value
            .get("sources")
            .and_then(Value::as_array)
            .map_or_else(String::new, |sources| {
                format!(
                    " [{}]",
                    sources
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            });
    format!("{snippet}{source}\n")
}
