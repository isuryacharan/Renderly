//! Driver tests: the real Claude Code and Codex drivers, pointed at the `renderly-fake-agent`
//! test binary instead of the vendor CLI. No network, no account, no vendor CLI is run.

use renderly_app_lib::agent_chat::claude::ClaudeDriver;
use renderly_app_lib::agent_chat::codex::CodexDriver;
use renderly_app_lib::agent_chat::driver::{AgentDriver, SessionConfig};
use renderly_app_lib::agent_chat::events::{AgentEnvelope, AgentEvent, EventSink};
use renderly_app_lib::agent_chat::permission::PermissionBroker;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type Store = Arc<Mutex<Vec<AgentEnvelope>>>;

struct Harness {
    cfg: SessionConfig,
    store: Store,
    dir: PathBuf,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "renderly-agent-test-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("edit.renderly.json");
        std::fs::write(&project, "{}").unwrap();
        let store: Store = Arc::new(Mutex::new(Vec::new()));
        let s2 = Arc::clone(&store);
        let sink: EventSink = Arc::new(move |env| s2.lock().unwrap().push(env));
        let session_id = uuid::Uuid::new_v4().to_string();
        let broker = PermissionBroker::new(session_id.clone(), Arc::clone(&sink), false);
        let cfg = SessionConfig {
            session_id,
            project_path: project,
            mcp_binary: PathBuf::from("renderly-mcp-not-used-by-the-fake"),
            program: Some(fake_agent_path()),
            scratch_dir: dir.join("scratch"),
            broker,
            sink,
        };
        Self { cfg, store, dir }
    }

    fn events(&self) -> Vec<AgentEvent> {
        self.store
            .lock()
            .unwrap()
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

fn fake_agent_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_renderly-fake-agent"))
}

fn text_of(evs: &[AgentEvent]) -> String {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn turn_stop_reasons(evs: &[AgentEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            AgentEvent::TurnFinished { stop_reason } => Some(stop_reason.clone()),
            _ => None,
        })
        .collect()
}

fn pid_from_text(evs: &[AgentEvent]) -> Option<u32> {
    text_of(evs).split("pid=").nth(1).and_then(|rest| {
        rest.chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()
    })
}

/// Whether a process with this pid still exists.
fn process_alive(pid: u32) -> bool {
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output()
            .expect("tasklist");
        String::from_utf8_lossy(&out.stdout).contains(&pid.to_string())
    } else {
        Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

fn wait_dead(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while process_alive(pid) {
        assert!(Instant::now() < deadline, "process {pid} is still running");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---- Claude Code driver ------------------------------------------------------------------

#[test]
fn claude_streams_text_and_finishes_the_turn() {
    let h = Harness::new("claude-text");
    let mut session = ClaudeDriver.start(&h.cfg).expect("start");
    session.send_turn("say hello").unwrap();
    let evs = h.wait_turn_finished();
    assert!(
        matches!(evs[0], AgentEvent::SessionStarted { .. }),
        "{evs:?}"
    );
    assert_eq!(text_of(&evs), "Hello world");
    assert_eq!(turn_stop_reasons(&evs), vec!["end_turn"]);
    session.shutdown();
}

#[test]
fn claude_reports_a_renderly_tool_call_and_its_result() {
    let h = Harness::new("claude-tool");
    let mut session = ClaudeDriver.start(&h.cfg).expect("start");
    session
        .send_turn("trim the first clip, use a tool")
        .unwrap();
    let evs = h.wait_turn_finished();
    let started = evs.iter().find_map(|e| match e {
        AgentEvent::ToolStarted { id, name, input } => {
            Some((id.clone(), name.clone(), input.clone()))
        }
        _ => None,
    });
    let (id, name, input) = started.expect("tool_started");
    assert_eq!(id, "tu_1");
    assert_eq!(name, "mcp__renderly__apply_command");
    assert_eq!(input["command"]["command"], "TrimClip");
    assert!(evs.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished { id, is_error: false, .. } if id == "tu_1"
    )));
    assert_eq!(text_of(&evs), "Trimmed.");
    session.shutdown();
}

#[test]
fn claude_cancel_mid_turn_then_the_next_turn_still_works() {
    let h = Harness::new("claude-cancel");
    let mut session = ClaudeDriver.start(&h.cfg).expect("start");
    session.send_turn("slow job").unwrap();
    h.wait_for("first delta", |evs| text_of(evs).contains("Working"));
    session.cancel_turn().unwrap();
    let evs = h.wait_for("cancelled", |evs| {
        turn_stop_reasons(evs).contains(&"cancelled".to_string())
    });
    assert!(
        !evs.iter().any(|e| matches!(e, AgentEvent::Error { .. })),
        "{evs:?}"
    );
    // The next turn relaunches the CLI with --resume and completes normally.
    session.send_turn("say hello").unwrap();
    h.wait_for("second turn", |evs| turn_stop_reasons(evs).len() >= 2);
    let evs = h.events();
    assert!(text_of(&evs).ends_with("Hello world"), "{evs:?}");
    assert_eq!(turn_stop_reasons(&evs).last().unwrap(), "end_turn");
    session.shutdown();
}

#[test]
fn claude_shutdown_kills_the_process_and_reports_no_crash() {
    let h = Harness::new("claude-stop");
    let mut session = ClaudeDriver.start(&h.cfg).expect("start");
    session.send_turn("slow job").unwrap();
    let evs = h.wait_for("pid", |evs| pid_from_text(evs).is_some());
    let pid = pid_from_text(&evs).unwrap();
    assert!(process_alive(pid));
    session.shutdown();
    wait_dead(pid);
    // A deliberate stop is silent: no "exited unexpectedly" error follows.
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !h.events()
            .iter()
            .any(|e| matches!(e, AgentEvent::Error { .. })),
        "{:?}",
        h.events()
    );
}

#[test]
fn claude_start_fails_clearly_when_the_program_is_missing() {
    let mut h = Harness::new("claude-missing");
    h.cfg.program = Some(Path::new("C:/definitely/not/here/claude.exe").to_path_buf());
    let err = match ClaudeDriver.start(&h.cfg) {
        Ok(_) => panic!("start should fail"),
        Err(e) => e,
    };
    assert!(err.contains("could not start claude"), "{err}");
}

// ---- Codex driver --------------------------------------------------------------------------

#[test]
fn codex_streams_text_and_tool_cards() {
    let h = Harness::new("codex-text");
    let mut session = CodexDriver.start(&h.cfg).expect("start");
    session.send_turn("hi").unwrap();
    let evs = h.wait_turn_finished();
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::SessionStarted { .. })));
    assert_eq!(text_of(&evs), "Hi there");
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolStarted { name, .. } if name == "shell")));
    assert!(evs
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolFinished { id, .. } if id == "c1")));
    assert_eq!(turn_stop_reasons(&evs), vec!["completed"]);
    session.shutdown();
}

#[test]
fn codex_approval_allowed_once_reaches_the_server() {
    let h = Harness::new("codex-allow");
    let mut session = CodexDriver.start(&h.cfg).expect("start");
    session.send_turn("run an approval command").unwrap();
    let evs = h.wait_for("permission_request", |evs| {
        evs.iter()
            .any(|e| matches!(e, AgentEvent::PermissionRequest { .. }))
    });
    let request_id = match evs.iter().find_map(|e| match e {
        AgentEvent::PermissionRequest {
            request_id,
            options,
            ..
        } => {
            assert!(options.iter().any(|o| o.id == "allow_once"));
            Some(request_id.clone())
        }
        _ => None,
    }) {
        Some(id) => id,
        None => panic!("no permission request"),
    };
    h.cfg.broker.answer(&request_id, "allow_once").unwrap();
    let evs = h.wait_turn_finished();
    assert_eq!(text_of(&evs), "approved");
    session.shutdown();
}

#[test]
fn codex_approval_denied_reaches_the_server() {
    let h = Harness::new("codex-deny");
    let mut session = CodexDriver.start(&h.cfg).expect("start");
    session.send_turn("run an approval command").unwrap();
    let evs = h.wait_for("permission_request", |evs| {
        evs.iter()
            .any(|e| matches!(e, AgentEvent::PermissionRequest { .. }))
    });
    let request_id = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionRequest { request_id, .. } => Some(request_id.clone()),
            _ => None,
        })
        .unwrap();
    h.cfg.broker.answer(&request_id, "reject_once").unwrap();
    let evs = h.wait_turn_finished();
    assert_eq!(text_of(&evs), "declined");
    session.shutdown();
}

#[test]
fn codex_cancel_interrupts_the_turn() {
    let h = Harness::new("codex-cancel");
    let mut session = CodexDriver.start(&h.cfg).expect("start");
    session.send_turn("slow job").unwrap();
    h.wait_for("first delta", |evs| text_of(evs).contains("Working"));
    session.cancel_turn().unwrap();
    let evs = h.wait_turn_finished();
    assert_eq!(turn_stop_reasons(&evs), vec!["interrupted"]);
    session.shutdown();
}

#[test]
fn codex_shutdown_kills_the_app_server() {
    let h = Harness::new("codex-stop");
    let mut session = CodexDriver.start(&h.cfg).expect("start");
    session.send_turn("slow job").unwrap();
    let evs = h.wait_for("pid", |evs| pid_from_text(evs).is_some());
    let pid = pid_from_text(&evs).unwrap();
    session.shutdown();
    wait_dead(pid);
}
