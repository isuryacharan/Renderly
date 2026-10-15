//! Local Piper and BYO OpenAI TTS for Phase 1 voiceover tracks.

use crate::proc::SerialSpawn;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;
use thiserror::Error;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum VoiceoverProvider {
    /// Local Piper ONNX model via `piper` CLI. Set `RENDERLY_PIPER_MODEL`.
    PiperLocal {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        voice: Option<String>,
    },
    /// OpenAI TTS — requires `OPENAI_API_KEY` in the environment (BYO, opt-in).
    OpenAi { voice: String },
}

#[derive(Debug, Error)]
pub enum TtsError {
    #[error("piper not available; install piper and set RENDERLY_PIPER_MODEL")]
    PiperNotAvailable,
    #[error("OPENAI_API_KEY not set")]
    OpenAiKeyMissing,
    #[error("openai tts request failed: {0}")]
    OpenAiRequest(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("piper failed: {0}")]
    PiperFailed(String),
}

static PIPER: OnceLock<PathBuf> = OnceLock::new();

pub fn synthesize_to_wav(
    text: &str,
    provider: &VoiceoverProvider,
    output_wav: &Path,
) -> Result<(), TtsError> {
    match provider {
        VoiceoverProvider::PiperLocal { .. } => synthesize_piper(text, output_wav),
        VoiceoverProvider::OpenAi { voice } => synthesize_openai(text, voice, output_wav),
    }
}

fn synthesize_piper(text: &str, output_wav: &Path) -> Result<(), TtsError> {
    let model = std::env::var("RENDERLY_PIPER_MODEL").map_err(|_| TtsError::PiperNotAvailable)?;
    let piper = find_piper().ok_or(TtsError::PiperNotAvailable)?;

    let dir = std::env::temp_dir().join(format!("renderly-piper-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let input_txt = dir.join("input.txt");
    std::fs::write(&input_txt, text)?;

    let mut cmd = crate::proc::command(&piper);
    cmd.args(["--model", &model, "--output_file"])
        .arg(output_wav)
        .args(["--input_file"])
        .arg(&input_txt);

    if let Ok(config) = std::env::var("RENDERLY_PIPER_CONFIG") {
        cmd.args(["--config", &config]);
    }

    let status = cmd
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status_serial()
        .map_err(|e| TtsError::PiperFailed(e.to_string()))?;
    std::fs::remove_dir_all(&dir).ok();
    if !status.success() {
        return Err(TtsError::PiperFailed("piper exited with error".into()));
    }
    Ok(())
}

fn synthesize_openai(text: &str, voice: &str, output_wav: &Path) -> Result<(), TtsError> {
    let api_key = std::env::var("OPENAI_API_KEY").map_err(|_| TtsError::OpenAiKeyMissing)?;
    let body = serde_json::json!({
        "model": "tts-1",
        "input": text,
        "voice": voice,
        "response_format": "wav",
    });

    let response = reqwest::blocking::Client::new()
        .post("https://api.openai.com/v1/audio/speech")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .map_err(|e| TtsError::OpenAiRequest(e.to_string()))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        return Err(TtsError::OpenAiRequest(format!("HTTP {status}: {body}")));
    }

    let bytes = response
        .bytes()
        .map_err(|e| TtsError::OpenAiRequest(e.to_string()))?;
    let mut file = std::fs::File::create(output_wav)?;
    file.write_all(&bytes)?;
    Ok(())
}

fn find_piper() -> Option<PathBuf> {
    if let Some(path) = PIPER.get() {
        return Some(path.clone());
    }
    for name in ["piper", "piper.exe"] {
        if let Some(path) = which_tool(name) {
            let _ = PIPER.set(path.clone());
            return Some(path);
        }
    }
    None
}

fn which_tool(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(name);
            candidate.is_file().then_some(candidate)
        })
    })
}

pub fn is_piper_available() -> bool {
    find_piper().is_some() && std::env::var("RENDERLY_PIPER_MODEL").is_ok()
}

pub fn is_openai_available() -> bool {
    std::env::var("OPENAI_API_KEY").is_ok()
}
