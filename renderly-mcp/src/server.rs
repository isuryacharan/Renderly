//! MCP tool handlers for Renderly — every edit dispatches through `renderly_core::apply_command`
//! when headless, or through the live Tauri WebSocket bridge (E1/E2) when the app is open
//! on the same project.

use crate::bridge_client::{try_live_bridge, BridgeClient, EditorStatusHeadless};
use renderly_core::{
    apply_command, audio_peaks as analyze_audio_peaks, commands::ExportPreset,
    detect_scenes as analyze_scenes, detect_silence as analyze_silence, export_project, media,
    perceive, project::Project, transcribe_media, Command,
};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct RenderlyMcp {
    #[expect(dead_code, reason = "tool_handler macro accesses tool_router")]
    tool_router: ToolRouter<Self>,
    session: Arc<Mutex<Option<Session>>>,
}

#[derive(Debug)]
struct Session {
    path: PathBuf,
    project: Project,
}

impl RenderlyMcp {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
            session: Arc::new(Mutex::new(None)),
        }
    }

    fn with_session<F, R>(&self, f: F) -> Result<R, String>
    where
        F: FnOnce(&mut Session) -> Result<R, String>,
    {
        let mut guard = self
            .session
            .lock()
            .map_err(|e| format!("session lock poisoned: {e}"))?;
        let session = guard
            .as_mut()
            .ok_or_else(|| "no project open; call open_project or new_project first".to_string())?;
        f(session)
    }

    fn save(session: &Session) -> Result<(), String> {
        let data = serde_json::to_string_pretty(&session.project)
            .map_err(|e| format!("serialize project: {e}"))?;
        std::fs::write(&session.path, data).map_err(|e| format!("write project: {e}"))
    }

    fn block_on_bridge<F, T>(f: F) -> Result<T, String>
    where
        F: std::future::Future<Output = Result<T, String>>,
    {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(f))
    }

    /// When the desktop app is live on the same project path, run `live`; otherwise `headless`.
    fn with_live_or_headless<R>(
        &self,
        live: impl FnOnce(&mut BridgeClient) -> Result<R, String>,
        headless: impl FnOnce(&mut Session) -> Result<R, String>,
    ) -> Result<R, String> {
        let path = {
            let guard = self
                .session
                .lock()
                .map_err(|e| format!("session lock poisoned: {e}"))?;
            guard.as_ref().map(|s| s.path.clone())
        };
        let bridged = Self::block_on_bridge(async { Ok(try_live_bridge(path.as_deref()).await) })?;
        if let Some((_disc, mut client)) = bridged {
            return live(&mut client);
        }
        self.with_session(headless)
    }

    async fn sync_session_from_bridge(
        session: &mut Session,
        client: &mut BridgeClient,
    ) -> Result<(), String> {
        let value = client.call("get_project", json!({})).await?;
        // Bridge returns `{ project, revision }` (B3); accept a bare Project for older shapes.
        let project_val = value.get("project").cloned().unwrap_or(value);
        session.project = serde_json::from_value(project_val).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NewProjectRequest {
    pub path: String,
    #[serde(default = "default_name")]
    pub name: String,
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    #[serde(default = "default_fps")]
    pub fps: f64,
}

fn default_name() -> String {
    "untitled".into()
}
fn default_width() -> u32 {
    1080
}
fn default_height() -> u32 {
    1920
}
fn default_fps() -> f64 {
    60.0
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct OpenProjectRequest {
    pub path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ApplyCommandRequest {
    /// A single Command object — same JSON as renderly-cli `apply`.
    pub command: serde_json::Value,
}

/// Parse the `command` tool parameter into a `Command`, accepting both a bare JSON object
/// and a JSON-encoded string. Claude Code's MCP client serialises `serde_json::Value`
/// parameters as a JSON string (the schema carries no explicit `type: object`), while
/// other clients send the object directly — both shapes must work. Callers re-serialise
/// the parsed command before forwarding (e.g. to the live bridge) so downstream always
/// sees a canonical object.
fn parse_command_param(value: &serde_json::Value) -> Result<Command, String> {
    match value {
        serde_json::Value::String(s) => {
            serde_json::from_str(s).map_err(|e| format!("invalid command JSON: {e}"))
        }
        v => serde_json::from_value(v.clone()).map_err(|e| format!("invalid command JSON: {e}")),
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportRequest {
    pub output_path: String,
    #[serde(default = "default_preset")]
    pub preset: String,
}

fn default_preset() -> String {
    "tiktok".into()
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProbeMediaRequest {
    pub path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TranscriptRequest {
    pub media_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenderFrameRequest {
    pub time_secs: f64,
    #[serde(default = "default_preset")]
    pub preset: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SilenceDetectRequest {
    pub media_id: String,
    #[serde(default = "default_noise_db")]
    pub noise_db: f64,
    #[serde(default = "default_min_silence")]
    pub min_duration_secs: f64,
}

fn default_noise_db() -> f64 {
    -30.0
}
fn default_min_silence() -> f64 {
    0.5
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SceneDetectRequest {
    pub media_id: String,
    #[serde(default = "default_scene_threshold")]
    pub threshold: f64,
}

fn default_scene_threshold() -> f64 {
    0.4
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AudioPeaksRequest {
    pub media_id: String,
    #[serde(default = "default_buckets")]
    pub buckets: u32,
}

fn default_buckets() -> u32 {
    256
}

#[tool_router]
impl RenderlyMcp {
    #[tool(description = "Create a new empty .renderly.json project file and open it")]
    fn new_project(&self, Parameters(req): Parameters<NewProjectRequest>) -> String {
        self.new_project_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn new_project_impl(&self, req: NewProjectRequest) -> Result<String, String> {
        use renderly_core::project::Settings;
        let path = PathBuf::from(&req.path);
        let project = Project::new(
            req.name,
            Settings {
                fps: req.fps,
                width: req.width,
                height: req.height,
                sample_rate: 48000,
                duck_db: -12.0,
            },
        );
        let data = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
        std::fs::write(&path, data).map_err(|e| e.to_string())?;
        *self.session.lock().map_err(|e| e.to_string())? = Some(Session { path, project });
        Ok(format!("created and opened {}", req.path))
    }

    #[tool(description = "Open an existing .renderly.json project file")]
    fn open_project(&self, Parameters(req): Parameters<OpenProjectRequest>) -> String {
        self.open_project_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn open_project_impl(&self, req: OpenProjectRequest) -> Result<String, String> {
        let path = PathBuf::from(&req.path);
        let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let project: Project = serde_json::from_str(&data).map_err(|e| e.to_string())?;
        *self.session.lock().map_err(|e| e.to_string())? = Some(Session { path, project });
        Ok(format!("opened {}", req.path))
    }

    #[tool(description = "Return the current project JSON")]
    fn get_project(&self) -> String {
        self.get_project_impl()
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn get_project_impl(&self) -> Result<String, String> {
        self.with_live_or_headless(
            |client| {
                let value =
                    Self::block_on_bridge(async { client.call("get_project", json!({})).await })?;
                // Prefer bare project JSON for agents; fall back to full snapshot if needed.
                let project = value.get("project").unwrap_or(&value);
                serde_json::to_string_pretty(project).map_err(|e| e.to_string())
            },
            |session| serde_json::to_string_pretty(&session.project).map_err(|e| e.to_string()),
        )
    }

    #[tool(
        name = "apply_command",
        description = "Apply one renderly-core Command and save the project"
    )]
    fn apply_command_tool(&self, Parameters(req): Parameters<ApplyCommandRequest>) -> String {
        self.apply_command_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn apply_command_impl(&self, req: ApplyCommandRequest) -> Result<String, String> {
        let cmd = parse_command_param(&req.command)?;
        // Re-serialise the parsed command so the bridge always receives a JSON object,
        // not the raw (possibly string-encoded) value the MCP client sent us.
        let cmd_value = serde_json::to_value(&cmd).map_err(|e| format!("internal error: {e}"))?;
        self.with_live_or_headless(
            |client| {
                let result = Self::block_on_bridge(async {
                    client
                        .call("apply_command", json!({ "command": cmd_value }))
                        .await
                })?;
                // Keep the MCP-side session mirror fresh for perception tools that stay headless.
                if let Ok(mut guard) = self.session.lock() {
                    if let Some(session) = guard.as_mut() {
                        let _ = Self::block_on_bridge(async {
                            Self::sync_session_from_bridge(session, client).await
                        });
                    }
                }
                Ok(result
                    .get("outcome")
                    .and_then(|v| v.as_str())
                    .unwrap_or("ok")
                    .to_string())
            },
            |session| {
                let outcome =
                    apply_command(&mut session.project, cmd).map_err(|e| e.to_string())?;
                Self::save(session)?;
                Ok(format!("{outcome:?}"))
            },
        )
    }

    #[tool(description = "Render the open project to an MP4 file")]
    fn export(&self, Parameters(req): Parameters<ExportRequest>) -> String {
        self.export_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn export_impl(&self, req: ExportRequest) -> Result<String, String> {
        let preset = parse_preset(&req.preset)?;
        self.with_live_or_headless(
            |client| {
                Self::block_on_bridge(async {
                    client
                        .call(
                            "export",
                            json!({
                                "output_path": req.output_path,
                                "preset": req.preset,
                            }),
                        )
                        .await
                })?;
                Ok(format!("exported to {}", req.output_path))
            },
            |session| {
                export_project(
                    &session.project,
                    PathBuf::from(&req.output_path).as_path(),
                    preset,
                )
                .map_err(|e| e.to_string())?;
                Ok(format!("exported to {}", req.output_path))
            },
        )
    }

    #[tool(
        name = "detect_silence",
        description = "Detect silent spans in a project media item (FFmpeg silencedetect)"
    )]
    fn detect_silence_tool(&self, Parameters(req): Parameters<SilenceDetectRequest>) -> String {
        self.detect_silence_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn detect_silence_impl(&self, req: SilenceDetectRequest) -> Result<String, String> {
        let media_id: uuid::Uuid = req
            .media_id
            .parse()
            .map_err(|e| format!("invalid media_id UUID: {e}"))?;
        self.with_session(|session| {
            let path = media_path(&session.project, media_id)?;
            let spans = analyze_silence(&path, req.noise_db, req.min_duration_secs)
                .map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&spans).map_err(|e| e.to_string())
        })
    }

    #[tool(
        name = "detect_scenes",
        description = "Detect scene cuts in a project media item (FFmpeg scene filter)"
    )]
    fn detect_scenes_tool(&self, Parameters(req): Parameters<SceneDetectRequest>) -> String {
        self.detect_scenes_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn detect_scenes_impl(&self, req: SceneDetectRequest) -> Result<String, String> {
        let media_id: uuid::Uuid = req
            .media_id
            .parse()
            .map_err(|e| format!("invalid media_id UUID: {e}"))?;
        self.with_session(|session| {
            let path = media_path(&session.project, media_id)?;
            let cuts = analyze_scenes(&path, req.threshold).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&cuts).map_err(|e| e.to_string())
        })
    }

    #[tool(description = "Downsampled audio peak envelope for waveform / agent perception")]
    fn get_audio_peaks(&self, Parameters(req): Parameters<AudioPeaksRequest>) -> String {
        self.get_audio_peaks_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn get_audio_peaks_impl(&self, req: AudioPeaksRequest) -> Result<String, String> {
        let media_id: uuid::Uuid = req
            .media_id
            .parse()
            .map_err(|e| format!("invalid media_id UUID: {e}"))?;
        self.with_session(|session| {
            let path = media_path(&session.project, media_id)?;
            let peaks = analyze_audio_peaks(&path, req.buckets).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&peaks).map_err(|e| e.to_string())
        })
    }

    #[tool(description = "Probe a media file on disk (duration, dimensions, kind)")]
    fn probe_media(&self, Parameters(req): Parameters<ProbeMediaRequest>) -> String {
        self.probe_media_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn probe_media_impl(&self, req: ProbeMediaRequest) -> Result<String, String> {
        let probed = media::probe(PathBuf::from(&req.path).as_path()).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&probed).map_err(|e| e.to_string())
    }

    #[tool(description = "Whisper transcript for a media_id in the open project (local STT)")]
    fn get_transcript(&self, Parameters(req): Parameters<TranscriptRequest>) -> String {
        self.get_transcript_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn get_transcript_impl(&self, req: TranscriptRequest) -> Result<String, String> {
        let media_id: uuid::Uuid = req
            .media_id
            .parse()
            .map_err(|e| format!("invalid media_id UUID: {e}"))?;
        self.with_session(|session| {
            let transcript =
                transcribe_media(&session.project, media_id).map_err(|e| e.to_string())?;
            serde_json::to_string_pretty(&transcript).map_err(|e| e.to_string())
        })
    }

    #[tool(
        description = "Render a PNG preview of the composited frame at time_secs (base64-encoded PNG in JSON)"
    )]
    fn render_frame(&self, Parameters(req): Parameters<RenderFrameRequest>) -> String {
        self.render_frame_impl(req)
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn render_frame_impl(&self, req: RenderFrameRequest) -> Result<String, String> {
        let preset = parse_preset(&req.preset)?;
        self.with_live_or_headless(
            |client| {
                let value = Self::block_on_bridge(async {
                    client
                        .call(
                            "render_frame",
                            json!({
                                "time_secs": req.time_secs,
                                "preset": req.preset,
                            }),
                        )
                        .await
                })?;
                serde_json::to_string(&value).map_err(|e| e.to_string())
            },
            |session| {
                let png = perceive::render_frame_png(&session.project, req.time_secs, preset)
                    .map_err(|e| e.to_string())?;
                let encoded = base64_encode(&png);
                serde_json::to_string(&serde_json::json!({
                    "time_secs": req.time_secs,
                    "png_base64": encoded,
                    "byte_len": png.len(),
                }))
                .map_err(|e| e.to_string())
            },
        )
    }

    #[tool(
        description = "Whether the Renderly desktop app is live on this project, plus playhead/selection"
    )]
    fn get_editor_status(&self) -> String {
        self.get_editor_status_impl()
            .unwrap_or_else(|e| format!("error: {e}"))
    }

    fn get_editor_status_impl(&self) -> Result<String, String> {
        let session_path = {
            let guard = self
                .session
                .lock()
                .map_err(|e| format!("session lock poisoned: {e}"))?;
            guard.as_ref().map(|s| s.path.clone())
        };
        // Status does not require a path match — report live app even if MCP has no session.
        let status = Self::block_on_bridge(async {
            if let Some((_disc, mut client)) = try_live_bridge(None).await {
                // If MCP has a project open on a different path, still report live but note mismatch.
                let mut value = client.call("get_editor_status", json!({})).await?;
                if let (Some(session_path), Some(obj)) =
                    (session_path.as_ref(), value.as_object_mut())
                {
                    let live_path = obj
                        .get("project_path")
                        .and_then(|v| v.as_str())
                        .map(PathBuf::from);
                    let matched = live_path
                        .as_ref()
                        .map(|p| {
                            p.canonicalize().unwrap_or_else(|_| p.clone())
                                == session_path
                                    .canonicalize()
                                    .unwrap_or_else(|_| session_path.clone())
                        })
                        .unwrap_or(false);
                    obj.insert("path_match".into(), json!(matched));
                }
                Ok(value)
            } else {
                Ok(serde_json::to_value(EditorStatusHeadless {
                    live: false,
                    project_path: session_path.map(|p| p.to_string_lossy().into_owned()),
                    playhead: None,
                    selection: None,
                })
                .unwrap_or(json!({ "live": false })))
            }
        })?;
        serde_json::to_string_pretty(&status).map_err(|e| e.to_string())
    }
}

fn media_path(project: &Project, media_id: uuid::Uuid) -> Result<std::path::PathBuf, String> {
    project
        .find_media(media_id)
        .map(|m| m.path.clone())
        .ok_or_else(|| format!("media not found: {media_id}"))
}

fn parse_preset(name: &str) -> Result<ExportPreset, String> {
    match name {
        "tiktok" => Ok(ExportPreset::TikTok9x16),
        "youtube" => Ok(ExportPreset::Youtube16x9),
        other => Err(format!("unknown preset '{other}', use tiktok or youtube")),
    }
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((triple >> 18) & 63) as usize] as char);
        out.push(TABLE[((triple >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((triple >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(triple & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[tool_handler]
impl ServerHandler for RenderlyMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Renderly MCP — drive video edits through the command API. \
                 Open or create a project first, then apply_command with JSON commands \
                 (ImportMedia, AddTrack, AddClip, GenerateCaptions, GenerateVoiceover, \
                 SetAudioFade, SetTrackAudioRole, Export, …). \
                 When the Renderly desktop app is running on the same project, edits go \
                 through the live bridge (shared undo + live GUI). Use get_editor_status \
                 to check. Perception: probe_media, get_transcript, render_frame, \
                 detect_silence, detect_scenes, get_audio_peaks.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!(
                "renderly-mcp-test-{tag}-{}.renderly.json",
                std::process::id()
            ))
            .to_string_lossy()
            .into_owned()
    }

    /// Point bridge discovery at a file that never exists, once per test process, so a real
    /// desktop app running on this machine can never turn these headless tests live (the
    /// session-less tools and `get_editor_status` skip the project-path check).
    fn isolate_bridge_discovery() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            std::env::set_var(
                crate::bridge_client::DISCOVERY_ENV,
                std::env::temp_dir().join("renderly-mcp-test-no-such-bridge.json"),
            );
        });
    }

    /// `with_live_or_headless` uses `block_in_place`, which must run on a multi-thread
    /// runtime worker — spawn the sync body onto one. The session points at a temp path,
    /// so even if a real desktop bridge is running on this machine the project-path check
    /// keeps these tests headless.
    fn on_worker(f: impl FnOnce() + Send + 'static) {
        isolate_bridge_discovery();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async { tokio::spawn(async move { f() }).await.unwrap() });
    }

    #[test]
    fn parse_command_param_accepts_object_and_string() {
        let object = serde_json::json!({ "command": "AddTrack", "kind": "video", "name": "V1" });
        let as_object = parse_command_param(&object).expect("bare object parses");
        // Claude Code's MCP client sends Value params as a JSON-encoded STRING — the
        // regression this pins is that both shapes must produce the same Command.
        let as_string = parse_command_param(&serde_json::Value::String(object.to_string()))
            .expect("JSON-encoded string parses");
        assert_eq!(
            serde_json::to_value(&as_object).unwrap(),
            serde_json::to_value(&as_string).unwrap()
        );
    }

    #[test]
    fn parse_command_param_rejects_garbage() {
        assert!(parse_command_param(&serde_json::Value::String("not json".into())).is_err());
        assert!(parse_command_param(&serde_json::json!({ "command": "NoSuchCommand" })).is_err());
    }

    #[test]
    fn new_project_writes_file_and_opens_session() {
        let path = temp_path("new");
        let mcp = RenderlyMcp::new();
        let msg = mcp
            .new_project_impl(NewProjectRequest {
                path: path.clone(),
                name: "test".into(),
                width: 1920,
                height: 1080,
                fps: 30.0,
            })
            .unwrap();
        assert!(msg.contains("created"));
        let data = std::fs::read_to_string(&path).unwrap();
        let project: Project = serde_json::from_str(&data).unwrap();
        assert_eq!(project.name, "test");
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn headless_apply_command_persists_to_disk() {
        on_worker(|| {
            let path = temp_path("apply");
            let mcp = RenderlyMcp::new();
            mcp.new_project_impl(NewProjectRequest {
                path: path.clone(),
                name: "test".into(),
                width: 1280,
                height: 720,
                fps: 30.0,
            })
            .unwrap();

            let outcome = mcp
                .apply_command_impl(ApplyCommandRequest {
                    command: serde_json::json!({
                        "command": "AddTrack", "kind": "video", "name": "V1"
                    }),
                })
                .unwrap();
            assert!(!outcome.starts_with("error"));

            // The edit must be saved back to the project file (headless contract).
            let data = std::fs::read_to_string(&path).unwrap();
            let project: Project = serde_json::from_str(&data).unwrap();
            assert_eq!(project.tracks.len(), 1);
            assert_eq!(project.tracks[0].name, "V1");

            let shown = mcp.get_project_impl().unwrap();
            assert!(shown.contains("\"V1\""));
            std::fs::remove_file(&path).ok();
        });
    }

    #[test]
    fn apply_command_rejects_invalid_command_json() {
        on_worker(|| {
            let path = temp_path("invalid");
            let mcp = RenderlyMcp::new();
            mcp.new_project_impl(NewProjectRequest {
                path: path.clone(),
                name: "test".into(),
                width: 1280,
                height: 720,
                fps: 30.0,
            })
            .unwrap();
            let err = mcp
                .apply_command_impl(ApplyCommandRequest {
                    command: serde_json::json!({ "command": "NoSuchCommand" }),
                })
                .unwrap_err();
            assert!(err.contains("invalid command JSON"));
            std::fs::remove_file(&path).ok();
        });
    }

    // ---- shared helpers for the expanded suite --------------------------------------------

    /// Unique scratch directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("renderly-mcp-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn join(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
        fn project_path(&self) -> String {
            self.join("p.renderly.json").to_string_lossy().into_owned()
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    fn new_req(path: &str) -> NewProjectRequest {
        NewProjectRequest {
            path: path.to_string(),
            name: "t".into(),
            width: 1280,
            height: 720,
            fps: 30.0,
        }
    }

    fn open_new(dir: &TempDir) -> RenderlyMcp {
        let mcp = RenderlyMcp::new();
        mcp.new_project_impl(new_req(&dir.project_path())).unwrap();
        mcp
    }

    fn apply(mcp: &RenderlyMcp, command: serde_json::Value) -> Result<String, String> {
        mcp.apply_command_impl(ApplyCommandRequest { command })
    }

    fn project_of(mcp: &RenderlyMcp) -> Project {
        serde_json::from_str(&mcp.get_project_impl().unwrap()).unwrap()
    }

    fn file_project(path: &str) -> Project {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn rand_id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        let val = |c: u8| -> u32 {
            match c {
                b'A'..=b'Z' => (c - b'A') as u32,
                b'a'..=b'z' => (c - b'a') as u32 + 26,
                b'0'..=b'9' => (c - b'0') as u32 + 52,
                b'+' => 62,
                b'/' => 63,
                other => panic!("bad base64 byte {other}"),
            }
        };
        let bytes = s.as_bytes();
        assert_eq!(bytes.len() % 4, 0, "base64 length must be a multiple of 4");
        let mut out = Vec::new();
        for chunk in bytes.chunks(4) {
            let pad = chunk.iter().filter(|&&c| c == b'=').count();
            let mut n = 0u32;
            for &c in &chunk[..4 - pad] {
                n = (n << 6) | val(c);
            }
            n <<= 6 * pad as u32;
            out.push((n >> 16) as u8);
            if pad < 2 {
                out.push((n >> 8) as u8);
            }
            if pad < 1 {
                out.push(n as u8);
            }
        }
        out
    }

    /// Generate a 3 s 320x240 clip with a white-noise track that is muted from 1.0 s to 2.5 s.
    /// (Noise rather than a sine: `audio_peaks` decodes at a very low sample rate, whose
    /// resampler low-pass removes a 440 Hz tone almost entirely.)
    fn gen_clip(path: &std::path::Path) {
        let ffmpeg = media::ffmpeg_path().expect("ffmpeg path");
        let out = std::process::Command::new(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args([
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=3:size=320x240:rate=30",
            ])
            .args(["-f", "lavfi", "-i", "anoisesrc=amplitude=0.5:duration=3"])
            .args(["-af", "volume=enable='between(t,1,2.5)':volume=0"])
            .args([
                "-shortest",
                "-pix_fmt",
                "yuv420p",
                "-c:v",
                "libx264",
                "-c:a",
                "aac",
            ])
            .arg(path)
            .output()
            .expect("spawn ffmpeg");
        assert!(
            out.status.success(),
            "ffmpeg failed to generate fixture: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(path.is_file());
    }

    /// Open a new project in `dir`, import a generated clip, return (mcp, media_id string).
    fn project_with_clip(dir: &TempDir) -> (RenderlyMcp, String) {
        let clip = dir.join("clip.mp4");
        gen_clip(&clip);
        let mcp = open_new(dir);
        apply(
            &mcp,
            serde_json::json!({ "command": "ImportMedia", "path": clip.to_string_lossy() }),
        )
        .expect("import clip");
        let project = project_of(&mcp);
        assert_eq!(project.media.len(), 1);
        let id = project.media[0].id.to_string();
        (mcp, id)
    }

    fn skip_without_ffmpeg() -> bool {
        if media::ffmpeg_available() {
            return false;
        }
        eprintln!("SKIP: ffmpeg/ffprobe not available");
        true
    }

    // ---- pure helpers ---------------------------------------------------------------------

    #[test]
    fn tool_router_lists_exactly_the_documented_tools() {
        let mut names: Vec<String> = RenderlyMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        let mut expected = vec![
            "apply_command",
            "detect_scenes",
            "detect_silence",
            "export",
            "get_audio_peaks",
            "get_editor_status",
            "get_project",
            "get_transcript",
            "new_project",
            "open_project",
            "probe_media",
            "render_frame",
        ];
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn server_info_enables_tools_and_has_instructions() {
        let info = RenderlyMcp::new().get_info();
        assert!(info.capabilities.tools.is_some());
        let text = info.instructions.unwrap_or_default();
        assert!(text.contains("apply_command"));
        assert!(text.contains("get_editor_status"));
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (input, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(input.as_bytes()), expected, "input {input:?}");
        }
    }

    #[test]
    fn base64_round_trips_all_byte_values() {
        let data: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        for len in [0, 1, 2, 3, 4, 5, 255, 256, 511, 512] {
            let slice = &data[..len];
            assert_eq!(base64_decode(&base64_encode(slice)), slice, "len {len}");
        }
    }

    #[test]
    fn parse_preset_known_and_unknown() {
        assert!(matches!(
            parse_preset("tiktok"),
            Ok(ExportPreset::TikTok9x16)
        ));
        assert!(matches!(
            parse_preset("youtube"),
            Ok(ExportPreset::Youtube16x9)
        ));
        let err = parse_preset("vimeo").unwrap_err();
        assert!(err.contains("unknown preset 'vimeo'"), "{err}");
        assert!(parse_preset("").is_err());
        assert!(
            parse_preset("TikTok").is_err(),
            "preset names are case-sensitive"
        );
    }

    #[test]
    fn request_defaults_match_documented_values() {
        let n: NewProjectRequest = serde_json::from_str(r#"{"path":"x"}"#).unwrap();
        assert_eq!(
            (n.name.as_str(), n.width, n.height, n.fps),
            ("untitled", 1080, 1920, 60.0)
        );
        let e: ExportRequest = serde_json::from_str(r#"{"output_path":"o.mp4"}"#).unwrap();
        assert_eq!(e.preset, "tiktok");
        let s: SilenceDetectRequest = serde_json::from_str(r#"{"media_id":"m"}"#).unwrap();
        assert_eq!((s.noise_db, s.min_duration_secs), (-30.0, 0.5));
        let c: SceneDetectRequest = serde_json::from_str(r#"{"media_id":"m"}"#).unwrap();
        assert_eq!(c.threshold, 0.4);
        let a: AudioPeaksRequest = serde_json::from_str(r#"{"media_id":"m"}"#).unwrap();
        assert_eq!(a.buckets, 256);
        let r: RenderFrameRequest = serde_json::from_str(r#"{"time_secs":1.5}"#).unwrap();
        assert_eq!(r.preset, "tiktok");
    }

    // ---- no project open ------------------------------------------------------------------

    #[test]
    fn every_session_tool_errors_cleanly_before_a_project_is_open() {
        on_worker(|| {
            let mcp = RenderlyMcp::new();
            let id = rand_id();
            let results = [
                ("get_project", mcp.get_project()),
                (
                    "apply_command",
                    mcp.apply_command_tool(Parameters(ApplyCommandRequest {
                        command: serde_json::json!({
                            "command": "AddTrack", "kind": "video", "name": "V1"
                        }),
                    })),
                ),
                (
                    "export",
                    mcp.export(Parameters(ExportRequest {
                        output_path: "never.mp4".into(),
                        preset: "tiktok".into(),
                    })),
                ),
                (
                    "detect_silence",
                    mcp.detect_silence_tool(Parameters(SilenceDetectRequest {
                        media_id: id.clone(),
                        noise_db: -30.0,
                        min_duration_secs: 0.5,
                    })),
                ),
                (
                    "detect_scenes",
                    mcp.detect_scenes_tool(Parameters(SceneDetectRequest {
                        media_id: id.clone(),
                        threshold: 0.4,
                    })),
                ),
                (
                    "get_audio_peaks",
                    mcp.get_audio_peaks(Parameters(AudioPeaksRequest {
                        media_id: id.clone(),
                        buckets: 8,
                    })),
                ),
                (
                    "get_transcript",
                    mcp.get_transcript(Parameters(TranscriptRequest { media_id: id })),
                ),
                (
                    "render_frame",
                    mcp.render_frame(Parameters(RenderFrameRequest {
                        time_secs: 0.0,
                        preset: "tiktok".into(),
                    })),
                ),
            ];
            for (tool, out) in results {
                assert!(
                    out.starts_with("error:") && out.contains("no project open"),
                    "{tool} returned {out:?}"
                );
            }
            assert!(!std::path::Path::new("never.mp4").exists());
        });
    }

    // ---- open_project ---------------------------------------------------------------------

    #[test]
    fn open_project_missing_file_errors_and_keeps_no_session() {
        on_worker(|| {
            let dir = TempDir::new("open-missing");
            let mcp = RenderlyMcp::new();
            let out = mcp.open_project(Parameters(OpenProjectRequest {
                path: dir
                    .join("nope.renderly.json")
                    .to_string_lossy()
                    .into_owned(),
            }));
            assert!(out.starts_with("error:"), "{out}");
            assert!(mcp.get_project().contains("no project open"));
        });
    }

    #[test]
    fn open_project_invalid_json_errors() {
        on_worker(|| {
            let dir = TempDir::new("open-bad");
            let path = dir.join("bad.renderly.json");
            std::fs::write(&path, "{ definitely not a project").unwrap();
            let mcp = RenderlyMcp::new();
            let err = mcp
                .open_project_impl(OpenProjectRequest {
                    path: path.to_string_lossy().into_owned(),
                })
                .unwrap_err();
            assert!(!err.is_empty());
            assert!(mcp.get_project().contains("no project open"));
            // The bad file is not modified by a failed open.
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "{ definitely not a project"
            );
        });
    }

    #[test]
    fn open_project_valid_file_opens_session_and_get_project_returns_it() {
        on_worker(|| {
            let dir = TempDir::new("open-ok");
            let first = open_new(&dir);
            apply(
                &first,
                serde_json::json!({ "command": "AddTrack", "kind": "audio", "name": "Music" }),
            )
            .unwrap();

            // A fresh server instance (new process, in effect) opens what the first saved.
            let second = RenderlyMcp::new();
            let msg = second
                .open_project_impl(OpenProjectRequest {
                    path: dir.project_path(),
                })
                .unwrap();
            assert!(msg.starts_with("opened"));
            let project = project_of(&second);
            assert_eq!(project.name, "t");
            assert_eq!(project.tracks.len(), 1);
            assert_eq!(project.tracks[0].name, "Music");
        });
    }

    #[test]
    fn new_project_into_missing_directory_errors() {
        let dir = TempDir::new("new-bad-dir");
        let mcp = RenderlyMcp::new();
        let bad = dir.join("no-such-subdir").join("p.renderly.json");
        let out = mcp.new_project(Parameters(new_req(&bad.to_string_lossy())));
        assert!(out.starts_with("error:"), "{out}");
    }

    // ---- apply_command --------------------------------------------------------------------

    #[test]
    fn apply_command_accepts_string_encoded_command_and_persists() {
        on_worker(|| {
            let dir = TempDir::new("apply-string");
            let mcp = open_new(&dir);
            let cmd =
                serde_json::json!({ "command": "AddTrack", "kind": "caption", "name": "Subs" });
            let out = mcp.apply_command_tool(Parameters(ApplyCommandRequest {
                command: serde_json::Value::String(cmd.to_string()),
            }));
            assert!(out.contains("TrackAdded"), "{out}");
            let on_disk = file_project(&dir.project_path());
            assert_eq!(on_disk.tracks.len(), 1);
            assert_eq!(on_disk.tracks[0].name, "Subs");
        });
    }

    #[test]
    fn failing_core_command_errors_and_leaves_file_and_session_unchanged() {
        on_worker(|| {
            let dir = TempDir::new("apply-fail");
            let mcp = open_new(&dir);
            let bytes_before = std::fs::read(dir.project_path()).unwrap();
            let json_before = mcp.get_project_impl().unwrap();

            let err = apply(
                &mcp,
                serde_json::json!({
                    "command": "AddClip",
                    "track_id": rand_id(),
                    "media_id": rand_id(),
                    "position_secs": 0.0,
                    "source_in_secs": 0.0,
                    "source_out_secs": 1.0
                }),
            )
            .unwrap_err();
            assert!(err.contains("not found"), "{err}");

            assert_eq!(std::fs::read(dir.project_path()).unwrap(), bytes_before);
            assert_eq!(mcp.get_project_impl().unwrap(), json_before);

            // Through the tool wrapper the same failure is an `error:` string, not a panic.
            let out = mcp.apply_command_tool(Parameters(ApplyCommandRequest {
                command: serde_json::json!({ "command": "NoSuchCommand" }),
            }));
            assert!(out.starts_with("error: invalid command JSON"), "{out}");
            assert_eq!(std::fs::read(dir.project_path()).unwrap(), bytes_before);
        });
    }

    #[test]
    fn multi_command_sequence_accumulates_state_on_disk() {
        on_worker(|| {
            let dir = TempDir::new("apply-seq");
            let mcp = open_new(&dir);
            let track_id = rand_id();
            apply(
                &mcp,
                serde_json::json!({
                    "command": "AddTrack", "kind": "caption", "name": "Subs", "id": track_id
                }),
            )
            .unwrap();
            for (i, text) in ["hello", "world"].iter().enumerate() {
                apply(
                    &mcp,
                    serde_json::json!({
                        "command": "AddCaption",
                        "track_id": track_id,
                        "text": text,
                        "position_secs": i as f64 * 2.0,
                        "duration_secs": 1.5,
                        "style_id": "default"
                    }),
                )
                .unwrap();
            }

            // An overlapping caption is rejected and must not disturb the earlier state.
            let before = std::fs::read(dir.project_path()).unwrap();
            assert!(apply(
                &mcp,
                serde_json::json!({
                    "command": "AddCaption",
                    "track_id": track_id,
                    "text": "overlap",
                    "position_secs": 0.5,
                    "duration_secs": 1.0,
                    "style_id": "default"
                }),
            )
            .is_err());
            assert_eq!(std::fs::read(dir.project_path()).unwrap(), before);

            let on_disk = file_project(&dir.project_path());
            assert_eq!(on_disk.tracks.len(), 1);
            assert_eq!(on_disk.tracks[0].id.to_string(), track_id);
            let texts: Vec<&str> = on_disk.tracks[0]
                .clips
                .iter()
                .map(|c| match c {
                    renderly_core::project::Clip::Caption(c) => c.text.as_str(),
                    other => panic!("unexpected clip {other:?}"),
                })
                .collect();
            assert_eq!(texts, ["hello", "world"]);
            // The in-memory session agrees with disk.
            assert_eq!(project_of(&mcp).tracks[0].clips.len(), 2);
        });
    }

    // ---- media-id validation on analysis tools ---------------------------------------------

    #[test]
    fn analysis_tools_reject_invalid_and_unknown_media_ids() {
        on_worker(|| {
            let dir = TempDir::new("media-ids");
            let mcp = open_new(&dir);
            let unknown = rand_id();

            let silence = |id: &str| {
                mcp.detect_silence_tool(Parameters(SilenceDetectRequest {
                    media_id: id.into(),
                    noise_db: -30.0,
                    min_duration_secs: 0.5,
                }))
            };
            let scenes = |id: &str| {
                mcp.detect_scenes_tool(Parameters(SceneDetectRequest {
                    media_id: id.into(),
                    threshold: 0.4,
                }))
            };
            let peaks = |id: &str| {
                mcp.get_audio_peaks(Parameters(AudioPeaksRequest {
                    media_id: id.into(),
                    buckets: 16,
                }))
            };
            let transcript = |id: &str| {
                mcp.get_transcript(Parameters(TranscriptRequest {
                    media_id: id.into(),
                }))
            };

            for (tool, f) in [
                ("detect_silence", &silence as &dyn Fn(&str) -> String),
                ("detect_scenes", &scenes),
                ("get_audio_peaks", &peaks),
                ("get_transcript", &transcript),
            ] {
                let bad = f("not-a-uuid");
                assert!(
                    bad.starts_with("error: invalid media_id UUID"),
                    "{tool}: {bad}"
                );
                let missing = f(&unknown);
                assert!(missing.starts_with("error:"), "{tool}: {missing}");
                // transcribe_media checks ffmpeg before looking the media up.
                if tool != "get_transcript" || media::ffmpeg_available() {
                    assert!(
                        missing.contains("media not found") && missing.contains(&unknown),
                        "{tool}: {missing}"
                    );
                }
            }
        });
    }

    // ---- presets --------------------------------------------------------------------------

    #[test]
    fn unknown_preset_is_rejected_by_export_and_render_frame() {
        on_worker(|| {
            let dir = TempDir::new("preset");
            let mcp = open_new(&dir);
            let out = mcp.export(Parameters(ExportRequest {
                output_path: dir.join("o.mp4").to_string_lossy().into_owned(),
                preset: "vimeo".into(),
            }));
            assert!(out.starts_with("error: unknown preset 'vimeo'"), "{out}");
            assert!(!dir.join("o.mp4").exists());

            let out = mcp.render_frame(Parameters(RenderFrameRequest {
                time_secs: 0.0,
                preset: "vimeo".into(),
            }));
            assert!(out.starts_with("error: unknown preset 'vimeo'"), "{out}");
        });
    }

    #[test]
    fn export_of_empty_timeline_returns_error_not_panic() {
        on_worker(|| {
            let dir = TempDir::new("export-empty");
            let mcp = open_new(&dir);
            let out_path = dir.join("o.mp4");
            for preset in ["tiktok", "youtube"] {
                let out = mcp.export(Parameters(ExportRequest {
                    output_path: out_path.to_string_lossy().into_owned(),
                    preset: preset.into(),
                }));
                assert!(out.starts_with("error:"), "{out}");
                if media::ffmpeg_available() {
                    assert!(out.contains("no enabled video clips"), "{out}");
                }
            }
            assert!(!out_path.exists());
        });
    }

    // ---- probe_media ----------------------------------------------------------------------

    #[test]
    fn probe_media_missing_path_errors() {
        let dir = TempDir::new("probe-missing");
        let mcp = RenderlyMcp::new();
        let out = mcp.probe_media(Parameters(ProbeMediaRequest {
            path: dir.join("ghost.mp4").to_string_lossy().into_owned(),
        }));
        assert!(out.starts_with("error:"), "{out}");
    }

    #[test]
    fn probe_media_reports_generated_clip_properties() {
        if skip_without_ffmpeg() {
            return;
        }
        let dir = TempDir::new("probe-ok");
        let clip = dir.join("clip.mp4");
        gen_clip(&clip);
        let mcp = RenderlyMcp::new();
        let out = mcp
            .probe_media_impl(ProbeMediaRequest {
                path: clip.to_string_lossy().into_owned(),
            })
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["width"], 320);
        assert_eq!(v["height"], 240);
        let dur = v["duration_secs"].as_f64().unwrap();
        assert!((2.8..=3.3).contains(&dur), "duration {dur}");
    }

    // ---- ffmpeg-backed perception ---------------------------------------------------------

    #[test]
    fn audio_peaks_silence_and_scenes_on_generated_clip() {
        if skip_without_ffmpeg() {
            return;
        }
        on_worker(|| {
            let dir = TempDir::new("perceive");
            let (mcp, media_id) = project_with_clip(&dir);

            let peaks_json = mcp
                .get_audio_peaks_impl(AudioPeaksRequest {
                    media_id: media_id.clone(),
                    buckets: 30,
                })
                .unwrap();
            let peaks: serde_json::Value = serde_json::from_str(&peaks_json).unwrap();
            let arr = peaks["peaks"].as_array().unwrap();
            assert_eq!(arr.len(), 30, "requested bucket count");
            assert!(peaks["bucket_secs"].as_f64().unwrap() > 0.0);
            // The noise is audible at the start; the muted 1.0-2.5s span (buckets 11..24 of 30)
            // must be much quieter than the audible start.
            let v: Vec<f64> = arr.iter().map(|p| p.as_f64().unwrap()).collect();
            let loud = v[..8].iter().cloned().fold(0.0, f64::max);
            let muted = v[13..22].iter().cloned().fold(0.0, f64::max);
            assert!(loud > 0.01, "audible peaks too low: {loud}");
            assert!(muted < loud / 4.0, "muted {muted} vs loud {loud}");

            let silence_json = mcp
                .detect_silence_impl(SilenceDetectRequest {
                    media_id: media_id.clone(),
                    noise_db: -30.0,
                    min_duration_secs: 0.5,
                })
                .unwrap();
            let spans: serde_json::Value = serde_json::from_str(&silence_json).unwrap();
            let spans = spans.as_array().unwrap();
            assert!(!spans.is_empty(), "muted 1.0-2.5s section must be detected");
            let s = &spans[0];
            let start = s["start_secs"].as_f64().unwrap();
            let dur = s["duration_secs"].as_f64().unwrap();
            assert!((0.8..=1.3).contains(&start), "start {start}");
            assert!(dur >= 1.0, "duration {dur}");

            // A threshold above the clip length's silence yields no spans.
            let none = mcp
                .detect_silence_impl(SilenceDetectRequest {
                    media_id: media_id.clone(),
                    noise_db: -30.0,
                    min_duration_secs: 2.9,
                })
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&none).unwrap(),
                serde_json::json!([])
            );

            let scenes_json = mcp
                .detect_scenes_impl(SceneDetectRequest {
                    media_id,
                    threshold: 0.4,
                })
                .unwrap();
            assert!(serde_json::from_str::<serde_json::Value>(&scenes_json)
                .unwrap()
                .is_array());
        });
    }

    #[test]
    fn render_frame_returns_decodable_png() {
        if skip_without_ffmpeg() {
            return;
        }
        on_worker(|| {
            let dir = TempDir::new("render");
            let (mcp, media_id) = project_with_clip(&dir);
            let track_id = rand_id();
            apply(
                &mcp,
                serde_json::json!({
                    "command": "AddTrack", "kind": "video", "name": "V1", "id": track_id
                }),
            )
            .unwrap();
            apply(
                &mcp,
                serde_json::json!({
                    "command": "AddClip",
                    "track_id": track_id,
                    "media_id": media_id,
                    "position_secs": 0.0,
                    "source_in_secs": 0.0,
                    "source_out_secs": 2.0
                }),
            )
            .unwrap();

            let out = mcp.render_frame(Parameters(RenderFrameRequest {
                time_secs: 0.5,
                preset: "youtube".into(),
            }));
            if out.contains("no suitable GPU adapter") {
                eprintln!("SKIP: no GPU adapter available for render_frame");
                return;
            }
            assert!(!out.starts_with("error:"), "{out}");
            let v: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["time_secs"], 0.5);
            let png = base64_decode(v["png_base64"].as_str().unwrap());
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
            assert_eq!(v["byte_len"].as_u64().unwrap() as usize, png.len());
        });
    }

    // ---- get_editor_status ----------------------------------------------------------------

    #[test]
    fn editor_status_headless_reports_not_live_and_session_path() {
        on_worker(|| {
            let dir = TempDir::new("status");
            let mcp = RenderlyMcp::new();

            // `on_worker` points discovery at a nonexistent file, so this is deterministic
            // even when a real desktop app is running on the machine.
            let v: serde_json::Value =
                serde_json::from_str(&mcp.get_editor_status_impl().unwrap()).unwrap();
            assert_eq!(v["live"], false);
            assert!(v["project_path"].is_null());
            assert!(v["playhead"].is_null());
            assert!(v["selection"].is_null());

            mcp.new_project_impl(new_req(&dir.project_path())).unwrap();
            let v: serde_json::Value = serde_json::from_str(&mcp.get_editor_status()).unwrap();
            assert_eq!(v["live"], false);
            assert_eq!(v["project_path"], dir.project_path());
        });
    }
}
