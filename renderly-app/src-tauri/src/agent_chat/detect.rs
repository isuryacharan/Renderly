//! Detection of installed agent CLIs and their sign-in state. Detection only runs the CLI's own
//! version and status commands. It never reads the CLIs' credential files and never sends a
//! prompt, so it uses no model quota.

use super::driver::AgentId;
use super::process;
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignIn {
    SignedIn,
    SignedOut,
    /// The CLI offers no non-interactive status check that Renderly relies on.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub id: AgentId,
    pub name: String,
    pub installed: bool,
    pub version: Option<String>,
    pub sign_in: SignIn,
    /// The exact command to run in a terminal to sign in, when Renderly knows it.
    pub sign_in_command: Option<String>,
    /// What to show when the CLI is missing.
    pub install_hint: String,
    pub note: Option<String>,
}

pub fn detect(agent: AgentId) -> AgentInfo {
    match agent {
        AgentId::ClaudeCode => detect_claude(),
        AgentId::Codex => detect_codex(),
    }
}

fn detect_claude() -> AgentInfo {
    let program = process::resolve_program("claude");
    let installed = program.is_some();
    let version = program
        .as_ref()
        .and_then(|p| process::run_capture(p, &["--version"]))
        .and_then(|(ok, out)| ok.then_some(out));
    // `claude auth status` exits 0 when signed in and 1 when not (documented in the CLI reference).
    let sign_in = match program
        .as_ref()
        .and_then(|p| process::run_capture(p, &["auth", "status"]))
    {
        Some((true, _)) => SignIn::SignedIn,
        Some((false, _)) => SignIn::SignedOut,
        None => SignIn::Unknown,
    };
    AgentInfo {
        id: AgentId::ClaudeCode,
        name: AgentId::ClaudeCode.display_name().into(),
        installed,
        version,
        sign_in,
        sign_in_command: Some("claude auth login".into()),
        install_hint:
            "Install Claude Code (see https://code.claude.com/docs/en/setup), then sign in.".into(),
        note: None,
    }
}

fn detect_codex() -> AgentInfo {
    let program = process::resolve_program("codex");
    let installed = program.is_some();
    let version = program
        .as_ref()
        .and_then(|p| process::run_capture(p, &["--version"]))
        .and_then(|(ok, out)| ok.then_some(out));
    AgentInfo {
        id: AgentId::Codex,
        name: AgentId::Codex.display_name().into(),
        installed,
        version,
        sign_in: SignIn::Unknown,
        sign_in_command: Some("codex login".into()),
        install_hint: "Install the Codex CLI from OpenAI, then run `codex login`.".into(),
        note: Some(
            "Codex support is untested against the real CLI. If a session fails to start, sign \
             in with `codex login` in a terminal and try again."
                .into(),
        ),
    }
}
