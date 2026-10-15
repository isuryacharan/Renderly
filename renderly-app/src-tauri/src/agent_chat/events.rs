//! The one event enum every agent driver normalises into. The backend sends each event to the
//! webview as the Tauri event `agent:event` with payload [`AgentEnvelope`]:
//!
//! ```json
//! { "session_id": "…", "type": "text_delta", "text": "Hello" }
//! ```
//!
//! Variants (all `type` values are snake_case):
//! - `session_started { agent_id }`: the CLI process is up and ready for a turn.
//! - `text_delta { text }`: streamed assistant text, in order.
//! - `tool_started { id, name, input }`: a tool call began (`input` is the raw JSON input).
//! - `tool_finished { id, output_summary, is_error }`: the matching tool call ended.
//! - `permission_request { request_id, tool, input, options }`: the user must decide; answer
//!   with `agent_answer_permission(session_id, request_id, option_id)`. `options[].id` is one of
//!   `allow_once`, `allow_always` (only for edit tools) or `reject_once`.
//! - `turn_finished { stop_reason }`: the turn ended. `stop_reason` is the CLI's value
//!   (`end_turn`, `cancelled`, `interrupted`, `error`, ...).
//! - `error { message }`: something went wrong; the session may still be usable.
//! - `session_ended { reason }`: the session is gone (stopped, project closed, CLI exited).

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    SessionStarted {
        agent_id: String,
    },
    TextDelta {
        text: String,
    },
    ToolStarted {
        id: String,
        name: String,
        input: Value,
    },
    ToolFinished {
        id: String,
        output_summary: String,
        is_error: bool,
    },
    PermissionRequest {
        request_id: String,
        tool: String,
        input: Value,
        options: Vec<PermissionOption>,
    },
    TurnFinished {
        stop_reason: String,
    },
    Error {
        message: String,
    },
    SessionEnded {
        reason: String,
    },
}

/// One answer the user may give to a permission request.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PermissionOption {
    pub id: String,
    pub label: String,
}

/// Option ids the frontend sends back through `agent_answer_permission`.
pub const OPTION_ALLOW_ONCE: &str = "allow_once";
pub const OPTION_ALLOW_ALWAYS: &str = "allow_always";
pub const OPTION_REJECT_ONCE: &str = "reject_once";

pub fn permission_options(allow_always: bool) -> Vec<PermissionOption> {
    let mut options = vec![PermissionOption {
        id: OPTION_ALLOW_ONCE.into(),
        label: "Allow once".into(),
    }];
    if allow_always {
        options.push(PermissionOption {
            id: OPTION_ALLOW_ALWAYS.into(),
            label: "Always for this session".into(),
        });
    }
    options.push(PermissionOption {
        id: OPTION_REJECT_ONCE.into(),
        label: "Deny".into(),
    });
    options
}

/// The payload of one `agent:event` Tauri event.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentEnvelope {
    pub session_id: String,
    #[serde(flatten)]
    pub event: AgentEvent,
}

/// Where drivers publish events. The app wraps `AppHandle::emit`; tests collect into a Vec.
pub type EventSink = std::sync::Arc<dyn Fn(AgentEnvelope) + Send + Sync>;

/// Shorten tool output for a card (never more than `max` characters, on a char boundary).
pub fn summarize(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let cut: String = trimmed.chars().take(max).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn envelope_flattens_the_event_next_to_the_session_id() {
        let env = AgentEnvelope {
            session_id: "s1".into(),
            event: AgentEvent::TextDelta { text: "Hi".into() },
        };
        assert_eq!(
            serde_json::to_value(&env).unwrap(),
            json!({ "session_id": "s1", "type": "text_delta", "text": "Hi" })
        );
    }

    #[test]
    fn permission_request_carries_its_options() {
        let ev = AgentEvent::PermissionRequest {
            request_id: "p1".into(),
            tool: "Bash".into(),
            input: json!({ "command": "ls" }),
            options: permission_options(false),
        };
        let v = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "permission_request");
        let ids: Vec<&str> = v["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["allow_once", "reject_once"]);
    }

    #[test]
    fn summarize_truncates_on_char_boundaries() {
        assert_eq!(summarize("  abc  ", 10), "abc");
        assert_eq!(summarize("ééééé", 3), "ééé…");
    }
}
