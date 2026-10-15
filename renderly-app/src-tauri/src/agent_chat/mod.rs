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
pub mod mcp_http;
pub mod outline;
pub mod permission;
pub mod process;
pub mod tools;

use crate::AppState;
use detect::AgentInfo;
use driver::{driver_for, AgentId, AgentSession, LaunchOptions, McpEndpointInfo, SessionConfig};
use events::{AgentEnvelope, AgentEvent, EventSink};
use permission::{Decision, PermissionBroker};
use renderly_core::project::{Id, Project};
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
    /// The in-process tool server for this session (Claude). Dropping it stops the server.
    endpoint: Option<mcp_http::McpEndpoint>,
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

/// What the panel knows that the app does not: the clips the user has selected.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatContext {
    #[serde(default)]
    pub selected_clip_ids: Vec<String>,
}

/// The text sent to the CLI for one user turn: the live context block (outline, playhead,
/// selection, open sequence) generated from the project, then the user's message.
pub fn compose_turn(
    project: &Project,
    playhead_secs: f64,
    sequence: Option<Id>,
    text: &str,
    ctx: Option<&ChatContext>,
) -> String {
    let selected: &[String] = ctx.map(|c| c.selected_clip_ids.as_slice()).unwrap_or(&[]);
    format!(
        "{}{text}",
        outline::context_block(project, playhead_secs, selected, sequence)
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
        // A finished turn closes its undo group, so the user's own edits are separate steps.
        if matches!(env.event, AgentEvent::TurnFinished { .. }) {
            app.state::<AppState>().end_agent_group();
        }
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
        if let Some(endpoint) = &active.endpoint {
            endpoint.stop();
        }
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
    options: Option<LaunchOptions>,
) -> Result<String, String> {
    if hub.active.lock().map_err(|e| e.to_string())?.is_some() {
        return Err("an agent session is already running; stop it first".into());
    }
    let options = options.unwrap_or_default().validated()?;
    let project_path = state.with_session(|s| Ok(s.path.clone()))?;
    let session_id = Uuid::new_v4().to_string();
    // Edits are auto-allowed by default (§4.3); the panel shows the toggle.
    let broker = PermissionBroker::new(session_id.clone(), sink_for(&app), true);
    // Claude gets the in-process tool server; Codex still uses renderly-mcp (see design doc).
    let (endpoint, mcp_endpoint, mcp_binary) = match agent_id {
        AgentId::ClaudeCode => {
            let endpoint = mcp_http::start(app.clone(), Arc::clone(&broker)).await?;
            let info = McpEndpointInfo {
                url: endpoint.url.clone(),
                token: endpoint.token.clone(),
            };
            (Some(endpoint), Some(info), PathBuf::new())
        }
        AgentId::Codex => (None, None, mcp_binary_path()?),
    };
    let cfg = SessionConfig {
        session_id: session_id.clone(),
        project_path,
        mcp_endpoint,
        options,
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
        endpoint,
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
    state: State<'_, AppState>,
    hub: State<'_, AgentChatHub>,
    session_id: String,
    text: String,
    context: Option<ChatContext>,
) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("message is empty".into());
    }
    let project = state.with_session(|s| Ok(Arc::clone(&s.project)))?;
    let prompt = compose_turn(
        &project,
        state.playhead_secs(),
        *state.active_sequence.lock(),
        text,
        context.as_ref(),
    );
    // Every edit this turn makes becomes one undo step; the group closes on TurnFinished.
    state.begin_agent_group();
    let sent = with_active(&hub, &session_id, |a| a.session.send_turn(&prompt));
    if sent.is_err() {
        state.end_agent_group();
    }
    sent
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
        if let Some(endpoint) = &active.endpoint {
            endpoint.stop();
        }
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

    fn project_with_one_track() -> Project {
        let mut p = Project::new("Demo", renderly_core::project::Settings::default());
        p.tracks.push(renderly_core::project::Track::new(
            renderly_core::project::TrackKind::Video,
            "V1",
        ));
        p
    }

    #[test]
    fn each_turn_starts_with_the_live_context_block_then_the_message() {
        let p = project_with_one_track();
        let ctx = ChatContext {
            selected_clip_ids: vec!["c1".into(), "c2".into()],
        };
        let out = compose_turn(&p, 3.5, None, "trim the first clip", Some(&ctx));
        assert!(out.starts_with("[Renderly context: live project state"));
        assert!(out.contains("playhead 0:03.5"));
        assert!(out.contains("selected clips: c1, c2"));
        assert!(out.contains("track 0 video 'V1'"));
        assert!(out.ends_with("[end context]\n\ntrim the first clip"));
    }

    #[test]
    fn turn_without_selection_has_no_selection_line() {
        let p = project_with_one_track();
        let out = compose_turn(&p, 0.0, None, "hi", None);
        assert!(!out.contains("selected clips"));
        assert!(out.ends_with("hi"));
    }
}
