use cortex_application::ApplicationError;
use uuid::Uuid;

/// A stable, deliberately non-diagnostic error suitable for an MCP client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpError {
    pub code: String,
    pub message: String,
    pub correlation_id: Option<Uuid>,
}

impl McpError {
    #[must_use]
    pub fn invalid_input() -> Self {
        Self {
            code: "cortex_invalid_input".to_owned(),
            message: "The supplied tool input is invalid.".to_owned(),
            correlation_id: None,
        }
    }

    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            code: "cortex_unavailable".to_owned(),
            message: "Cortex is temporarily unavailable.".to_owned(),
            correlation_id: None,
        }
    }

    #[must_use]
    pub fn permission_denied() -> Self {
        Self {
            code: "cortex_permission_denied".to_owned(),
            message: "This principal is not permitted to perform that operation.".to_owned(),
            correlation_id: None,
        }
    }

    #[must_use]
    pub(crate) const fn with_correlation(mut self, correlation_id: Uuid) -> Self {
        self.correlation_id = Some(correlation_id);
        self
    }
}

/// Converts application categories to public MCP errors without retaining diagnostics.
#[must_use]
pub fn map_application_error(error: &ApplicationError) -> McpError {
    match error {
        ApplicationError::Validation { .. }
        | ApplicationError::NotFound { .. }
        | ApplicationError::Conflict { .. }
        | ApplicationError::MalformedModelOutput { .. } => McpError::invalid_input(),
        ApplicationError::PermissionDenied | ApplicationError::PolicyDenied(_) => {
            McpError::permission_denied()
        }
        ApplicationError::InferenceUnavailable
        | ApplicationError::InferenceTimeout
        | ApplicationError::NoSuitableModel => McpError::unavailable(),
        ApplicationError::Storage(_) | ApplicationError::Internal => McpError {
            code: "cortex_internal_error".to_owned(),
            message: "Cortex could not complete that operation.".to_owned(),
            correlation_id: None,
        },
    }
}
