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
        return format!("error: {}\n", error.code);
    }
    value
        .data
        .as_ref()
        .map_or_else(|| "\n".to_owned(), render_value)
}
fn render_value(value: &Value) -> String {
    match value {
        Value::Array(values) => values.iter().map(render_hit).collect(),
        _ => format!("{value}\n"),
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
