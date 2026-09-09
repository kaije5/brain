use std::{
    convert::Infallible,
    error::Error,
    fmt::Display,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::{Buf, Bytes};
use cortexd::{
    AuthenticatedIpcClient, AuthenticatedLocalClient, DaemonRequest, DaemonResponse,
    PROTOCOL_VERSION, WireResult,
};
use http::{Request, Response, StatusCode, header, uri::Authority};
use http_body::Body;
use http_body_util::{BodyExt, Full, Limited, combinators::BoxBody};
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
use tower_service::Service;
use uuid::Uuid;

use crate::tools::decode_arguments;
use crate::{McpError, tool_schema};

const MAX_TOOL_BYTES: usize = 32 * 1024;
const MAX_HTTP_RESPONSE_BYTES: usize = 64 * 1024;
const MIN_HTTP_RESPONSE_BYTES: usize = 512;
const TOOL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_TIMEOUT: Duration = Duration::from_secs(7);
const MIN_HTTP_TIMEOUT: Duration = Duration::from_millis(1);

/// An authenticated identity supplied by the gateway after its external authentication layer.
/// It has no public constructor from untrusted tool arguments.
#[derive(Clone)]
pub struct McpPrincipal {
    client: PrincipalClient,
}

#[derive(Clone)]
enum PrincipalClient {
    InProcess(std::sync::Arc<AuthenticatedLocalClient>),
    Ipc(std::sync::Arc<AuthenticatedIpcClient>),
}

impl McpPrincipal {
    #[must_use]
    pub fn from_authenticated(client: AuthenticatedLocalClient) -> Self {
        Self {
            client: PrincipalClient::InProcess(std::sync::Arc::new(client)),
        }
    }

    /// Creates a principal backed by the daemon's file-enrolled local IPC client.
    #[must_use]
    pub fn from_ipc(client: AuthenticatedIpcClient) -> Self {
        Self {
            client: PrincipalClient::Ipc(std::sync::Arc::new(client)),
        }
    }

    async fn request(
        &self,
        request: &DaemonRequest,
    ) -> Result<DaemonResponse, cortexd::DaemonError> {
        match &self.client {
            PrincipalClient::InProcess(client) => client.request(request).await,
            PrincipalClient::Ipc(client) => client.request(request).await,
        }
    }
}

/// Local MCP adaptation over the daemon-owned local IPC boundary.
#[derive(Clone, Default)]
pub struct McpServer;

impl McpServer {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Calls a declared MCP tool through authenticated daemon IPC.
    ///
    /// # Errors
    /// Returns a stable, redacted adapter error for invalid input, denial, timeout, or daemon failure.
    pub async fn call_tool_as(
        &self,
        principal: &McpPrincipal,
        name: &str,
        arguments: Value,
    ) -> Result<Value, McpError> {
        let schema = tool_schema(name).ok_or_else(McpError::invalid_input)?;
        let arguments = decode_arguments(name, arguments)?;
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
        let response = timeout(TOOL_TIMEOUT, principal.request(&request))
            .await
            .map_err(|_| McpError::unavailable().with_correlation(request.request_id))?
            .map_err(|_| McpError::unavailable().with_correlation(request.request_id))?;
        match response.result {
            WireResult::Success { value } if serialized_len(&value)? <= MAX_TOOL_BYTES => Ok(value),
            WireResult::Success { .. } => {
                Err(McpError::unavailable().with_correlation(request.request_id))
            }
            WireResult::Error { code } => {
                Err(map_wire_error(&code).with_correlation(request.request_id))
            }
        }
    }
}

/// HTTP constraints to be applied by the loopback gateway when exposing RMCP's service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpSecurityConfig {
    allowed_hosts: Vec<String>,
    allowed_origins: Vec<String>,
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
    transport_timeout: Duration,
    cancellation_token: CancellationToken,
}

impl HttpSecurityConfig {
    #[must_use]
    pub fn loopback(cancellation_token: CancellationToken) -> Self {
        Self {
            allowed_hosts: vec!["127.0.0.1".to_owned(), "localhost".to_owned()],
            allowed_origins: vec!["http://127.0.0.1".to_owned(), "http://localhost".to_owned()],
            max_request_body_bytes: MAX_TOOL_BYTES,
            max_response_body_bytes: MAX_HTTP_RESPONSE_BYTES,
            transport_timeout: MAX_HTTP_TIMEOUT,
            cancellation_token,
        }
    }

    /// Lowers the response-body limit for a deployment or boundary test. Values cannot raise the
    /// hard adapter cap or shrink below the fixed typed-error envelope.
    #[must_use]
    pub fn with_response_body_limit(mut self, bytes: usize) -> Self {
        self.max_response_body_bytes =
            bytes.clamp(MIN_HTTP_RESPONSE_BYTES, MAX_HTTP_RESPONSE_BYTES);
        self
    }

    /// Lowers the total time allowed to read a request or collect a stateless response.
    #[must_use]
    pub fn with_transport_timeout(mut self, duration: Duration) -> Self {
        self.transport_timeout = duration.clamp(MIN_HTTP_TIMEOUT, MAX_HTTP_TIMEOUT);
        self
    }
}

/// Produces the RMCP Streamable HTTP configuration used by the Task 12 loopback server.
/// Version 0.16 delegates host/origin/body checks to the surrounding HTTP layer, while RMCP
/// owns session lifecycle and cancellation.
#[must_use]
fn rmcp_streamable_http_config(config: &HttpSecurityConfig) -> StreamableHttpServerConfig {
    StreamableHttpServerConfig {
        cancellation_token: config.cancellation_token.clone(),
        sse_keep_alive: None,
        sse_retry: None,
        stateful_mode: false,
    }
}

/// The only public Streamable HTTP construction path. It validates the HTTP authority and origin,
/// fully bounds the request before RMCP parsing, and bounds the completed stateless response.
#[must_use]
pub fn streamable_http_service(security: &HttpSecurityConfig) -> SecureStreamableHttpService {
    let configuration = rmcp_streamable_http_config(security);
    SecureStreamableHttpService {
        inner: StreamableHttpService::new(
            || Ok(McpServer::new()),
            std::sync::Arc::default(),
            configuration,
        ),
        security: security.clone(),
    }
}

/// A transport-enforcing wrapper around RMCP's stateless Streamable HTTP service.
#[derive(Clone)]
pub struct SecureStreamableHttpService {
    inner: StreamableHttpService<McpServer>,
    security: HttpSecurityConfig,
}

impl<B> Service<Request<B>> for SecureStreamableHttpService
where
    B: Body + Send + 'static,
    B::Data: Buf + Send,
    B::Error: Display + Error + Send + Sync + 'static,
{
    type Response = Response<BoxBody<Bytes, Infallible>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move { Ok(service.handle(request).await) })
    }
}

impl SecureStreamableHttpService {
    async fn handle<B>(&self, request: Request<B>) -> Response<BoxBody<Bytes, Infallible>>
    where
        B: Body + Send + 'static,
        B::Data: Buf + Send,
        B::Error: Display + Error + Send + Sync + 'static,
    {
        if !valid_host(&request, &self.security.allowed_hosts)
            || !valid_origin(&request, &self.security.allowed_origins)
        {
            return transport_error(StatusCode::FORBIDDEN, "cortex_transport_rejected");
        }
        if content_length_exceeds(&request, self.security.max_request_body_bytes) {
            return transport_error(StatusCode::PAYLOAD_TOO_LARGE, "cortex_request_too_large");
        }

        let (parts, body) = request.into_parts();
        let body = match timeout(
            self.security.transport_timeout,
            Limited::new(body, self.security.max_request_body_bytes).collect(),
        )
        .await
        {
            Ok(Ok(collected)) => collected.to_bytes(),
            Ok(Err(_)) => {
                return transport_error(StatusCode::PAYLOAD_TOO_LARGE, "cortex_request_too_large");
            }
            Err(_) => {
                return transport_error(StatusCode::REQUEST_TIMEOUT, "cortex_transport_timeout");
            }
        };
        let request = Request::from_parts(parts, Full::new(body));
        let response = self.inner.handle(request).await;
        bound_response(
            response,
            self.security.max_response_body_bytes,
            self.security.transport_timeout,
        )
        .await
    }
}

fn valid_host<B>(request: &Request<B>, allowed_hosts: &[String]) -> bool {
    request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<Authority>().ok())
        .is_some_and(|authority| {
            allowed_hosts
                .iter()
                .any(|allowed| authority.host().eq_ignore_ascii_case(allowed))
        })
}

fn valid_origin<B>(request: &Request<B>, allowed_origins: &[String]) -> bool {
    request.headers().get(header::ORIGIN).is_none_or(|origin| {
        origin.to_str().ok().is_some_and(|origin| {
            allowed_origins
                .iter()
                .any(|allowed| origin == allowed.as_str())
        })
    })
}

fn content_length_exceeds<B>(request: &Request<B>, limit: usize) -> bool {
    request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > limit)
}

async fn bound_response(
    response: Response<BoxBody<Bytes, Infallible>>,
    limit: usize,
    transport_timeout: Duration,
) -> Response<BoxBody<Bytes, Infallible>> {
    let (parts, body) = response.into_parts();
    match timeout(transport_timeout, Limited::new(body, limit).collect()).await {
        Ok(Ok(collected)) => Response::from_parts(parts, Full::new(collected.to_bytes()).boxed()),
        Ok(Err(_)) => transport_error(StatusCode::BAD_GATEWAY, "cortex_response_too_large"),
        Err(_) => transport_error(StatusCode::GATEWAY_TIMEOUT, "cortex_transport_timeout"),
    }
}

fn transport_error(status: StatusCode, code: &'static str) -> Response<BoxBody<Bytes, Infallible>> {
    let body = serde_json::to_vec(&json!({
        "error": {
            "code": code,
            "message": "The MCP transport rejected the request."
        }
    }))
    .unwrap_or_default();
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
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
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, RmcpError> {
        let arguments = request.arguments.map_or_else(|| json!({}), Value::Object);
        let Some(parts) = context.extensions.get::<http::request::Parts>() else {
            return Ok(CallToolResult::structured_error(
                json!({"code":"cortex_permission_denied","message":"This principal is not permitted to perform that operation."}),
            ));
        };
        let Some(principal) = parts.extensions.get::<McpPrincipal>() else {
            return Ok(CallToolResult::structured_error(
                json!({"code":"cortex_permission_denied","message":"This principal is not permitted to perform that operation."}),
            ));
        };
        match self.call_tool_as(principal, &request.name, arguments).await {
            Ok(value) => Ok(CallToolResult::structured(value)),
            Err(error) => Ok(CallToolResult::structured_error(json!({
                "code":error.code,
                "message":error.message,
                "correlation_id":error.correlation_id.map(|value| value.to_string())
            }))),
        }
    }
}

fn rmcp_tools() -> Vec<Tool> {
    crate::tool_schemas()
        .into_iter()
        .map(|schema| {
            Tool::new(schema.name, schema.description, object(schema.input_schema)).annotate(
                ToolAnnotations::new()
                    .read_only(!schema.destructive)
                    .destructive(schema.destructive)
                    .idempotent(false)
                    .open_world(false),
            )
        })
        .collect()
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
