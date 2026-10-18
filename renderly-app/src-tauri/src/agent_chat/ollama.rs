//! Ollama driver: a local model, with Renderly running the agent loop itself. Ollama has no agent
//! CLI, so there is no subprocess and no MCP. Each turn is a streamed chat with the model; when the
//! model asks for a tool, the tool runs in-process through [`ToolHost`](super::tools::ToolHost),
//! the same functions the MCP server uses, and the result goes back in the next request.
//!
//! # Sources
//!
//! Ollama's HTTP API, <https://github.com/ollama/ollama/blob/main/docs/api.md> (also published at
//! docs.ollama.com/api). Shapes relied on:
//! - `GET /api/tags` → `{models: [{name, model, details, …}]}`.
//! - `POST /api/show` with `{model}` → `{capabilities: [...], model_info: {…}, details, …}`. Tool
//!   support is `"tools"` in `capabilities`; the context window is
//!   `model_info["<architecture>.context_length"]`; `"thinking"` and `"vision"` are the other
//!   capabilities used. Captured 2026-10-16 from a local server.
//! - `POST /api/chat` with `{model, messages, tools, stream: true, think, options: {num_ctx}}`.
//!   Messages use roles `system`, `user`, `assistant` and `tool`. An assistant message may carry
//!   `tool_calls: [{function: {name, arguments: {…}}}]`, and `arguments` is an object. A tool result
//!   is `{role: "tool", tool_name, content}`. `tools` is `[{type: "function", function: {name,
//!   description, parameters}}]`.
//! - Streamed reply: one JSON object per line (NDJSON). Each has `message.content` (text),
//!   `message.thinking` (reasoning, not shown), and optionally `message.tool_calls`. Captured: a
//!   tool call arrived complete in one chunk. The last object has `done: true` and `done_reason`.
//! - `think`: a boolean, or a string `"low"`, `"medium"`, `"high"`. Captured with qwen3:4b: the
//!   string is accepted and streams thinking. With `think: false`, qwen3 still wrote
//!   `<think>…</think>` into `content`, so [`ThinkFilter`] removes those blocks from the text shown.
//! - Errors: a non-2xx response, or a streamed `{"error": "…"}` line.
//!
//! # Context size
//!
//! `options.num_ctx` is `min(model context window, 32768)`, and 8192 when the model reports no
//! window. The loop keeps history under about `num_ctx × 3` characters by dropping the oldest user
//! turns, and never drops the system brief or the newest turn.
//!
//! # Tools
//!
//! Local models handle a small schema far better than the 29 KB `edit` schema, so Ollama gets a
//! slim set: typed tools for the common edits, a generic `edit` with `commands: [object]`, and
//! `edit_help`, which returns the command reference on demand. `look` is offered only to models
//! that report `vision`. See [`tool_definitions`]. The schema sizes are measured in the tests.
//!
//! # Cancel and limits
//!
//! Cancel aborts the task, which closes the HTTP stream. At most [`MAX_TOOL_ROUNDS`] tool rounds
//! run in one turn; then the turn stops with a message.

use super::driver::{AgentDriver, AgentId, AgentSession, LaunchOptions, SessionConfig};
use super::events::{summarize, AgentEnvelope, AgentEvent, EventSink};
use super::models::{
    ollama_models, parse_ollama_show, AgentModels, ModelOption, ModelStatus, OllamaCaps,
};
use super::permission::Decision;
use super::tools::{self, ToolHost};
use base64::Engine;
use renderly_core::edit_contract::{EditError, WriteOptions};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where Ollama listens when nothing else is set.
pub const DEFAULT_HOST: &str = "http://127.0.0.1:11434";
/// The environment variable Ollama itself reads for its address.
pub const HOST_ENV: &str = "OLLAMA_HOST";
/// Tool rounds allowed in one turn before the loop stops.
pub const MAX_TOOL_ROUNDS: usize = 16;
/// The largest context window requested, whatever the model reports.
pub const NUM_CTX_CAP: u64 = 32_768;
/// The context window used when the model reports none.
pub const NUM_CTX_FALLBACK: u64 = 8_192;

/// The brief for a local model. It replaces nothing (there is no vendor prompt), so it states the
/// whole role. It names the slim tool set; the command reference is behind `edit_help`.
pub const OLLAMA_BRIEF: &str = "\
You are the editing assistant built into Renderly, a video editor. The user is editing the open \
project and talks to you while they work. Change the project with your tools; do not describe \
steps instead of doing them.

Tools:
- add_caption, trim_clip, split_clip, move_clip, delete_clip, set_clip_effects, set_transition, \
set_transform, add_track, import_media: typed edits. Use them for the common cases.
- edit: any other command. Pass commands: a list of objects tagged by \"command\". Call edit_help \
first to see the command names and fields.
- edit_help: the command reference. Call it with command to see one command's full schema.
- timeline: the outline of the open timeline with short ids. Call it before editing if you do not \
have an id.
- find_media: media files in a folder, for import_media.
- assets: effects, transitions and caption style ids.

Rules:
- Ids: use the short ids from the context or timeline. Times are seconds.
- Act directly when the request is clear. If it is ambiguous (which clip, how long, which style), \
ask one short question instead.
- One request can need several calls. Each call is one undo step for that call.
- Pass expected_revision to edit with the project_revision from your latest timeline. If an edit \
fails with revision_conflict, call timeline and retry. For a big edit, use dry_run first.
- Deleting needs allow_deletion with a reason and the exact full ids a dry run lists. Without a \
clear request to delete, ask first.
- You have no shell and no file access. Never claim to have run a command.

Replies: one or two short sentences saying what you did or what you need. Do not list ids, JSON \
or tool names unless the user asks.";

/// Host the user chose, else `OLLAMA_HOST`, else the default. `OLLAMA_HOST` may be given without a
/// scheme (`127.0.0.1:11434`), as Ollama's own CLI accepts it.
pub fn resolve_host(setting: Option<&str>) -> String {
    if let Some(h) = setting.filter(|h| !h.trim().is_empty()) {
        return h.trim().trim_end_matches('/').to_string();
    }
    match std::env::var(HOST_ENV) {
        Ok(v) if !v.trim().is_empty() => {
            let v = v.trim().trim_end_matches('/');
            if v.starts_with("http://") || v.starts_with("https://") {
                v.to_string()
            } else {
                format!("http://{v}")
            }
        }
        _ => DEFAULT_HOST.to_string(),
    }
}

/// The context window to request: the model's own, capped; the fallback when it reports none.
pub fn num_ctx_for(context_length: Option<u64>) -> u64 {
    match context_length {
        Some(n) if n > 0 => n.min(NUM_CTX_CAP),
        _ => NUM_CTX_FALLBACK,
    }
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| format!("could not create the HTTP client: {e}"))
}

/// Read one model's facts from `/api/show`.
pub async fn show_model(
    client: &reqwest::Client,
    host: &str,
    model: &str,
) -> Result<OllamaCaps, String> {
    let resp = client
        .post(format!("{host}/api/show"))
        .timeout(Duration::from_secs(10))
        .json(&json!({ "model": model }))
        .send()
        .await
        .map_err(connect_message)?;
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| format!("Ollama returned an unreadable reply for {model}: {e}"))?;
    if !status.is_success() {
        return Err(
            error_text(&body).unwrap_or_else(|| format!("Ollama returned {status} for {model}"))
        );
    }
    Ok(parse_ollama_show(model, &body))
}

/// The model list for the picker: `/api/tags`, then `/api/show` for each model, filtered to the
/// models that report `tools`.
pub async fn fetch_models(host: &str) -> AgentModels {
    let client = match http_client() {
        Ok(c) => c,
        Err(e) => return AgentModels::failed(AgentId::Ollama, ModelStatus::Error, e),
    };
    let tags = match client
        .get(format!("{host}/api/tags"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) if e.is_connect() || e.is_timeout() => {
            return AgentModels::failed(
                AgentId::Ollama,
                ModelStatus::NotRunning,
                not_running_message(host),
            )
        }
        Err(e) => return AgentModels::failed(AgentId::Ollama, ModelStatus::Error, e.to_string()),
    };
    let body: Value = match tags.json().await {
        Ok(v) => v,
        Err(e) => return AgentModels::failed(AgentId::Ollama, ModelStatus::Error, e.to_string()),
    };
    let names: Vec<String> = body
        .get("models")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|m| m.get("name").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    let mut caps = Vec::with_capacity(names.len());
    for name in &names {
        match show_model(&client, host, name).await {
            Ok(c) => caps.push(c),
            // A model that cannot be inspected is not offered; the others still are.
            Err(_) => caps.push(OllamaCaps {
                name: name.clone(),
                tools: false,
                vision: false,
                thinking: false,
                context_length: None,
            }),
        }
    }
    ollama_models(&caps)
}

/// Whether the Ollama server answers at `host`.
pub async fn is_reachable(host: &str) -> bool {
    match http_client() {
        Ok(client) => client
            .get(format!("{host}/api/version"))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .is_ok(),
        Err(_) => false,
    }
}

fn not_running_message(host: &str) -> String {
    format!("Ollama is not running at {host}. Start Ollama, then refresh.")
}

fn connect_message(e: reqwest::Error) -> String {
    if e.is_connect() || e.is_timeout() {
        "Ollama is not running. Start Ollama, then try again.".to_string()
    } else {
        e.to_string()
    }
}

fn error_text(body: &Value) -> Option<String> {
    body.get("error")
        .and_then(Value::as_str)
        .map(str::to_string)
}

// ---- Tools ---------------------------------------------------------------------------------

fn tool(name: &str, description: &str, parameters: Value) -> Value {
    json!({
        "type": "function",
        "function": { "name": name, "description": description, "parameters": parameters },
    })
}

/// The slim tool set for a model. `look` is included only when `vision` is true.
pub fn tool_definitions(vision: bool) -> Vec<Value> {
    let id =
        json!({ "type": "string", "description": "Short id (first 8 hex characters) or full id." });
    let secs = |d: &str| json!({ "type": "number", "description": d });
    let mut tools = vec![
        tool(
            "add_caption",
            "Add a caption on a caption track.",
            json!({ "type": "object", "properties": {
                "track_id": id, "text": { "type": "string" },
                "position_secs": secs("Start time in seconds."),
                "duration_secs": secs("Length in seconds."),
                "style_id": { "type": "string", "description": "Caption style id from assets. Default \"default\"." },
            }, "required": ["track_id", "text", "position_secs", "duration_secs"] }),
        ),
        tool(
            "trim_clip",
            "Change a clip's source in and/or out point, in seconds of the source media.",
            json!({ "type": "object", "properties": {
                "track_id": id, "clip_id": id,
                "new_source_in_secs": secs("New in point."),
                "new_source_out_secs": secs("New out point."),
            }, "required": ["track_id", "clip_id"] }),
        ),
        tool(
            "split_clip",
            "Split a clip in two at a timeline time.",
            json!({ "type": "object", "properties": {
                "track_id": id, "clip_id": id, "at_secs": secs("Timeline time to split at."),
            }, "required": ["track_id", "clip_id", "at_secs"] }),
        ),
        tool(
            "move_clip",
            "Move a clip to a new timeline position, optionally onto another track.",
            json!({ "type": "object", "properties": {
                "track_id": id, "clip_id": id,
                "new_position_secs": secs("New start time on the timeline."),
                "new_track_id": id,
            }, "required": ["track_id", "clip_id", "new_position_secs"] }),
        ),
        tool(
            "delete_clip",
            "Delete a clip. With ripple, later clips on the track move up to close the gap.",
            json!({ "type": "object", "properties": {
                "track_id": id, "clip_id": id,
                "ripple": { "type": "boolean", "description": "Close the gap. Default false." },
            }, "required": ["track_id", "clip_id"] }),
        ),
        tool(
            "set_clip_effects",
            "Replace a clip's whole effect list. Each effect is an object; edit_help with command SetClipEffects shows the fields.",
            json!({ "type": "object", "properties": {
                "track_id": id, "clip_id": id,
                "effects": { "type": "array", "items": { "type": "object" } },
            }, "required": ["track_id", "clip_id", "effects"] }),
        ),
        tool(
            "set_transition",
            "Set or clear the transition on the clip's outgoing edge. Pass transition null to clear it.",
            json!({ "type": "object", "properties": {
                "track_id": id, "clip_id": id,
                "transition": { "type": ["object", "null"] },
            }, "required": ["track_id", "clip_id", "transition"] }),
        ),
        tool(
            "set_transform",
            "Set a clip's position, scale and rotation. edit_help with command SetClipTransform shows the fields.",
            json!({ "type": "object", "properties": {
                "track_id": id, "clip_id": id, "transform": { "type": "object" },
            }, "required": ["track_id", "clip_id", "transform"] }),
        ),
        tool(
            "add_track",
            "Add an empty track.",
            json!({ "type": "object", "properties": {
                "kind": { "type": "string", "enum": ["video", "audio", "caption"] },
                "name": { "type": "string" },
            }, "required": ["kind", "name"] }),
        ),
        tool(
            "import_media",
            "Import a media file into the project. Use find_media first to get the path.",
            json!({ "type": "object", "properties": {
                "path": { "type": "string", "description": "Full path to the file." },
            }, "required": ["path"] }),
        ),
        tool(
            "edit",
            "Any other command, applied as one undo step. Call edit_help first for the command names and fields. Pass expected_revision from the last timeline's project_revision. Use dry_run to preview. To delete, set allow_deletion to a reason and the exact ids a dry run lists.",
            json!({ "type": "object", "properties": {
                "commands": { "type": "array", "items": { "type": "object" } },
                "expected_revision": { "type": "integer", "description": "project_revision from the last timeline." },
                "dry_run": { "type": "boolean", "description": "Preview only; change nothing." },
                "request_id": { "type": "string", "description": "Retry key; the same id returns the earlier result." },
                "allow_deletion": { "type": "object", "description": "Needed to delete anything.", "properties": {
                    "reason": { "type": "string" },
                    "ids": { "type": "array", "items": { "type": "string" } },
                }, "required": ["reason", "ids"] },
            }, "required": ["commands"] }),
        ),
        tool(
            "edit_help",
            "The command reference for edit. With command, the full schema of that one command.",
            json!({ "type": "object", "properties": {
                "command": { "type": "string", "description": "A command name, for example SetClipSpeed." },
            } }),
        ),
        tool(
            "timeline",
            "The outline of the open timeline with short ids.",
            json!({ "type": "object", "properties": {
                "sequence_id": { "type": "string", "description": "Optional sequence id." },
            } }),
        ),
        tool(
            "find_media",
            "List media files in a folder, for import_media.",
            json!({ "type": "object", "properties": {
                "dir": { "type": "string" },
                "recursive": { "type": "boolean" },
                "max": { "type": "integer" },
            }, "required": ["dir"] }),
        ),
        tool(
            "assets",
            "Effects with parameters, transitions, caption style ids and loaded packs.",
            json!({ "type": "object", "properties": {} }),
        ),
    ];
    if vision {
        tools.push(tool(
            "look",
            "See the composited frame at a time. Defaults to the playhead.",
            json!({ "type": "object", "properties": {
                "time_secs": secs("Time in seconds."),
                "max_width": { "type": "integer", "description": "Image width in pixels. Default 768." },
            } }),
        ));
    }
    tools
}

/// The Claude `edit` tool's input schema size in bytes (from the real `Command` enum), for the
/// comparison in the report.
pub fn claude_edit_schema_bytes() -> usize {
    let root = schemars::schema_for!(tools::EditRequest);
    serde_json::to_value(&root)
        .map(|v| v.to_string().len())
        .unwrap_or(0)
}

/// Translate one typed tool call into a `Command` JSON object. `None` means the name is not a typed
/// edit (the caller handles it another way).
pub fn typed_command(name: &str, args: &Value) -> Option<Result<Value, String>> {
    // (tool, command, fields copied as given)
    let table: &[(&str, &str, &[&str])] = &[
        (
            "add_caption",
            "AddCaption",
            &[
                "track_id",
                "text",
                "position_secs",
                "duration_secs",
                "style_id",
            ],
        ),
        (
            "trim_clip",
            "TrimClip",
            &[
                "track_id",
                "clip_id",
                "new_source_in_secs",
                "new_source_out_secs",
            ],
        ),
        (
            "split_clip",
            "SplitClip",
            &["track_id", "clip_id", "at_secs"],
        ),
        (
            "move_clip",
            "MoveClip",
            &["track_id", "clip_id", "new_position_secs", "new_track_id"],
        ),
        (
            "delete_clip",
            "DeleteClip",
            &["track_id", "clip_id", "ripple"],
        ),
        (
            "set_clip_effects",
            "SetClipEffects",
            &["track_id", "clip_id", "effects"],
        ),
        (
            "set_transition",
            "SetClipTransition",
            &["track_id", "clip_id", "transition"],
        ),
        (
            "set_transform",
            "SetClipTransform",
            &["track_id", "clip_id", "transform"],
        ),
        ("add_track", "AddTrack", &["kind", "name"]),
        ("import_media", "ImportMedia", &["path"]),
    ];
    let (_, command, keys) = table.iter().find(|(t, _, _)| *t == name)?;
    let mut out = serde_json::Map::new();
    out.insert("command".into(), json!(command));
    for key in *keys {
        if let Some(v) = args.get(*key) {
            out.insert((*key).into(), v.clone());
        }
    }
    // Defaults the core requires but the slim schema leaves optional.
    match *command {
        "AddCaption" => {
            out.entry("style_id").or_insert(json!("default"));
        }
        "DeleteClip" => {
            out.entry("ripple").or_insert(json!(false));
        }
        "SetClipTransition" => {
            out.entry("transition").or_insert(Value::Null);
        }
        _ => {}
    }
    Some(Ok(Value::Object(out)))
}

/// The command reference for `edit_help`: every command name with its fields, or the full schema of
/// one command.
pub fn command_reference(command: Option<&str>) -> String {
    let root =
        serde_json::to_value(schemars::schema_for!(renderly_core::Command)).unwrap_or(Value::Null);
    let variants = root
        .get("oneOf")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let name_of = |v: &Value| -> Option<String> {
        let c = v.get("properties")?.get("command")?;
        c.get("const")
            .and_then(Value::as_str)
            .or_else(|| c.get("enum")?.as_array()?.first()?.as_str())
            .map(str::to_string)
    };
    if let Some(wanted) = command {
        let found = variants
            .iter()
            .find(|v| name_of(v).is_some_and(|n| n.eq_ignore_ascii_case(wanted)));
        return match found {
            Some(v) => serde_json::to_string_pretty(v).unwrap_or_default(),
            None => {
                format!("No command named '{wanted}'. Call edit_help with no command for the list.")
            }
        };
    }
    let mut lines = vec![
        "Commands for the edit tool. Each is an object with \"command\" set to the name below, plus:".to_string(),
    ];
    for v in &variants {
        let Some(name) = name_of(v) else { continue };
        let fields: Vec<String> = v
            .get("properties")
            .and_then(Value::as_object)
            .map(|props| {
                props
                    .iter()
                    .filter(|(k, _)| k.as_str() != "command")
                    .map(|(k, s)| {
                        let required = v
                            .get("required")
                            .and_then(Value::as_array)
                            .is_some_and(|r| r.iter().any(|x| x.as_str() == Some(k)));
                        format!("{k}{}: {}", if required { "" } else { "?" }, type_name(s))
                    })
                    .collect()
            })
            .unwrap_or_default();
        lines.push(format!("- {name}({})", fields.join(", ")));
    }
    lines.push("'?' marks an optional field. Ids are short ids (first 8 hex characters).".into());
    lines.join("\n")
}

fn type_name(schema: &Value) -> String {
    if let Some(t) = schema.get("type") {
        return match t {
            Value::String(s) => s.clone(),
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("|"),
            _ => "value".into(),
        };
    }
    if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
        return r.rsplit('/').next().unwrap_or("value").to_string();
    }
    "value".into()
}

// ---- Think blocks --------------------------------------------------------------------------

/// Removes `<think>…</think>` blocks from streamed text. Tags can split across chunks, so the tail
/// that could start a tag is held back until the next chunk.
#[derive(Default)]
pub struct ThinkFilter {
    inside: bool,
    pending: String,
}

impl ThinkFilter {
    pub fn push(&mut self, chunk: &str) -> String {
        self.pending.push_str(chunk);
        let mut out = String::new();
        loop {
            if self.inside {
                match self.pending.find("</think>") {
                    Some(i) => {
                        self.pending.drain(..i + "</think>".len());
                        self.inside = false;
                    }
                    None => {
                        let keep = partial_suffix(&self.pending, "</think>");
                        let cut = self.pending.len() - keep;
                        self.pending.drain(..cut);
                        break;
                    }
                }
            } else {
                match self.pending.find("<think>") {
                    Some(i) => {
                        out.push_str(&self.pending[..i]);
                        self.pending.drain(..i + "<think>".len());
                        self.inside = true;
                    }
                    None => {
                        let keep = partial_suffix(&self.pending, "<think>");
                        let cut = self.pending.len() - keep;
                        out.push_str(&self.pending[..cut]);
                        self.pending.drain(..cut);
                        break;
                    }
                }
            }
        }
        out
    }

    /// The text held back at the end of the stream.
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.pending);
        if self.inside {
            self.inside = false;
            String::new()
        } else {
            rest
        }
    }
}

/// Length of the longest suffix of `s` that is a proper prefix of `tag` (ASCII tags only).
fn partial_suffix(s: &str, tag: &str) -> usize {
    (1..tag.len())
        .rev()
        .find(|&k| s.len() >= k && s.as_bytes()[s.len() - k..] == tag.as_bytes()[..k])
        .unwrap_or(0)
}

// ---- History -------------------------------------------------------------------------------

fn message_chars(m: &Value) -> usize {
    m.to_string().len()
}

/// Drop the oldest user turns (a user message and the replies after it) until the history fits in
/// `budget_chars`. The system brief at index 0 and the newest user turn are always kept.
pub fn trim_history(messages: &mut Vec<Value>, budget_chars: usize) {
    loop {
        let total: usize = messages.iter().map(message_chars).sum();
        if total <= budget_chars {
            return;
        }
        let users: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.get("role").and_then(Value::as_str) == Some("user"))
            .map(|(i, _)| i)
            .collect();
        if users.len() < 2 {
            return;
        }
        let start = users[0];
        let end = users[1];
        messages.drain(start..end);
    }
}

// ---- Driver --------------------------------------------------------------------------------

pub struct OllamaDriver;

impl AgentDriver for OllamaDriver {
    fn agent(&self) -> AgentId {
        AgentId::Ollama
    }

    fn start(&self, cfg: &SessionConfig) -> Result<Box<dyn AgentSession>, String> {
        let tools_host = cfg
            .tools
            .clone()
            .ok_or("the Renderly tools are not available for this session")?;
        let options = cfg.options.clone().validated()?;
        let host = resolve_host(options.ollama_host.as_deref());
        let model = options
            .model
            .clone()
            .ok_or("choose an Ollama model first; the picker lists the ones with tools")?;
        let client = http_client()?;
        let caps = tauri::async_runtime::block_on(show_model(&client, &host, &model))?;
        if !caps.tools {
            return Err(format!(
                "{model} does not support tools, so Renderly cannot edit with it. Pick a model that does."
            ));
        }
        let num_ctx = num_ctx_for(caps.context_length);
        let think = if caps.thinking {
            Some(match options.effort.as_deref() {
                Some(e @ ("low" | "medium" | "high")) => json!(e),
                _ => json!(true),
            })
        } else {
            None
        };
        let tools = tool_definitions(caps.vision);
        let session = OllamaSession {
            cfg: cfg.clone(),
            client,
            host,
            model,
            num_ctx,
            think,
            tools,
            tool_host: tools_host,
            messages: Arc::new(Mutex::new(vec![
                json!({ "role": "system", "content": OLLAMA_BRIEF }),
            ])),
            task: None,
            turn_active: Arc::new(AtomicBool::new(false)),
            vision: caps.vision,
        };
        (cfg.sink)(AgentEnvelope {
            session_id: cfg.session_id.clone(),
            event: AgentEvent::SessionStarted {
                agent_id: AgentId::Ollama.program_name().into(),
            },
        });
        Ok(Box::new(session))
    }
}

/// Everything one turn's task needs, cloned out of the session.
#[derive(Clone)]
struct TurnCtx {
    session_id: String,
    sink: EventSink,
    client: reqwest::Client,
    host: String,
    model: String,
    num_ctx: u64,
    think: Option<Value>,
    tools: Arc<Vec<Value>>,
    tool_host: Arc<dyn ToolHost>,
    broker: Arc<super::permission::PermissionBroker>,
    messages: Arc<Mutex<Vec<Value>>>,
    turn_active: Arc<AtomicBool>,
    vision: bool,
}

impl TurnCtx {
    fn emit(&self, event: AgentEvent) {
        (self.sink)(AgentEnvelope {
            session_id: self.session_id.clone(),
            event,
        });
    }
}

struct OllamaSession {
    cfg: SessionConfig,
    client: reqwest::Client,
    host: String,
    model: String,
    num_ctx: u64,
    think: Option<Value>,
    tools: Vec<Value>,
    tool_host: Arc<dyn ToolHost>,
    messages: Arc<Mutex<Vec<Value>>>,
    task: Option<tauri::async_runtime::JoinHandle<()>>,
    turn_active: Arc<AtomicBool>,
    vision: bool,
}

impl AgentSession for OllamaSession {
    fn send_turn(&mut self, text: &str) -> Result<(), String> {
        if self.turn_active.swap(true, Ordering::SeqCst) {
            return Err("a turn is already running".into());
        }
        if let Ok(mut m) = self.messages.lock() {
            m.push(json!({ "role": "user", "content": text }));
        }
        let ctx = TurnCtx {
            session_id: self.cfg.session_id.clone(),
            sink: self.cfg.sink.clone(),
            client: self.client.clone(),
            host: self.host.clone(),
            model: self.model.clone(),
            num_ctx: self.num_ctx,
            think: self.think.clone(),
            tools: Arc::new(self.tools.clone()),
            tool_host: Arc::clone(&self.tool_host),
            broker: Arc::clone(&self.cfg.broker),
            messages: Arc::clone(&self.messages),
            turn_active: Arc::clone(&self.turn_active),
            vision: self.vision,
        };
        self.task = Some(tauri::async_runtime::spawn(async move {
            let outcome = turn_loop(&ctx).await;
            let stop_reason = match outcome {
                Ok(stop) => stop,
                Err(message) => {
                    ctx.emit(AgentEvent::Error { message });
                    "error".to_string()
                }
            };
            ctx.turn_active.store(false, Ordering::SeqCst);
            ctx.emit(AgentEvent::TurnFinished { stop_reason });
        }));
        Ok(())
    }

    fn cancel_turn(&mut self) -> Result<(), String> {
        if !self.turn_active.swap(false, Ordering::SeqCst) {
            return Ok(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
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
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.turn_active.store(false, Ordering::SeqCst);
    }
}

impl Drop for OllamaSession {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The loop for one turn: stream a model reply, run any tool calls, repeat until the model answers
/// without one, or the round cap is hit. Returns the stop reason.
async fn turn_loop(ctx: &TurnCtx) -> Result<String, String> {
    let mut rounds = 0usize;
    loop {
        if rounds >= MAX_TOOL_ROUNDS {
            ctx.emit(AgentEvent::Error {
                message: format!(
                    "Stopped after {MAX_TOOL_ROUNDS} tool rounds in one turn. Ask again to continue."
                ),
            });
            return Ok("tool_round_limit".into());
        }
        let (text, calls, done_reason) = stream_round(ctx).await?;
        let mut assistant = json!({ "role": "assistant", "content": text });
        if !calls.is_empty() {
            assistant["tool_calls"] = Value::Array(calls.clone());
        }
        if let Ok(mut m) = ctx.messages.lock() {
            m.push(assistant);
        }
        if calls.is_empty() {
            return Ok(done_reason.unwrap_or_else(|| "stop".into()));
        }
        rounds += 1;
        for call in &calls {
            run_call(ctx, call).await;
        }
    }
}

/// One streamed `/api/chat` request. Emits text deltas as they arrive. Returns the visible text,
/// the tool calls, and the `done_reason`.
async fn stream_round(ctx: &TurnCtx) -> Result<(String, Vec<Value>, Option<String>), String> {
    let snapshot = {
        let mut m = ctx
            .messages
            .lock()
            .map_err(|e| format!("history lock: {e}"))?;
        let tools_chars: usize = ctx.tools.iter().map(message_chars).sum();
        let budget = (ctx.num_ctx as usize)
            .saturating_mul(3)
            .saturating_sub(tools_chars + 2_000);
        trim_history(&mut m, budget);
        m.clone()
    };
    let mut body = json!({
        "model": ctx.model,
        "messages": snapshot,
        "tools": *ctx.tools,
        "stream": true,
        "options": { "num_ctx": ctx.num_ctx },
    });
    if let Some(think) = &ctx.think {
        body["think"] = think.clone();
    }
    let mut resp = ctx
        .client
        .post(format!("{}/api/chat", ctx.host))
        .json(&body)
        .send()
        .await
        .map_err(connect_message)?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| error_text(&v))
            .unwrap_or_else(|| format!("Ollama returned {status}"));
        return Err(message);
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut text = String::new();
    let mut calls: Vec<Value> = Vec::new();
    let mut done_reason = None;
    let mut filter = ThinkFilter::default();
    while let Some(chunk) = resp.chunk().await.map_err(connect_message)? {
        buf.extend_from_slice(&chunk);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(message) = error_text(&v) {
                return Err(message);
            }
            if let Some(msg) = v.get("message") {
                if let Some(content) = msg.get("content").and_then(Value::as_str) {
                    let shown = filter.push(content);
                    if !shown.is_empty() {
                        text.push_str(&shown);
                        ctx.emit(AgentEvent::TextDelta { text: shown });
                    }
                }
                if let Some(items) = msg.get("tool_calls").and_then(Value::as_array) {
                    calls.extend(items.iter().cloned());
                }
            }
            if v.get("done").and_then(Value::as_bool) == Some(true) {
                done_reason = v
                    .get("done_reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
        }
    }
    let rest = filter.finish();
    if !rest.is_empty() {
        text.push_str(&rest);
        ctx.emit(AgentEvent::TextDelta { text: rest });
    }
    Ok((text, calls, done_reason))
}

/// Arguments arrive as an object; some builds send a JSON string. Both are accepted.
fn arguments_of(call: &Value) -> Value {
    let raw = call
        .get("function")
        .and_then(|f| f.get("arguments"))
        .cloned()
        .unwrap_or(Value::Null);
    match raw {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
        other => other,
    }
}

/// Run one tool call: permission, execution, the panel events, and the tool result message.
async fn run_call(ctx: &TurnCtx, call: &Value) {
    let name = call
        .get("function")
        .and_then(|f| f.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let args = arguments_of(call);
    let id = format!("ollama-{}", uuid::Uuid::new_v4().simple());

    // The panel sees the same names Claude Code gives the Renderly tools. Typed edits and the
    // generic `edit` are both shown as `edit`, so the panel's action lines match.
    let edit_commands: Option<Result<Vec<Value>, String>> = match name.as_str() {
        "edit" => Some(match args.get("commands").and_then(Value::as_array) {
            Some(list) if !list.is_empty() => Ok(list.clone()),
            _ => Err("edit needs a non-empty commands list".to_string()),
        }),
        other => typed_command(other, &args).map(|r| r.map(|c| vec![c])),
    };
    let (panel_name, input) = match &edit_commands {
        Some(Ok(commands)) => (
            "mcp__renderly__edit".to_string(),
            json!({ "commands": commands }),
        ),
        Some(Err(_)) => ("mcp__renderly__edit".to_string(), args.clone()),
        None => (format!("mcp__renderly__{name}"), args.clone()),
    };
    ctx.emit(AgentEvent::ToolStarted {
        id: id.clone(),
        name: panel_name.clone(),
        input: input.clone(),
    });

    let (text, is_error, image) = if let Some(Err(e)) = &edit_commands {
        (e.clone(), true, None)
    } else {
        match ctx.broker.decide(&panel_name, &input).await {
            Decision::Deny { message } => (message, true, None),
            Decision::Allow => execute(ctx, &name, &args, edit_commands).await,
        }
    };

    let limit = if panel_name == "mcp__renderly__edit" {
        4000
    } else {
        400
    };
    ctx.emit(AgentEvent::ToolFinished {
        id,
        output_summary: summarize(&text, limit),
        is_error,
    });
    if let Ok(mut m) = ctx.messages.lock() {
        m.push(json!({ "role": "tool", "tool_name": name, "content": text }));
        if let Some(b64) = image {
            // Tool results are text in Ollama; the frame goes in as an image on a user turn.
            m.push(json!({ "role": "user", "content": "Here is the frame you asked for.", "images": [b64] }));
        }
    }
}

/// Execute an allowed tool. Returns the text for the model, whether it failed, and an optional
/// PNG (base64) to attach.
/// The edit-contract envelope from the `edit` tool's arguments. Absent fields keep their defaults.
fn edit_options(args: &Value) -> Result<WriteOptions, EditError> {
    let allow = match args.get("allow_deletion").filter(|v| !v.is_null()) {
        None => None,
        Some(raw) => Some(
            serde_json::from_value::<tools::AllowDeletionArg>(raw.clone()).map_err(|e| {
                EditError::invalid(format!("allow_deletion needs a reason and ids: {e}"))
            })?,
        ),
    };
    tools::options_from(
        args.get("expected_revision").and_then(Value::as_u64),
        args.get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        args.get("request_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        allow.as_ref(),
    )
}

async fn execute(
    ctx: &TurnCtx,
    name: &str,
    args: &Value,
    edit_commands: Option<Result<Vec<Value>, String>>,
) -> (String, bool, Option<String>) {
    let host: &dyn ToolHost = ctx.tool_host.as_ref();
    if let Some(Ok(commands)) = edit_commands {
        let options = match edit_options(args) {
            Ok(options) => options,
            Err(e) => return (tools::edit_error_text(&e), true, None),
        };
        return match tools::run_edit(host, commands, options).await {
            Ok(text) => (text, false, None),
            Err(e) => (tools::edit_error_text(&e), true, None),
        };
    }
    match name {
        "edit_help" => (
            command_reference(args.get("command").and_then(Value::as_str)),
            false,
            None,
        ),
        "timeline" => {
            match tools::run_timeline(host, args.get("sequence_id").and_then(Value::as_str)) {
                Ok(t) => (t, false, None),
                Err(e) => (e, true, None),
            }
        }
        "assets" => match tools::run_assets(host) {
            Ok(t) => (t, false, None),
            Err(e) => (e, true, None),
        },
        "find_media" => {
            let Some(dir) = args.get("dir").and_then(Value::as_str) else {
                return ("find_media needs dir".into(), true, None);
            };
            let dir = std::path::PathBuf::from(dir);
            let recursive = args
                .get("recursive")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let max = args.get("max").and_then(Value::as_u64).unwrap_or(50) as usize;
            match tauri::async_runtime::spawn_blocking(move || {
                tools::run_find_media(&dir, recursive, max)
            })
            .await
            {
                Ok(Ok(t)) => (t, false, None),
                Ok(Err(e)) => (e, true, None),
                Err(e) => (format!("search failed: {e}"), true, None),
            }
        }
        "look" if ctx.vision => {
            let time = args.get("time_secs").and_then(Value::as_f64);
            let width = args
                .get("max_width")
                .and_then(Value::as_u64)
                .map(|w| w as u32);
            match tools::run_look(Arc::clone(&ctx.tool_host), time, width).await {
                Ok(frame) => {
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&frame.png);
                    (
                        format!("{} ({}x{})", frame.caption, frame.width, frame.height),
                        false,
                        Some(b64),
                    )
                }
                Err(e) => (e, true, None),
            }
        }
        other => (format!("Unknown tool '{other}'."), true, None),
    }
}

/// Exposed for the tests and the app's detection: the host the app would use.
pub fn ollama_model_option(id: &str, context_length: Option<u64>) -> ModelOption {
    ModelOption {
        id: id.to_string(),
        label: id.to_string(),
        description: None,
        efforts: Vec::new(),
        default_effort: None,
        context_length,
        vision: false,
        thinking: false,
    }
}

/// The options a session starts with, when the user has not picked a model: the first tool-capable
/// model in the fetched list.
pub fn default_options(models: &AgentModels, options: &LaunchOptions) -> LaunchOptions {
    let mut out = options.clone();
    if out.model.is_none() {
        out.model = models.fallback_model().map(str::to_string);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_precedence_is_setting_then_env_then_default() {
        std::env::remove_var(HOST_ENV);
        assert_eq!(resolve_host(None), DEFAULT_HOST);
        assert_eq!(
            resolve_host(Some("http://10.0.0.5:11434/")),
            "http://10.0.0.5:11434"
        );
        std::env::set_var(HOST_ENV, "127.0.0.1:9999");
        assert_eq!(resolve_host(None), "http://127.0.0.1:9999");
        assert_eq!(
            resolve_host(Some("http://127.0.0.1:1")),
            "http://127.0.0.1:1"
        );
        std::env::remove_var(HOST_ENV);
    }

    #[test]
    fn num_ctx_is_the_model_window_capped_with_a_fallback() {
        assert_eq!(num_ctx_for(Some(262_144)), 32_768);
        assert_eq!(num_ctx_for(Some(4_096)), 4_096);
        assert_eq!(num_ctx_for(None), NUM_CTX_FALLBACK);
    }

    #[test]
    fn think_filter_removes_blocks_split_across_chunks() {
        let mut f = ThinkFilter::default();
        let mut out = String::new();
        for chunk in ["Hi <thi", "nk>secret", " text</th", "ink>\n\nDone"] {
            out.push_str(&f.push(chunk));
        }
        out.push_str(&f.finish());
        assert_eq!(out, "Hi \n\nDone");
    }

    #[test]
    fn think_filter_keeps_a_lone_angle_bracket() {
        let mut f = ThinkFilter::default();
        let mut out = f.push("a < b");
        out.push_str(&f.finish());
        assert_eq!(out, "a < b");
    }

    #[test]
    fn slim_tools_have_the_typed_set_and_generic_edit() {
        let names: Vec<String> = tool_definitions(false)
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_string())
            .collect();
        for n in [
            "add_caption",
            "trim_clip",
            "split_clip",
            "move_clip",
            "delete_clip",
            "set_clip_effects",
            "set_transition",
            "set_transform",
            "add_track",
            "import_media",
            "edit",
            "edit_help",
            "timeline",
            "find_media",
            "assets",
        ] {
            assert!(names.iter().any(|x| x == n), "missing {n}");
        }
        assert!(
            !names.iter().any(|n| n == "look"),
            "look is offered only with vision"
        );
    }

    #[test]
    fn look_is_offered_only_to_vision_models() {
        let with = tool_definitions(true);
        assert!(with.iter().any(|t| t["function"]["name"] == "look"));
    }

    #[test]
    fn generic_edit_schema_is_only_a_commands_list() {
        let edit = tool_definitions(false)
            .into_iter()
            .find(|t| t["function"]["name"] == "edit")
            .unwrap();
        let params = &edit["function"]["parameters"];
        // The commands list plus the edit-contract envelope (command-api.md "Edit contract").
        let props = params["properties"].as_object().unwrap();
        assert_eq!(props.len(), 5, "{props:?}");
        assert!(props["commands"].is_object());
        assert!(props.contains_key("expected_revision") && props.contains_key("dry_run"));
        assert_eq!(params["required"], serde_json::json!(["commands"]));
    }

    #[test]
    fn slim_tool_schemas_are_much_smaller_than_the_claude_edit_schema() {
        let slim: usize = tool_definitions(true)
            .iter()
            .map(|t| t.to_string().len())
            .sum();
        let claude = claude_edit_schema_bytes();
        eprintln!("ollama slim tool schemas: {slim} bytes; claude edit schema: {claude} bytes");
        assert!(slim < claude, "slim set {slim} >= claude edit {claude}");
    }

    #[test]
    fn typed_calls_become_commands_with_the_core_field_names() {
        let c = typed_command(
            "add_caption",
            &json!({ "track_id": "ab12cd34", "text": "Hi", "position_secs": 1.0, "duration_secs": 2.0 }),
        )
        .unwrap()
        .unwrap();
        assert_eq!(c["command"], "AddCaption");
        assert_eq!(c["style_id"], "default");
        assert_eq!(c["track_id"], "ab12cd34");
        let d = typed_command("delete_clip", &json!({ "track_id": "t", "clip_id": "c" }))
            .unwrap()
            .unwrap();
        assert_eq!(d["ripple"], false);
        assert!(typed_command("edit", &json!({})).is_none());
    }

    #[test]
    fn typed_commands_parse_as_core_commands() {
        let cases = [
            (
                "add_caption",
                json!({ "track_id": "00000000-0000-0000-0000-000000000001", "text": "Hi", "position_secs": 0.0, "duration_secs": 1.0 }),
            ),
            (
                "trim_clip",
                json!({ "track_id": "00000000-0000-0000-0000-000000000001", "clip_id": "00000000-0000-0000-0000-000000000002", "new_source_in_secs": 1.0 }),
            ),
            (
                "split_clip",
                json!({ "track_id": "00000000-0000-0000-0000-000000000001", "clip_id": "00000000-0000-0000-0000-000000000002", "at_secs": 1.0 }),
            ),
            (
                "delete_clip",
                json!({ "track_id": "00000000-0000-0000-0000-000000000001", "clip_id": "00000000-0000-0000-0000-000000000002" }),
            ),
            (
                "add_track",
                json!({ "kind": "caption", "name": "Captions" }),
            ),
        ];
        for (name, args) in cases {
            let v = typed_command(name, &args).unwrap().unwrap();
            serde_json::from_value::<renderly_core::Command>(v.clone())
                .unwrap_or_else(|e| panic!("{name} does not parse: {e}: {v}"));
        }
    }

    #[test]
    fn command_reference_lists_commands_and_describes_one() {
        let list = command_reference(None);
        assert!(list.contains("- AddCaption("), "{list}");
        assert!(list.contains("text: string"), "{list}");
        let one = command_reference(Some("addcaption"));
        assert!(one.contains("AddCaption"), "{one}");
        assert!(command_reference(Some("Nope")).starts_with("No command named"));
    }

    #[test]
    fn trimming_drops_the_oldest_turn_and_keeps_the_brief_and_newest_turn() {
        let mut m = vec![
            json!({ "role": "system", "content": "brief" }),
            json!({ "role": "user", "content": "x".repeat(400) }),
            json!({ "role": "assistant", "content": "a" }),
            json!({ "role": "user", "content": "y".repeat(400) }),
            json!({ "role": "assistant", "content": "b" }),
            json!({ "role": "user", "content": "newest" }),
        ];
        trim_history(&mut m, 600);
        assert_eq!(m[0]["role"], "system");
        assert_eq!(m.last().unwrap()["content"], "newest");
        assert!(!m.iter().any(|x| x["content"] == "x".repeat(400).as_str()));
    }

    #[test]
    fn arguments_accept_an_object_or_a_json_string() {
        let obj = json!({ "function": { "name": "x", "arguments": { "a": 1 } } });
        assert_eq!(arguments_of(&obj)["a"], 1);
        let s = json!({ "function": { "name": "x", "arguments": "{\"a\":2}" } });
        assert_eq!(arguments_of(&s)["a"], 2);
    }

    #[test]
    fn default_options_pick_the_fallback_when_no_model_is_chosen() {
        let list = AgentModels::ready(
            AgentId::Ollama,
            vec![ollama_model_option("qwen3:4b", Some(262_144))],
            Some("qwen3:4b".into()),
        );
        let o = default_options(&list, &LaunchOptions::default());
        assert_eq!(o.model.as_deref(), Some("qwen3:4b"));
    }
}
