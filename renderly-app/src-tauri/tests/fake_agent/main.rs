//! Test-only stand-in for the `claude` and `codex` CLIs. It speaks just enough of each CLI's
//! stdio protocol for the driver tests, and nothing else. It never touches the network or any
//! account. Mode is chosen by the arguments: `app-server` anywhere in argv means Codex.
//!
//! Claude mode (stream-json): reads user messages from stdin and answers each with deltas and a
//! `result`. Prompt keywords select scripted behaviour:
//! - `tool`: a tool_use / tool_result pair for `mcp__renderly__apply_command`, then text.
//! - `slow`: streams one delta, then blocks until it is killed (cancel tests).
//!
//! Codex mode (JSON-RPC over JSONL): answers `initialize`, `thread/start`, `turn/start`, and
//! `turn/interrupt`. Prompt keywords:
//! - `approval`: sends `item/commandExecution/requestApproval`, waits for the answer, then
//!   replies with the decision.
//! - `slow`: streams a delta, then waits for `turn/interrupt`.

use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::time::Duration;

fn send(value: &Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "app-server") {
        codex();
    } else {
        claude();
    }
}

fn claude() {
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // The model-list probe: a control request, answered with no prompt sent to a model.
        if v.get("type").and_then(Value::as_str) == Some("control_request") {
            send(
                &json!({"type": "control_response", "response": {"subtype": "success",
                "request_id": v["request_id"].clone(),
                "response": {"models": [
                    {"value": "default", "resolvedModel": "claude-opus-5-5", "displayName": "Default (recommended)",
                     "description": "Opus 5.5", "supportsEffort": true,
                     "supportedEffortLevels": ["low", "medium", "high", "xhigh", "max"]},
                    {"value": "sonnet", "resolvedModel": "claude-sonnet-5-5", "displayName": "Sonnet 5.5",
                     "description": "Efficient for routine tasks", "supportsEffort": true,
                     "supportedEffortLevels": ["low", "medium", "high"]}
                ],
                "account": {"email": "fake@example.com", "subscriptionType": "Test"}}}}),
            );
            continue;
        }
        if v.get("type").and_then(Value::as_str) != Some("user") {
            continue;
        }
        let text = v["message"]["content"].as_str().unwrap_or("").to_string();
        if text.contains("slow") {
            // The pid lets the tests check that a stop really kills the process.
            delta(&format!("pid={}", std::process::id()));
            delta("Working");
            // Blocks until the driver kills us (cancel or stop).
            loop {
                std::thread::sleep(Duration::from_secs(3600));
            }
        }
        if text.contains("tool") {
            send(&json!({"type": "system", "subtype": "init", "session_id": "fake"}));
            send(&json!({"type": "assistant", "message": {"content": [{
                "type": "tool_use", "id": "tu_1", "name": "mcp__renderly__apply_command",
                "input": {"command": {"command": "TrimClip", "clip_id": "c1"}}
            }]}}));
            send(&json!({"type": "user", "message": {"content": [{
                "type": "tool_result", "tool_use_id": "tu_1",
                "content": [{"type": "text", "text": "{\"revision\":2}"}], "is_error": false
            }]}}));
            delta("Trimmed.");
        } else {
            delta("Hello");
            delta(" world");
        }
        send(
            &json!({"type": "result", "subtype": "success", "is_error": false,
                     "result": "done", "stop_reason": "end_turn"}),
        );
    }
}

fn delta(text: &str) {
    send(&json!({"type": "stream_event", "event": {
        "type": "content_block_delta", "index": 0,
        "delta": {"type": "text_delta", "text": text}
    }}));
}

fn respond(id: &Value, result: Value) {
    send(&json!({"id": id, "result": result}));
}

fn notify(method: &str, params: Value) {
    send(&json!({"method": method, "params": params}));
}

fn codex() {
    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();
    while let Some(Ok(line)) = lines.next() {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");
        let id = v.get("id").cloned();
        match method {
            "initialize" => respond(
                &id.unwrap_or(Value::Null),
                json!({"userAgent": "fake-codex"}),
            ),
            "initialized" => {}
            "thread/start" => respond(
                &id.unwrap_or(Value::Null),
                json!({"thread": {"id": "thr_1"}}),
            ),
            "turn/start" => {
                let id = id.unwrap_or(Value::Null);
                respond(
                    &id,
                    json!({"turn": {"id": "turn_1", "status": "inProgress", "items": [], "error": null}}),
                );
                let text = v["params"]["input"][0]["text"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                notify("turn/started", json!({"turn": {"id": "turn_1"}}));
                run_turn(&mut lines, &text);
            }
            "turn/interrupt" => respond(&id.unwrap_or(Value::Null), json!({})),
            // The model-list request (no turn is started).
            "model/list" => respond(
                &id.unwrap_or(Value::Null),
                json!({"data": [
                    {"id": "gpt-6-terra", "model": "gpt-6-terra", "displayName": "GPT-6-Terra",
                     "description": "Default", "hidden": false, "isDefault": true,
                     "defaultReasoningEffort": "medium",
                     "supportedReasoningEfforts": [
                         {"reasoningEffort": "low", "description": "Fast"},
                         {"reasoningEffort": "medium", "description": "Balanced"}]},
                    {"id": "gpt-6-luna", "model": "gpt-6-luna", "displayName": "GPT-6-Luna",
                     "description": "Light", "hidden": false, "isDefault": false,
                     "supportedReasoningEfforts": []}
                ], "nextCursor": null}),
            ),
            _ => {
                if let Some(id) = id {
                    send(
                        &json!({"id": id, "error": {"code": -32601, "message": "unknown method"}}),
                    );
                }
            }
        }
    }
}

fn agent_delta(text: &str) {
    notify(
        "item/agentMessage/delta",
        json!({"threadId": "thr_1", "turnId": "turn_1", "itemId": "m1", "delta": text}),
    );
}

fn run_turn(lines: &mut std::io::Lines<std::io::StdinLock<'_>>, text: &str) {
    if text.contains("approval") {
        send(
            &json!({"id": 900, "method": "item/commandExecution/requestApproval", "params": {
                "threadId": "thr_1", "turnId": "turn_1", "itemId": "c1", "command": "rm -rf scratch"
            }}),
        );
        let mut decision = String::from("none");
        while let Some(Ok(line)) = lines.next() {
            let Ok(reply) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if reply.get("id") == Some(&json!(900)) {
                decision = reply["result"]["decision"]
                    .as_str()
                    .unwrap_or("none")
                    .to_string();
                break;
            }
        }
        agent_delta(if decision == "accept" {
            "approved"
        } else {
            "declined"
        });
        notify(
            "turn/completed",
            json!({"turn": {"id": "turn_1", "status": "completed"}}),
        );
        return;
    }
    if text.contains("slow") {
        agent_delta(&format!("pid={}", std::process::id()));
        agent_delta("Working");
        while let Some(Ok(line)) = lines.next() {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg.get("method").and_then(Value::as_str) == Some("turn/interrupt") {
                respond(&msg.get("id").cloned().unwrap_or(Value::Null), json!({}));
                notify(
                    "turn/completed",
                    json!({"turn": {"id": "turn_1", "status": "interrupted"}}),
                );
                return;
            }
        }
        return;
    }
    agent_delta("Hi");
    agent_delta(" there");
    notify(
        "item/started",
        json!({"item": {"id": "c1", "type": "commandExecution", "command": "ls"}}),
    );
    notify(
        "item/completed",
        json!({"item": {
            "id": "c1", "type": "commandExecution", "status": "completed", "aggregatedOutput": "ok"
        }}),
    );
    notify(
        "turn/completed",
        json!({"turn": {"id": "turn_1", "status": "completed"}}),
    );
}
