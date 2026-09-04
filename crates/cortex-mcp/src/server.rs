use std::time::Duration;

use cortexd::{AuthenticatedLocalClient, DaemonRequest, PROTOCOL_VERSION, WireResult};
use rmcp::{
    ErrorData as RmcpError, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResult, ListToolsResult, ServerCapabilities, ServerInfo,
        Tool, ToolAnnotations, object,
    },
    transport::{StreamableHttpServerConfig, StreamableHttpService},
};
use serde_json::{Value, json};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{McpError, tool_schema};

const MAX_TOOL_BYTES: usize = 32 * 1024;
const TOOL_TIMEOUT: Duration = Duration::from_secs(5);

/// An authenticated identity supplied by the gateway after its external authentication layer.
/// It has no public constructor from untrusted tool arguments.
#[derive(Clone)]
pub struct McpPrincipal {
    client: AuthenticatedLocalClient,
}

impl McpPrincipal {
    #[must_use]
    pub fn from_authenticated(client: AuthenticatedLocalClient) -> Self {
        Self { client }
    }
}

/// Local MCP adaptation over the daemon-owned local IPC boundary.
#[derive(Clone)]
pub struct McpServer {
    principal: McpPrincipal,
}

impl McpServer {
    #[must_use]
    pub fn new(principal: McpPrincipal) -> Self {
        Self { principal }
    }

    /// Calls a declared MCP tool through authenticated daemon IPC.
    ///
    /// # Errors
    /// Returns a stable, redacted adapter error for invalid input, denial, timeout, or daemon failure.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> Result<Value, McpError> {
        let schema = tool_schema(name).ok_or_else(McpError::invalid_input)?;
        validate_arguments(&schema.input_properties, &arguments)?;
        if serialized_len(&arguments)? > MAX_TOOL_BYTES {
            return Err(McpError::invalid_input());
        }
        let request = DaemonRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            // The daemon deliberately ignores this value after local authentication.
            principal_id: Uuid::now_v7(),
            operation_id: Uuid::now_v7(),
            capability: schema.name,
            payload: arguments,
        };
        let response = timeout(TOOL_TIMEOUT, self.principal.client.request(&request))
            .await
            .map_err(|_| McpError::unavailable())?
            .map_err(|_| McpError::unavailable())?;
        match response.result {
            WireResult::Success { value } if serialized_len(&value)? <= MAX_TOOL_BYTES => Ok(value),
            WireResult::Success { .. } => Err(McpError::unavailable()),
            WireResult::Error { code } => Err(map_wire_error(&code)),
        }
    }
}

/// HTTP constraints to be applied by the loopback gateway when exposing RMCP's service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpSecurityConfig {
    pub allowed_hosts: Vec<String>,
    pub allowed_origins: Vec<String>,
    pub max_request_body_bytes: usize,
    pub cancellation_token: CancellationToken,
}

impl HttpSecurityConfig {
    #[must_use]
    pub fn loopback(cancellation_token: CancellationToken) -> Self {
        Self {
            allowed_hosts: vec!["127.0.0.1".to_owned(), "localhost".to_owned()],
            allowed_origins: vec!["http://127.0.0.1".to_owned(), "http://localhost".to_owned()],
            max_request_body_bytes: MAX_TOOL_BYTES,
            cancellation_token,
        }
    }

    #[must_use]
    pub fn accepts(&self, host: &str, origin: Option<&str>, body_bytes: usize) -> bool {
        body_bytes <= self.max_request_body_bytes
            && self.allowed_hosts.iter().any(|allowed| allowed == host)
            && origin
                .is_none_or(|value| self.allowed_origins.iter().any(|allowed| value == allowed))
    }
}

/// Produces the RMCP Streamable HTTP configuration used by the Task 12 loopback server.
/// Version 0.16 delegates host/origin/body checks to the surrounding HTTP layer, while RMCP
/// owns session lifecycle and cancellation.
#[must_use]
pub fn rmcp_streamable_http_config(config: &HttpSecurityConfig) -> StreamableHttpServerConfig {
    StreamableHttpServerConfig {
        cancellation_token: config.cancellation_token.clone(),
        stateful_mode: false,
        ..StreamableHttpServerConfig::default()
    }
}

/// Builds RMCP's Streamable HTTP service. The caller must apply `HttpSecurityConfig::accepts`
/// at its HTTP boundary before requests reach this service.
#[must_use]
pub fn streamable_http_service(
    principal: McpPrincipal,
    security: &HttpSecurityConfig,
) -> StreamableHttpService<McpServer> {
    let configuration = rmcp_streamable_http_config(security);
    StreamableHttpService::new(
        move || Ok(McpServer::new(principal.clone())),
        std::sync::Arc::default(),
        configuration,
    )
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            instructions: Some("Cortex v0.1 local knowledge tools.".to_owned()),
            ..ServerInfo::default()
        }
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, RmcpError> {
        Ok(ListToolsResult {
            tools: rmcp_tools(),
            ..ListToolsResult::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, RmcpError> {
        let arguments = request.arguments.map_or_else(|| json!({}), Value::Object);
        match self.call_tool(&request.name, arguments).await {
            Ok(value) => Ok(CallToolResult::structured(value)),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "code": error.code,
                "message": error.message,
            }))),
        }
    }
}

fn rmcp_tools() -> Vec<Tool> {
    crate::tool_schemas()
        .into_iter()
        .map(|schema| {
            let properties = schema
                .input_properties
                .into_keys()
                .map(|name| (name, json!({"type": "string"})))
                .collect::<serde_json::Map<String, Value>>();
            Tool::new(
                schema.name,
                schema.description,
                object(json!({
                    "type": "object",
                    "properties": properties,
                    "additionalProperties": false,
                })),
            )
            .annotate(
                ToolAnnotations::new()
                    .read_only(!schema.destructive)
                    .destructive(schema.destructive)
                    .idempotent(false)
                    .open_world(false),
            )
        })
        .collect()
}

fn validate_arguments(
    properties: &std::collections::BTreeMap<String, String>,
    arguments: &Value,
) -> Result<(), McpError> {
    let object = arguments.as_object().ok_or_else(McpError::invalid_input)?;
    if object.keys().any(|key| !properties.contains_key(key)) {
        return Err(McpError::invalid_input());
    }
    Ok(())
}

fn serialized_len(value: &Value) -> Result<usize, McpError> {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .map_err(|_| McpError::invalid_input())
}

fn map_wire_error(code: &str) -> McpError {
    match code {
        "permission_denied" | "unauthenticated" => McpError::permission_denied(),
        "invalid_request" | "unsupported_capability" => McpError::invalid_input(),
        _ => McpError::unavailable(),
    }
}
