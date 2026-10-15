//! In-app agent chat (docs/agent-chat-design.md). Drives the user's installed, signed-in agent
//! CLI (Claude Code, Codex) as a child process; the agent edits the open project only through
//! `renderly-mcp`, which reaches this app over the loopback bridge, so edits share the undo stack.
//!
//! Phase 1: one session at a time, started against the open project. Renderly never reads or
//! stores vendor credentials and never calls a model API.

pub mod claude;
pub mod codex;
pub mod detect;
pub mod driver;
pub mod events;
pub mod permission;
pub mod process;

use crate::AppState;
use detect::AgentInfo;
use driver::{driver_for, AgentId, AgentSession, SessionConfig};
use events::{AgentEnvelope, AgentEvent, EventSink};
use permission::{Decision, PermissionBroker};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;

/// The Tauri event name carrying [`AgentEnvelope`] payloads.
pub const EVENT_NAME: &str = "agent:event";

/// The one active session, if any.
pub struct AgentChatHub {
    active: Mutex<Option<ActiveSession>>,
}

struct ActiveSession {
    id: String,
    broker: Arc<PermissionBroker>,
    session: Box<dyn AgentSession>,
}

impl AgentChatHub {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
        }
    }
}

impl Default for AgentChatHub {
    fn default() -> Self {
        Self::new()
    }
}

/// Context chips the user attached to a prompt. Each one is optional.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatContext {
    #[serde(default)]
    pub playhead_secs: Option<f64>,
    #[serde(default)]
    pub selected_clip_ids: Vec<String>,
    #[serde(default)]
    pub sequence_id: Option<String>,
}

/// Prepend a short, structured preamble to the user's text. Empty context adds nothing.
pub fn build_prompt(text: &str, ctx: Option<&ChatContext>) -> String {
    let Some(ctx) = ctx else {
        return text.to_string();
    };
    let mut lines = Vec::new();
    if let Some(t) = ctx.playhead_secs {
        lines.push(format!("playhead: {t:.2} s"));
    }
    if !ctx.selected_clip_ids.is_empty() {
        lines.push(format!(
            "selected clips: {}",
            ctx.selected_clip_ids.join(", ")
        ));
    }
    if let Some(seq) = &ctx.sequence_id {
        lines.push(format!("open sequence: {seq}"));
    }
    if lines.is_empty() {
        return text.to_string();
    }
    format!(
        "[Renderly context]\n{}\n[end context]\n\n{text}",
        lines.join("\n")
    )
}

/// Find the `renderly-mcp` binary: `RENDERLY_MCP_BIN` (tests, sandboxes), then next to the app
/// executable (installed builds), then the workspace target directories (`tauri dev`).
pub fn mcp_binary_path() -> Result<PathBuf, String> {
    let name = if cfg!(windows) {
        "renderly-mcp.exe"
    } else {
        "renderly-mcp"
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = std::env::var_os("RENDERLY_MCP_BIN") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(name));
        }
    }
    if let Some(target) = std::env::var_os("CARGO_TARGET_DIR") {
        candidates.push(PathBuf::from(&target).join("debug").join(name));
        candidates.push(PathBuf::from(&target).join("release").join(name));
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    candidates.push(manifest.join("../../target/debug").join(name));
    candidates.push(manifest.join("../../target/release").join(name));
    candidates.into_iter().find(|p| p.is_file()).ok_or_else(|| {
        "renderly-mcp was not found next to the app; build it with `cargo build -p renderly-mcp`"
            .to_string()
    })
}

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join("renderly-agent-chat")
}

fn sink_for(app: &AppHandle) -> EventSink {
    let app = app.clone();
    Arc::new(move |env: AgentEnvelope| {
        let _ = app.emit(EVENT_NAME, env);
    })
}

/// Stop the active session, if any, with `reason` in the final `session_ended` event. Used on
/// project close and app exit so no CLI process outlives the app.
pub fn stop_active(app: &AppHandle, reason: &str) {
    let hub = app.state::<AgentChatHub>();
    let taken = hub.active.lock().ok().and_then(|mut a| a.take());
    if let Some(mut active) = taken {
        active.broker.cancel_all();
        active.session.shutdown();
        let _ = app.emit(
            EVENT_NAME,
            AgentEnvelope {
                session_id: active.id,
                event: AgentEvent::SessionEnded {
                    reason: reason.into(),
                },
            },
        );
    }
}

/// Entry point for the bridge's `agent_permission` method (sent by `renderly-mcp` when Claude
/// Code asks for permission). With no session running, the answer is always deny.
pub async fn bridge_permission(
    app: &AppHandle,
    tool_name: &str,
    input: serde_json::Value,
) -> serde_json::Value {
    let broker = app
        .state::<AgentChatHub>()
        .active
        .lock()
        .ok()
        .and_then(|a| a.as_ref().map(|s| Arc::clone(&s.broker)));
    match broker {
        Some(broker) => broker
            .decide(tool_name, &input)
            .await
            .to_permission_json(&input),
        None => Decision::Deny {
            message: "no agent session is running in Renderly; permission denied".into(),
        }
        .to_permission_json(&input),
    }
}

#[tauri::command]
pub async fn agent_list() -> Result<Vec<AgentInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        AgentId::ALL
            .iter()
            .map(|a| detect::detect(*a))
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn agent_start(
    app: AppHandle,
    state: State<'_, AppState>,
    hub: State<'_, AgentChatHub>,
    agent_id: AgentId,
) -> Result<String, String> {
    if hub.active.lock().map_err(|e| e.to_string())?.is_some() {
        return Err("an agent session is already running; stop it first".into());
    }
    let project_path = state.with_session(|s| Ok(s.path.clone()))?;
    let session_id = Uuid::new_v4().to_string();
    // Edits are auto-allowed by default (§4.3); the panel shows the toggle.
    let broker = PermissionBroker::new(session_id.clone(), sink_for(&app), true);
    let mcp_binary = mcp_binary_path()?;
    let cfg = SessionConfig {
        session_id: session_id.clone(),
        project_path,
        mcp_binary,
        program: None,
        scratch_dir: scratch_dir(),
        broker: Arc::clone(&broker),
        sink: sink_for(&app),
    };
    let session = tauri::async_runtime::spawn_blocking(move || driver_for(agent_id).start(&cfg))
        .await
        .map_err(|e| e.to_string())??;
    let mut active = hub.active.lock().map_err(|e| e.to_string())?;
    if active.is_some() {
        // Another start won the race; this one is not needed.
        let mut session = session;
        session.shutdown();
        return Err("an agent session is already running; stop it first".into());
    }
    *active = Some(ActiveSession {
        id: session_id.clone(),
        broker,
        session,
    });
    Ok(session_id)
}

fn with_active<T>(
    hub: &AgentChatHub,
    session_id: &str,
    f: impl FnOnce(&mut ActiveSession) -> Result<T, String>,
) -> Result<T, String> {
    let mut guard = hub.active.lock().map_err(|e| e.to_string())?;
    match guard.as_mut() {
        Some(active) if active.id == session_id => f(active),
        _ => Err(format!("no agent session {session_id}")),
    }
}

#[tauri::command]
pub async fn agent_send(
    hub: State<'_, AgentChatHub>,
    session_id: String,
    text: String,
    context: Option<ChatContext>,
) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("message is empty".into());
    }
    let prompt = build_prompt(text, context.as_ref());
    with_active(&hub, &session_id, |a| a.session.send_turn(&prompt))
}

#[tauri::command]
pub async fn agent_answer_permission(
    hub: State<'_, AgentChatHub>,
    session_id: String,
    request_id: String,
    option_id: String,
) -> Result<(), String> {
    with_active(&hub, &session_id, |a| {
        a.broker.answer(&request_id, &option_id)
    })
}

#[tauri::command]
pub async fn agent_cancel(hub: State<'_, AgentChatHub>, session_id: String) -> Result<(), String> {
    with_active(&hub, &session_id, |a| {
        a.broker.cancel_all();
        a.session.cancel_turn()
    })
}

#[tauri::command]
pub async fn agent_set_auto_edits(
    hub: State<'_, AgentChatHub>,
    session_id: String,
    enabled: bool,
) -> Result<(), String> {
    with_active(&hub, &session_id, |a| {
        a.broker.set_auto_edits(enabled);
        Ok(())
    })
}

#[tauri::command]
pub async fn agent_stop(
    app: AppHandle,
    hub: State<'_, AgentChatHub>,
    session_id: String,
) -> Result<(), String> {
    let taken = {
        let mut guard = hub.active.lock().map_err(|e| e.to_string())?;
        match guard.as_ref() {
            Some(a) if a.id == session_id => guard.take(),
            _ => return Err(format!("no agent session {session_id}")),
        }
    };
    if let Some(mut active) = taken {
        active.broker.cancel_all();
        active.session.shutdown();
        let _ = app.emit(
            EVENT_NAME,
            AgentEnvelope {
                session_id: active.id,
                event: AgentEvent::SessionEnded {
                    reason: "stopped".into(),
                },
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_without_context_is_unchanged() {
        assert_eq!(build_prompt("trim it", None), "trim it");
        assert_eq!(
            build_prompt("trim it", Some(&ChatContext::default())),
            "trim it"
        );
    }

    #[test]
    fn prompt_gets_a_structured_preamble_from_the_chips() {
        let ctx = ChatContext {
            playhead_secs: Some(3.5),
            selected_clip_ids: vec!["c1".into(), "c2".into()],
            sequence_id: Some("seq-9".into()),
        };
        let p = build_prompt("trim the first clip", Some(&ctx));
        assert!(p.starts_with("[Renderly context]\nplayhead: 3.50 s\n"));
        assert!(p.contains("selected clips: c1, c2"));
        assert!(p.contains("open sequence: seq-9"));
        assert!(p.ends_with("[end context]\n\ntrim the first clip"));
    }
}
