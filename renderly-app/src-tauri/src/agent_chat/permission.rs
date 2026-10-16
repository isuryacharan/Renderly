//! Permission policy for agent tool calls (docs/agent-chat-design.md §4.3, §5.1).
//!
//! Every permission request from the agent reaches [`PermissionBroker::decide`], whether it
//! arrives through the `permission_prompt` MCP tool (Claude Code) or an approval request from
//! the CLI (Codex). The broker classifies the tool by name, applies the defaults, and only then
//! asks the user through an `permission_request` event. Anything not answered stays pending;
//! cancelling or stopping the session denies it.

use crate::agent_chat::events::{
    permission_options, AgentEnvelope, AgentEvent, EventSink, OPTION_ALLOW_ALWAYS,
    OPTION_ALLOW_ONCE,
};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

/// Prefix Claude Code adds to the names of tools that come from an MCP server.
const RENDERLY_PREFIX: &str = "mcp__renderly__";

/// Read-only Renderly tools: never prompt.
const PERCEPTION: &[&str] = &[
    // The in-app tool set (tools.rs) and the Ollama help tool: all read-only.
    "timeline",
    "look",
    "find_media",
    "assets",
    "edit_help",
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

/// Commands inside `apply_command` / `apply_commands` that write files or download data.
const FILE_COMMANDS: &[&str] = &["Export", "ExportOtio", "ImportOtio", "GenerateVoiceover"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// Read-only Renderly perception: allowed without a prompt.
    Perception,
    /// Undoable project edits through the command API: auto-allowed when the session toggle is on.
    Edit,
    /// Writes a file outside the project or renders to disk: always asks.
    FileWrite,
    /// Downloads a model: always asks.
    ModelDownload,
    /// The agent's own tools (shell, file edit, web, ...): always asks.
    Native,
    /// Anything not classified: asks.
    Unknown,
}

/// Classify a tool by the name the agent reports (`mcp__renderly__apply_command`, `Bash`, ...).
pub fn classify(tool_name: &str, input: &Value) -> ToolClass {
    let Some(name) = tool_name.strip_prefix(RENDERLY_PREFIX) else {
        return ToolClass::Native;
    };
    if PERCEPTION.contains(&name) {
        return ToolClass::Perception;
    }
    match name {
        // `edit` is the in-app tool; its input is {commands: [...]}, the same as apply_commands.
        "edit" => {
            let list = input
                .get("commands")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if list.iter().any(is_file_command) {
                ToolClass::FileWrite
            } else {
                ToolClass::Edit
            }
        }
        "apply_command" => {
            if input.get("command").is_some_and(is_file_command) {
                ToolClass::FileWrite
            } else {
                ToolClass::Edit
            }
        }
        "apply_commands" => {
            let list = input
                .get("commands")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if list.iter().any(is_file_command) {
                ToolClass::FileWrite
            } else {
                ToolClass::Edit
            }
        }
        "export" | "export_otio" | "import_otio" | "new_project" | "open_project" => {
            ToolClass::FileWrite
        }
        "download_model" => ToolClass::ModelDownload,
        _ => ToolClass::Unknown,
    }
}

fn is_file_command(command: &Value) -> bool {
    command
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|c| FILE_COMMANDS.contains(&c))
}

/// Key used to remember "Always for this session" answers. Scoped to the tool and, for
/// `apply_command`, the command name, so approving one kind of edit does not approve others.
fn remember_key(tool_name: &str, input: &Value) -> String {
    let sub = input
        .get("command")
        .and_then(|c| c.get("command"))
        .and_then(Value::as_str)
        .unwrap_or("");
    format!("{tool_name}:{sub}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { message: String },
}

impl Decision {
    /// The JSON Claude Code expects back from a `--permission-prompt-tool`.
    pub fn to_permission_json(&self, input: &Value) -> Value {
        match self {
            Decision::Allow => serde_json::json!({ "behavior": "allow", "updatedInput": input }),
            Decision::Deny { message } => {
                serde_json::json!({ "behavior": "deny", "message": message })
            }
        }
    }
}

/// The user's answer, parsed from an option id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    AllowOnce,
    AllowAlways,
    Reject,
}

impl Answer {
    pub fn from_option_id(id: &str) -> Option<Answer> {
        match id {
            OPTION_ALLOW_ONCE => Some(Answer::AllowOnce),
            OPTION_ALLOW_ALWAYS => Some(Answer::AllowAlways),
            "reject_once" => Some(Answer::Reject),
            _ => None,
        }
    }
}

struct Pending {
    allow_always_offered: bool,
    tx: oneshot::Sender<Answer>,
}

/// Per-session permission state: the auto-edit toggle, "always" answers, and pending prompts.
pub struct PermissionBroker {
    sink: EventSink,
    session_id: String,
    auto_edits: AtomicBool,
    always: Mutex<HashSet<String>>,
    pending: Mutex<HashMap<String, Pending>>,
    next_request: AtomicU64,
}

impl PermissionBroker {
    pub fn new(session_id: String, sink: EventSink, auto_edits: bool) -> Arc<Self> {
        Arc::new(Self {
            sink,
            session_id,
            auto_edits: AtomicBool::new(auto_edits),
            always: Mutex::new(HashSet::new()),
            pending: Mutex::new(HashMap::new()),
            next_request: AtomicU64::new(1),
        })
    }

    pub fn set_auto_edits(&self, enabled: bool) {
        self.auto_edits.store(enabled, Ordering::SeqCst);
    }

    pub fn auto_edits(&self) -> bool {
        self.auto_edits.load(Ordering::SeqCst)
    }

    /// Decide one tool call. Resolves without a prompt for perception tools, for edits when the
    /// auto-edit toggle is on, and for "always" answers already given this session. Otherwise it
    /// emits `permission_request` and waits for [`answer`](Self::answer) or
    /// [`cancel_all`](Self::cancel_all); it never times out into an approval.
    pub async fn decide(&self, tool_name: &str, input: &Value) -> Decision {
        let class = classify(tool_name, input);
        match class {
            ToolClass::Perception => return Decision::Allow,
            ToolClass::Edit if self.auto_edits() => return Decision::Allow,
            _ => {}
        }
        let remember = remember_key(tool_name, input);
        if self
            .always
            .lock()
            .map(|s| s.contains(&remember))
            .unwrap_or(false)
        {
            return Decision::Allow;
        }
        // "Always" is offered only for undoable edits; file writes, downloads and the agent's
        // own tools are asked for every time (§4.3).
        let allow_always_offered = class == ToolClass::Edit;
        let request_id = format!("perm-{}", self.next_request.fetch_add(1, Ordering::SeqCst));
        let (tx, rx) = oneshot::channel();
        match self.pending.lock() {
            Ok(mut pending) => {
                pending.insert(
                    request_id.clone(),
                    Pending {
                        allow_always_offered,
                        tx,
                    },
                );
            }
            Err(_) => {
                return Decision::Deny {
                    message: "permission state unavailable".into(),
                }
            }
        }
        (self.sink)(AgentEnvelope {
            session_id: self.session_id.clone(),
            event: AgentEvent::PermissionRequest {
                request_id: request_id.clone(),
                tool: tool_name.to_string(),
                input: input.clone(),
                options: permission_options(allow_always_offered),
            },
        });
        match rx.await {
            Ok(Answer::AllowOnce) => Decision::Allow,
            Ok(Answer::AllowAlways) => {
                if let Ok(mut always) = self.always.lock() {
                    always.insert(remember);
                }
                Decision::Allow
            }
            Ok(Answer::Reject) => Decision::Deny {
                message: "denied by the user in Renderly".into(),
            },
            Err(_) => Decision::Deny {
                message: "permission request was cancelled".into(),
            },
        }
    }

    /// Resolve a pending request with the user's option. Errors if the id is unknown or the
    /// option is not offered for that request.
    pub fn answer(&self, request_id: &str, option_id: &str) -> Result<(), String> {
        let answer = Answer::from_option_id(option_id)
            .ok_or_else(|| format!("unknown option {option_id}"))?;
        let mut pending = self
            .pending
            .lock()
            .map_err(|e| format!("permission lock poisoned: {e}"))?;
        let entry = pending
            .remove(request_id)
            .ok_or_else(|| format!("no pending permission request {request_id}"))?;
        if answer == Answer::AllowAlways && !entry.allow_always_offered {
            // Not offered for this tool: treat as a one-off allow instead of remembering it.
            let _ = entry.tx.send(Answer::AllowOnce);
            return Ok(());
        }
        // A dropped receiver means the turn already ended; the answer is moot, not an error.
        let _ = entry.tx.send(answer);
        Ok(())
    }

    /// Deny every pending request (cancel or stop).
    pub fn cancel_all(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            for (_, entry) in pending.drain() {
                let _ = entry.tx.send(Answer::Reject);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn perception_tools_are_allowed_without_a_prompt() {
        assert_eq!(
            classify("mcp__renderly__render_frame", &json!({})),
            ToolClass::Perception
        );
    }

    #[test]
    fn apply_command_is_an_edit_unless_it_writes_a_file() {
        let trim = json!({ "command": { "command": "TrimClip" } });
        assert_eq!(
            classify("mcp__renderly__apply_command", &trim),
            ToolClass::Edit
        );
        let export = json!({ "command": { "command": "Export" } });
        assert_eq!(
            classify("mcp__renderly__apply_command", &export),
            ToolClass::FileWrite
        );
        let batch =
            json!({ "commands": [{ "command": "AddClip" }, { "command": "GenerateVoiceover" }] });
        assert_eq!(
            classify("mcp__renderly__apply_commands", &batch),
            ToolClass::FileWrite
        );
    }

    #[test]
    fn export_download_and_native_tools_map_to_their_classes() {
        assert_eq!(
            classify("mcp__renderly__export", &json!({})),
            ToolClass::FileWrite
        );
        assert_eq!(
            classify("mcp__renderly__download_model", &json!({})),
            ToolClass::ModelDownload
        );
        assert_eq!(classify("Bash", &json!({})), ToolClass::Native);
        assert_eq!(classify("Edit", &json!({})), ToolClass::Native);
    }

    #[test]
    fn unknown_renderly_tools_ask() {
        assert_eq!(
            classify("mcp__renderly__brand_new_tool", &json!({})),
            ToolClass::Unknown
        );
    }

    #[test]
    fn deny_json_matches_the_claude_permission_contract() {
        let v = Decision::Deny {
            message: "no".into(),
        }
        .to_permission_json(&json!({}));
        assert_eq!(v, json!({ "behavior": "deny", "message": "no" }));
        let a = Decision::Allow.to_permission_json(&json!({ "x": 1 }));
        assert_eq!(
            a,
            json!({ "behavior": "allow", "updatedInput": { "x": 1 } })
        );
    }

    fn collecting_sink() -> (EventSink, Arc<Mutex<Vec<AgentEnvelope>>>) {
        let store = Arc::new(Mutex::new(Vec::new()));
        let s2 = Arc::clone(&store);
        let sink: EventSink = Arc::new(move |env| {
            if let Ok(mut v) = s2.lock() {
                v.push(env);
            }
        });
        (sink, store)
    }

    fn request_id_of(store: &Arc<Mutex<Vec<AgentEnvelope>>>) -> Option<String> {
        store.lock().ok()?.iter().find_map(|e| match &e.event {
            AgentEvent::PermissionRequest { request_id, .. } => Some(request_id.clone()),
            _ => None,
        })
    }

    #[test]
    fn auto_edit_toggle_allows_edits_without_asking() {
        let (sink, store) = collecting_sink();
        let broker = PermissionBroker::new("s".into(), sink, true);
        let d = tauri::async_runtime::block_on(broker.decide(
            "mcp__renderly__apply_command",
            &json!({ "command": { "command": "TrimClip" } }),
        ));
        assert_eq!(d, Decision::Allow);
        assert!(store.lock().unwrap().is_empty(), "no prompt expected");
    }

    #[test]
    fn export_asks_and_allow_once_is_honoured() {
        let (sink, store) = collecting_sink();
        let broker = PermissionBroker::new("s".into(), sink, true);
        let b2 = Arc::clone(&broker);
        let handle = tauri::async_runtime::spawn(async move {
            b2.decide("mcp__renderly__export", &json!({ "output_path": "o.mp4" }))
                .await
        });
        // Wait for the prompt to be emitted, then answer it.
        let id = loop {
            if let Some(id) = request_id_of(&store) {
                break id;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        broker.answer(&id, "allow_once").unwrap();
        let d = tauri::async_runtime::block_on(handle).unwrap();
        assert_eq!(d, Decision::Allow);
    }

    #[test]
    fn export_offers_no_always_option_and_deny_is_honoured() {
        let (sink, store) = collecting_sink();
        let broker = PermissionBroker::new("s".into(), sink, true);
        let b2 = Arc::clone(&broker);
        let handle = tauri::async_runtime::spawn(async move {
            b2.decide("Bash", &json!({ "command": "rm -rf x" })).await
        });
        let id = loop {
            if let Some(id) = request_id_of(&store) {
                break id;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        let offered: Vec<String> = match &store.lock().unwrap()[0].event {
            AgentEvent::PermissionRequest { options, .. } => {
                options.iter().map(|o| o.id.clone()).collect()
            }
            other => panic!("unexpected {other:?}"),
        };
        assert_eq!(offered, vec!["allow_once", "reject_once"]);
        broker.answer(&id, "reject_once").unwrap();
        let d = tauri::async_runtime::block_on(handle).unwrap();
        assert!(matches!(d, Decision::Deny { .. }));
    }

    #[test]
    fn allow_always_is_remembered_for_the_session_for_edits() {
        let (sink, store) = collecting_sink();
        let broker = PermissionBroker::new("s".into(), sink, false);
        let input = json!({ "command": { "command": "SetClipOpacity" } });
        let b2 = Arc::clone(&broker);
        let i2 = input.clone();
        let handle = tauri::async_runtime::spawn(async move {
            b2.decide("mcp__renderly__apply_command", &i2).await
        });
        let id = loop {
            if let Some(id) = request_id_of(&store) {
                break id;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        broker.answer(&id, "allow_always").unwrap();
        assert_eq!(
            tauri::async_runtime::block_on(handle).unwrap(),
            Decision::Allow
        );
        // The second call of the same kind is allowed without a new prompt.
        let again =
            tauri::async_runtime::block_on(broker.decide("mcp__renderly__apply_command", &input));
        assert_eq!(again, Decision::Allow);
        assert_eq!(
            store
                .lock()
                .unwrap()
                .iter()
                .filter(|e| matches!(e.event, AgentEvent::PermissionRequest { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn cancel_all_denies_pending_requests() {
        let (sink, store) = collecting_sink();
        let broker = PermissionBroker::new("s".into(), sink, false);
        let b2 = Arc::clone(&broker);
        let handle =
            tauri::async_runtime::spawn(async move { b2.decide("Bash", &json!({})).await });
        loop {
            if request_id_of(&store).is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        broker.cancel_all();
        assert!(matches!(
            tauri::async_runtime::block_on(handle).unwrap(),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn answering_an_unknown_request_is_an_error() {
        let (sink, _store) = collecting_sink();
        let broker = PermissionBroker::new("s".into(), sink, false);
        assert!(broker.answer("perm-404", "allow_once").is_err());
    }
}
