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
//! - `--mcp-config <file>` + `--strict-mcp-config`: `renderly` is the only MCP server. The
//!   config is written to a file (not passed inline) so Windows `.cmd` shims get no quoting.
//! - `--permission-prompt-tool mcp__renderly__permission_prompt`: Claude Code calls this MCP
//!   tool (served by `renderly-mcp`) for every tool call that is not pre-allowed. The reply
//!   text is JSON `{"behavior":"allow","updatedInput":…}` or `{"behavior":"deny","message":…}`.
//!   This is the documented mechanism; the reply shape follows the documented `canUseTool`
//!   contract, and it has not been checked against the real binary.
//! - `--allowedTools <csv>`: read-only Renderly perception tools run without a prompt.
//! - `--append-system-prompt-file <path>`: Renderly's system prompt, appended to the default.
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
    json_line, mcp_server_command, project_dir, write_system_prompt, AgentDriver, AgentId,
    AgentSession, SessionConfig,
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

/// Renderly's read-only perception tools: pre-allowed so they never prompt.
pub const PRE_ALLOWED_TOOLS: &[&str] = &[
    "describe_commands",
    "get_project",
    "get_editor_status",
    "render_frame",
    "get_transcript",
    "detect_silence",
    "detect_scenes",
    "get_audio_peaks",
    "probe_media",
    "list_models",
    "get_background_matte_status",
];

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
        let mcp_config = write_mcp_config(cfg)?;
        let system_prompt_file = write_system_prompt(cfg)?;
        let mut session = ClaudeSession {
            cfg: cfg.clone(),
            program,
            mcp_config,
            system_prompt_file,
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

/// `claude` MCP config (JSON file): `renderly` is the only server, started with `--project`.
pub fn mcp_config_json(mcp_binary: &str, project_args: &[String]) -> Value {
    json!({
        "mcpServers": {
            "renderly": {
                "type": "stdio",
                "command": mcp_binary,
                "args": project_args,
            }
        }
    })
}

fn write_mcp_config(cfg: &SessionConfig) -> Result<PathBuf, String> {
    std::fs::create_dir_all(&cfg.scratch_dir).map_err(|e| format!("scratch dir: {e}"))?;
    let (command, args) = mcp_server_command(cfg);
    let path = cfg.scratch_dir.join(format!("{}-mcp.json", cfg.session_id));
    let body = mcp_config_json(&command, &args);
    std::fs::write(&path, body.to_string()).map_err(|e| format!("write MCP config: {e}"))?;
    Ok(path)
}

/// Build the argv for one `claude` launch. Pure, so the flag set is unit-tested.
pub fn claude_args(
    session_id: &str,
    mcp_config: &Path,
    system_prompt_file: &Path,
    resume: bool,
) -> Vec<String> {
    let allowed: Vec<String> = PRE_ALLOWED_TOOLS
        .iter()
        .map(|t| format!("mcp__renderly__{t}"))
        .collect();
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--mcp-config".into(),
        mcp_config.to_string_lossy().into_owned(),
        "--strict-mcp-config".into(),
        "--permission-prompt-tool".into(),
        PERMISSION_PROMPT_TOOL.into(),
        "--append-system-prompt-file".into(),
        system_prompt_file.to_string_lossy().into_owned(),
    ];
    if resume {
        args.push("--resume".into());
        args.push(session_id.into());
    } else {
        args.push("--session-id".into());
        args.push(session_id.into());
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
}

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
                Some(AgentEvent::ToolStarted {
                    id,
                    name: b
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    input: b.get("input").cloned().unwrap_or(Value::Null),
                })
            })
            .collect(),
        Some("user") => content_blocks(&v)
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .filter_map(|b| {
                let id = b.get("tool_use_id").and_then(Value::as_str)?.to_string();
                Some(AgentEvent::ToolFinished {
                    id,
                    output_summary: summarize(&tool_result_text(b), 400),
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
        let args = claude_args(
            &self.cfg.session_id,
            &self.mcp_config,
            &self.system_prompt_file,
            resume,
        );
        let mut child = process::spawn(&self.program, &args, &project_dir(&self.cfg.project_path))
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

    #[test]
    fn launch_flags_match_the_documented_contract() {
        let args = claude_args(
            "11111111-2222-3333-4444-555555555555",
            Path::new("/tmp/mcp.json"),
            Path::new("/tmp/sys.txt"),
            false,
        );
        for flag in [
            "-p",
            "--input-format",
            "--output-format",
            "--verbose",
            "--include-partial-messages",
            "--strict-mcp-config",
            "--permission-prompt-tool",
            "--append-system-prompt-file",
            "--session-id",
            "--allowedTools",
        ] {
            assert!(args.iter().any(|a| a == flag), "missing {flag}");
        }
        let pos = args
            .iter()
            .position(|a| a == "--permission-prompt-tool")
            .unwrap();
        assert_eq!(args[pos + 1], PERMISSION_PROMPT_TOOL);
        let allowed = args.last().unwrap();
        assert!(allowed.contains("mcp__renderly__get_project"));
        assert!(
            !allowed.contains("apply_command"),
            "edits must go through the gate"
        );
    }

    #[test]
    fn resume_uses_resume_not_session_id() {
        let args = claude_args("abc", Path::new("m"), Path::new("s"), true);
        assert!(args.iter().any(|a| a == "--resume"));
        assert!(!args.iter().any(|a| a == "--session-id"));
    }

    #[test]
    fn mcp_config_registers_only_renderly_with_project() {
        let v = mcp_config_json(
            "C:/app/renderly-mcp.exe",
            &["--project".into(), "C:/p/a.renderly.json".into()],
        );
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers["renderly"]["args"][1], "C:/p/a.renderly.json");
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
