//! Codex driver: runs the user's installed, signed-in `codex` CLI as `codex app-server` and
//! speaks its JSON-RPC-style protocol over stdio (newline-delimited JSON, `jsonrpc` omitted).
//!
//! # Status: implemented from the published docs, UNTESTED against the real binary
//!
//! Codex is not installed where this was written, so nothing here has run against `codex`.
//! What is taken from the docs (<https://learn.chatgpt.com/docs/app-server>, the page that
//! `developers.openai.com/codex/app-server` redirects to):
//! - Launch: `codex app-server` (stdio is the default transport, JSONL).
//! - `initialize` with `params.clientInfo`, then the `initialized` notification.
//! - `thread/start` with optional `cwd` and `approvalPolicy`; response `result.thread.id`.
//! - `turn/start` with `threadId` and `input: [{type:"text", text}]`; response `result.turn.id`.
//! - `turn/interrupt` with `threadId` and `turnId`; the turn then completes as `interrupted`.
//! - Notifications: `item/agentMessage/delta`, `item/started`, `item/completed`, `turn/completed`.
//!
//! NOT documented, so assumed and to be verified on a real install:
//! - How MCP servers are configured for one session. The docs say servers are read from config
//!   on disk. This driver passes per-invocation overrides, `codex -c mcp_servers.renderly.command=…
//!   -c mcp_servers.renderly.args=[…] app-server`, so the user's `~/.codex/config.toml` is not
//!   edited. If `codex` does not accept `-c` before the subcommand, the session fails to start
//!   with the CLI's own error.
//! - Approval requests: the docs name `item/permissions/requestApproval` but not command or file
//!   approvals. This driver answers `item/commandExecution/requestApproval` and
//!   `item/fileChange/requestApproval` with `{"decision":"accept"|"decline"}`, and every other
//!   server request with a JSON-RPC error.
//! - Item payload fields (`item.command`, `item.server`, `item.tool`, ...) and the delta field
//!   name (`delta` or `text` are both accepted).

use super::driver::{
    json_line, mcp_server_command, project_dir, AgentDriver, AgentId, AgentSession, SessionConfig,
};
use super::events::{summarize, AgentEnvelope, AgentEvent, EventSink};
use super::process::{self, StderrTail};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const READY_TIMEOUT: Duration = Duration::from_secs(30);
const INTERRUPT_WAIT: Duration = Duration::from_secs(2);

pub struct CodexDriver;

impl AgentDriver for CodexDriver {
    fn agent(&self) -> AgentId {
        AgentId::Codex
    }

    fn start(&self, cfg: &SessionConfig) -> Result<Box<dyn AgentSession>, String> {
        let program = match &cfg.program {
            Some(p) => p.clone(),
            None => process::resolve_program(AgentId::Codex.program_name())
                .ok_or("Codex (codex) is not installed or not on PATH")?,
        };
        let mut session = CodexSession {
            cfg: cfg.clone(),
            program,
            child: None,
            stdin: Arc::new(Mutex::new(None)),
            thread_id: Arc::new(Mutex::new(None)),
            turn_id: Arc::new(Mutex::new(None)),
            next_id: AtomicU64::new(10),
            turn_active: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(false)),
        };
        session.spawn()?;
        Ok(Box::new(session))
    }
}

/// Argv for `codex app-server`, with the Renderly MCP server passed as config overrides.
pub fn codex_args(mcp_command: &str, mcp_args: &[String]) -> Vec<String> {
    let toml_args = format!(
        "[{}]",
        mcp_args
            .iter()
            .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", ")
    );
    vec![
        "-c".into(),
        format!(
            "mcp_servers.renderly.command=\"{}\"",
            mcp_command.replace('\\', "/")
        ),
        "-c".into(),
        format!("mcp_servers.renderly.args={toml_args}"),
        "app-server".into(),
    ]
}

/// What one line from the server means to the driver.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    /// Answer to one of our requests.
    Response {
        id: u64,
        result: Value,
        error: Option<Value>,
    },
    /// The server asks us something (approvals). `id` is echoed back.
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Unknown,
}

pub fn classify(line: &str) -> Incoming {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return Incoming::Unknown;
    };
    let method = v.get("method").and_then(Value::as_str).map(str::to_string);
    let params = v.get("params").cloned().unwrap_or(Value::Null);
    match (method, v.get("id")) {
        (Some(method), Some(id)) => Incoming::Request {
            id: id.clone(),
            method,
            params,
        },
        (Some(method), None) => Incoming::Notification { method, params },
        (None, Some(id)) => match id.as_u64() {
            Some(id) => Incoming::Response {
                id,
                result: v.get("result").cloned().unwrap_or(Value::Null),
                error: v.get("error").cloned(),
            },
            None => Incoming::Unknown,
        },
        (None, None) => Incoming::Unknown,
    }
}

/// Map a server notification to the normalised events. Unknown methods produce nothing.
pub fn notification_events(method: &str, params: &Value) -> Vec<AgentEvent> {
    match method {
        "item/agentMessage/delta" => {
            let text = params
                .get("delta")
                .or_else(|| params.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("");
            if text.is_empty() {
                Vec::new()
            } else {
                vec![AgentEvent::TextDelta {
                    text: text.to_string(),
                }]
            }
        }
        "item/started" => {
            let Some(item) = params.get("item") else {
                return Vec::new();
            };
            match tool_card(item) {
                Some((id, name, input)) => vec![AgentEvent::ToolStarted { id, name, input }],
                None => Vec::new(),
            }
        }
        "item/completed" => {
            let Some(item) = params.get("item") else {
                return Vec::new();
            };
            match tool_card(item) {
                Some((id, _, _)) => {
                    let status = item.get("status").and_then(Value::as_str).unwrap_or("");
                    let output = item
                        .get("aggregatedOutput")
                        .or_else(|| item.get("result"))
                        .and_then(Value::as_str)
                        .unwrap_or(status);
                    vec![AgentEvent::ToolFinished {
                        id,
                        output_summary: summarize(output, 400),
                        is_error: status == "failed" || status == "declined",
                    }]
                }
                None => Vec::new(),
            }
        }
        "turn/completed" => {
            let turn = params.get("turn");
            let status = turn
                .and_then(|t| t.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed")
                .to_string();
            let mut events = Vec::new();
            if status == "failed" {
                let message = turn
                    .and_then(|t| t.get("error"))
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Codex turn failed")
                    .to_string();
                events.push(AgentEvent::Error { message });
            }
            events.push(AgentEvent::TurnFinished {
                stop_reason: status,
            });
            events
        }
        "error" => {
            let message = params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Codex reported an error")
                .to_string();
            vec![AgentEvent::Error { message }]
        }
        _ => Vec::new(),
    }
}

/// Tool-call card for the item kinds Renderly shows. Messages, reasoning and user items are not
/// tool calls and return `None`.
fn tool_card(item: &Value) -> Option<(String, String, Value)> {
    let id = item.get("id").and_then(Value::as_str)?.to_string();
    match item.get("type").and_then(Value::as_str)? {
        "commandExecution" => Some((
            id,
            "shell".into(),
            json!({ "command": item.get("command").cloned().unwrap_or(Value::Null) }),
        )),
        "mcpToolCall" => {
            let server = item.get("server").and_then(Value::as_str).unwrap_or("mcp");
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            Some((
                id,
                format!("{server}.{tool}"),
                item.get("arguments").cloned().unwrap_or(Value::Null),
            ))
        }
        "fileChange" => Some((
            id,
            "file_change".into(),
            json!({ "changes": item.get("changes").cloned().unwrap_or(Value::Null) }),
        )),
        _ => None,
    }
}

fn write_json(stdin: &Mutex<Option<ChildStdin>>, value: &Value) -> Result<(), String> {
    use std::io::Write;
    let mut guard = stdin.lock().map_err(|e| format!("stdin lock: {e}"))?;
    let handle = guard.as_mut().ok_or("codex is not running")?;
    handle
        .write_all(json_line(value).as_bytes())
        .and_then(|_| handle.flush())
        .map_err(|e| format!("could not write to codex: {e}"))
}

struct CodexSession {
    cfg: SessionConfig,
    program: PathBuf,
    child: Option<Child>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    thread_id: Arc<Mutex<Option<String>>>,
    turn_id: Arc<Mutex<Option<String>>>,
    next_id: AtomicU64,
    turn_active: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}

impl CodexSession {
    fn spawn(&mut self) -> Result<(), String> {
        let (mcp_command, mcp_args) = mcp_server_command(&self.cfg);
        let args = codex_args(&mcp_command, &mcp_args);
        let mut child = process::spawn(&self.program, &args, &project_dir(&self.cfg.project_path))
            .map_err(|e| format!("could not start codex: {e}"))?;
        let stdin = child.stdin.take().ok_or("codex stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("codex stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("codex stderr unavailable")?;
        let tail: StderrTail = process::drain_stderr(stderr);
        *self.stdin.lock().map_err(|e| format!("stdin lock: {e}"))? = Some(stdin);
        self.alive = Arc::new(AtomicBool::new(true));

        let (ready_tx, ready_rx) = channel::<Result<(), String>>();
        let thread_start_id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let reader = Reader {
            sink: self.cfg.sink.clone(),
            session_id: self.cfg.session_id.clone(),
            stdin: Arc::clone(&self.stdin),
            thread_id: Arc::clone(&self.thread_id),
            turn_id: Arc::clone(&self.turn_id),
            turn_active: Arc::clone(&self.turn_active),
            alive: Arc::clone(&self.alive),
            broker: Arc::clone(&self.cfg.broker),
            tail: tail.clone(),
            ready: Some(ready_tx),
            pending_thread_start: Some(thread_start_id),
        };
        std::thread::spawn(move || reader.run(stdout));

        write_json(
            &self.stdin,
            &json!({ "method": "initialize", "id": 0, "params": {
                "clientInfo": { "name": "renderly", "title": "Renderly", "version": env!("CARGO_PKG_VERSION") }
            }}),
        )?;
        write_json(
            &self.stdin,
            &json!({ "method": "initialized", "params": {} }),
        )?;
        write_json(
            &self.stdin,
            &json!({ "method": "thread/start", "id": thread_start_id, "params": {
                "cwd": project_dir(&self.cfg.project_path).to_string_lossy().into_owned(),
                "approvalPolicy": "on-request",
            }}),
        )?;

        let result = ready_rx
            .recv_timeout(READY_TIMEOUT)
            .unwrap_or_else(|_| Err("codex did not finish starting in time".into()));
        if let Err(message) = result {
            self.alive.store(false, Ordering::SeqCst);
            process::kill_tree(&mut child);
            return Err(message);
        }
        self.child = Some(child);
        (self.cfg.sink)(AgentEnvelope {
            session_id: self.cfg.session_id.clone(),
            event: AgentEvent::SessionStarted {
                agent_id: AgentId::Codex.program_name().into(),
            },
        });
        Ok(())
    }

    fn kill_process(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Ok(mut guard) = self.stdin.lock() {
            *guard = None;
        }
        if let Some(mut child) = self.child.take() {
            process::kill_tree(&mut child);
        }
    }
}

impl AgentSession for CodexSession {
    fn send_turn(&mut self, text: &str) -> Result<(), String> {
        let thread = self
            .thread_id
            .lock()
            .map_err(|e| format!("thread lock: {e}"))?
            .clone()
            .ok_or("codex has no thread yet")?;
        if let Ok(mut t) = self.turn_id.lock() {
            *t = None;
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        self.turn_active.store(true, Ordering::SeqCst);
        let sent = write_json(
            &self.stdin,
            &json!({ "method": "turn/start", "id": id, "params": {
                "threadId": thread,
                "input": [{ "type": "text", "text": text }],
            }}),
        );
        if sent.is_err() {
            self.turn_active.store(false, Ordering::SeqCst);
        }
        sent
    }

    fn cancel_turn(&mut self) -> Result<(), String> {
        if !self.turn_active.load(Ordering::SeqCst) {
            return Ok(());
        }
        self.cfg.broker.cancel_all();
        let deadline = Instant::now() + INTERRUPT_WAIT;
        let turn = loop {
            if let Ok(t) = self.turn_id.lock() {
                if let Some(id) = t.clone() {
                    break Some(id);
                }
            }
            if Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let thread = self.thread_id.lock().ok().and_then(|t| t.clone());
        match (thread, turn) {
            (Some(thread), Some(turn)) => {
                let id = self.next_id.fetch_add(1, Ordering::SeqCst);
                write_json(
                    &self.stdin,
                    &json!({ "method": "turn/interrupt", "id": id, "params": {
                        "threadId": thread, "turnId": turn,
                    }}),
                )
            }
            _ => {
                // No turn id to interrupt: stop the process. The next turn needs a new thread.
                self.kill_process();
                self.turn_active.store(false, Ordering::SeqCst);
                (self.cfg.sink)(AgentEnvelope {
                    session_id: self.cfg.session_id.clone(),
                    event: AgentEvent::TurnFinished {
                        stop_reason: "cancelled".into(),
                    },
                });
                Ok(())
            }
        }
    }

    fn shutdown(&mut self) {
        self.cfg.broker.cancel_all();
        self.kill_process();
        self.turn_active.store(false, Ordering::SeqCst);
    }
}

impl Drop for CodexSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Reads server output on its own thread: answers the handshake, routes notifications to
/// events, and turns approval requests into broker decisions.
struct Reader {
    sink: EventSink,
    session_id: String,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    thread_id: Arc<Mutex<Option<String>>>,
    turn_id: Arc<Mutex<Option<String>>>,
    turn_active: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    broker: Arc<super::permission::PermissionBroker>,
    tail: StderrTail,
    ready: Option<Sender<Result<(), String>>>,
    pending_thread_start: Option<u64>,
}

impl Reader {
    fn emit(&self, event: AgentEvent) {
        (self.sink)(AgentEnvelope {
            session_id: self.session_id.clone(),
            event,
        });
    }

    fn run(mut self, stdout: impl std::io::Read) {
        process::read_lines(stdout, |line| self.handle(line));
        // EOF: report only if this process was not deliberately stopped.
        if self.alive.swap(false, Ordering::SeqCst) {
            if let Some(ready) = self.ready.take() {
                let _ = ready.send(Err(format!(
                    "codex exited while starting:\n{}",
                    self.tail.last_lines()
                )));
            } else {
                if self.turn_active.swap(false, Ordering::SeqCst) {
                    self.emit(AgentEvent::TurnFinished {
                        stop_reason: "error".into(),
                    });
                }
                self.broker.cancel_all();
                self.emit(AgentEvent::Error {
                    message: format!("Codex exited unexpectedly:\n{}", self.tail.last_lines()),
                });
            }
        }
    }

    fn handle(&mut self, line: &str) {
        match classify(line) {
            Incoming::Response { id, result, error } => {
                if let Some(err) = error {
                    let message = err
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("request failed")
                        .to_string();
                    if self.pending_thread_start == Some(id) {
                        if let Some(ready) = self.ready.take() {
                            let _ = ready.send(Err(message));
                        }
                    } else {
                        self.emit(AgentEvent::Error { message });
                    }
                    return;
                }
                if self.pending_thread_start == Some(id) {
                    match result
                        .get("thread")
                        .and_then(|t| t.get("id"))
                        .and_then(Value::as_str)
                    {
                        Some(thread) => {
                            if let Ok(mut t) = self.thread_id.lock() {
                                *t = Some(thread.to_string());
                            }
                            if let Some(ready) = self.ready.take() {
                                let _ = ready.send(Ok(()));
                            }
                        }
                        None => {
                            if let Some(ready) = self.ready.take() {
                                let _ = ready.send(Err("codex returned no thread".into()));
                            }
                        }
                    }
                    return;
                }
                if let Some(turn) = result
                    .get("turn")
                    .and_then(|t| t.get("id"))
                    .and_then(Value::as_str)
                {
                    if let Ok(mut t) = self.turn_id.lock() {
                        *t = Some(turn.to_string());
                    }
                }
            }
            Incoming::Request { id, method, params } => match method.as_str() {
                "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
                    self.spawn_approval(id, method, params);
                }
                other => {
                    let reply = json!({ "id": id, "error": {
                        "code": -32601,
                        "message": format!("Renderly does not handle {other}"),
                    }});
                    let _ = write_json(&self.stdin, &reply);
                }
            },
            Incoming::Notification { method, params } => {
                if method == "turn/completed" {
                    self.turn_active.store(false, Ordering::SeqCst);
                }
                for event in notification_events(&method, &params) {
                    self.emit(event);
                }
            }
            Incoming::Unknown => {}
        }
    }

    /// Ask the broker on a helper thread (the reader keeps reading), then answer the server.
    fn spawn_approval(&self, id: Value, method: String, params: Value) {
        let broker = Arc::clone(&self.broker);
        let stdin = Arc::clone(&self.stdin);
        std::thread::spawn(move || {
            let tool = format!("codex:{method}");
            let decision = tauri::async_runtime::block_on(broker.decide(&tool, &params));
            let decision_value = match decision {
                super::permission::Decision::Allow => "accept",
                super::permission::Decision::Deny { .. } => "decline",
            };
            let _ = write_json(
                &stdin,
                &json!({ "id": id, "result": { "decision": decision_value } }),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_server_args_pass_the_mcp_server_as_overrides() {
        let args = codex_args(
            "C:/app/renderly-mcp.exe",
            &["--project".into(), "C:/p/a.renderly.json".into()],
        );
        assert_eq!(args.last().unwrap(), "app-server");
        assert!(args
            .iter()
            .any(|a| a == "mcp_servers.renderly.command=\"C:/app/renderly-mcp.exe\""));
        assert!(args
            .iter()
            .any(|a| a == "mcp_servers.renderly.args=[\"--project\", \"C:/p/a.renderly.json\"]"));
    }

    #[test]
    fn classifies_responses_requests_and_notifications() {
        assert!(matches!(
            classify(r#"{"id":1,"result":{"thread":{"id":"thr_1"}}}"#),
            Incoming::Response { id: 1, .. }
        ));
        assert!(matches!(
            classify(r#"{"id":900,"method":"item/commandExecution/requestApproval","params":{}}"#),
            Incoming::Request { .. }
        ));
        assert!(matches!(
            classify(r#"{"method":"turn/completed","params":{}}"#),
            Incoming::Notification { .. }
        ));
        assert_eq!(classify("garbage"), Incoming::Unknown);
    }

    #[test]
    fn agent_message_deltas_become_text() {
        let evs = notification_events(
            "item/agentMessage/delta",
            &json!({ "delta": "Hi", "itemId": "i1" }),
        );
        assert_eq!(evs, vec![AgentEvent::TextDelta { text: "Hi".into() }]);
    }

    #[test]
    fn command_items_become_tool_cards() {
        let started = notification_events(
            "item/started",
            &json!({ "item": { "id": "c1", "type": "commandExecution", "command": "ls" } }),
        );
        assert!(
            matches!(&started[0], AgentEvent::ToolStarted { id, name, .. } if id == "c1" && name == "shell")
        );
        let done = notification_events(
            "item/completed",
            &json!({ "item": { "id": "c1", "type": "commandExecution", "status": "failed", "aggregatedOutput": "boom" } }),
        );
        assert_eq!(
            done,
            vec![AgentEvent::ToolFinished {
                id: "c1".into(),
                output_summary: "boom".into(),
                is_error: true
            }]
        );
    }

    #[test]
    fn messages_and_reasoning_items_are_not_tool_cards() {
        assert!(notification_events(
            "item/started",
            &json!({ "item": { "id": "m1", "type": "agentMessage" } })
        )
        .is_empty());
    }

    #[test]
    fn turn_completed_reports_its_status() {
        assert_eq!(
            notification_events(
                "turn/completed",
                &json!({ "turn": { "status": "interrupted" } })
            ),
            vec![AgentEvent::TurnFinished {
                stop_reason: "interrupted".into()
            }]
        );
    }

    #[test]
    fn unknown_notifications_are_ignored() {
        assert!(notification_events("thread/started", &json!({})).is_empty());
    }

    #[test]
    fn mcp_tool_items_are_named_server_dot_tool() {
        let started = notification_events(
            "item/started",
            &json!({ "item": { "id": "t1", "type": "mcpToolCall", "server": "renderly", "tool": "get_project", "arguments": {} } }),
        );
        assert!(
            matches!(&started[0], AgentEvent::ToolStarted { name, .. } if name == "renderly.get_project")
        );
    }
}
