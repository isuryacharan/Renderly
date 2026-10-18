//! The `AgentDriver` abstraction: one driver per vendor CLI, each speaking that CLI's own
//! headless protocol. A driver starts a session (one CLI process), sends user turns, cancels a
//! turn, and shuts down. Drivers publish normalised [`AgentEvent`](super::events::AgentEvent)s
//! through the session's [`EventSink`](super::events::EventSink) and route permission requests
//! through the session's [`PermissionBroker`](super::permission::PermissionBroker).

use super::events::EventSink;
use super::permission::PermissionBroker;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentId {
    ClaudeCode,
    Codex,
    Ollama,
}

impl AgentId {
    pub const ALL: [AgentId; 3] = [AgentId::ClaudeCode, AgentId::Codex, AgentId::Ollama];

    /// The executable name looked up on `PATH`. For Ollama this is the install check; the
    /// service itself is reached over HTTP.
    pub fn program_name(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "claude",
            AgentId::Codex => "codex",
            AgentId::Ollama => "ollama",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "Claude Code",
            AgentId::Codex => "Codex",
            AgentId::Ollama => "Ollama (local)",
        }
    }
}

/// The model, effort and tools switch the user picked for a session. There are no fixed lists
/// here: the model and effort are whatever the agent reported (see `models.rs`). Values are
/// shape-checked before use, so arbitrary text never reaches an argv or a request body.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchOptions {
    /// The agent's model value. `None` means the agent's own default.
    #[serde(default)]
    pub model: Option<String>,
    /// The agent's effort level. `None` means the agent's own default.
    #[serde(default)]
    pub effort: Option<String>,
    /// When true, the agent may use its own built-in tools (each still asks first).
    #[serde(default)]
    pub native_tools: bool,
    /// Ollama only: the server URL. `None` uses `OLLAMA_HOST`, then the default.
    #[serde(default)]
    pub ollama_host: Option<String>,
}

/// Longest model or effort value accepted.
const MAX_OPTION_LEN: usize = 128;

fn option_shape_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPTION_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '/' | '-'))
}

impl LaunchOptions {
    /// Checks the shape of the model, effort and host values. Membership in the agent's list is
    /// checked where that list is known (see `agent_start`).
    pub fn validated(self) -> Result<Self, String> {
        if let Some(model) = &self.model {
            if !option_shape_ok(model) {
                return Err(format!("invalid model '{model}'"));
            }
        }
        if let Some(effort) = &self.effort {
            if !option_shape_ok(effort) {
                return Err(format!("invalid effort '{effort}'"));
            }
        }
        if let Some(host) = &self.ollama_host {
            let ok = host.len() <= 256
                && (host.starts_with("http://") || host.starts_with("https://"))
                && !host.chars().any(char::is_whitespace);
            if !ok {
                return Err(format!("invalid Ollama host '{host}'"));
            }
        }
        Ok(self)
    }
}

/// The per-session in-process MCP endpoint a driver registers with its CLI.
#[derive(Clone, Debug)]
pub struct McpEndpointInfo {
    pub url: String,
    pub token: String,
}

/// Everything a driver needs to start one session.
#[derive(Clone)]
pub struct SessionConfig {
    pub session_id: String,
    /// The open project. Used for the session's file names and, for Codex, its folder.
    pub project_path: PathBuf,
    /// The in-process Renderly tool server, reached over MCP Streamable HTTP. Used by Claude
    /// Code and Codex. Absent for Ollama, which calls `tools` directly.
    pub mcp_endpoint: Option<McpEndpointInfo>,
    /// The Renderly tools, called in-process. Used by Ollama, which runs its own loop.
    pub tools: Option<Arc<dyn super::tools::ToolHost>>,
    pub options: LaunchOptions,
    /// Overrides the `PATH` lookup of the vendor CLI (tests, non-standard installs).
    pub program: Option<PathBuf>,
    /// Where launch files (MCP config, system prompt) are written.
    pub scratch_dir: PathBuf,
    pub broker: Arc<PermissionBroker>,
    pub sink: EventSink,
}

pub trait AgentDriver: Send + Sync {
    fn agent(&self) -> AgentId;
    /// Spawn the CLI for this session. Errors name what is missing (for example, not installed).
    fn start(&self, cfg: &SessionConfig) -> Result<Box<dyn AgentSession>, String>;
}

pub trait AgentSession: Send {
    /// Start a turn with `text`. Events arrive through the sink until `turn_finished`.
    fn send_turn(&mut self, text: &str) -> Result<(), String>;
    /// Stop the turn in progress. The session stays usable for the next turn.
    fn cancel_turn(&mut self) -> Result<(), String>;
    /// Kill the CLI process and everything it started. Idempotent.
    fn shutdown(&mut self);
}

pub fn driver_for(agent: AgentId) -> Box<dyn AgentDriver> {
    match agent {
        AgentId::ClaudeCode => Box::new(super::claude::ClaudeDriver),
        AgentId::Codex => Box::new(super::codex::CodexDriver),
        AgentId::Ollama => Box::new(super::ollama::OllamaDriver),
    }
}

/// The system prompt for the in-app editing assistant. It replaces Claude Code's default
/// coding prompt (`--system-prompt-file`). Facts here are checked against renderly-core.
pub const RENDERLY_BRIEF: &str = "\
You are the editing assistant built into Renderly, a video editor. The user is editing the open \
project and talks to you while they work. You change the project with your tools, not by \
describing steps.

Tools (server `renderly`):
- edit: apply one or more commands as ONE undo step. Each command is a JSON object tagged by \
\"command\". Use the schema; it lists every field. Send related changes in one call.
- timeline: outline of the open timeline with short ids (first 8 hex characters). Call it when \
the context block is truncated or when you need an id you do not have.
- look: a PNG of the composited frame at a time (default: playhead). Use it to check visual \
results when the result matters, not after every change.
- find_media: media files in a folder, for ImportMedia.
- assets: built-in effects with parameters, transitions, caption styles, loaded packs.

Rules:
- Act directly. If the request is clear, make the edit, then reply. If it is ambiguous (which \
clip, how long, which style), ask one short question instead of guessing.
- Ids: pass short ids from the context or timeline; they are resolved for you. Times are seconds.
- Transform: x and y are the clip centre offset from the canvas centre in normalised device \
coordinates (-1 to 1; +x right, +y up). scale_x and scale_y: 1.0 is the cover-fit size (the clip \
fills the canvas and crops the overflow). rotation_deg is in degrees. fit_x and fit_y move the \
cover window inside a source that overflows (-1 to 1; 0 is centred).
- Keyframe times are relative to the start of the clip, in seconds.
- SetClipEffects replaces the clip's whole effect list. Read the clip's current effects from the \
timeline outline and send the full list with your change. Params you omit use their defaults.
- Speed: SetClipSpeed changes playback rate; the timeline span shrinks or grows accordingly.
- Undo: all edits in one message are one undo step, and the user can revert the whole turn.
- Stale ids: timeline and edit report project_revision. Pass expected_revision to edit with the \
revision from your latest timeline read. If it fails with revision_conflict, call timeline and retry.
- Big or destructive edits: send dry_run: true first, check what it lists under changed, then send \
the same commands without dry_run.
- Deleting clips, tracks, media or sequences is refused unless allow_deletion gives a reason and \
the exact full ids a dry run lists under deleted. Without the user's clear request to delete, \
offer the deletion and wait.
- Never edit the project file yourself. Never run shell commands; you have no shell.

Replies: one or two short sentences saying what you did or what you need. The app shows the \
exact changes itself, so do not list them again, and do not describe tool names, JSON or ids \
unless the user asks.";

/// The system prompt text the session gets. Kept as a function so drivers share one source.
pub fn system_prompt() -> &'static str {
    RENDERLY_BRIEF
}

/// The folder a session runs in: the project file's parent directory.
pub fn project_dir(project_file: &Path) -> PathBuf {
    project_file
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Write the system prompt to the scratch dir and return its path.
pub fn write_system_prompt(cfg: &SessionConfig) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&cfg.scratch_dir).map_err(|e| format!("scratch dir: {e}"))?;
    let path = cfg
        .scratch_dir
        .join(format!("{}-system-prompt.txt", cfg.session_id));
    std::fs::write(&path, system_prompt()).map_err(|e| format!("write system prompt: {e}"))?;
    Ok(path)
}

/// One newline-terminated JSON message for a CLI's stdin.
pub fn json_line(value: &serde_json::Value) -> String {
    format!("{value}\n")
}
