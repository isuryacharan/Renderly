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

/// Everything a driver needs to start one session.
#[derive(Clone)]
pub struct SessionConfig {
    pub session_id: String,
    /// The open project. The session's cwd is its folder and `renderly-mcp --project` gets it.
    pub project_path: PathBuf,
    /// Absolute path of the `renderly-mcp` binary to register as the agent's only MCP server.
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

/// The system prompt given to every agent session.
pub fn system_prompt(project: &Path) -> String {
    format!(
        "You are editing a Renderly video project ({}). Make every change through the \
         `renderly` MCP tools (apply_command / apply_commands and the read-only perception \
         tools). Never edit the project file directly. Make small, verifiable edits and say \
         what you changed.",
        project.display()
    )
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
    std::fs::write(&path, system_prompt(&cfg.project_path))
        .map_err(|e| format!("write system prompt: {e}"))?;
    Ok(path)
}

/// One newline-terminated JSON message for a CLI's stdin.
pub fn json_line(value: &serde_json::Value) -> String {
    format!("{value}\n")
}
