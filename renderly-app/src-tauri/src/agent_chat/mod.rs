//! In-app agent chat (docs/agent-chat-design.md). Drives the user's installed, signed-in agent
//! CLI (Claude Code, Codex) or a local Ollama model. Claude Code and Codex reach Renderly's tools
//! over an in-process MCP endpoint (`mcp_http.rs`); Ollama runs its own loop and calls the same
//! tool functions directly (`ollama.rs`). Edits go through the app's `apply_commands` path, so they
//! share the undo stack.
//!
//! Renderly never reads or stores vendor credentials and never calls a model API directly.

pub mod claude;
pub mod codex;
pub mod detect;
pub mod driver;
pub mod events;
pub mod mcp_http;
pub mod models;
pub mod ollama;
pub mod outline;
pub mod permission;
pub mod process;
pub mod tools;

use crate::AppState;
use detect::AgentInfo;
use driver::{driver_for, AgentId, AgentSession, LaunchOptions, McpEndpointInfo, SessionConfig};
use events::{AgentEnvelope, AgentEvent, EventSink};
use models::{AgentModels, ModelStatus};
use permission::{Decision, PermissionBroker};
use renderly_core::project::{Id, Project};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Emitter, Manager, State};
use tools::{AppToolHost, ToolHost};
use uuid::Uuid;

/// The Tauri event name carrying [`AgentEnvelope`] payloads.
pub const EVENT_NAME: &str = "agent:event";

/// The one active session, if any. Also holds the model lists fetched this app session.
pub struct AgentChatHub {
    active: Mutex<Option<ActiveSession>>,
    /// Model lists by [`model_cache_key`]. Fetched lazily, kept until refreshed.
    models: Mutex<HashMap<String, AgentModels>>,
}

struct ActiveSession {
    id: String,
    broker: Arc<PermissionBroker>,
    session: Box<dyn AgentSession>,
    /// The in-process tool server (Claude Code, Codex). Dropping it stops the server.
    endpoint: Option<mcp_http::McpEndpoint>,
}

impl AgentChatHub {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(None),
            models: Mutex::new(HashMap::new()),
        }
    }
}

impl Default for AgentChatHub {
    fn default() -> Self {
        Self::new()
    }
}

/// The cache key for one agent's list. Ollama's list depends on the server, so the host is part
/// of its key.
fn model_cache_key(agent: AgentId, ollama_host: Option<&str>) -> String {
    match agent {
        AgentId::Ollama => format!("ollama|{}", ollama::resolve_host(ollama_host)),
        other => other.program_name().to_string(),
    }
}

/// What the panel knows that the app does not: the clips the user has selected.
#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatContext {
    #[serde(default)]
    pub selected_clip_ids: Vec<String>,
}

/// The text sent to the agent for one user turn: the live context block (outline, playhead,
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

fn scratch_dir() -> PathBuf {
    std::env::temp_dir().join("renderly-agent-chat")
}

fn sink_for(app: &AppHandle) -> EventSink {
    let app = app.clone();
    Arc::new(move |env: AgentEnvelope| {
        // A finished turn closes its undo group, so the user's own edits are separate steps.
        if matches!(env.event, AgentEvent::TurnFinished { .. }) {
            app.state::<AppState>().commit_transaction_announced(&app);
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

/// Install and running state for each agent. Model lists are not here; see `agent_models`.
#[tauri::command]
pub async fn agent_list() -> Result<Vec<AgentInfo>, String> {
    let mut infos = tauri::async_runtime::spawn_blocking(|| {
        vec![
            detect::detect(AgentId::ClaudeCode),
            detect::detect(AgentId::Codex),
        ]
    })
    .await
    .map_err(|e| e.to_string())?;
    let host = ollama::resolve_host(None);
    let installed = process::resolve_program(AgentId::Ollama.program_name()).is_some();
    let reachable = ollama::is_reachable(&host).await;
    infos.push(detect::ollama_info(installed, reachable));
    Ok(infos)
}

/// The model list for one agent, read from the agent at runtime and cached for the app session.
/// `refresh` re-reads it. Nothing is hard-coded: Claude Code and Codex report their own lists,
/// and Ollama lists the installed models that report tool support.
#[tauri::command]
pub async fn agent_models(
    hub: State<'_, AgentChatHub>,
    agent_id: AgentId,
    ollama_host: Option<String>,
    refresh: Option<bool>,
) -> Result<AgentModels, String> {
    let key = model_cache_key(agent_id, ollama_host.as_deref());
    if !refresh.unwrap_or(false) {
        if let Some(cached) = hub.models.lock().map_err(|e| e.to_string())?.get(&key) {
            return Ok(cached.clone());
        }
    }
    let fetched = fetch_models(agent_id, ollama_host.as_deref()).await;
    hub.models
        .lock()
        .map_err(|e| e.to_string())?
        .insert(key, fetched.clone());
    Ok(fetched)
}

async fn fetch_models(agent_id: AgentId, ollama_host: Option<&str>) -> AgentModels {
    match agent_id {
        AgentId::ClaudeCode | AgentId::Codex => {
            let Some(program) = process::resolve_program(agent_id.program_name()) else {
                let info = detect::detect(agent_id);
                return AgentModels::failed(agent_id, ModelStatus::NotInstalled, info.install_hint);
            };
            let scratch = scratch_dir();
            tauri::async_runtime::spawn_blocking(move || match agent_id {
                AgentId::ClaudeCode => claude::fetch_claude_models(&program, &scratch),
                _ => codex::fetch_codex_models(&program, &scratch),
            })
            .await
            .unwrap_or_else(|e| AgentModels::failed(agent_id, ModelStatus::Error, e.to_string()))
        }
        AgentId::Ollama => {
            let host = ollama::resolve_host(ollama_host);
            let list = ollama::fetch_models(&host).await;
            if list.status == ModelStatus::NotRunning
                && process::resolve_program(AgentId::Ollama.program_name()).is_none()
            {
                return AgentModels::failed(
                    AgentId::Ollama,
                    ModelStatus::NotInstalled,
                    "Ollama is not installed. Install it from https://ollama.com, then refresh.",
                );
            }
            list
        }
    }
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
    let mut options = options.unwrap_or_default().validated()?;
    // Check the choice against the list the agent reported, when that list is known. Ollama needs
    // a model, so it falls back to the list's own default.
    let key = model_cache_key(agent_id, options.ollama_host.as_deref());
    let cached = hub
        .models
        .lock()
        .map_err(|e| e.to_string())?
        .get(&key)
        .cloned();
    if let Some(list) = cached.filter(|l| l.status == ModelStatus::Ready) {
        match &options.model {
            Some(m) if list.find(m).is_none() => {
                return Err(format!(
                    "'{m}' is not in the list {} reported. Refresh the models and pick again.",
                    agent_id.display_name()
                ))
            }
            None if agent_id == AgentId::Ollama => {
                options.model = list.fallback_model().map(str::to_string)
            }
            _ => {}
        }
    }
    if agent_id == AgentId::Ollama && options.model.is_none() {
        return Err("choose an Ollama model first; refresh the list if none is shown".into());
    }
    let project_path = state.with_session(|s| Ok(s.path.clone()))?;
    let session_id = Uuid::new_v4().to_string();
    // Edits are auto-allowed by default (§4.3); the panel shows the toggle.
    let broker = PermissionBroker::new(session_id.clone(), sink_for(&app), true);
    // Claude Code and Codex get the in-process MCP endpoint. Ollama calls the same tools directly.
    let (endpoint, mcp_endpoint, tools): (
        Option<mcp_http::McpEndpoint>,
        Option<McpEndpointInfo>,
        Option<Arc<dyn ToolHost>>,
    ) = match agent_id {
        AgentId::ClaudeCode | AgentId::Codex => {
            let endpoint = mcp_http::start(app.clone(), Arc::clone(&broker)).await?;
            let info = McpEndpointInfo {
                url: endpoint.url.clone(),
                token: endpoint.token.clone(),
            };
            (Some(endpoint), Some(info), None)
        }
        AgentId::Ollama => (
            None,
            None,
            Some(Arc::new(AppToolHost { app: app.clone() }) as Arc<dyn ToolHost>),
        ),
    };
    let cfg = SessionConfig {
        session_id: session_id.clone(),
        project_path,
        mcp_endpoint,
        tools,
        options,
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
    state.begin_transaction("Agent turn");
    let sent = with_active(&hub, &session_id, |a| a.session.send_turn(&prompt));
    if sent.is_err() {
        state.commit_transaction();
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

    #[test]
    fn model_lists_are_cached_per_agent_and_per_ollama_host() {
        assert_eq!(model_cache_key(AgentId::Codex, None), "codex");
        assert_eq!(
            model_cache_key(AgentId::Ollama, Some("http://10.0.0.5:11434")),
            "ollama|http://10.0.0.5:11434"
        );
    }
}
