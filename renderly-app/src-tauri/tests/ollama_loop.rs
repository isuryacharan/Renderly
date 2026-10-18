//! Ollama driver tests against a scripted fake of Ollama's HTTP API (std TcpListener, no real
//! model). The Renderly tools run in-process against an in-memory project, through the same
//! `apply_command` the app's command path uses. Nothing here downloads or runs a model.

use renderly_app_lib::agent_chat::driver::{AgentDriver, LaunchOptions, SessionConfig};
use renderly_app_lib::agent_chat::events::{AgentEnvelope, AgentEvent, EventSink};
use renderly_app_lib::agent_chat::models::ModelStatus;
use renderly_app_lib::agent_chat::ollama::{self, OllamaDriver};
use renderly_app_lib::agent_chat::permission::PermissionBroker;
use renderly_app_lib::agent_chat::tools::{BoxFut, ToolHost};
use renderly_core::project::{Id, Project, Settings, Track, TrackKind};
use renderly_core::Command;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The caption track in the test project. A fixed id, so the fake model can name it.
const CAPTION_TRACK: &str = "00000000-0000-0000-0000-0000000000c1";

// ---- In-memory tool host -------------------------------------------------------------------

struct TestHost {
    project: Mutex<Arc<Project>>,
    revision: Mutex<u64>,
    ledger: Mutex<renderly_core::edit_contract::WriteLedger>,
}

impl TestHost {
    fn new() -> Arc<Self> {
        let mut project = Project::new("Demo", Settings::default());
        let mut track = Track::new(TrackKind::Caption, "Captions");
        track.id = CAPTION_TRACK.parse::<Id>().expect("fixed track id");
        project.tracks.push(track);
        Arc::new(Self {
            project: Mutex::new(Arc::new(project)),
            revision: Mutex::new(0),
            ledger: Mutex::new(renderly_core::edit_contract::WriteLedger::new()),
        })
    }

    fn json(&self) -> String {
        let p = self
            .project
            .lock()
            .map(|g| Arc::clone(&g))
            .expect("project lock");
        serde_json::to_string(&*p).expect("project json")
    }
}

impl ToolHost for TestHost {
    fn project(&self) -> Result<Arc<Project>, String> {
        self.project
            .lock()
            .map(|g| Arc::clone(&g))
            .map_err(|e| e.to_string())
    }

    fn playhead_secs(&self) -> f64 {
        0.0
    }

    fn active_sequence(&self) -> Option<Id> {
        None
    }

    fn revision(&self) -> u64 {
        self.revision.lock().map(|g| *g).unwrap_or(0)
    }

    fn apply_write(
        &self,
        commands: Vec<Value>,
        options: renderly_core::edit_contract::WriteOptions,
    ) -> BoxFut<'_, Result<renderly_app_lib::CommandsResult, renderly_core::edit_contract::EditError>>
    {
        Box::pin(async move {
            let cmds: Vec<Command> = commands
                .into_iter()
                .map(|v| {
                    serde_json::from_value(v).map_err(|e| {
                        renderly_core::edit_contract::EditError::invalid(e.to_string())
                    })
                })
                .collect::<Result<_, _>>()?;
            let current = self
                .project()
                .map_err(renderly_core::edit_contract::EditError::internal)?;
            let revision = self.revision();
            let mut plan = {
                let ledger = self.ledger.lock().map_err(|e| {
                    renderly_core::edit_contract::EditError::internal(e.to_string())
                })?;
                renderly_core::edit_contract::plan_write(
                    &current,
                    revision,
                    &ledger,
                    &options,
                    renderly_core::edit_contract::Caller::Agent,
                    cmds,
                )?
            };
            let typed = plan.outcomes().to_vec();
            let committed_at = match plan.take_next() {
                Some(next) => {
                    *self.project.lock().map_err(|e| {
                        renderly_core::edit_contract::EditError::internal(e.to_string())
                    })? = Arc::new(next);
                    let mut rev = self.revision.lock().map_err(|e| {
                        renderly_core::edit_contract::EditError::internal(e.to_string())
                    })?;
                    *rev += 1;
                    Some(*rev)
                }
                None => None,
            };
            let write = {
                let mut ledger = self.ledger.lock().map_err(|e| {
                    renderly_core::edit_contract::EditError::internal(e.to_string())
                })?;
                plan.finish(committed_at, &mut ledger)
            };
            Ok(renderly_app_lib::CommandsResult {
                write,
                outcomes: typed.iter().map(|o| format!("{o:?}")).collect(),
                typed_outcomes: typed,
            })
        })
    }

    fn render_png(
        &self,
        _time_secs: f64,
        _preset: renderly_core::commands::ExportPreset,
    ) -> Result<Vec<u8>, String> {
        Err("rendering is not available in tests".into())
    }
}

// ---- Fake Ollama server --------------------------------------------------------------------

/// What the fake server knows about one model, as `/api/show` reports it.
#[derive(Clone)]
struct FakeModel {
    name: &'static str,
    capabilities: &'static [&'static str],
    context: Option<u64>,
}

struct FakeOllama {
    url: String,
    /// Every `/api/chat` request body, in order.
    chats: Arc<Mutex<Vec<Value>>>,
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some((method, path, body))
}

fn write_json_response(stream: &mut std::net::TcpStream, status: &str, body: &Value) {
    let text = body.to_string();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        text.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(text.as_bytes());
    let _ = stream.flush();
}

/// Start the fake. `models` is what `/api/tags` lists and `/api/show` describes.
fn start_fake(models: Vec<FakeModel>) -> FakeOllama {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake ollama");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let chats: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let chats_for_server = Arc::clone(&chats);
    let round = Arc::new(AtomicUsize::new(0));
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let models = models.clone();
            let chats = Arc::clone(&chats_for_server);
            let round = Arc::clone(&round);
            std::thread::spawn(move || {
                let Some((method, path, body)) = read_request(&mut stream) else {
                    return;
                };
                match (method.as_str(), path.as_str()) {
                    ("GET", "/api/tags") => {
                        let list: Vec<Value> = models
                            .iter()
                            .map(|m| json!({ "name": m.name, "model": m.name }))
                            .collect();
                        write_json_response(&mut stream, "200 OK", &json!({ "models": list }));
                    }
                    ("POST", "/api/show") => {
                        let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        let name = req["model"].as_str().unwrap_or("");
                        match models.iter().find(|m| m.name == name) {
                            Some(m) => {
                                let mut info = serde_json::Map::new();
                                if let Some(ctx) = m.context {
                                    info.insert("general.architecture".into(), json!("qwen3"));
                                    info.insert("qwen3.context_length".into(), json!(ctx));
                                }
                                write_json_response(
                                    &mut stream,
                                    "200 OK",
                                    &json!({ "capabilities": m.capabilities, "model_info": info }),
                                );
                            }
                            None => write_json_response(
                                &mut stream,
                                "404 Not Found",
                                &json!({ "error": format!("model '{name}' not found") }),
                            ),
                        }
                    }
                    ("POST", "/api/chat") => {
                        let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                        chats.lock().map(|mut c| c.push(req.clone())).ok();
                        let n = round.fetch_add(1, Ordering::SeqCst);
                        stream_chat(&mut stream, &req, n);
                    }
                    _ => write_json_response(
                        &mut stream,
                        "404 Not Found",
                        &json!({ "error": "not found" }),
                    ),
                }
            });
        }
    });
    FakeOllama { url, chats }
}

fn ndjson(stream: &mut std::net::TcpStream, line: &Value) {
    let _ = stream.write_all(format!("{line}\n").as_bytes());
    let _ = stream.flush();
}

/// Script one streamed reply from the last user or tool message.
fn stream_chat(stream: &mut std::net::TcpStream, req: &Value, _n: usize) {
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n",
    );
    let messages = req["messages"].as_array().cloned().unwrap_or_default();
    let last = messages.last().cloned().unwrap_or(Value::Null);
    let last_role = last["role"].as_str().unwrap_or("");
    let last_text = last["content"].as_str().unwrap_or("").to_string();
    let chunk = |content: &str| json!({ "model": "qwen3:4b", "message": { "role": "assistant", "content": content }, "done": false });
    let done = json!({ "model": "qwen3:4b", "message": { "role": "assistant", "content": "" }, "done": true, "done_reason": "stop" });

    // A turn that asked for "loop" keeps asking for a perception tool, to exercise the round cap.
    let looping = messages
        .iter()
        .any(|m| m["role"] == "user" && m["content"].as_str().is_some_and(|c| c.contains("loop")));
    if looping {
        ndjson(
            stream,
            &json!({ "model": "qwen3:4b", "message": { "role": "assistant", "content": "", "tool_calls": [
            { "function": { "name": "edit_help", "arguments": { "command": "AddCaption" } } } ] }, "done": false }),
        );
        ndjson(stream, &done);
    } else if last_role == "tool" {
        // The model has seen the tool result: answer in text.
        ndjson(stream, &chunk("Added "));
        ndjson(stream, &chunk("the caption."));
        ndjson(stream, &done);
    } else if last_text.contains("slow") {
        // Stream one piece, then stall until the client goes away.
        ndjson(stream, &chunk("Working"));
        std::thread::sleep(Duration::from_secs(60));
    } else {
        // A typed edit: add a caption on the caption track.
        ndjson(
            stream,
            &json!({ "model": "qwen3:4b", "message": { "role": "assistant", "content": "", "tool_calls": [
            { "function": { "name": "add_caption", "arguments": {
                "track_id": CAPTION_TRACK, "text": "Hello", "position_secs": 1.0, "duration_secs": 2.0 } } } ] }, "done": false }),
        );
        ndjson(stream, &done);
    }
}

fn qwen_and_gemma() -> Vec<FakeModel> {
    vec![
        FakeModel {
            name: "qwen3:4b",
            capabilities: &["completion", "tools", "thinking"],
            context: Some(262_144),
        },
        FakeModel {
            name: "gemma3:4b",
            capabilities: &["completion"],
            context: None,
        },
    ]
}

// ---- Harness -------------------------------------------------------------------------------

type Store = Arc<Mutex<Vec<AgentEnvelope>>>;

struct Harness {
    cfg: SessionConfig,
    store: Store,
    host: Arc<TestHost>,
    dir: PathBuf,
}

impl Harness {
    fn new(tag: &str, ollama_url: &str, model: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "renderly-ollama-test-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("dir");
        let store: Store = Arc::new(Mutex::new(Vec::new()));
        let s2 = Arc::clone(&store);
        let sink: EventSink = Arc::new(move |env| s2.lock().expect("store").push(env));
        let session_id = uuid::Uuid::new_v4().to_string();
        // Edits are auto-allowed, as the app starts sessions.
        let broker = PermissionBroker::new(session_id.clone(), Arc::clone(&sink), true);
        let host = TestHost::new();
        let cfg = SessionConfig {
            session_id,
            project_path: dir.join("edit.renderly.json"),
            mcp_endpoint: None,
            tools: Some(host.clone() as Arc<dyn ToolHost>),
            options: LaunchOptions {
                model: Some(model.to_string()),
                ollama_host: Some(ollama_url.to_string()),
                ..LaunchOptions::default()
            },
            program: None,
            scratch_dir: dir.join("scratch"),
            broker,
            sink,
        };
        Self {
            cfg,
            store,
            host,
            dir,
        }
    }

    fn events(&self) -> Vec<AgentEvent> {
        self.store
            .lock()
            .expect("store")
            .iter()
            .map(|e| e.event.clone())
            .collect()
    }

    fn wait_for(&self, what: &str, pred: impl Fn(&[AgentEvent]) -> bool) -> Vec<AgentEvent> {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let evs = self.events();
            if pred(&evs) {
                return evs;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {evs:#?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_turn_finished(&self) -> Vec<AgentEvent> {
        self.wait_for("turn_finished", |evs| {
            evs.iter()
                .any(|e| matches!(e, AgentEvent::TurnFinished { .. }))
        })
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text_of(evs: &[AgentEvent]) -> String {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn stop_reasons(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::TurnFinished { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        })
        .collect()
}

// ---- Tests ---------------------------------------------------------------------------------

#[test]
fn a_tool_call_round_trip_runs_the_tool_in_process_and_returns_the_result() {
    let fake = start_fake(qwen_and_gemma());
    let h = Harness::new("roundtrip", &fake.url, "qwen3:4b");
    let mut session = OllamaDriver.start(&h.cfg).expect("start");
    session.send_turn("Add a caption that says Hello.").unwrap();
    let evs = h.wait_turn_finished();

    // The panel sees the edit as Claude Code's name for it, with the command as input.
    let started = evs.iter().find_map(|e| match e {
        AgentEvent::ToolStarted { name, input, .. } => Some((name.clone(), input.clone())),
        _ => None,
    });
    let (name, input) = started.expect("tool_started");
    assert_eq!(name, "mcp__renderly__edit");
    assert_eq!(input["commands"][0]["command"], "AddCaption");
    assert!(
        evs.iter().any(|e| matches!(
            e,
            AgentEvent::ToolFinished {
                is_error: false,
                ..
            }
        )),
        "{evs:?}"
    );
    assert_eq!(text_of(&evs), "Added the caption.");
    assert_eq!(stop_reasons(&evs), vec!["stop"]);

    // The edit reached the project: the caption text is in it.
    assert!(
        h.host.json().contains("Hello"),
        "caption not in the project"
    );

    // The first request carried the brief, the slim tools, and the context size.
    let chats = fake.chats.lock().unwrap().clone();
    assert_eq!(
        chats.len(),
        2,
        "one request for the call, one for the answer"
    );
    let first = &chats[0];
    assert_eq!(first["stream"], true);
    assert_eq!(
        first["options"]["num_ctx"], 32_768,
        "262144 capped at 32768"
    );
    assert_eq!(first["think"], true, "thinking model, no effort chosen");
    assert_eq!(first["messages"][0]["role"], "system");
    let tool_names: Vec<&str> = first["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert!(tool_names.contains(&"add_caption") && tool_names.contains(&"edit"));
    assert!(!tool_names.contains(&"look"), "qwen3 has no vision");

    // The second request carries the tool result back to the model.
    let second = &chats[1];
    let msgs = second["messages"].as_array().unwrap();
    let tool_msg = msgs
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .expect("tool message");
    assert_eq!(tool_msg["tool_name"], "add_caption");
    assert!(tool_msg["content"]
        .as_str()
        .unwrap()
        .contains("Applied 1 command(s)"));
    session.shutdown();
}

/// One real turn against the local Ollama server and a real model. Ignored by default because it
/// needs `ollama serve` and the model pulled. It asks for one explicit caption, so the model has a
/// clear tool to call. Run with `cargo test --test ollama_loop -- --ignored`.
#[test]
#[ignore = "needs a local Ollama server with qwen3:4b; run with --ignored"]
fn real_qwen3_round_trip_against_the_local_server() {
    let host = ollama::resolve_host(None);
    let mut h = Harness::new("real", &host, "qwen3:4b");
    // Low thinking keeps the turn short; a cold model load can take a minute on its own.
    h.cfg.options.effort = Some("low".into());
    let mut session = OllamaDriver
        .start(&h.cfg)
        .expect("start against the local server");
    session
        .send_turn(&format!(
            "Call the add_caption tool once: track_id {CAPTION_TRACK}, text \"Hello\", position_secs 1, duration_secs 2."
        ))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(600);
    let evs = loop {
        let evs = h.events();
        if evs
            .iter()
            .any(|e| matches!(e, AgentEvent::TurnFinished { .. }))
        {
            break evs;
        }
        assert!(
            Instant::now() < deadline,
            "the real turn did not finish: {evs:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    eprintln!("real turn events: {evs:#?}");
    assert!(
        !evs.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
        "{evs:?}"
    );
    assert!(
        h.host.json().contains("Hello"),
        "the model did not add the caption: {evs:?}"
    );
    session.shutdown();
}

#[test]
fn the_round_cap_stops_a_turn_that_keeps_asking_for_tools() {
    let fake = start_fake(qwen_and_gemma());
    let h = Harness::new("cap", &fake.url, "qwen3:4b");
    let mut session = OllamaDriver.start(&h.cfg).expect("start");
    session.send_turn("loop forever").unwrap();
    let evs = h.wait_turn_finished();
    let message = evs.iter().find_map(|e| match e {
        AgentEvent::Error { message } => Some(message.clone()),
        _ => None,
    });
    let message = message.expect("a clear error when the cap is hit");
    assert!(message.contains("16 tool rounds"), "{message}");
    assert_eq!(stop_reasons(&evs), vec!["tool_round_limit"]);
    assert_eq!(fake.chats.lock().unwrap().len(), ollama::MAX_TOOL_ROUNDS);
    session.shutdown();
}

#[test]
fn cancel_mid_stream_aborts_the_request_and_reports_cancelled() {
    let fake = start_fake(qwen_and_gemma());
    let h = Harness::new("cancel", &fake.url, "qwen3:4b");
    let mut session = OllamaDriver.start(&h.cfg).expect("start");
    session.send_turn("slow answer please").unwrap();
    h.wait_for("first delta", |evs| text_of(evs).contains("Working"));
    session.cancel_turn().unwrap();
    let evs = h.wait_turn_finished();
    assert_eq!(stop_reasons(&evs), vec!["cancelled"]);
    assert!(
        !evs.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
        "{evs:?}"
    );
    session.shutdown();
}

#[test]
fn only_tool_capable_models_are_offered_and_others_are_refused() {
    let fake = start_fake(qwen_and_gemma());
    let list = tauri::async_runtime::block_on(ollama::fetch_models(&fake.url));
    assert_eq!(list.status, ModelStatus::Ready, "{:?}", list.message);
    let ids: Vec<&str> = list.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, vec!["qwen3:4b"], "gemma3:4b reports no tools");
    assert_eq!(list.models[0].context_length, Some(262_144));
    assert_eq!(
        list.models[0].efforts,
        vec!["low", "medium", "high"],
        "thinking model offers effort"
    );

    let h = Harness::new("filter", &fake.url, "gemma3:4b");
    let err = match OllamaDriver.start(&h.cfg) {
        Ok(_) => panic!("a model without tools must not start"),
        Err(e) => e,
    };
    assert!(err.contains("does not support tools"), "{err}");
}

#[test]
fn a_server_that_is_down_is_reported_as_not_running() {
    // Port 1 refuses connections; nothing listens there.
    let list = tauri::async_runtime::block_on(ollama::fetch_models("http://127.0.0.1:1"));
    assert_eq!(list.status, ModelStatus::NotRunning);
    assert!(list.models.is_empty());
    assert!(list.message.unwrap_or_default().contains("Start Ollama"));
    assert!(!tauri::async_runtime::block_on(ollama::is_reachable(
        "http://127.0.0.1:1"
    )));
}

#[test]
fn an_empty_tag_list_says_to_pull_a_tool_model() {
    let fake = start_fake(vec![FakeModel {
        name: "gemma3:4b",
        capabilities: &["completion"],
        context: None,
    }]);
    let list = tauri::async_runtime::block_on(ollama::fetch_models(&fake.url));
    assert_eq!(list.status, ModelStatus::NoModels);
    assert!(list.message.unwrap_or_default().contains("ollama pull"));
}
