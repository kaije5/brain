use cortex_application::ApplicationError;

/// A stable, deliberately non-diagnostic error suitable for an MCP client.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpError {
    pub code: String,
    pub message: String,
}

impl McpError {
    #[must_use]
    pub fn invalid_input() -> Self {
        Self {
            code: "cortex_invalid_input".to_owned(),
            message: "The supplied tool input is invalid.".to_owned(),
        }
    }

    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            code: "cortex_unavailable".to_owned(),
            message: "Cortex is temporarily unavailable.".to_owned(),
        }
    }

    #[must_use]
    pub fn permission_denied() -> Self {
        Self {
            code: "cortex_permission_denied".to_owned(),
            message: "This principal is not permitted to perform that operation.".to_owned(),
        }
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
        ApplicationError::InferenceUnavailable | ApplicationError::InferenceTimeout => {
            McpError::unavailable()
        }
        ApplicationError::Storage(_) | ApplicationError::Internal => McpError {
            code: "cortex_internal_error".to_owned(),
            message: "Cortex could not complete that operation.".to_owned(),
        },
    }
}
