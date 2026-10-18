//! Live agent bridge (improvement-plan E1/E2): JSON-RPC 2.0 over a loopback WebSocket.
//!
//! Binds `127.0.0.1:0`, writes `{pid,port,token,project_path}` to the discovery file, and
//! proxies edit/playback methods into the same `AppState` paths the Tauri commands use —
//! so agent edits share undo history and emit `project:changed` for the GUI.

use crate::AppState;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use rand::RngCore;
use renderly_core::compose::PreviewOverride;
use renderly_core::edit_contract::{Caller, EditError, WriteOptions};
use renderly_core::project::Id;
use renderly_core::{
    commands::ExportPreset, export_project_with_settings, ExportError, ExportSettings,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

/// Well-known discovery path shared by the app and `renderly-mcp` (no Tauri needed on the
/// MCP side). Windows: `%LOCALAPPDATA%/renderly/bridge.json`; Unix:
/// `$XDG_DATA_HOME/renderly/bridge.json` or `~/.local/share/renderly/bridge.json`.
pub fn discovery_path() -> PathBuf {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeDiscovery {
    pub pid: u32,
    pub port: u16,
    pub token: String,
    #[serde(default)]
    pub project_path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i32,
    message: String,
    /// The typed edit-contract error for writes (`{code, message, hint, data?}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// Shared auth + discovery bookkeeping for the running bridge.
pub struct BridgeHandle {
    token: String,
    port: u16,
    discovery_path: PathBuf,
    /// Latest project path mirrored into the discovery file for MCP path matching.
    project_path: Mutex<Option<String>>,
}

impl BridgeHandle {
    pub fn set_project_path(&self, path: Option<PathBuf>) {
        let s = path.map(|p| p.to_string_lossy().into_owned());
        *self.project_path.lock() = s.clone();
        let _ = write_discovery(&self.discovery_path, self.port, &self.token, s.as_deref());
    }
}

fn write_discovery(
    path: &Path,
    port: u16,
    token: &str,
    project_path: Option<&str>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let disc = BridgeDiscovery {
        pid: std::process::id(),
        port,
        token: token.to_string(),
        project_path: project_path.map(|s| s.to_string()),
    };
    let data = serde_json::to_string_pretty(&disc).map_err(|e| e.to_string())?;
    std::fs::write(path, data).map_err(|e| e.to_string())
}

fn remove_discovery(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A non-empty string param, or `None` when absent or empty.
fn optional_string(params: &Value, key: &str) -> Option<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// A required param deserialized into `T`, with the param name in the error.
fn param_value<T: serde::de::DeserializeOwned>(params: &Value, key: &str) -> Result<T, String> {
    let raw = params
        .get(key)
        .cloned()
        .ok_or_else(|| format!("params.{key} required"))?;
    serde_json::from_value(raw).map_err(|e| format!("params.{key} invalid: {e}"))
}

fn check_token(expected: &str, params: &Value) -> Result<(), String> {
    let got = params
        .get("token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "missing auth token".to_string())?;
    if got != expected {
        return Err("invalid auth token".into());
    }
    Ok(())
}

/// A failed bridge call: the JSON-RPC message and, for writes, the typed edit-contract error
/// (`{code, message, hint, data?}`) in the JSON-RPC `error.data`.
struct RpcFailure {
    message: String,
    data: Option<Value>,
}

impl From<String> for RpcFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            data: None,
        }
    }
}

impl From<EditError> for RpcFailure {
    fn from(err: EditError) -> Self {
        Self {
            message: err.message.clone(),
            data: serde_json::to_value(&err).ok(),
        }
    }
}

async fn dispatch(
    app: &AppHandle,
    token: &str,
    method: &str,
    params: Value,
) -> Result<Value, RpcFailure> {
    check_token(token, &params)?;
    match method {
        "apply_command" | "apply_commands" => dispatch_write(app, method, params).await,
        _ => dispatch_other(app, method, params)
            .await
            .map_err(RpcFailure::from),
    }
}

/// The write methods: the edit contract's envelope (`expected_revision`, `dry_run`,
/// `request_id`, `allow_deletion`) is read from the same params object as the command(s). Agents
/// call the bridge, so the deletion guard applies (`Caller::Agent`).
async fn dispatch_write(app: &AppHandle, method: &str, params: Value) -> Result<Value, RpcFailure> {
    let state = app.state::<AppState>();
    let options: WriteOptions = serde_json::from_value(params.clone())
        .map_err(|e| EditError::invalid(format!("invalid write options: {e}")))?;
    let merge_key = optional_string(&params, "merge_key");
    // No mutation_id — GUI must refetch on project:changed (external edit).
    if method == "apply_command" {
        let command = params
            .get("command")
            .cloned()
            .ok_or_else(|| EditError::invalid("params.command required"))?;
        let result = state
            .apply_command_inner(app, command, None, merge_key, options, Caller::Agent)
            .await?;
        return serde_json::to_value(result).map_err(|e| EditError::internal(e.to_string()).into());
    }
    let commands = params
        .get("commands")
        .cloned()
        .ok_or_else(|| EditError::invalid("params.commands required"))?;
    let commands: Vec<Value> = serde_json::from_value(commands)
        .map_err(|e| EditError::invalid(format!("params.commands must be an array: {e}")))?;
    let result = state
        .apply_commands_inner(app, commands, None, merge_key, options, Caller::Agent)
        .await?;
    serde_json::to_value(result).map_err(|e| EditError::internal(e.to_string()).into())
}

async fn dispatch_other(app: &AppHandle, method: &str, params: Value) -> Result<Value, String> {
    let state = app.state::<AppState>();

    match method {
        "get_project" => {
            let project = state.with_session(|s| Ok(Arc::clone(&s.project)))?;
            Ok(json!({
                "project": &*project,
                "revision": state.revision_value(),
            }))
        }
        "undo" => {
            let status = state.undo_inner(app, None).await?;
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        "redo" => {
            let status = state.redo_inner(app, None).await?;
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        "begin_transaction" => {
            let label = optional_string(&params, "label").unwrap_or_else(|| "Agent edit".into());
            serde_json::to_value(state.begin_transaction(&label)).map_err(|e| e.to_string())
        }
        "commit_transaction" => {
            serde_json::to_value(state.commit_transaction_announced(app)).map_err(|e| e.to_string())
        }
        "rollback_transaction" => {
            let status = state.rollback_transaction_inner(app, None).await?;
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        "end_gesture" => serde_json::to_value(state.end_gesture()).map_err(|e| e.to_string()),
        "set_preview_override" => {
            let target: Id = param_value(&params, "target")?;
            let over: PreviewOverride = param_value(&params, "override")?;
            state.set_preview_override(target, over);
            Ok(json!({ "ok": true }))
        }
        "clear_preview_override" => {
            let target: Id = param_value(&params, "target")?;
            state.clear_preview_override(target);
            Ok(json!({ "ok": true }))
        }
        "play" => {
            let time = params
                .get("time_secs")
                .and_then(|v| v.as_f64())
                .unwrap_or_else(|| state.playhead_secs());
            state.set_playhead_secs(time);
            let project = state.playback_project()?;
            state.playback.play(app.clone(), project, time);
            Ok(json!({ "ok": true }))
        }
        "pause" => {
            let t = tauri::async_runtime::spawn_blocking({
                let app = app.clone();
                move || app.state::<AppState>().playback.pause()
            })
            .await
            .map_err(|e| e.to_string())?;
            state.set_playhead_secs(t);
            Ok(json!({ "time_secs": t }))
        }
        "seek" => {
            let time = params
                .get("time_secs")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| "params.time_secs required".to_string())?;
            state.set_playhead_secs(time);
            // While playing, coalesce into the running play session. While paused, there
            // is nothing left to do backend-side — the webview canvas repaints itself
            // from the playhead the frontend reads via `bridge:playhead`/status polling.
            state.playback.seek_while_playing(time);
            Ok(json!({ "ok": true }))
        }
        "set_playhead" => {
            // E3: move the user's playhead so the agent can direct attention.
            let time = params
                .get("time_secs")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| "params.time_secs required".to_string())?;
            state.set_playhead_secs(time);
            state.playback.seek_while_playing(time);
            let _ = app.emit("bridge:playhead", json!({ "time_secs": time }));
            Ok(json!({ "ok": true, "time_secs": time }))
        }
        "export" => {
            let output_path = params
                .get("output_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "params.output_path required".to_string())?
                .to_string();
            let output_for_result = output_path.clone();
            let preset = params.get("preset").cloned().unwrap_or(json!("tiktok"));
            let preset = crate::parse_export_preset(&preset)?;
            let project = state.with_session(|s| Ok(Arc::clone(&s.project)))?;
            let mut settings = ExportSettings::from_preset(&preset, &project);
            if let Some(enc) = params.get("encode") {
                crate::merge_encode_into_settings(&mut settings, enc)?;
            }
            state.export_cancel.store(false, Ordering::SeqCst);
            let cancel = Arc::clone(&state.export_cancel);
            tauri::async_runtime::spawn_blocking(move || {
                match export_project_with_settings(
                    &project,
                    Path::new(&output_path),
                    settings,
                    &mut |_| !cancel.load(Ordering::SeqCst),
                ) {
                    Ok(()) => Ok(()),
                    Err(ExportError::Cancelled) => Err("export cancelled".into()),
                    Err(e) => Err(e.to_string()),
                }
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(json!({ "ok": true, "output_path": output_for_result }))
        }
        "render_frame" => {
            // E3: a fresh `FrameRenderer` over the live session project (not a headless
            // reload from disk) — see `AppState::render_live_frame_png`.
            let time = params
                .get("time_secs")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let preset = params
                .get("preset")
                .and_then(|v| v.as_str())
                .unwrap_or("tiktok");
            let preset = match preset {
                "tiktok" => ExportPreset::TikTok9x16,
                "youtube" => ExportPreset::Youtube16x9,
                other => return Err(format!("unknown preset '{other}'")),
            };
            let app = app.clone();
            let png = tauri::async_runtime::spawn_blocking(move || {
                app.state::<AppState>().render_live_frame_png(time, preset)
            })
            .await
            .map_err(|e| e.to_string())??;
            Ok(json!({
                "time_secs": time,
                "png_base64": base64_encode(&png),
                "byte_len": png.len(),
            }))
        }
        "agent_permission" => {
            // renderly-mcp forwards Claude Code's --permission-prompt-tool requests here. The
            // answer waits for the user in the Agent panel; nothing is approved by default.
            let tool = params
                .get("tool_name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let input = params.get("input").cloned().unwrap_or(Value::Null);
            Ok(crate::agent_chat::bridge_permission(app, &tool, input).await)
        }
        "get_editor_status" => {
            let (project_path, name) = state
                .with_session(|s| {
                    Ok((
                        Some(s.path.to_string_lossy().into_owned()),
                        Some(s.project.name.clone()),
                    ))
                })
                .unwrap_or((None, None));
            Ok(json!({
                "live": true,
                "project_path": project_path,
                "project_name": name,
                "playhead": state.playhead_secs(),
                "playing": state.playback.is_playing(),
                "selection": state.selection_json(),
                "revision": state.revision_value(),
                "project_revision": state.revision_value(),
            }))
        }
        other => Err(format!("unknown method '{other}'")),
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

async fn handle_connection(app: AppHandle, token: String, stream: TcpStream) {
    let ws = match accept_async(stream).await {
        Ok(ws) => ws,
        Err(_) => return,
    };
    let (mut write, mut read) = ws.split();
    while let Some(msg) = read.next().await {
        let Ok(msg) = msg else { break };
        if msg.is_close() {
            break;
        }
        let Message::Text(text) = msg else { continue };
        let req: JsonRpcRequest = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("parse error: {e}"),
                        data: None,
                    }),
                };
                let _ = write
                    .send(Message::Text(
                        serde_json::to_string(&resp).unwrap_or_default().into(),
                    ))
                    .await;
                continue;
            }
        };
        if req.jsonrpc.as_deref().is_some_and(|v| v != "2.0") {
            // Accept missing jsonrpc for lenient clients; reject wrong versions.
        }
        let result = dispatch(&app, &token, &req.method, req.params).await;
        let resp = match result {
            Ok(value) => JsonRpcResponse {
                jsonrpc: "2.0",
                id: req.id,
                result: Some(value),
                error: None,
            },
            Err(failure) => JsonRpcResponse {
                jsonrpc: "2.0",
                id: req.id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32000,
                    message: failure.message,
                    data: failure.data,
                }),
            },
        };
        let body = serde_json::to_string(&resp).unwrap_or_default();
        if write.send(Message::Text(body.into())).await.is_err() {
            break;
        }
    }
}

/// Bind loopback, write discovery, accept connections until the app exits.
pub async fn run_bridge(app: AppHandle) -> Result<Arc<BridgeHandle>, String> {
    let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .map_err(|e| format!("bridge bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("bridge local_addr: {e}"))?
        .port();
    let token = random_token();
    let path = discovery_path();
    let project_path = {
        let state = app.state::<AppState>();
        state
            .with_session(|s| Ok(s.path.to_string_lossy().into_owned()))
            .ok()
    };
    write_discovery(&path, port, &token, project_path.as_deref())?;

    let handle = Arc::new(BridgeHandle {
        token: token.clone(),
        port,
        discovery_path: path.clone(),
        project_path: Mutex::new(project_path),
    });

    let accept_app = app.clone();
    let accept_token = token.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let app = accept_app.clone();
            let token = accept_token.clone();
            tauri::async_runtime::spawn(async move {
                handle_connection(app, token, stream).await;
            });
        }
    });

    // Best-effort cleanup when the main window closes.
    if let Some(window) = app.get_webview_window("main") {
        let path_for_close = path.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::Destroyed = event {
                remove_discovery(&path_for_close);
            }
        });
    }

    Ok(handle)
}

/// Call after open/close/create so MCP path-matching stays current.
pub fn sync_discovery_project(app: &AppHandle, path: Option<PathBuf>) {
    if let Some(bridge) = app.try_state::<Arc<BridgeHandle>>() {
        bridge.set_project_path(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_token_accepts_only_exact_match() {
        assert!(check_token("secret", &json!({ "token": "secret" })).is_ok());
        assert_eq!(
            check_token("secret", &json!({ "token": "wrong" })).unwrap_err(),
            "invalid auth token"
        );
        assert_eq!(
            check_token("secret", &json!({})).unwrap_err(),
            "missing auth token"
        );
        // Non-string token must not pass.
        assert!(check_token("secret", &json!({ "token": 42 })).is_err());
    }

    #[test]
    fn random_token_is_hex_and_unique() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn discovery_file_round_trips_for_mcp_client() {
        let path = std::env::temp_dir()
            .join(format!("renderly-bridge-test-{}", std::process::id()))
            .join("bridge.json");
        write_discovery(&path, 45678, "tok", Some("D:/p.renderly.json")).unwrap();
        let disc: BridgeDiscovery =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(disc.pid, std::process::id());
        assert_eq!(disc.port, 45678);
        assert_eq!(disc.token, "tok");
        assert_eq!(disc.project_path.as_deref(), Some("D:/p.renderly.json"));

        // Clearing the project path must keep the file valid for path-match checks.
        write_discovery(&path, 45678, "tok", None).unwrap();
        let disc: BridgeDiscovery =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(disc.project_path.is_none());

        remove_discovery(&path);
        assert!(!path.exists());
        std::fs::remove_dir(path.parent().unwrap()).ok();
    }

    #[test]
    fn base64_encode_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn write_failures_carry_the_typed_error_in_rpc_data() {
        let failure = RpcFailure::from(EditError::revision_conflict(1, 4));
        let response = JsonRpcResponse {
            jsonrpc: "2.0",
            id: Some(json!(7)),
            result: None,
            error: Some(JsonRpcError {
                code: -32000,
                message: failure.message.clone(),
                data: failure.data.clone(),
            }),
        };
        let wire = serde_json::to_value(&response).unwrap();
        assert_eq!(wire["error"]["data"]["code"], "revision_conflict");
        assert_eq!(wire["error"]["data"]["data"]["current_revision"], 4);
        assert_eq!(
            wire["error"]["data"]["hint"],
            "re-read with timeline, then retry"
        );
        assert!(wire["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("stale")));
    }

    #[test]
    fn plain_failures_have_no_data_field() {
        let failure = RpcFailure::from("bridge says no".to_string());
        let wire = serde_json::to_value(JsonRpcError {
            code: -32000,
            message: failure.message,
            data: failure.data,
        })
        .unwrap();
        assert!(wire.get("data").is_none());
        assert_eq!(wire["message"], "bridge says no");
    }
}
