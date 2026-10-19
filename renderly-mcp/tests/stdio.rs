//! End-to-end tests: spawn the real `renderly-mcp` binary and speak MCP (newline-delimited
//! JSON-RPC 2.0 over stdio, as rmcp's stdio transport frames it) by hand.

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Every tool `server.rs` defines. Kept in sync by `tools/list` equality below.
const TOOLS: &[&str] = &[
    "apply_command",
    "describe_commands",
    "describe_effects",
    "detect_scenes",
    "detect_silence",
    "download_model",
    "export",
    "export_otio",
    "get_audio_peaks",
    "get_background_matte_status",
    "get_editor_status",
    "get_project",
    "get_transcript",
    "import_otio",
    "list_library",
    "list_models",
    "new_project",
    "open_project",
    "permission_prompt",
    "probe_media",
    "redo",
    "render_frame",
    "undo",
];

const RECV_TIMEOUT: Duration = Duration::from_secs(60);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("renderly-mcp-stdio-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A running server process. Always killed on drop so a failed assertion cannot leak it.
struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next_id: u64,
}

impl McpProcess {
    fn spawn(discovery_file: &Path) -> Self {
        Self::spawn_with_args(discovery_file, &[])
    }

    fn spawn_with_args(discovery_file: &Path, args: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_renderly-mcp"))
            .args(args)
            // Never let a real desktop app on this machine hijack the session.
            .env("RENDERLY_BRIDGE_DISCOVERY", discovery_file)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn renderly-mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            lines,
            next_id: 1,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin already closed");
        writeln!(stdin, "{msg}").expect("write to server stdin");
        stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// Send a request and wait (bounded) for the response with the matching id, skipping
    /// any server notifications in between.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let deadline = Instant::now() + RECV_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    let v: Value = serde_json::from_str(&line)
                        .unwrap_or_else(|e| panic!("non-JSON line on stdout ({e}): {line}"));
                    if v.get("id") == Some(&json!(id)) {
                        return v;
                    }
                }
                Err(RecvTimeoutError::Timeout) => panic!("timed out waiting for {method} response"),
                Err(RecvTimeoutError::Disconnected) => {
                    panic!("server closed stdout while waiting for {method} response")
                }
            }
        }
    }

    fn handshake(&mut self) -> Value {
        let init = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "stdio-test", "version": "0.0.0" }
            }),
        );
        self.notify("notifications/initialized", json!({}));
        init
    }

    /// `tools/call`, returning (is_error, concatenated text content).
    fn call_tool(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let resp = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert!(resp.get("error").is_none(), "{name}: protocol error {resp}");
        let result = &resp["result"];
        let text = result["content"]
            .as_array()
            .unwrap_or_else(|| panic!("{name}: no content in {resp}"))
            .iter()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("");
        (result["isError"].as_bool().unwrap_or(false), text)
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn missing_discovery(dir: &TempDir) -> PathBuf {
    dir.0.join("no-bridge.json")
}

#[test]
fn stdio_session_new_apply_get_project() {
    let dir = TempDir::new();
    let project_path = dir.0.join("e2e.renderly.json");
    let mut mcp = McpProcess::spawn(&missing_discovery(&dir));

    let init = mcp.handshake();
    let result = &init["result"];
    assert!(result["capabilities"]["tools"].is_object(), "{init}");
    assert!(result["protocolVersion"].is_string());
    assert!(result["instructions"]
        .as_str()
        .unwrap()
        .contains("Renderly"));

    // tools/list contains every tool server.rs defines (and nothing else).
    let list = mcp.request("tools/list", json!({}));
    let mut names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, TOOLS);
    for t in list["result"]["tools"].as_array().unwrap() {
        assert!(
            t["description"].is_string(),
            "tool without description: {t}"
        );
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
    }

    // Before any project is open, session tools report it as a tool-level text result.
    let (_, text) = mcp.call_tool("get_project", json!({}));
    assert!(text.contains("no project open"), "{text}");

    let (is_err, text) = mcp.call_tool(
        "new_project",
        json!({
            "path": project_path.to_string_lossy(),
            "name": "e2e",
            "width": 1280,
            "height": 720,
            "fps": 30.0
        }),
    );
    assert!(!is_err, "{text}");
    assert!(text.contains("created and opened"), "{text}");
    assert!(project_path.is_file());

    // Object-shaped command...
    let (_, text) = mcp.call_tool(
        "apply_command",
        json!({ "command": { "command": "AddTrack", "kind": "video", "name": "V1" } }),
    );
    assert!(text.contains("TrackAdded"), "{text}");
    // ...and the string-encoded shape Claude Code's client sends.
    let (_, text) = mcp.call_tool(
        "apply_command",
        json!({ "command": json!({ "command": "AddTrack", "kind": "audio", "name": "A1" }).to_string() }),
    );
    assert!(text.contains("TrackAdded"), "{text}");
    // A failing command is reported as an error text and does not kill the session.
    let (_, text) = mcp.call_tool(
        "apply_command",
        json!({ "command": { "command": "Bogus" } }),
    );
    // Typed contract error: JSON with a code and a hint.
    let err: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    assert_eq!(err["code"], "invalid_command", "{text}");
    assert!(err["hint"].is_string(), "{text}");

    let (_, text) = mcp.call_tool("get_project", json!({}));
    let project: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(project["name"], "e2e");
    let tracks = project["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0]["name"], "V1");
    assert_eq!(tracks[1]["name"], "A1");

    // Headless status.
    let (_, text) = mcp.call_tool("get_editor_status", json!({}));
    let status: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(status["live"], false);
    assert_eq!(
        status["project_path"],
        project_path.to_string_lossy().as_ref()
    );

    // The edits are on disk.
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&project_path).unwrap()).unwrap();
    assert_eq!(on_disk["tracks"].as_array().unwrap().len(), 2);

    // OpenTimelineIO round trip over the wire: export, then import into a new project file.
    let otio_path = dir.0.join("e2e.otio");
    let (is_err, text) = mcp.call_tool(
        "export_otio",
        json!({ "output_path": otio_path.to_string_lossy() }),
    );
    assert!(!is_err && text.contains("exported to"), "{text}");
    let otio: Value = serde_json::from_str(&std::fs::read_to_string(&otio_path).unwrap()).unwrap();
    assert_eq!(otio["OTIO_SCHEMA"], "Timeline.1");
    assert_eq!(otio["tracks"]["children"].as_array().unwrap().len(), 2);
    let imported_path = dir.0.join("imported.renderly.json");
    let (is_err, text) = mcp.call_tool(
        "import_otio",
        json!({ "input_path": otio_path.to_string_lossy(),
                "output_path": imported_path.to_string_lossy() }),
    );
    assert!(!is_err, "{text}");
    let summary: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(summary["tracks"], 2);
    assert_eq!(summary["warnings"], json!([]));
    // The imported project is now the open one; the first project's file is untouched.
    let (_, text) = mcp.call_tool("get_project", json!({}));
    let reopened: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reopened["name"], "e2e");
    assert_eq!(reopened["tracks"].as_array().unwrap().len(), 2);
    assert!(imported_path.is_file());
    let (is_err, text) = mcp.call_tool(
        "import_otio",
        json!({ "input_path": dir.0.join("nope.otio").to_string_lossy(),
                "output_path": dir.0.join("x.json").to_string_lossy() }),
    );
    assert!(is_err || text.starts_with("error:"), "{text}");

    // Unknown tool => JSON-RPC error, server stays alive.
    let resp = mcp.request(
        "tools/call",
        json!({ "name": "no_such_tool", "arguments": {} }),
    );
    assert!(
        resp.get("error").is_some() || resp["result"]["isError"] == true,
        "{resp}"
    );
    let (_, text) = mcp.call_tool("get_project", json!({}));
    assert!(text.contains("\"V1\""));
}

#[test]
fn stdio_project_flag_opens_the_project_at_startup() {
    let dir = TempDir::new();
    let project_path = dir.0.join("pinned.renderly.json");
    let project = renderly_core::project::Project::new(
        "pinned-at-startup".to_string(),
        renderly_core::project::Settings {
            fps: 30.0,
            width: 640,
            height: 360,
            sample_rate: 48000,
            duck_db: -12.0,
        },
    );
    std::fs::write(
        &project_path,
        serde_json::to_string_pretty(&project).unwrap(),
    )
    .unwrap();
    let project_arg = project_path.to_string_lossy().into_owned();
    let mut mcp = McpProcess::spawn_with_args(
        &missing_discovery(&dir),
        &["--project", project_arg.as_str()],
    );
    mcp.handshake();
    // No open_project call: the session must already be on the pinned project.
    let (is_error, text) = mcp.call_tool("get_project", json!({}));
    assert!(!is_error, "get_project failed: {text}");
    assert!(text.contains("pinned-at-startup"), "{text}");
}

#[test]
fn stdio_permission_prompt_denies_when_the_app_is_not_live() {
    let dir = TempDir::new();
    let project_path = dir.0.join("perm.renderly.json");
    let project = renderly_core::project::Project::new(
        "perm".to_string(),
        renderly_core::project::Settings {
            fps: 30.0,
            width: 640,
            height: 360,
            sample_rate: 48000,
            duck_db: -12.0,
        },
    );
    std::fs::write(&project_path, serde_json::to_string(&project).unwrap()).unwrap();
    let project_arg = project_path.to_string_lossy().into_owned();
    let mut mcp = McpProcess::spawn_with_args(
        &missing_discovery(&dir),
        &["--project", project_arg.as_str()],
    );
    mcp.handshake();
    let (is_error, text) = mcp.call_tool(
        "permission_prompt",
        json!({ "tool_name": "mcp__renderly__export", "input": {}, "tool_use_id": "tu_1" }),
    );
    assert!(!is_error, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["behavior"], "deny", "{text}");
    assert!(
        v["message"].as_str().unwrap().contains("not running"),
        "{text}"
    );
}

#[test]
fn stdio_project_flag_with_missing_file_fails_to_start() {
    let dir = TempDir::new();
    let missing = dir.0.join("nope.renderly.json");
    let missing_arg = missing.to_string_lossy().into_owned();
    let mut child = Command::new(env!("CARGO_BIN_EXE_renderly-mcp"))
        .args(["--project", missing_arg.as_str()])
        .env("RENDERLY_BRIDGE_DISCOVERY", missing_discovery(&dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn renderly-mcp");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(
                !status.success(),
                "a bad --project must not start a session"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server did not exit on a bad --project"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn stdio_server_exits_when_stdin_closes() {
    let dir = TempDir::new();
    let mut mcp = McpProcess::spawn(&missing_discovery(&dir));
    mcp.handshake();
    // Close the pipe: a well-behaved stdio server shuts down rather than hanging.
    drop(mcp.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if mcp.child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "server did not exit after stdin closed"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Fake desktop-app bridge on loopback; serves any number of connections and records
/// every request it receives.
fn start_fake_bridge(token: &'static str, project_path: String) -> (u16, Arc<Mutex<Vec<Value>>>) {
    let log: Arc<Mutex<Vec<Value>>> = Arc::default();
    let (port_tx, port_rx) = channel();
    let log2 = log.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            port_tx.send(listener.local_addr().unwrap().port()).unwrap();
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let log = log2.clone();
                let project_path = project_path.clone();
                tokio::spawn(async move {
                    let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                        return;
                    };
                    while let Some(Ok(msg)) = ws.next().await {
                        let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                            continue;
                        };
                        let req: Value = serde_json::from_str(&text).unwrap();
                        log.lock().unwrap().push(req.clone());
                        let id = req["id"].clone();
                        let reply = if req["params"]["token"] != json!(token) {
                            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32001,"message":"invalid token"}})
                        } else {
                            match req["method"].as_str().unwrap_or("") {
                                "get_editor_status" => json!({"jsonrpc":"2.0","id":id,"result":{
                                    "live": true,
                                    "project_path": project_path.clone(),
                                    "playhead": 1.25,
                                    "selection": null
                                }}),
                                "get_project" => json!({"jsonrpc":"2.0","id":id,"result":{
                                    "project": {"name": "from-live-app", "tracks": []},
                                    "revision": 7
                                }}),
                                "apply_command" => json!({"jsonrpc":"2.0","id":id,"result":{
                                    "revision": 8, "patch": [], "outcome": "LiveOutcome"
                                }}),
                                _ => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"unknown method"}}),
                            }
                        };
                        if ws
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                reply.to_string().into(),
                            ))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                });
            }
        });
    });
    (port_rx.recv_timeout(Duration::from_secs(10)).unwrap(), log)
}

#[test]
fn stdio_proxies_to_live_bridge_when_discovery_matches_session() {
    const TOKEN: &str = "stdio-test-token";
    let dir = TempDir::new();
    let project_path = dir.0.join("live.renderly.json");
    let (port, log) = start_fake_bridge(TOKEN, project_path.to_string_lossy().into_owned());

    // Discovery names the same project the MCP session will open, and a live pid (ours).
    let discovery = dir.0.join("bridge.json");
    std::fs::write(
        &discovery,
        json!({
            "pid": std::process::id(),
            "port": port,
            "token": TOKEN,
            "project_path": project_path.to_string_lossy(),
        })
        .to_string(),
    )
    .unwrap();

    let mut mcp = McpProcess::spawn(&discovery);
    mcp.handshake();
    let (_, text) = mcp.call_tool(
        "new_project",
        json!({ "path": project_path.to_string_lossy() }),
    );
    assert!(text.contains("created"), "{text}");

    // get_project goes through the bridge and unwraps `{ project, revision }`.
    let (_, text) = mcp.call_tool("get_project", json!({}));
    let project: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(project["name"], "from-live-app", "{text}");

    // apply_command is forwarded as a canonical object with the token, and the bridge's
    // outcome string is returned.
    let (_, text) = mcp.call_tool(
        "apply_command",
        json!({ "command": json!({ "command": "AddTrack", "kind": "video", "name": "V9" }).to_string() }),
    );
    // The live bridge's edit-contract result comes back as JSON, with its outcome.
    let written: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(written["outcome"], "LiveOutcome", "{text}");

    // get_editor_status is proxied and annotated with path_match against the session.
    let (_, text) = mcp.call_tool("get_editor_status", json!({}));
    let status: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(status["live"], true, "{text}");
    assert_eq!(status["playhead"], 1.25);
    assert_eq!(status["path_match"], true, "{text}");

    let log = log.lock().unwrap();
    let apply = log
        .iter()
        .find(|r| r["method"] == "apply_command")
        .expect("bridge saw apply_command");
    assert_eq!(apply["params"]["token"], TOKEN);
    assert_eq!(apply["params"]["command"]["command"], "AddTrack");
    assert_eq!(apply["params"]["command"]["name"], "V9");
    assert!(apply["params"]["command"].is_object());

    // The headless project file was NOT edited by the proxied command.
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&project_path).unwrap()).unwrap();
    assert!(on_disk["tracks"].as_array().unwrap().is_empty());
}

/// With no project open, edit and render tools must NOT fall through to whatever project the
/// desktop app has open — they fail with "no project open" and the bridge sees nothing.
#[test]
fn stdio_does_not_attach_to_the_app_without_an_open_project() {
    const TOKEN: &str = "stdio-noattach-token";
    let dir = TempDir::new();
    let app_project = dir.0.join("in-the-app.renderly.json");
    let (port, log) = start_fake_bridge(TOKEN, app_project.to_string_lossy().into_owned());
    let discovery = dir.0.join("bridge.json");
    std::fs::write(
        &discovery,
        json!({
            "pid": std::process::id(),
            "port": port,
            "token": TOKEN,
            "project_path": app_project.to_string_lossy(),
        })
        .to_string(),
    )
    .unwrap();

    let mut mcp = McpProcess::spawn(&discovery);
    mcp.handshake();
    for (tool, args) in [
        ("get_project", json!({})),
        (
            "apply_command",
            json!({ "command": { "command": "AddTrack", "kind": "video", "name": "V1" } }),
        ),
        ("render_frame", json!({ "time_secs": 0.0 })),
        (
            "export",
            json!({ "output_path": dir.0.join("x.mp4").to_string_lossy() }),
        ),
    ] {
        let (_, text) = mcp.call_tool(tool, args);
        assert!(text.contains("no project open"), "{tool}: {text}");
    }

    // Status still reports the running app; it is read-only and needs no project.
    let (_, text) = mcp.call_tool("get_editor_status", json!({}));
    let status: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(status["live"], true, "{text}");

    let log = log.lock().unwrap();
    assert!(
        log.iter().all(|r| r["method"] == "get_editor_status"),
        "only the status call may reach the app: {log:?}"
    );
}

#[test]
fn stdio_edit_contract_over_the_wire() {
    let dir = TempDir::new();
    let project_path = dir.0.join("contract.renderly.json");
    let mut mcp = McpProcess::spawn(&missing_discovery(&dir));
    mcp.handshake();

    let (is_err, text) = mcp.call_tool(
        "new_project",
        json!({ "path": project_path.to_string_lossy(), "name": "contract" }),
    );
    assert!(!is_err, "{text}");

    // describe_commands names the capability set and the deletion rule.
    let (_, text) = mcp.call_tool("describe_commands", json!({}));
    assert!(text.contains("renderly.capabilities.v1"), "{text}");
    assert!(text.contains("deletion_rule"), "{text}");

    // Reads carry project_revision.
    let (_, text) = mcp.call_tool("get_project", json!({}));
    let project: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(project["project_revision"], 0);

    // A committed write returns the contract result with the new revision.
    let (is_err, text) = mcp.call_tool(
        "apply_command",
        json!({
            "command": { "command": "AddTrack", "kind": "caption", "name": "Subs" },
            "expected_revision": 0,
            "request_id": "wire-1"
        }),
    );
    assert!(!is_err, "{text}");
    let written: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(written["committed"], true);
    assert_eq!(written["revision"], 1);
    assert_eq!(written["changed"]["created"].as_array().unwrap().len(), 1);
    let track_id = written["changed"]["created"][0]
        .as_str()
        .unwrap()
        .to_string();

    // A stale expected_revision is a tool error with a typed code; nothing changes on disk.
    let before = std::fs::read(&project_path).unwrap();
    let (is_err, text) = mcp.call_tool(
        "apply_command",
        json!({
            "command": { "command": "RenameTrack", "track_id": track_id, "name": "X" },
            "expected_revision": 0
        }),
    );
    assert!(is_err, "{text}");
    let err: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(err["code"], "revision_conflict");
    assert_eq!(err["data"]["current_revision"], 1);
    assert_eq!(std::fs::read(&project_path).unwrap(), before);

    // A dry run returns the patch and changes nothing.
    let (is_err, text) = mcp.call_tool(
        "apply_command",
        json!({
            "command": { "command": "RenameTrack", "track_id": track_id, "name": "Preview" },
            "expected_revision": 1,
            "dry_run": true
        }),
    );
    assert!(!is_err, "{text}");
    let dry: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(dry["committed"], false);
    assert!(!dry["patch"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read(&project_path).unwrap(), before);

    // A retry of a committed request is a replay, not a second write.
    let (_, text) = mcp.call_tool(
        "apply_command",
        json!({
            "command": { "command": "AddTrack", "kind": "caption", "name": "Subs" },
            "expected_revision": 0,
            "request_id": "wire-1"
        }),
    );
    let replay: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["revision"], 1);

    // Deleting a track needs allow_deletion with its exact ids.
    let (is_err, text) = mcp.call_tool(
        "apply_command",
        json!({
            "command": { "command": "DeleteTrack", "track_id": track_id },
            "expected_revision": 1
        }),
    );
    assert!(is_err, "{text}");
    let refused: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(refused["code"], "deletion_not_allowed");
    assert!(refused["message"].as_str().unwrap().contains(&track_id));

    let (is_err, text) = mcp.call_tool(
        "apply_command",
        json!({
            "command": { "command": "DeleteTrack", "track_id": track_id },
            "expected_revision": 1,
            "allow_deletion": { "reason": "cleanup", "ids": [track_id] }
        }),
    );
    assert!(!is_err, "{text}");
    let deleted: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(deleted["revision"], 2);
    assert_eq!(deleted["changed"]["deleted"][0], track_id);
}
