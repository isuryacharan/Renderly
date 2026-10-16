//! Claude Code driver: runs the user's installed, signed-in `claude` CLI in headless streaming
//! mode. Renderly never handles the login; the CLI uses the account it was signed into.
//!
//! # Flags and message shapes relied on
//!
//! Sources: Claude Code CLI reference <https://code.claude.com/docs/en/cli-reference> and
//! headless mode <https://code.claude.com/docs/en/headless>; the input message shape from
//! <https://code.claude.com/docs/en/agent-sdk/user-input> (`{"type":"user","message":{...}}`).
//!
//! Launch (one process per session, stdin and stdout are NDJSON):
//! - `-p` (`--print`): non-interactive mode.
//! - `--input-format stream-json`: user turns are read from stdin, one JSON object per line.
//! - `--output-format stream-json --verbose`: events are written as NDJSON to stdout.
//! - `--include-partial-messages`: assistant text arrives as `stream_event` deltas.
//! - `--mcp-config <file>` + `--strict-mcp-config`: `renderly` is the only MCP server, an
//!   in-process Streamable HTTP endpoint on 127.0.0.1 with a bearer token (see `mcp_http.rs`).
//!   The config is written to a file (not passed inline) so Windows shims get no quoting.
//! - `--setting-sources ""`, `--disable-slash-commands`, and a scratch working directory: no
//!   user, project or local settings, hooks, plugins, skills or project CLAUDE.md. Auth and
//!   the model still come from the user's own login. (`--safe-mode` also disables the MCP
//!   config, so it is not used. Whether a user-level `~/.claude/CLAUDE.md` still loads is not
//!   verified; see docs/agent-chat-design.md.)
//! - `--system-prompt-file <path>`: Renderly's brief REPLACES Claude Code's coding prompt.
//!   (The flag is present in the installed binary; `--system-prompt-file` is listed in the
//!   binary's option table, verified with `Select-String` on `claude.exe`.)
//! - `--tools Read,Grep,Glob` by default (read-only built-ins). With "Let the agent use its own
//!   tools" on, `--tools default` (all built-ins), and each still asks via the permission tool.
//! - `--model <alias>` and `--effort <level>`: chosen in the panel, validated before use.
//! - `--permission-prompt-tool mcp__renderly__permission_prompt`: Claude Code asks this tool
//!   for built-in tool calls that are not pre-allowed. Renderly's own tools are pre-allowed
//!   and never prompt. The reply is JSON `{"behavior":"allow","updatedInput":…}` or
//!   `{"behavior":"deny","message":…}`.
//! - `--allowedTools <csv>`: Renderly's tools and the read-only built-ins run without a prompt.
//! - `--session-id <uuid>` on the first launch, `--resume <uuid>` after a cancel or crash.
//!
//! Output messages (`type` field):
//! - `system` (`subtype` `init`): session metadata. Ignored.
//! - `stream_event`: `event.type == "content_block_delta"` with `delta.type == "text_delta"`
//!   carries `delta.text`. The documented jq filter in headless mode uses the same path.
//! - `assistant`: `message.content[]` blocks; `type == "tool_use"` gives `id`, `name`, `input`.
//!   Text blocks are ignored here because the deltas already streamed them.
//! - `user`: `message.content[]` blocks; `type == "tool_result"` gives `tool_use_id`,
//!   `content` (string or array of text blocks) and `is_error`.
//! - `result`: last message of a turn; `stop_reason` when present, else `subtype`.
//!
//! Cancel: the CLI ends a turn on SIGINT per the docs, but SIGINT is not reliably deliverable
//! to a child on Windows. Cancel therefore kills the process tree and the next turn relaunches
//! with `--resume`, which keeps the conversation.

use super::driver::{
    json_line, write_system_prompt, AgentDriver, AgentId, AgentSession, LaunchOptions,
    McpEndpointInfo, SessionConfig,
};
use super::events::{summarize, AgentEnvelope, AgentEvent, EventSink};
use super::process::{self, StderrTail};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The MCP tool that answers Claude Code's permission prompts, as named by Claude Code
/// (`mcp__<server>__<tool>`).
pub const PERMISSION_PROMPT_TOOL: &str = "mcp__renderly__permission_prompt";

/// Renderly's tools: pre-allowed, so they never prompt. They run in-process in the app.
pub const PRE_ALLOWED_TOOLS: &[&str] = &["edit", "timeline", "look", "find_media", "assets"];

/// Claude Code's read-only built-ins. Always available and never prompt.
pub const READ_ONLY_BUILTINS: &[&str] = &["Read", "Grep", "Glob"];

pub struct ClaudeDriver;

impl AgentDriver for ClaudeDriver {
    fn agent(&self) -> AgentId {
        AgentId::ClaudeCode
    }

    fn start(&self, cfg: &SessionConfig) -> Result<Box<dyn AgentSession>, String> {
        let program = match &cfg.program {
            Some(p) => p.clone(),
            None => super::process::resolve_program(AgentId::ClaudeCode.program_name())
                .ok_or("Claude Code (claude) is not installed or not on PATH")?,
        };
        let options = cfg.options.clone().validated()?;
        let mcp_config = write_mcp_config(cfg)?;
        let system_prompt_file = write_system_prompt(cfg)?;
        let mut session = ClaudeSession {
            cfg: cfg.clone(),
            program,
            mcp_config,
            system_prompt_file,
            options,
            launched_once: false,
            child: None,
            stdin: None,
            stderr: StderrTail::default(),
            alive: Arc::new(AtomicBool::new(false)),
            turn_active: Arc::new(AtomicBool::new(false)),
        };
        session.spawn()?;
        Ok(Box::new(session))
    }
}

/// `claude` MCP config (JSON file): `renderly` is the only server, the in-process HTTP endpoint,
/// called with the session's bearer token.
pub fn mcp_config_json(endpoint: &McpEndpointInfo) -> Value {
    json!({
        "mcpServers": {
            "renderly": {
                "type": "http",
                "url": endpoint.url,
                "headers": { "Authorization": format!("Bearer {}", endpoint.token) },
            }
        }
    })
}

fn write_mcp_config(cfg: &SessionConfig) -> Result<PathBuf, String> {
    let endpoint = cfg
        .mcp_endpoint
        .as_ref()
        .ok_or("the Renderly tool server is not running for this session")?;
    std::fs::create_dir_all(&cfg.scratch_dir).map_err(|e| format!("scratch dir: {e}"))?;
    let path = cfg.scratch_dir.join(format!("{}-mcp.json", cfg.session_id));
    let body = mcp_config_json(endpoint);
    std::fs::write(&path, body.to_string()).map_err(|e| format!("write MCP config: {e}"))?;
    Ok(path)
}

/// The request id of the model-list probe, so its reply is told apart from other output.
const INIT_REQUEST_ID: &str = "renderly-init";

/// Read the models Claude Code offers, with no prompt and no model call.
///
/// Sources: the Agent SDK's control protocol (`control_request` with `request.subtype` of
/// `initialize`, sent on stdin in stream-json mode). Probed against `claude` 2.1.301 on
/// 2026-10-16: the `control_response` is `{"type":"control_response","response":{"subtype":
/// "success","request_id":…,"response":{…,"models":[{value, resolvedModel, displayName,
/// description, supportsEffort, supportedEffortLevels, …}],"account":{…}}}}`. Without an
/// `account` object the CLI is not signed in (see `models::parse_claude_models`).
pub fn fetch_claude_models(program: &Path, scratch: &Path) -> super::models::AgentModels {
    use super::models::{parse_claude_models, AgentModels, ModelStatus};
    let args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--setting-sources",
        "",
        "--disable-slash-commands",
        "--tools",
        "Read,Grep,Glob",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Err(e) = std::fs::create_dir_all(scratch) {
        return AgentModels::failed(
            AgentId::ClaudeCode,
            ModelStatus::Error,
            format!("scratch dir: {e}"),
        );
    }
    let mut child = match process::spawn(program, &args, scratch) {
        Ok(c) => c,
        Err(e) => {
            return AgentModels::failed(
                AgentId::ClaudeCode,
                ModelStatus::Error,
                format!("could not start claude: {e}"),
            )
        }
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        process::kill_tree(&mut child);
        return AgentModels::failed(
            AgentId::ClaudeCode,
            ModelStatus::Error,
            "claude pipes unavailable",
        );
    };
    let request = json!({
        "type": "control_request",
        "request_id": INIT_REQUEST_ID,
        "request": { "subtype": "initialize" },
    });
    use std::io::Write;
    let sent = stdin
        .write_all(json_line(&request).as_bytes())
        .and_then(|_| stdin.flush());
    let reply = match sent {
        Ok(()) => super::models::await_reply(stdout, std::time::Duration::from_secs(30), |v| {
            v.get("type").and_then(Value::as_str) == Some("control_response")
                && v.get("response")
                    .and_then(|r| r.get("request_id"))
                    .and_then(Value::as_str)
                    == Some(INIT_REQUEST_ID)
        }),
        Err(_) => None,
    };
    process::kill_tree(&mut child);
    match reply {
        Some(v) => parse_claude_models(&v)
            .unwrap_or_else(|e| AgentModels::failed(AgentId::ClaudeCode, ModelStatus::Error, e)),
        None => AgentModels::failed(
            AgentId::ClaudeCode,
            ModelStatus::Error,
            "Claude Code did not answer the model request in time.",
        ),
    }
}

/// Inputs for one `claude` launch. Borrowed so the argv builder stays a pure function.
pub struct ClaudeLaunch<'a> {
    pub session_id: &'a str,
    pub mcp_config: &'a Path,
    pub system_prompt_file: &'a Path,
    pub resume: bool,
    pub options: &'a LaunchOptions,
}

/// Build the argv for one `claude` launch. Pure, so the flag set is unit-tested.
pub fn claude_args(launch: &ClaudeLaunch) -> Vec<String> {
    let allowed: Vec<String> = PRE_ALLOWED_TOOLS
        .iter()
        .map(|t| format!("mcp__renderly__{t}"))
        .chain(READ_ONLY_BUILTINS.iter().map(|t| t.to_string()))
        .collect();
    let builtin_tools = if launch.options.native_tools {
        "default".to_string()
    } else {
        READ_ONLY_BUILTINS.join(",")
    };
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--mcp-config".into(),
        launch.mcp_config.to_string_lossy().into_owned(),
        "--strict-mcp-config".into(),
        // No user, project or local settings; no hooks, plugins or skills; no project CLAUDE.md.
        "--setting-sources".into(),
        String::new(),
        "--disable-slash-commands".into(),
        "--tools".into(),
        builtin_tools,
        "--permission-prompt-tool".into(),
        PERMISSION_PROMPT_TOOL.into(),
        "--system-prompt-file".into(),
        launch.system_prompt_file.to_string_lossy().into_owned(),
    ];
    // Model and effort are passed only when the user chose one; otherwise Claude Code's own
    // defaults apply. Both values come from the list Claude Code reported (see `models.rs`).
    if let Some(model) = &launch.options.model {
        args.push("--model".into());
        args.push(model.clone());
    }
    if let Some(effort) = &launch.options.effort {
        args.push("--effort".into());
        args.push(effort.clone());
    }
    if launch.resume {
        args.push("--resume".into());
        args.push(launch.session_id.into());
    } else {
        args.push("--session-id".into());
        args.push(launch.session_id.into());
    }
    // Last, because `--allowedTools` takes a list.
    args.push("--allowedTools".into());
    args.push(allowed.join(","));
    args
}

/// The stdin message for one user turn (`{"type":"user","message":{"role":"user","content":…}}`).
pub fn user_message(text: &str) -> String {
    json_line(&json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    }))
}

/// Per-process state for turning stream-json lines into events.
#[derive(Default)]
pub struct StreamState {
    seen_tool_ids: HashSet<String>,
    /// Tool name by tool_use id, so a result can be read according to its tool.
    tool_names: std::collections::HashMap<String, String>,
}

/// The Renderly edit tool as Claude Code names it.
pub const EDIT_TOOL: &str = "mcp__renderly__edit";

/// Parse one stdout line of `claude --output-format stream-json` into zero or more events.
/// Unknown or malformed lines produce nothing.
pub fn parse_line(line: &str, state: &mut StreamState) -> Vec<AgentEvent> {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Vec::new();
    };
    match v.get("type").and_then(Value::as_str) {
        Some("stream_event") => {
            let delta = v.get("event").and_then(|e| e.get("delta"));
            let is_text =
                delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta");
            match (
                is_text,
                delta.and_then(|d| d.get("text")).and_then(Value::as_str),
            ) {
                (true, Some(text)) if !text.is_empty() => {
                    vec![AgentEvent::TextDelta {
                        text: text.to_string(),
                    }]
                }
                _ => Vec::new(),
            }
        }
        Some("assistant") => content_blocks(&v)
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .filter_map(|b| {
                let id = b.get("id").and_then(Value::as_str)?.to_string();
                if !state.seen_tool_ids.insert(id.clone()) {
                    return None;
                }
                let name = b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                state.tool_names.insert(id.clone(), name.clone());
                Some(AgentEvent::ToolStarted {
                    id,
                    name,
                    input: b.get("input").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
        Some("user") => content_blocks(&v)
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .filter_map(|b| {
                let id = b.get("tool_use_id").and_then(Value::as_str)?.to_string();
                // The edit tool's result lists one line per command; keep all of it.
                let limit = if state.tool_names.get(&id).map(String::as_str) == Some(EDIT_TOOL) {
                    4000
                } else {
                    400
                };
                Some(AgentEvent::ToolFinished {
                    id,
                    output_summary: summarize(&tool_result_text(b), limit),
                    is_error: b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect(),
        Some("result") => {
            let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("");
            let stop_reason = v
                .get("stop_reason")
                .and_then(Value::as_str)
                .unwrap_or(subtype)
                .to_string();
            let mut events = Vec::new();
            if is_error {
                let message = v
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("Claude Code reported an error")
                    .to_string();
                events.push(AgentEvent::Error { message });
            }
            events.push(AgentEvent::TurnFinished { stop_reason });
            events
        }
        _ => Vec::new(),
    }
}

fn content_blocks(v: &Value) -> Vec<Value> {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A tool_result's `content` is a string or an array of blocks; keep only the text.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

struct ClaudeSession {
    cfg: SessionConfig,
    program: PathBuf,
    mcp_config: PathBuf,
    system_prompt_file: PathBuf,
    options: LaunchOptions,
    launched_once: bool,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    stderr: StderrTail,
    /// True while the current process is supposed to be running (cleared before we kill it).
    alive: Arc<AtomicBool>,
    turn_active: Arc<AtomicBool>,
}

impl ClaudeSession {
    fn spawn(&mut self) -> Result<(), String> {
        let resume = self.launched_once;
        let args = claude_args(&ClaudeLaunch {
            session_id: &self.cfg.session_id,
            mcp_config: &self.mcp_config,
            system_prompt_file: &self.system_prompt_file,
            resume,
            options: &self.options,
        });
        // The scratch folder, not the project folder: a project's own CLAUDE.md is never read.
        let cwd = self.cfg.scratch_dir.clone();
        std::fs::create_dir_all(&cwd).map_err(|e| format!("scratch dir: {e}"))?;
        let mut child = process::spawn(&self.program, &args, &cwd)
            .map_err(|e| format!("could not start claude: {e}"))?;
        let stdin = child.stdin.take().ok_or("claude stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("claude stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("claude stderr unavailable")?;
        self.stderr = process::drain_stderr(stderr);
        // A fresh flag per process, so the old process's reader cannot touch the new one.
        self.alive = Arc::new(AtomicBool::new(true));

        let sink: EventSink = self.cfg.sink.clone();
        let session_id = self.cfg.session_id.clone();
        let alive = Arc::clone(&self.alive);
        let turn_active = Arc::clone(&self.turn_active);
        let stderr_tail = self.stderr.clone();
        let broker_for_cleanup = Arc::clone(&self.cfg.broker);
        std::thread::spawn(move || {
            let emit = |event: AgentEvent| {
                sink(AgentEnvelope {
                    session_id: session_id.clone(),
                    event,
                })
            };
            let mut state = StreamState::default();
            process::read_lines(stdout, |line| {
                for event in parse_line(line, &mut state) {
                    if matches!(event, AgentEvent::TurnFinished { .. }) {
                        turn_active.store(false, Ordering::SeqCst);
                    }
                    emit(event);
                }
            });
            // EOF. A deliberate kill clears `alive` first and gets no message.
            if alive.swap(false, Ordering::SeqCst) {
                if turn_active.swap(false, Ordering::SeqCst) {
                    emit(AgentEvent::TurnFinished {
                        stop_reason: "error".into(),
                    });
                }
                broker_for_cleanup.cancel_all();
                let tail = stderr_tail.last_lines();
                let message = if tail.is_empty() {
                    "Claude Code exited unexpectedly".to_string()
                } else {
                    format!("Claude Code exited unexpectedly:\n{tail}")
                };
                emit(AgentEvent::Error { message });
            }
        });

        self.child = Some(child);
        self.stdin = Some(stdin);
        if !self.launched_once {
            self.launched_once = true;
            (self.cfg.sink)(AgentEnvelope {
                session_id: self.cfg.session_id.clone(),
                event: AgentEvent::SessionStarted {
                    agent_id: AgentId::ClaudeCode.program_name().into(),
                },
            });
        }
        Ok(())
    }

    fn kill_process(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            process::kill_tree(&mut child);
        }
    }
}

impl AgentSession for ClaudeSession {
    fn send_turn(&mut self, text: &str) -> Result<(), String> {
        // A CLI that exited on its own (crash, login expired) is relaunched with --resume.
        let exited = match self.child.as_mut() {
            Some(child) => child.try_wait().ok().flatten().is_some(),
            None => true,
        };
        if exited {
            self.child = None;
            self.stdin = None;
            self.spawn()?;
        }
        let stdin = self.stdin.as_mut().ok_or("claude is not running")?;
        use std::io::Write;
        stdin
            .write_all(user_message(text).as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("could not send to claude: {e}"))?;
        self.turn_active.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn cancel_turn(&mut self) -> Result<(), String> {
        if !self.turn_active.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        self.kill_process();
        self.cfg.broker.cancel_all();
        (self.cfg.sink)(AgentEnvelope {
            session_id: self.cfg.session_id.clone(),
            event: AgentEvent::TurnFinished {
                stop_reason: "cancelled".into(),
            },
        });
        Ok(())
    }

    fn shutdown(&mut self) {
        self.cfg.broker.cancel_all();
        self.kill_process();
        self.turn_active.store(false, Ordering::SeqCst);
    }
}

impl Drop for ClaudeSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(options: &LaunchOptions, resume: bool) -> Vec<String> {
        claude_args(&ClaudeLaunch {
            session_id: "11111111-2222-3333-4444-555555555555",
            mcp_config: Path::new("/tmp/mcp.json"),
            system_prompt_file: Path::new("/tmp/sys.txt"),
            resume,
            options,
        })
    }

    fn value_after<'a>(args: &'a [String], flag: &str) -> &'a str {
        let pos = args
            .iter()
            .position(|a| a == flag)
            .unwrap_or_else(|| panic!("missing {flag}"));
        &args[pos + 1]
    }

    #[test]
    fn default_launch_is_renderly_only_with_read_only_builtins() {
        let args = launch(&LaunchOptions::default(), false);
        for flag in [
            "-p",
            "--input-format",
            "--output-format",
            "--verbose",
            "--include-partial-messages",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--permission-prompt-tool",
            "--system-prompt-file",
            "--session-id",
            "--allowedTools",
        ] {
            assert!(args.iter().any(|a| a == flag), "missing {flag}");
        }
        assert!(!args.iter().any(|a| a == "--append-system-prompt-file"));
        // Settings are off: an empty setting-sources value, not a missing flag.
        assert_eq!(value_after(&args, "--setting-sources"), "");
        assert_eq!(value_after(&args, "--tools"), "Read,Grep,Glob");
        assert_eq!(
            value_after(&args, "--permission-prompt-tool"),
            PERMISSION_PROMPT_TOOL
        );
        // No model or effort chosen: neither flag is passed, so Claude Code's defaults apply.
        assert!(!args.iter().any(|a| a == "--model"));
        assert!(!args.iter().any(|a| a == "--effort"));
        assert_eq!(value_after(&args, "--system-prompt-file"), "/tmp/sys.txt");
        let allowed = args.last().unwrap();
        assert!(allowed.contains("mcp__renderly__edit"));
        assert!(allowed.contains("mcp__renderly__timeline"));
        assert!(allowed.contains("Read"));
        assert!(
            !allowed.contains("Bash"),
            "shell tools are never pre-allowed"
        );
        assert!(!allowed.contains("Write") && !allowed.contains("Edit"));
    }

    #[test]
    fn native_tools_switch_enables_all_builtins_but_keeps_the_prompt_tool() {
        let options = LaunchOptions {
            native_tools: true,
            ..LaunchOptions::default()
        };
        let args = launch(&options, false);
        assert_eq!(value_after(&args, "--tools"), "default");
        assert_eq!(
            value_after(&args, "--permission-prompt-tool"),
            PERMISSION_PROMPT_TOOL
        );
        // Built-in writes and shell still ask: they are not in the pre-allowed list.
        let allowed = args.last().unwrap();
        assert!(!allowed.contains("Bash"));
    }

    #[test]
    fn model_and_effort_are_passed_through() {
        let options = LaunchOptions {
            model: Some("opus".into()),
            effort: Some("high".into()),
            native_tools: false,
            ollama_host: None,
        };
        let args = launch(&options, false);
        assert_eq!(value_after(&args, "--model"), "opus");
        assert_eq!(value_after(&args, "--effort"), "high");
    }

    #[test]
    fn option_values_are_shape_checked_not_list_checked() {
        // A value the list did not name is still a valid shape: the list check happens at start.
        let new_model = LaunchOptions {
            model: Some("claude-future-5-0".into()),
            ..LaunchOptions::default()
        };
        assert!(new_model.validated().is_ok());
        let bad_model = LaunchOptions {
            model: Some("sonnet --dangerously-skip".into()),
            ..LaunchOptions::default()
        };
        assert!(bad_model.validated().is_err());
        let bad_effort = LaunchOptions {
            effort: Some("high;rm".into()),
            ..LaunchOptions::default()
        };
        assert!(bad_effort.validated().is_err());
        assert!(LaunchOptions::default().validated().is_ok());
    }

    #[test]
    fn resume_uses_resume_not_session_id() {
        let args = launch(&LaunchOptions::default(), true);
        assert!(args.iter().any(|a| a == "--resume"));
        assert!(!args.iter().any(|a| a == "--session-id"));
    }

    #[test]
    fn allowed_tools_stays_the_last_argument() {
        for native in [false, true] {
            let options = LaunchOptions {
                native_tools: native,
                ..LaunchOptions::default()
            };
            let args = launch(&options, false);
            assert_eq!(args[args.len() - 2], "--allowedTools");
        }
    }

    #[test]
    fn mcp_config_is_an_http_entry_with_bearer_auth() {
        let endpoint = McpEndpointInfo {
            url: "http://127.0.0.1:50123/mcp".into(),
            token: "tok123".into(),
        };
        let v = mcp_config_json(&endpoint);
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1);
        let r = &servers["renderly"];
        assert_eq!(r["type"], "http");
        assert_eq!(r["url"], "http://127.0.0.1:50123/mcp");
        assert_eq!(r["headers"]["Authorization"], "Bearer tok123");
    }

    #[test]
    fn parses_streamed_text_and_tool_cards() {
        let mut st = StreamState::default();
        let delta = r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hel"}}}"#;
        assert_eq!(
            parse_line(delta, &mut st),
            vec![AgentEvent::TextDelta { text: "Hel".into() }]
        );
        let tool = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu_1","name":"mcp__renderly__apply_command","input":{"command":{"command":"TrimClip"}}}]}}"#;
        let evs = parse_line(tool, &mut st);
        assert!(matches!(&evs[0], AgentEvent::ToolStarted { id, .. } if id == "tu_1"));
        // The same tool_use seen again (partial message replay) is not duplicated.
        assert!(parse_line(tool, &mut st).is_empty());
        let result = r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"tu_1","content":[{"type":"text","text":"ok"}],"is_error":false}]}}"#;
        assert_eq!(
            parse_line(result, &mut st),
            vec![AgentEvent::ToolFinished {
                id: "tu_1".into(),
                output_summary: "ok".into(),
                is_error: false
            }]
        );
    }

    #[test]
    fn result_ends_the_turn_and_errors_are_reported_first() {
        let mut st = StreamState::default();
        let ok = r#"{"type":"result","subtype":"success","is_error":false,"result":"done","stop_reason":"end_turn"}"#;
        assert_eq!(
            parse_line(ok, &mut st),
            vec![AgentEvent::TurnFinished {
                stop_reason: "end_turn".into()
            }]
        );
        let bad =
            r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":"too long"}"#;
        let evs = parse_line(bad, &mut st);
        assert_eq!(evs.len(), 2);
        assert!(matches!(evs[0], AgentEvent::Error { .. }));
        assert_eq!(
            evs[1],
            AgentEvent::TurnFinished {
                stop_reason: "error_max_turns".into()
            }
        );
    }

    #[test]
    fn unknown_and_malformed_lines_are_ignored() {
        let mut st = StreamState::default();
        assert!(parse_line("not json", &mut st).is_empty());
        assert!(parse_line(r#"{"type":"system","subtype":"init"}"#, &mut st).is_empty());
    }

    #[test]
    fn user_message_has_the_documented_shape() {
        let line = user_message("hi");
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "hi");
    }
}
