//! Client for the live Tauri WebSocket bridge (improvement-plan E2).
//!
//! Discovery: `%LOCALAPPDATA%/renderly/bridge.json` (Windows) or
//! `$XDG_DATA_HOME/renderly/bridge.json` (Unix). Connects only when the discovery file
//! exists and the loopback WebSocket handshake succeeds; otherwise callers stay headless.

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};

#[derive(Debug, Clone, Deserialize)]
pub struct BridgeDiscovery {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub project_path: Option<String>,
}

/// Environment variable that overrides the discovery-file location. Exists so tests (and
/// sandboxed setups) can point the MCP server at a fake bridge — or at "no bridge" — instead
/// of whatever real desktop app happens to be running on the machine. Unset in normal use.
pub const DISCOVERY_ENV: &str = "RENDERLY_BRIDGE_DISCOVERY";

pub fn discovery_path() -> PathBuf {
    if let Some(over) = std::env::var_os(DISCOVERY_ENV) {
        return PathBuf::from(over);
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("USERPROFILE")
                    .map(|h| PathBuf::from(h).join("AppData").join("Local"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
                    .unwrap_or_else(|| PathBuf::from("."))
            })
    };
    base.join("renderly").join("bridge.json")
}

pub fn read_discovery() -> Option<BridgeDiscovery> {
    read_discovery_from(&discovery_path())
}

pub fn read_discovery_from(path: &Path) -> Option<BridgeDiscovery> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Best-effort pid liveness. Failure → treat as alive and rely on TCP connect.
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        match output {
            Ok(out) => {
                let text = String::from_utf8_lossy(&out.stdout);
                text.contains(&pid.to_string())
            }
            Err(_) => true,
        }
    }
    #[cfg(unix)]
    {
        // `kill(pid, 0)` is the portable Unix liveness check. `/proc/{pid}` only exists
        // on Linux — macOS/BSD have no such path, so a procfs probe falsely reports
        // every pid as dead there.
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        let rc = unsafe { kill(pid as i32, 0) };
        if rc == 0 {
            true
        } else {
            // ESRCH → gone. EPERM → exists but not signalable → still alive.
            // Any other errno: treat as alive and let TCP decide. Compare the raw errno:
            // Rust maps ESRCH to ErrorKind::Uncategorized (NotFound is ENOENT), so a
            // kind()-based check would misreport dead processes as alive.
            const ESRCH: i32 = 3; // same value on Linux, macOS, and the BSDs
            std::io::Error::last_os_error().raw_os_error() != Some(ESRCH)
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = pid;
        true
    }
}

fn paths_match(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// True when discovery names a project path and it matches `session_path`.
pub fn discovery_matches_project(disc: &BridgeDiscovery, session_path: &Path) -> bool {
    match &disc.project_path {
        Some(p) if !p.is_empty() => paths_match(Path::new(p), session_path),
        _ => false,
    }
}

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub struct BridgeClient {
    ws: Ws,
    token: String,
    next_id: u64,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    message: String,
    /// The app's typed edit-contract error (`{code, message, hint, data?}`) for writes.
    #[serde(default)]
    data: Option<Value>,
}

/// Why a bridge call failed. `Rpc` keeps the JSON-RPC `data` payload, which carries the typed
/// edit-contract error for writes; `Transport` covers everything before a response arrives.
#[derive(Debug, Clone, PartialEq)]
pub enum CallFailure {
    Rpc {
        message: String,
        data: Option<Value>,
    },
    Transport(String),
}

impl std::fmt::Display for CallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc { message, .. } | Self::Transport(message) => f.write_str(message),
        }
    }
}

impl BridgeClient {
    pub async fn connect(disc: &BridgeDiscovery) -> Result<Self, String> {
        let url = format!("ws://127.0.0.1:{}/", disc.port);
        let (ws, _) = tokio::time::timeout(Duration::from_secs(2), connect_async(&url))
            .await
            .map_err(|_| "bridge connect timed out".to_string())?
            .map_err(|e| format!("bridge connect: {e}"))?;
        Ok(Self {
            ws,
            token: disc.token.clone(),
            next_id: 1,
        })
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.call_checked(method, params)
            .await
            .map_err(|f| f.to_string())
    }

    /// Like [`call`](Self::call), but a JSON-RPC error keeps its `data` (the typed edit-contract
    /// error for writes) instead of collapsing to a message.
    pub async fn call_checked(
        &mut self,
        method: &str,
        mut params: Value,
    ) -> Result<Value, CallFailure> {
        let transport = CallFailure::Transport;
        if !params.is_object() {
            params = json!({});
        }
        if let Some(obj) = params.as_object_mut() {
            obj.insert("token".into(), json!(self.token));
        }
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.ws
            .send(Message::Text(req.to_string().into()))
            .await
            .map_err(|e| transport(format!("bridge send: {e}")))?;

        let deadline = Duration::from_secs(120);
        let started = std::time::Instant::now();
        while started.elapsed() < deadline {
            let msg = tokio::time::timeout(Duration::from_secs(30), self.ws.next())
                .await
                .map_err(|_| transport("bridge recv timed out".to_string()))?
                .ok_or_else(|| transport("bridge closed".to_string()))?
                .map_err(|e| transport(format!("bridge recv: {e}")))?;
            let Message::Text(text) = msg else {
                continue;
            };
            let resp: JsonRpcResponse =
                serde_json::from_str(&text).map_err(|e| transport(format!("bridge parse: {e}")))?;
            if let Some(err) = resp.error {
                return Err(CallFailure::Rpc {
                    message: err.message,
                    data: err.data,
                });
            }
            return resp
                .result
                .ok_or_else(|| transport("bridge response missing result".to_string()));
        }
        Err(transport("bridge call timed out".into()))
    }
}

/// Try live bridge; `None` means stay headless.
pub async fn try_live_bridge(
    session_path: Option<&Path>,
) -> Option<(BridgeDiscovery, BridgeClient)> {
    try_live_bridge_with(read_discovery()?, session_path).await
}

/// [`try_live_bridge`] with an explicit discovery record (the seam tests drive directly).
pub async fn try_live_bridge_with(
    disc: BridgeDiscovery,
    session_path: Option<&Path>,
) -> Option<(BridgeDiscovery, BridgeClient)> {
    if !pid_alive(disc.pid) {
        return None;
    }
    if let Some(path) = session_path {
        if !discovery_matches_project(&disc, path) {
            return None;
        }
    }
    match BridgeClient::connect(&disc).await {
        Ok(client) => Some((disc, client)),
        Err(_) => None,
    }
}

#[derive(Debug, Serialize)]
pub struct EditorStatusHeadless {
    pub live: bool,
    pub project_path: Option<String>,
    pub playhead: Option<f64>,
    pub selection: Option<Value>,
    /// The headless session's revision (committed writes since it was opened).
    pub project_revision: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_parses_with_and_without_project_path() {
        let full: BridgeDiscovery = serde_json::from_str(
            r#"{"pid":123,"port":4567,"token":"abc","project_path":"C:/p.renderly.json"}"#,
        )
        .unwrap();
        assert_eq!(full.pid, 123);
        assert_eq!(full.port, 4567);
        assert_eq!(full.project_path.as_deref(), Some("C:/p.renderly.json"));

        let bare: BridgeDiscovery =
            serde_json::from_str(r#"{"pid":1,"port":2,"token":"t"}"#).unwrap();
        assert!(bare.project_path.is_none());
    }

    #[test]
    fn discovery_matches_project_requires_same_canonical_path() {
        let dir = std::env::temp_dir();
        let file = dir.join(format!(
            "renderly-mcp-disc-{}.renderly.json",
            std::process::id()
        ));
        std::fs::write(&file, "{}").unwrap();

        let matching = BridgeDiscovery {
            pid: 1,
            port: 1,
            token: String::new(),
            project_path: Some(file.to_string_lossy().into_owned()),
        };
        assert!(discovery_matches_project(&matching, &file));

        let other = BridgeDiscovery {
            pid: 1,
            port: 1,
            token: String::new(),
            project_path: Some(
                dir.join("other.renderly.json")
                    .to_string_lossy()
                    .into_owned(),
            ),
        };
        assert!(!discovery_matches_project(&other, &file));

        let none = BridgeDiscovery {
            pid: 1,
            port: 1,
            token: String::new(),
            project_path: None,
        };
        assert!(!discovery_matches_project(&none, &file));

        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn pid_liveness_sanity() {
        assert!(!pid_alive(0));
        // Our own pid is definitionally alive.
        assert!(pid_alive(std::process::id()));
    }

    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    const TOKEN: &str = "secret-token";

    #[derive(Clone, Copy)]
    enum Mode {
        /// Validate the token and answer a handful of canned methods.
        Normal,
        /// Read one request, then drop the connection without answering.
        DropAfterRequest,
    }

    /// Loopback fake of the desktop app's bridge (docs/bridge-protocol.md). Accepts a single
    /// connection on an ephemeral port; returns the port and the log of requests received.
    async fn fake_bridge(mode: Mode) -> (u16, Arc<Mutex<Vec<Value>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let log: Arc<Mutex<Vec<Value>>> = Arc::default();
        let log2 = log.clone();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            while let Some(Ok(msg)) = ws.next().await {
                let Message::Text(text) = msg else { continue };
                let req: Value = serde_json::from_str(&text).unwrap();
                log2.lock().unwrap().push(req.clone());
                if matches!(mode, Mode::DropAfterRequest) {
                    return;
                }
                let id = req["id"].clone();
                let method = req["method"].as_str().unwrap_or("").to_string();
                let params = &req["params"];
                let reply = if params["token"] != json!(TOKEN) {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"invalid token"}})
                } else {
                    match method.as_str() {
                        "echo" => {
                            let mut p = params.clone();
                            p.as_object_mut().unwrap().remove("token");
                            // A stray non-text frame first: the client must skip it.
                            ws.send(Message::Binary(vec![1, 2, 3].into()))
                                .await
                                .unwrap();
                            json!({"jsonrpc":"2.0","id":id,"result":p})
                        }
                        "boom" => {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":"kaboom"}})
                        }
                        "empty" => json!({"jsonrpc":"2.0","id":id}),
                        "garbage" => {
                            ws.send(Message::Text("not json".into())).await.unwrap();
                            continue;
                        }
                        _ => json!({"jsonrpc":"2.0","id":id,"result":{"ok":true}}),
                    }
                };
                ws.send(Message::Text(reply.to_string().into()))
                    .await
                    .unwrap();
            }
        });
        (port, log)
    }

    fn disc(port: u16, token: &str, project_path: Option<String>) -> BridgeDiscovery {
        BridgeDiscovery {
            pid: std::process::id(),
            port,
            token: token.into(),
            project_path,
        }
    }

    async fn closed_port() -> u16 {
        // Grab an ephemeral port, then free it so a connect is refused.
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        l.local_addr().unwrap().port()
    }

    #[test]
    fn read_discovery_from_handles_missing_malformed_and_valid_files() {
        let dir = std::env::temp_dir().join(format!("renderly-mcp-disc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        assert!(read_discovery_from(&dir.join("absent.json")).is_none());

        let bad = dir.join("bad.json");
        std::fs::write(&bad, "{ not json").unwrap();
        assert!(read_discovery_from(&bad).is_none());

        let incomplete = dir.join("incomplete.json");
        std::fs::write(&incomplete, r#"{"pid":1,"port":2}"#).unwrap();
        assert!(read_discovery_from(&incomplete).is_none());

        let good = dir.join("good.json");
        std::fs::write(
            &good,
            r#"{"pid":7,"port":8,"token":"tok","project_path":null}"#,
        )
        .unwrap();
        let d = read_discovery_from(&good).expect("valid file parses");
        assert_eq!((d.pid, d.port, d.token.as_str()), (7, 8, "tok"));
        assert!(d.project_path.is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_project_path_never_matches() {
        let d = disc(1, "t", Some(String::new()));
        assert!(!discovery_matches_project(&d, Path::new("anything")));
    }

    #[test]
    fn paths_match_falls_back_to_literal_comparison_for_missing_files() {
        assert!(paths_match(
            Path::new("/no/such/file.json"),
            Path::new("/no/such/file.json")
        ));
        assert!(!paths_match(
            Path::new("/no/such/a.json"),
            Path::new("/no/such/b.json")
        ));
    }

    #[tokio::test]
    async fn call_round_trips_and_injects_token_and_incrementing_ids() {
        let (port, log) = fake_bridge(Mode::Normal).await;
        let mut client = BridgeClient::connect(&disc(port, TOKEN, None))
            .await
            .unwrap();

        let r = client.call("echo", json!({"a": 1})).await.unwrap();
        assert_eq!(r, json!({"a": 1}));
        // Non-object params are coerced to an empty object (token still attached).
        let r = client.call("echo", json!("scalar")).await.unwrap();
        assert_eq!(r, json!({}));
        let r = client.call("whatever", json!({})).await.unwrap();
        assert_eq!(r, json!({"ok": true}));

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(log[0]["jsonrpc"], "2.0");
        assert_eq!(log[0]["method"], "echo");
        assert_eq!(log[0]["params"]["token"], TOKEN);
        let ids: Vec<u64> = log.iter().map(|r| r["id"].as_u64().unwrap()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn call_surfaces_error_responses() {
        let (port, _log) = fake_bridge(Mode::Normal).await;
        let mut client = BridgeClient::connect(&disc(port, TOKEN, None))
            .await
            .unwrap();
        assert_eq!(client.call("boom", json!({})).await.unwrap_err(), "kaboom");
        // The connection stays usable after an error response.
        assert!(client.call("ping", json!({})).await.is_ok());
    }

    #[tokio::test]
    async fn call_with_wrong_token_is_rejected_by_bridge() {
        let (port, _log) = fake_bridge(Mode::Normal).await;
        let mut client = BridgeClient::connect(&disc(port, "wrong", None))
            .await
            .unwrap();
        let err = client.call("echo", json!({})).await.unwrap_err();
        assert_eq!(err, "invalid token");
    }

    #[tokio::test]
    async fn call_reports_closed_connection() {
        let (port, _log) = fake_bridge(Mode::DropAfterRequest).await;
        let mut client = BridgeClient::connect(&disc(port, TOKEN, None))
            .await
            .unwrap();
        let err = client.call("echo", json!({})).await.unwrap_err();
        assert!(
            err.contains("bridge closed") || err.contains("bridge recv"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn call_reports_missing_result_and_unparseable_reply() {
        let (port, _log) = fake_bridge(Mode::Normal).await;
        let mut client = BridgeClient::connect(&disc(port, TOKEN, None))
            .await
            .unwrap();
        assert_eq!(
            client.call("empty", json!({})).await.unwrap_err(),
            "bridge response missing result"
        );
        let err = client.call("garbage", json!({})).await.unwrap_err();
        assert!(err.starts_with("bridge parse"), "{err}");
    }

    #[tokio::test]
    async fn connect_fails_when_nothing_listens() {
        let port = closed_port().await;
        assert!(BridgeClient::connect(&disc(port, TOKEN, None))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn try_live_bridge_with_applies_pid_path_and_connect_gates() {
        let dir = std::env::temp_dir();
        let project = dir.join(format!(
            "renderly-mcp-live-{}.renderly.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&project, "{}").unwrap();
        let project_str = project.to_string_lossy().into_owned();

        // Matching project path + live pid + listening port => connected.
        let (port, _log) = fake_bridge(Mode::Normal).await;
        let d = disc(port, TOKEN, Some(project_str.clone()));
        let (got, mut client) = try_live_bridge_with(d.clone(), Some(&project))
            .await
            .expect("should connect");
        assert_eq!(got.port, port);
        assert_eq!(
            client.call("ping", json!({})).await.unwrap(),
            json!({"ok": true})
        );

        // No session path (status queries) skips the path check.
        let (port2, _l) = fake_bridge(Mode::Normal).await;
        assert!(try_live_bridge_with(disc(port2, TOKEN, None), None)
            .await
            .is_some());

        // Path mismatch => headless, even though the bridge is reachable.
        let (port3, _l) = fake_bridge(Mode::Normal).await;
        let other = dir.join("some-other-project.renderly.json");
        assert!(
            try_live_bridge_with(disc(port3, TOKEN, Some(project_str.clone())), Some(&other))
                .await
                .is_none()
        );

        // Dead pid => headless.
        let mut dead = d.clone();
        dead.pid = 0;
        assert!(try_live_bridge_with(dead, Some(&project)).await.is_none());

        // Unreachable port => headless.
        let closed = closed_port().await;
        assert!(
            try_live_bridge_with(disc(closed, TOKEN, Some(project_str)), Some(&project))
                .await
                .is_none()
        );

        std::fs::remove_file(&project).ok();
    }
}
