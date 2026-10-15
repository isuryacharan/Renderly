//! The in-process MCP endpoint for one agent session: MCP Streamable HTTP served by rmcp on
//! `127.0.0.1` at a random port, guarded by a per-session bearer token. Claude Code connects
//! to it with an `http` entry in its `--mcp-config`. The server lives exactly as long as the
//! session: stopping the session cancels the accept loop and every connection.

use super::permission::PermissionBroker;
use super::tools::RenderlyTools;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::AUTHORIZATION;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use rmcp::transport::streamable_http_server::{
    session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
};
use std::convert::Infallible;
use std::sync::Arc;
use tauri::AppHandle;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_service::Service;

type BoxBody = http_body_util::combinators::BoxBody<Bytes, Infallible>;

/// A running session endpoint. Dropping it stops the server.
pub struct McpEndpoint {
    /// Streamable HTTP URL, e.g. `http://127.0.0.1:52817/mcp`.
    pub url: String,
    /// The bearer token every request must present.
    pub token: String,
    cancel: CancellationToken,
}

impl McpEndpoint {
    pub fn stop(&self) {
        self.cancel.cancel();
    }
}

impl Drop for McpEndpoint {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// A fresh 256-bit token, hex-encoded.
pub fn new_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Constant-time comparison, so a wrong token does not leak how much of it matched.
fn tokens_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn unauthorized() -> Response<BoxBody> {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Bearer")
        .body(Full::new(Bytes::from_static(b"unauthorized")).boxed())
        .unwrap_or_else(|_| Response::new(Full::new(Bytes::new()).boxed()))
}

fn bearer_matches(req: &Request<Incoming>, token: &str) -> bool {
    req.headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|presented| tokens_equal(presented, token))
}

/// Start the Renderly tool endpoint for one session. `broker` answers the permission prompts
/// for built-in tools. Fails if the loopback port cannot be bound.
pub async fn start(app: AppHandle, broker: Arc<PermissionBroker>) -> Result<McpEndpoint, String> {
    serve(move || Ok(RenderlyTools::new(app.clone(), Arc::clone(&broker)))).await
}

/// Serve any MCP server over Streamable HTTP on `127.0.0.1:<random>` behind the bearer token.
/// `make_server` is called per MCP session. Used by [`start`] and by the transport tests.
pub async fn serve<S>(
    make_server: impl Fn() -> Result<S, std::io::Error> + Send + Sync + 'static,
) -> Result<McpEndpoint, String>
where
    S: rmcp::ServerHandler + Send + 'static,
{
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("could not open the agent tool port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("agent tool port: {e}"))?
        .port();
    let token = new_token();
    let cancel = CancellationToken::new();
    // Stateless, JSON responses: each request is self-contained, so the server keeps no
    // session state that could outlive the agent process.
    let config = StreamableHttpServerConfig::default()
        .with_stateful_mode(false)
        .with_json_response(true)
        .with_cancellation_token(cancel.clone());
    let service = StreamableHttpService::new(
        make_server,
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let service = Arc::new(service);

    let accept_token = token.clone();
    let accept_cancel = cancel.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = accept_cancel.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { continue };
                    let service = Arc::clone(&service);
                    let token = accept_token.clone();
                    tokio::spawn(async move {
                        let handler = service_fn(move |req: Request<Incoming>| {
                            let service = Arc::clone(&service);
                            let token = token.clone();
                            async move {
                                if !bearer_matches(&req, &token) {
                                    return Ok::<_, Infallible>(unauthorized());
                                }
                                let mut svc = (*service).clone();
                                svc.call(req).await
                            }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(stream), handler)
                            .await;
                    });
                }
            }
        }
    });

    Ok(McpEndpoint {
        url: format!("http://127.0.0.1:{port}/mcp"),
        token,
        cancel,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::{
        handler::server::{router::tool::ToolRouter, wrapper::Parameters},
        model::{ServerCapabilities, ServerInfo},
        schemars, tool, tool_handler, tool_router, ServerHandler,
    };
    use serde_json::{json, Value};

    /// A stand-in tool server, so the transport can be tested without a running app.
    #[derive(Clone)]
    struct Echo {
        #[expect(dead_code, reason = "tool_handler macro accesses tool_router")]
        tool_router: ToolRouter<Self>,
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    struct EchoRequest {
        text: String,
    }

    #[tool_router]
    impl Echo {
        fn new() -> Self {
            Self {
                tool_router: Self::tool_router(),
            }
        }

        #[tool(description = "Echo the text back")]
        fn echo(&self, Parameters(req): Parameters<EchoRequest>) -> String {
            req.text
        }
    }

    #[tool_handler]
    impl ServerHandler for Echo {
        fn get_info(&self) -> ServerInfo {
            ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
        }
    }

    async fn post(
        client: &reqwest::Client,
        url: &str,
        token: Option<&str>,
        body: Value,
    ) -> Result<(reqwest::StatusCode, String), String> {
        let mut req = client
            .post(url)
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-06-18")
            .json(&body);
        if let Some(t) = token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        Ok((status, text))
    }

    fn initialize() -> Value {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" }
            }
        })
    }

    #[tokio::test]
    async fn requests_without_or_with_a_wrong_token_are_rejected() {
        let ep = serve(|| Ok(Echo::new())).await.expect("serve");
        let client = reqwest::Client::new();
        let (status, _) = post(&client, &ep.url, None, initialize()).await.unwrap();
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
        let (status, _) = post(&client, &ep.url, Some("not-the-token"), initialize())
            .await
            .unwrap();
        assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED);
        ep.stop();
    }

    #[tokio::test]
    async fn authorized_client_lists_and_calls_tools_over_http() {
        let ep = serve(|| Ok(Echo::new())).await.expect("serve");
        let client = reqwest::Client::new();
        let token = ep.token.clone();

        let (status, body) = post(&client, &ep.url, Some(&token), initialize())
            .await
            .unwrap();
        assert_eq!(status, reqwest::StatusCode::OK, "{body}");
        assert!(
            body.contains("serverInfo") || body.contains("protocolVersion"),
            "{body}"
        );

        let list = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} });
        let (status, body) = post(&client, &ep.url, Some(&token), list).await.unwrap();
        assert_eq!(status, reqwest::StatusCode::OK, "{body}");
        assert!(body.contains("\"echo\""), "tools/list: {body}");

        let call = json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "echo", "arguments": { "text": "hello over http" } }
        });
        let (status, body) = post(&client, &ep.url, Some(&token), call).await.unwrap();
        assert_eq!(status, reqwest::StatusCode::OK, "{body}");
        assert!(body.contains("hello over http"), "tools/call: {body}");
        ep.stop();
    }

    #[tokio::test]
    async fn stopping_the_endpoint_closes_the_port() {
        let ep = serve(|| Ok(Echo::new())).await.expect("serve");
        let url = ep.url.clone();
        let client = reqwest::Client::new();
        ep.stop();
        let mut closed = false;
        for _ in 0..40 {
            if post(&client, &url, Some("x"), initialize()).await.is_err() {
                closed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(closed, "the endpoint kept answering after stop");
    }

    #[test]
    fn tokens_compare_in_constant_time_shape() {
        assert!(tokens_equal("abc", "abc"));
        assert!(!tokens_equal("abc", "abd"));
        assert!(!tokens_equal("abc", "abcd"));
    }

    #[test]
    fn new_tokens_are_long_and_unique() {
        let a = new_token();
        let b = new_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
