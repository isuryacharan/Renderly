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
}

impl AgentId {
    pub const ALL: [AgentId; 2] = [AgentId::ClaudeCode, AgentId::Codex];

    /// The executable name looked up on `PATH`.
    pub fn program_name(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "claude",
            AgentId::Codex => "codex",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "Claude Code",
            AgentId::Codex => "Codex",
        }
    }
}

/// Model and effort the user picked for the Claude session, plus the built-in tools switch.
/// Validated before use so arbitrary text never reaches the CLI's argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchOptions {
    pub model: String,
    pub effort: String,
    /// When true, the agent may use Claude Code's built-in tools (each still asks first).
    pub native_tools: bool,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            model: DEFAULT_MODEL.into(),
            effort: DEFAULT_EFFORT.into(),
            native_tools: false,
        }
    }
}

/// Model aliases the panel offers (`claude --help`: "fable", "opus", "sonnet").
pub const MODEL_CHOICES: &[&str] = &["sonnet", "opus", "fable"];
pub const DEFAULT_MODEL: &str = "sonnet";
/// Effort levels `claude --effort` accepts.
pub const EFFORT_CHOICES: &[&str] = &["low", "medium", "high", "xhigh", "max"];
pub const DEFAULT_EFFORT: &str = "low";

impl LaunchOptions {
    /// Checks the model and effort against the allowed lists.
    pub fn validated(self) -> Result<Self, String> {
        if !MODEL_CHOICES.contains(&self.model.as_str()) {
            return Err(format!("unknown model '{}'", self.model));
        }
        if !EFFORT_CHOICES.contains(&self.effort.as_str()) {
            return Err(format!("unknown effort '{}'", self.effort));
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
    /// The in-process Renderly tool server (Claude Code). Absent for drivers that use
    /// `mcp_binary` instead.
    pub mcp_endpoint: Option<McpEndpointInfo>,
    pub options: LaunchOptions,
    /// Absolute path of the `renderly-mcp` binary to register as the agent's only MCP server
    /// (Codex, until its HTTP MCP support is verified).
    pub mcp_binary: PathBuf,
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

/// The MCP server registered for a session: the absolute `renderly-mcp` path with
/// `--project <open project>`, so the session starts on the project the app has open.
pub fn mcp_server_command(cfg: &SessionConfig) -> (String, Vec<String>) {
    (
        cfg.mcp_binary.to_string_lossy().into_owned(),
        vec![
            "--project".to_string(),
            cfg.project_path.to_string_lossy().into_owned(),
        ],
    )
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
