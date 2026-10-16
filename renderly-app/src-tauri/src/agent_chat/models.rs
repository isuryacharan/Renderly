//! Model lists for the agent picker, read from each agent at runtime. Renderly keeps no model
//! list of its own. Each agent answers a usage-free request:
//!
//! - Claude Code: a stream-json `control_request` with `subtype: "initialize"`. The
//!   `control_response` carries `response.models[]` (see `claude.rs`, `fetch_claude_models`).
//!   No prompt is sent to a model.
//! - Codex: JSON-RPC `model/list` after `initialize` (see `codex.rs`, `fetch_codex_models`).
//!   The result is `{data: [Model], nextCursor}`. No turn is started.
//! - Ollama: `GET /api/tags`, then `POST /api/show` per model for `capabilities` and
//!   `model_info` (see `ollama.rs`). Only models that report `tools` are offered.
//!
//! This module holds the shared types and the pure parsers, so the shapes are unit-tested
//! against captured responses without launching anything.

use super::driver::AgentId;
use serde::Serialize;
use serde_json::Value;

/// One model the user can pick for an agent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelOption {
    /// The value passed to the agent (`--model`, `thread/start.model`, Ollama `model`).
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// Effort levels the picker offers for this model. Empty means effort is not offered.
    pub efforts: Vec<String>,
    /// The agent's own default effort for this model, when it reports one.
    pub default_effort: Option<String>,
    /// Ollama only: the model's context window in tokens, from `model_info`.
    pub context_length: Option<u64>,
    /// Ollama only: the model reports `vision`, so the `look` tool is offered.
    pub vision: bool,
    /// Ollama only: the model reports `thinking`, so effort maps to `think`.
    pub thinking: bool,
}

/// Why a model list is or is not available. Drives the message the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelStatus {
    /// The list was read from the agent.
    Ready,
    /// The agent's program is not installed.
    NotInstalled,
    /// The agent is installed but reports no signed-in account.
    NotSignedIn,
    /// The agent's local service does not answer (Ollama).
    NotRunning,
    /// The agent answered, but offers no model Renderly can use (Ollama: none with tools).
    NoModels,
    /// Any other failure; `message` says what happened.
    Error,
}

/// The fetched list for one agent, cached for the app session.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModels {
    pub agent_id: AgentId,
    pub status: ModelStatus,
    pub models: Vec<ModelOption>,
    /// The agent's default model, from the fetched data (not hard-coded).
    pub default_model: Option<String>,
    /// A line for the panel when `status` is not `Ready`.
    pub message: Option<String>,
}

impl AgentModels {
    pub fn failed(agent_id: AgentId, status: ModelStatus, message: impl Into<String>) -> Self {
        Self {
            agent_id,
            status,
            models: Vec::new(),
            default_model: None,
            message: Some(message.into()),
        }
    }

    pub fn ready(
        agent_id: AgentId,
        models: Vec<ModelOption>,
        default_model: Option<String>,
    ) -> Self {
        let status = if models.is_empty() {
            ModelStatus::NoModels
        } else {
            ModelStatus::Ready
        };
        let message =
            (status == ModelStatus::NoModels).then(|| "The agent reported no models.".to_string());
        Self {
            agent_id,
            status,
            models,
            default_model,
            message,
        }
    }

    /// The model to use when the user has no valid choice: the agent's default if it reported
    /// one, else the first model.
    pub fn fallback_model(&self) -> Option<&str> {
        self.default_model
            .as_deref()
            .filter(|d| self.models.iter().any(|m| m.id == *d))
            .or_else(|| self.models.first().map(|m| m.id.as_str()))
    }

    /// The option for `id`, if the list contains it.
    pub fn find(&self, id: &str) -> Option<&ModelOption> {
        self.models.iter().find(|m| m.id == id)
    }
}

/// Parse the `control_response` of Claude Code's `initialize` control request.
///
/// Shape used (captured from `claude` 2.1.301, usage-free): `response.response.models[]` with
/// `value`, `resolvedModel`, `displayName`, `description`, `supportsEffort`,
/// `supportedEffortLevels`. `value` is the alias passed to `--model`. The entry whose value is
/// `default` is the default. `response.response.account` is absent when no account is signed in.
pub fn parse_claude_models(control_response: &Value) -> Result<AgentModels, String> {
    let inner = control_response
        .get("response")
        .and_then(|r| r.get("response"))
        .ok_or("Claude Code's initialize response had no payload")?;
    if inner.get("account").is_none() {
        return Ok(AgentModels::failed(
            AgentId::ClaudeCode,
            ModelStatus::NotSignedIn,
            "Claude Code is not signed in. Run `claude auth login` in a terminal, then refresh.",
        ));
    }
    let list = inner
        .get("models")
        .and_then(Value::as_array)
        .ok_or("Claude Code's initialize response had no models")?;
    let mut models = Vec::new();
    let mut default_model = None;
    for m in list {
        let Some(value) = m.get("value").and_then(Value::as_str) else {
            continue;
        };
        // `default` is an alias that resolves to a real model; it is the picker's first entry.
        if value == "default" {
            default_model = Some(value.to_string());
        }
        let efforts = if m.get("supportsEffort").and_then(Value::as_bool) == Some(true) {
            string_list(m.get("supportedEffortLevels"))
        } else {
            Vec::new()
        };
        models.push(ModelOption {
            id: value.to_string(),
            label: m
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(value)
                .to_string(),
            description: m
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            efforts,
            default_effort: None,
            context_length: None,
            vision: false,
            thinking: false,
        });
    }
    Ok(AgentModels::ready(
        AgentId::ClaudeCode,
        models,
        default_model,
    ))
}

/// Parse a Codex `model/list` result (`{data: [Model], nextCursor}`).
///
/// Shape used (captured from `codex-cli` 0.163.0): `data[].model` is the slug for
/// `thread/start.model`; `displayName`, `description`, `hidden`, `isDefault`,
/// `defaultReasoningEffort`, and `supportedReasoningEfforts[].reasoningEffort`.
pub fn parse_codex_models(result: &Value) -> Result<AgentModels, String> {
    let data = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Codex model/list returned no data")?;
    let mut models = Vec::new();
    let mut default_model = None;
    for m in data {
        if m.get("hidden").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let Some(slug) = m.get("model").and_then(Value::as_str) else {
            continue;
        };
        if m.get("isDefault").and_then(Value::as_bool) == Some(true) {
            default_model = Some(slug.to_string());
        }
        let efforts: Vec<String> = m
            .get("supportedReasoningEfforts")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|e| e.get("reasoningEffort").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        models.push(ModelOption {
            id: slug.to_string(),
            label: m
                .get("displayName")
                .and_then(Value::as_str)
                .unwrap_or(slug)
                .to_string(),
            description: m
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_string),
            efforts,
            default_effort: m
                .get("defaultReasoningEffort")
                .and_then(Value::as_str)
                .map(str::to_string),
            context_length: None,
            vision: false,
            thinking: false,
        });
    }
    Ok(AgentModels::ready(AgentId::Codex, models, default_model))
}

/// Facts read from one Ollama model's `/api/show` response.
#[derive(Debug, Clone, PartialEq)]
pub struct OllamaCaps {
    pub name: String,
    pub tools: bool,
    pub vision: bool,
    pub thinking: bool,
    /// `model_info["<architecture>.context_length"]`.
    pub context_length: Option<u64>,
}

/// Read `capabilities` and the context length from an Ollama `/api/show` response.
pub fn parse_ollama_show(name: &str, show: &Value) -> OllamaCaps {
    let caps = string_list(show.get("capabilities"));
    let has = |c: &str| caps.iter().any(|x| x == c);
    let context_length = show
        .get("model_info")
        .and_then(Value::as_object)
        .and_then(|info| {
            info.iter()
                .find(|(k, _)| k.ends_with(".context_length"))
                .and_then(|(_, v)| v.as_u64())
        });
    OllamaCaps {
        name: name.to_string(),
        tools: has("tools"),
        vision: has("vision"),
        thinking: has("thinking"),
        context_length,
    }
}

/// Build the Ollama list from `/api/tags` names and their `/api/show` facts. Only tool-capable
/// models are offered. An empty result is `NoModels` with the pull hint.
pub fn ollama_models(caps: &[OllamaCaps]) -> AgentModels {
    let models: Vec<ModelOption> = caps
        .iter()
        .filter(|c| c.tools)
        .map(|c| ModelOption {
            id: c.name.clone(),
            label: c.name.clone(),
            description: None,
            // Effort maps to Ollama's `think` parameter, so it is offered only for thinking models.
            efforts: if c.thinking {
                vec!["low".into(), "medium".into(), "high".into()]
            } else {
                Vec::new()
            },
            default_effort: None,
            context_length: c.context_length,
            vision: c.vision,
            thinking: c.thinking,
        })
        .collect();
    if models.is_empty() {
        return AgentModels::failed(
            AgentId::Ollama,
            ModelStatus::NoModels,
            "No installed Ollama model supports tools. Pull one in a terminal, for example \
             `ollama pull qwen3`, then refresh.",
        );
    }
    let first = models.first().map(|m| m.id.clone());
    AgentModels::ready(AgentId::Ollama, models, first)
}

/// Read stdout lines on a helper thread until one parses as JSON and satisfies `matches`, or
/// `timeout` passes. The reader thread ends when the pipe closes, which happens when the caller
/// kills the child.
pub fn await_reply(
    stdout: impl std::io::Read + Send + 'static,
    timeout: std::time::Duration,
    matches: impl Fn(&Value) -> bool + Send + 'static,
) -> Option<Value> {
    let (tx, rx) = std::sync::mpsc::channel::<Value>();
    std::thread::spawn(move || {
        super::process::read_lines(stdout, |line| {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                let _ = tx.send(v);
            }
        });
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.checked_duration_since(std::time::Instant::now())?;
        match rx.recv_timeout(left) {
            Ok(v) if matches(&v) => return Some(v),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Trimmed from a real `claude` 2.1.301 `initialize` control_response.
    fn claude_fixture(signed_in: bool) -> Value {
        let mut inner = json!({
            "models": [
                { "value": "default", "resolvedModel": "claude-opus-5-5", "displayName": "Default (recommended)",
                  "description": "Opus 5.5", "supportsEffort": true,
                  "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"] },
                { "value": "sonnet", "resolvedModel": "claude-sonnet-5-5", "displayName": "Sonnet 5.5",
                  "description": "Efficient for routine tasks", "supportsEffort": true,
                  "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"] },
                { "value": "claude-haiku-4-5-20251001", "resolvedModel": "claude-haiku-4-5-20251001",
                  "displayName": "Haiku 4.5", "description": "Fastest for quick answers" }
            ],
            "commands": [], "agents": []
        });
        if signed_in {
            inner["account"] =
                json!({ "email": "x@example.com", "subscriptionType": "Claude Pro" });
        }
        json!({ "type": "control_response", "response": { "subtype": "success", "request_id": "r", "response": inner } })
    }

    #[test]
    fn claude_models_come_from_the_control_response() {
        let list = parse_claude_models(&claude_fixture(true)).unwrap();
        assert_eq!(list.status, ModelStatus::Ready);
        let ids: Vec<&str> = list.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["default", "sonnet", "claude-haiku-4-5-20251001"]);
        assert_eq!(list.default_model.as_deref(), Some("default"));
        assert_eq!(list.models[0].label, "Default (recommended)");
        assert_eq!(
            list.models[0].efforts,
            vec!["low", "medium", "high", "xhigh", "max"]
        );
        // A model without supportsEffort offers no effort levels.
        assert!(list.models[2].efforts.is_empty());
    }

    #[test]
    fn claude_without_account_is_not_signed_in() {
        let list = parse_claude_models(&claude_fixture(false)).unwrap();
        assert_eq!(list.status, ModelStatus::NotSignedIn);
        assert!(list.models.is_empty());
        assert!(list.message.unwrap().contains("claude auth login"));
    }

    /// Trimmed from a real `codex-cli` 0.163.0 `model/list` result.
    fn codex_fixture() -> Value {
        json!({ "data": [
            { "id": "gpt-6-luna", "model": "gpt-6-luna", "displayName": "GPT-6-Luna",
              "description": "Fast and affordable model for easier tasks.", "hidden": false, "isDefault": false,
              "defaultReasoningEffort": "medium",
              "supportedReasoningEfforts": [
                  { "reasoningEffort": "low", "description": "Fast responses" },
                  { "reasoningEffort": "medium", "description": "Balanced" } ] },
            { "id": "gpt-6-terra", "model": "gpt-6-terra", "displayName": "GPT-6-Terra",
              "description": "Default model", "hidden": false, "isDefault": true,
              "defaultReasoningEffort": "medium",
              "supportedReasoningEfforts": [ { "reasoningEffort": "high", "description": "Deep" } ] },
            { "id": "old", "model": "old", "displayName": "Old", "description": "", "hidden": true,
              "isDefault": false, "supportedReasoningEfforts": [] }
        ], "nextCursor": null })
    }

    #[test]
    fn codex_models_carry_their_own_effort_options() {
        let list = parse_codex_models(&codex_fixture()).unwrap();
        assert_eq!(list.status, ModelStatus::Ready);
        let ids: Vec<&str> = list.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["gpt-6-luna", "gpt-6-terra"],
            "hidden models are dropped"
        );
        assert_eq!(list.default_model.as_deref(), Some("gpt-6-terra"));
        assert_eq!(list.models[0].efforts, vec!["low", "medium"]);
        assert_eq!(list.models[0].default_effort.as_deref(), Some("medium"));
    }

    #[test]
    fn ollama_models_keep_only_tool_capable_models() {
        let shows = [
            json!({ "capabilities": ["completion", "tools", "thinking"],
                    "model_info": { "general.architecture": "qwen3", "qwen3.context_length": 262144 } }),
            json!({ "capabilities": ["completion"],
                    "model_info": { "gemma3.context_length": 131072 } }),
            json!({ "capabilities": ["completion", "tools", "vision"],
                    "model_info": { "gemma4.context_length": 262144 } }),
        ];
        let names = ["qwen3:4b", "gemma3:4b", "gemma4:12b"];
        let caps: Vec<OllamaCaps> = names
            .iter()
            .zip(shows.iter())
            .map(|(n, s)| parse_ollama_show(n, s))
            .collect();
        let list = ollama_models(&caps);
        assert_eq!(list.status, ModelStatus::Ready);
        let ids: Vec<&str> = list.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["qwen3:4b", "gemma4:12b"],
            "gemma3 has no tools and is filtered out"
        );
        assert_eq!(list.models[0].context_length, Some(262144));
        assert!(list.models[0].thinking && !list.models[0].vision);
        assert_eq!(list.models[0].efforts, vec!["low", "medium", "high"]);
        // Effort is hidden for a model without thinking.
        assert!(list.models[1].efforts.is_empty());
        assert!(list.models[1].vision);
    }

    #[test]
    fn ollama_with_no_tool_models_says_to_pull_one() {
        let caps = vec![parse_ollama_show(
            "gemma3:4b",
            &json!({ "capabilities": ["completion"] }),
        )];
        let list = ollama_models(&caps);
        assert_eq!(list.status, ModelStatus::NoModels);
        assert!(list.message.unwrap().contains("ollama pull"));
    }

    #[test]
    fn fallback_prefers_the_reported_default_then_the_first_model() {
        let list = parse_codex_models(&codex_fixture()).unwrap();
        assert_eq!(list.fallback_model(), Some("gpt-6-terra"));
        let mut no_default = list.clone();
        no_default.default_model = Some("gone".into());
        assert_eq!(no_default.fallback_model(), Some("gpt-6-luna"));
    }
}
