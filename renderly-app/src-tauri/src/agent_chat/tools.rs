//! The tools the in-app agent sees. There is one implementation of each tool, here, as a plain
//! function over a [`ToolHost`]. Two callers use it:
//!
//! - the MCP server in `mcp_http.rs` (Claude Code and Codex) wraps these in rmcp handlers;
//! - the Ollama loop (`ollama.rs`) calls them directly, with no MCP in between.
//!
//! The host is the app's own state ([`AppToolHost`]) or, in tests, an in-memory project. Edits
//! go through the same `apply_commands` path the GUI uses, so they are live, emit
//! `project:changed`, and share the undo history. Nothing here is a second edit path
//! (AGENTS.md section 0.1).

use super::outline;
use super::permission::PermissionBroker;
use crate::AppState;
use renderly_core::commands::ExportPreset;
use renderly_core::project::{Id, Project};
use renderly_core::{Command, CommandOutcome};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, ContentBlock, ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler,
};
use serde::Deserialize;
use serde_json::Value;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// The MCP server name Claude Code sees; tool ids are `mcp__renderly__<tool>`.
pub const SERVER_NAME: &str = "renderly";

/// A boxed future that borrows the host for `'a`. Boxed so the trait stays object-safe.
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Everything the tools need from the open project. Implemented by the app ([`AppToolHost`])
/// and by the tests.
pub trait ToolHost: Send + Sync {
    /// The open project. A cheap `Arc` clone.
    fn project(&self) -> Result<Arc<Project>, String>;
    /// The playhead in seconds (the default time for `look`).
    fn playhead_secs(&self) -> f64;
    /// The sequence open in the timeline, if any (the default for `timeline`).
    fn active_sequence(&self) -> Option<Id>;
    /// Apply `commands` as ONE undo step. Each element is a JSON command with short ids already
    /// resolved. Returns the typed outcome of each command.
    fn apply_commands(
        &self,
        commands: Vec<Value>,
    ) -> BoxFut<'_, Result<Vec<CommandOutcome>, String>>;
    /// Render the composited frame at `time_secs` as PNG bytes. Blocking.
    fn render_png(&self, time_secs: f64, preset: ExportPreset) -> Result<Vec<u8>, String>;
}

/// The app's own state, reached through the Tauri handle.
pub struct AppToolHost {
    pub app: AppHandle,
}

impl ToolHost for AppToolHost {
    fn project(&self) -> Result<Arc<Project>, String> {
        self.app
            .state::<AppState>()
            .with_session(|s| Ok(Arc::clone(&s.project)))
    }

    fn playhead_secs(&self) -> f64 {
        self.app.state::<AppState>().playhead_secs()
    }

    fn active_sequence(&self) -> Option<Id> {
        *self.app.state::<AppState>().active_sequence.lock()
    }

    fn apply_commands(
        &self,
        commands: Vec<Value>,
    ) -> BoxFut<'_, Result<Vec<CommandOutcome>, String>> {
        let app = self.app.clone();
        Box::pin(async move {
            let state = app.state::<AppState>();
            let result = state
                .apply_commands_inner(&app, commands, None, None)
                .await?;
            Ok(result.typed_outcomes)
        })
    }

    fn render_png(&self, time_secs: f64, preset: ExportPreset) -> Result<Vec<u8>, String> {
        self.app
            .state::<AppState>()
            .render_live_frame_png(time_secs, preset)
    }
}

/// The in-process tool set served over MCP. One instance per MCP session; cheap to clone.
#[derive(Clone)]
pub struct RenderlyTools {
    host: Arc<dyn ToolHost>,
    broker: Arc<PermissionBroker>,
    #[expect(dead_code, reason = "tool_handler macro accesses tool_router")]
    tool_router: ToolRouter<Self>,
}

impl RenderlyTools {
    pub fn new(app: AppHandle, broker: Arc<PermissionBroker>) -> Self {
        Self::with_host(Arc::new(AppToolHost { app }), broker)
    }

    pub fn with_host(host: Arc<dyn ToolHost>, broker: Arc<PermissionBroker>) -> Self {
        Self {
            host,
            broker,
            tool_router: Self::tool_router(),
        }
    }
}

/// The `edit` input. `commands` is kept as raw JSON so short ids can be resolved first; its
/// schema is the real `Command` schema from renderly-core.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditRequest {
    /// One or more commands, applied in order as ONE undo step. If any fails, none are applied.
    #[schemars(schema_with = "command_list_schema")]
    pub commands: Vec<Value>,
}

fn command_list_schema(gen: &mut schemars::SchemaGenerator) -> schemars::Schema {
    gen.subschema_for::<Vec<Command>>()
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct TimelineRequest {
    /// Optional sequence id (short ids work) to outline instead of the root timeline.
    #[serde(default)]
    pub sequence_id: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct LookRequest {
    /// Time in seconds to render. Defaults to the playhead.
    #[serde(default)]
    pub time_secs: Option<f64>,
    /// Width of the returned image in pixels (default 768, never above the project width).
    #[serde(default)]
    pub max_width: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FindMediaRequest {
    /// Folder to search.
    pub dir: String,
    /// Search sub-folders too (default false).
    #[serde(default)]
    pub recursive: Option<bool>,
    /// Maximum files to list (default 50).
    #[serde(default)]
    pub max: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PermissionPromptRequest {
    pub tool_name: String,
    #[serde(default)]
    pub input: Value,
    #[serde(default)]
    pub tool_use_id: Option<String>,
}

/// A rendered frame, as `look` returns it.
pub struct LookFrame {
    /// A line naming the time and the size.
    pub caption: String,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

fn text_result(text: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(text.into())])
}

fn error_result(text: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(text.into())])
}

/// The short ids of whatever an edit created, so the next edit can refer to them.
fn created_line(outcome: &CommandOutcome) -> Option<String> {
    let s = outline::short_id;
    match outcome {
        CommandOutcome::MediaImported { media_id } => Some(format!("media {}", s(*media_id))),
        CommandOutcome::TrackAdded { track_id } => Some(format!("track {}", s(*track_id))),
        CommandOutcome::ClipAdded { clip_id } => Some(format!("clip {}", s(*clip_id))),
        CommandOutcome::ClipSplit { left_id, right_id } => {
            Some(format!("clips {} {}", s(*left_id), s(*right_id)))
        }
        CommandOutcome::CaptionsGenerated { count } => Some(format!("{count} captions")),
        CommandOutcome::VoiceoverGenerated { media_id, clip_id } => {
            Some(format!("media {} clip {}", s(*media_id), s(*clip_id)))
        }
        CommandOutcome::SequenceCreated { sequence_id, .. } => {
            Some(format!("sequence {}", s(*sequence_id)))
        }
        CommandOutcome::ClipsNested { sequence_id, .. } => {
            Some(format!("sequence {}", s(*sequence_id)))
        }
        CommandOutcome::Applied => None,
    }
}

/// A frame size for `look`: at most `max_width` and the project's own width, even dimensions.
pub fn look_size(project_w: u32, project_h: u32, max_width: u32) -> (u32, u32) {
    let w = max_width.clamp(64, project_w.max(64));
    let h = ((w as f64) * (project_h as f64) / (project_w.max(1) as f64)).round() as u32;
    (w & !1, h.max(2) & !1)
}

/// `edit`: apply commands as one undo step and summarise each one in plain language.
pub async fn run_edit(host: &dyn ToolHost, mut commands: Vec<Value>) -> Result<String, String> {
    if commands.is_empty() {
        return Err("no commands given".into());
    }
    let before = host.project()?;
    for value in commands.iter_mut() {
        outline::resolve_short_ids(value, &before)?;
    }
    let parsed: Vec<Command> = commands
        .iter()
        .map(|v| {
            serde_json::from_value::<Command>(v.clone()).map_err(|e| {
                format!(
                    "invalid command {}: {e}",
                    v.get("command").unwrap_or(&Value::Null)
                )
            })
        })
        .collect::<Result<_, _>>()?;

    let outcomes = host.apply_commands(commands).await?;
    let after = host.project()?;

    let mut lines = Vec::new();
    let mut created = Vec::new();
    for (cmd, outcome) in parsed.iter().zip(outcomes.iter()) {
        lines.push(format!(
            "- {}",
            outline::summarize(cmd, &before, &after, outcome)
        ));
        if let Some(c) = created_line(outcome) {
            created.push(c);
        }
    }
    let mut text = format!(
        "Applied {} command(s) as one undo step:\n{}",
        parsed.len(),
        lines.join("\n")
    );
    if !created.is_empty() {
        text.push_str(&format!("\nCreated: {}", created.join("; ")));
    }
    Ok(text)
}

/// `timeline`: the outline of the root timeline, or of one sequence.
pub fn run_timeline(host: &dyn ToolHost, sequence_id: Option<&str>) -> Result<String, String> {
    let project = host.project()?;
    let sequence = match sequence_id {
        Some(raw) => Some(resolve_sequence(&project, raw)?),
        None => host.active_sequence(),
    };
    Ok(outline::timeline_outline(&project, sequence))
}

/// `assets`: the effect, transition and caption catalog and the loaded packs.
pub fn run_assets(host: &dyn ToolHost) -> Result<String, String> {
    let project = host.project()?;
    Ok(outline::assets_text(&project))
}

/// `find_media`: media files in a folder. Blocking (it probes files).
pub fn run_find_media(dir: &Path, recursive: bool, max: usize) -> Result<String, String> {
    outline::find_media_text(dir, recursive, max.clamp(1, 500))
}

/// `look`: the composited frame at a time. Renders off the async runtime.
pub async fn run_look(
    host: Arc<dyn ToolHost>,
    time_secs: Option<f64>,
    max_width: Option<u32>,
) -> Result<LookFrame, String> {
    let project = host.project()?;
    let time = time_secs.unwrap_or_else(|| host.playhead_secs()).max(0.0);
    let (width, height) = look_size(
        project.settings.width,
        project.settings.height,
        max_width.unwrap_or(768),
    );
    let preset = ExportPreset::Custom {
        width,
        height,
        fps: project.settings.fps,
    };
    let png = tauri::async_runtime::spawn_blocking(move || host.render_png(time, preset))
        .await
        .map_err(|e| format!("render task failed: {e}"))?
        .map_err(|e| format!("could not render the frame: {e}"))?;
    Ok(LookFrame {
        caption: format!("Frame at {} ({width}x{height})", outline::fmt_time(time)),
        width,
        height,
        png,
    })
}

#[tool_router]
impl RenderlyTools {
    #[tool(
        description = "Apply one or more edit commands to the open project as ONE undo step. Each command is a JSON object tagged by \"command\" (see the schema). Ids may be given as the first 8 hex characters shown by timeline. Returns one plain-language line per command, plus short ids of anything created."
    )]
    async fn edit(&self, Parameters(req): Parameters<EditRequest>) -> CallToolResult {
        match run_edit(self.host.as_ref(), req.commands).await {
            Ok(text) => text_result(text),
            Err(e) => error_result(e),
        }
    }

    #[tool(
        description = "Compact outline of the open timeline: settings, tracks in z-order (index 0 is the bottom), and clips with short ids, spans, source ranges, media names, speed, effects and transitions. Use it to find ids before editing."
    )]
    async fn timeline(&self, Parameters(req): Parameters<TimelineRequest>) -> CallToolResult {
        match run_timeline(self.host.as_ref(), req.sequence_id.as_deref()) {
            Ok(text) => text_result(text),
            Err(e) => error_result(e),
        }
    }

    #[tool(
        description = "Render the composited frame at a time as a PNG image so you can see it. Defaults to the playhead, at most the project width."
    )]
    async fn look(&self, Parameters(req): Parameters<LookRequest>) -> CallToolResult {
        match run_look(Arc::clone(&self.host), req.time_secs, req.max_width).await {
            Ok(frame) => CallToolResult::success(vec![
                ContentBlock::text(frame.caption),
                ContentBlock::image(
                    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &frame.png),
                    "image/png",
                ),
            ]),
            Err(e) => error_result(e),
        }
    }

    #[tool(
        description = "List video, audio and image files in a folder with size, and duration and dimensions for the first 20. Use it to find media to import with ImportMedia."
    )]
    async fn find_media(&self, Parameters(req): Parameters<FindMediaRequest>) -> CallToolResult {
        let dir = PathBuf::from(req.dir);
        let recursive = req.recursive.unwrap_or(false);
        let max = req.max.unwrap_or(50) as usize;
        match tauri::async_runtime::spawn_blocking(move || run_find_media(&dir, recursive, max))
            .await
        {
            Ok(Ok(text)) => text_result(text),
            Ok(Err(e)) => error_result(e),
            Err(e) => error_result(format!("search failed: {e}")),
        }
    }

    #[tool(
        description = "The asset catalog: built-in effects with parameters and defaults, transition kinds, caption style ids, and loaded packs and plugins."
    )]
    async fn assets(&self) -> CallToolResult {
        match run_assets(self.host.as_ref()) {
            Ok(text) => text_result(text),
            Err(e) => error_result(e),
        }
    }

    /// Claude Code's `--permission-prompt-tool`: answers only for built-in tools (the Renderly tools
    /// are pre-allowed). Decisions come from the session's permission broker.
    #[tool(description = "Permission prompt for Claude Code built-in tools. Returns {behavior}.")]
    async fn permission_prompt(
        &self,
        Parameters(req): Parameters<PermissionPromptRequest>,
    ) -> CallToolResult {
        let decision = self
            .broker
            .decide(&req.tool_name, &req.input)
            .await
            .to_permission_json(&req.input);
        text_result(decision.to_string())
    }
}

/// Resolve a sequence id (a prefix of at least one hex digit) to its full id.
pub fn resolve_sequence(project: &Project, raw: &str) -> Result<Id, String> {
    let needle = raw.to_ascii_lowercase();
    let matches: Vec<Id> = project
        .sequences
        .iter()
        .filter(|s| s.id.to_string().starts_with(&needle))
        .map(|s| s.id)
        .collect();
    match matches.as_slice() {
        [one] => Ok(*one),
        [] => Err(format!("no sequence matches '{raw}'")),
        _ => Err(format!("sequence id '{raw}' is ambiguous")),
    }
}

#[tool_handler]
impl ServerHandler for RenderlyTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .build(),
        )
        .with_instructions(
            "Renderly editor tools. Use timeline to see the project, edit to change it, look \
                 to see a frame, find_media and assets for media and effects.",
        )
    }
}

/// The tool names this server exposes, sorted. Used by tests.
pub fn tool_names() -> Vec<String> {
    let router = RenderlyTools::tool_router();
    let mut names: Vec<String> = router
        .list_all()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_is_the_agent_tool_set() {
        assert_eq!(
            tool_names(),
            vec![
                "assets",
                "edit",
                "find_media",
                "look",
                "permission_prompt",
                "timeline"
            ]
        );
    }

    #[test]
    fn edit_schema_describes_the_real_command_enum() {
        let root = schemars::schema_for!(EditRequest);
        let text = serde_json::to_value(&root)
            .unwrap_or(Value::Null)
            .to_string();
        // Tagged enum: one oneOf branch per variant, discriminated by a "command" const.
        assert!(
            text.contains("AddCaption"),
            "AddCaption missing from the edit schema"
        );
        assert!(text.contains("\"command\""));
        assert!(text.contains("style_id"), "AddCaption fields missing");
        assert!(text.contains("oneOf"));
        eprintln!("edit schema: {} bytes", text.len());
        // Compact enough for a tool listing.
        assert!(text.len() < 60_000, "edit schema is {} bytes", text.len());
    }

    #[test]
    fn look_size_keeps_aspect_and_even_dimensions() {
        // max_width bounds the image width; height follows the project's aspect ratio.
        assert_eq!(look_size(1080, 1920, 768), (768, 1364));
        assert_eq!(look_size(1920, 1080, 768), (768, 432));
        // Never wider than the project.
        assert_eq!(look_size(640, 360, 4000), (640, 360));
    }
}
