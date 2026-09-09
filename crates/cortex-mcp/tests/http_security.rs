use bytes::Bytes;
use cortex_mcp::{HttpSecurityConfig, streamable_http_service};
use http::{Request, StatusCode, header};
use http_body_util::{BodyExt, Full};
use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use tower_service::Service;

const MCP_ACCEPT: &str = "application/json, text/event-stream";

fn request(host: &str, origin: Option<&str>, body: impl Into<Bytes>) -> Request<Full<Bytes>> {
    let mut builder = Request::post("http://localhost/mcp")
        .header(header::HOST, host)
        .header(header::ACCEPT, MCP_ACCEPT)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    builder
        .body(Full::new(body.into()))
        .expect("test request is valid")
}

async fn body_text<B>(response: http::Response<B>) -> String
where
    B: http_body::Body<Data = Bytes>,
    B::Error: std::fmt::Debug,
{
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body can be collected")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("response is UTF-8")
}

#[tokio::test]
async fn returned_service_rejects_non_loopback_host_before_rmcp_parsing() {
    let mut service =
        streamable_http_service(&HttpSecurityConfig::loopback(CancellationToken::new()));
    let response = service
        .call(request("attacker.example", None, "not-json"))
        .await
        .expect("service is infallible");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_text(response).await;
    assert!(body.contains("cortex_transport_rejected"));
    assert!(!body.contains("attacker.example"));
}

#[tokio::test]
async fn returned_service_rejects_untrusted_origin_before_rmcp_parsing() {
    let mut service =
        streamable_http_service(&HttpSecurityConfig::loopback(CancellationToken::new()));
    let response = service
        .call(request(
            "localhost",
            Some("http://localhost.attacker"),
            "not-json",
        ))
        .await
        .expect("service is infallible");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = body_text(response).await;
    assert!(body.contains("cortex_transport_rejected"));
    assert!(!body.contains("localhost.attacker"));
}

#[tokio::test]
async fn returned_service_rejects_streamed_body_over_limit_before_rmcp_parsing() {
    let mut service =
        streamable_http_service(&HttpSecurityConfig::loopback(CancellationToken::new()));
    let response = service
        .call(request("localhost", None, vec![b'x'; 32 * 1024 + 1]))
        .await
        .expect("service is infallible");
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = body_text(response).await;
    assert!(body.contains("cortex_request_too_large"));
    assert!(!body.contains(&"x".repeat(64)));
}

#[tokio::test]
async fn returned_service_replaces_an_oversized_rmcp_response_with_a_bounded_error() {
    let config =
        HttpSecurityConfig::loopback(CancellationToken::new()).with_response_body_limit(512);
    let mut service = streamable_http_service(&config);
    let response = service
        .call(request(
            "localhost",
            None,
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#,
        ))
        .await
        .expect("service is infallible");
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body = body_text(response).await;
    assert!(body.len() <= 512);
    assert!(body.contains("cortex_response_too_large"));
}

#[tokio::test]
async fn returned_service_times_out_a_stalled_request_body_before_rmcp_parsing() {
    let config = HttpSecurityConfig::loopback(CancellationToken::new())
        .with_transport_timeout(Duration::from_millis(10));
    let mut service = streamable_http_service(&config);
    let request = Request::post("http://localhost/mcp")
        .header(header::HOST, "localhost")
        .header(header::ACCEPT, MCP_ACCEPT)
        .header(header::CONTENT_TYPE, "application/json")
        .body(PendingBody)
        .expect("test request is valid");
    let response = service.call(request).await.expect("service is infallible");
    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
    let body = body_text(response).await;
    assert!(body.contains("cortex_transport_timeout"));
}

struct PendingBody;

impl http_body::Body for PendingBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        Poll::Pending
    }
}
