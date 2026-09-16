#![forbid(unsafe_code)]

mod error;
mod server;
mod tools;

pub use error::{McpError, map_application_error};
pub use server::{
    HttpSecurityConfig, McpPrincipal, McpServer, SecureStreamableHttpService,
    streamable_http_service,
};
pub use tools::{ToolSchema, decode_arguments, tool_schema, tool_schemas, wire_capability};
