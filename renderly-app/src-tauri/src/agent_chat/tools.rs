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
use crate::{AppState, CommandsResult};
use renderly_core::commands::ExportPreset;
use renderly_core::edit_contract::{AllowDeletion, Caller, EditError, WriteOptions};
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
    /// The project's current revision (`project_revision` in reads).
    fn revision(&self) -> u64;
    /// The playhead in µs (the default time for `look`).
    fn playhead_us(&self) -> i64;
    /// The sequence open in the timeline, if any (the default for `timeline`).
    fn active_sequence(&self) -> Option<Id>;
    /// Apply `commands` through the edit contract as ONE undo step (or check them, for a dry
    /// run). Each element is a JSON command with short ids already resolved.
    fn apply_write(
        &self,
        commands: Vec<Value>,
        options: WriteOptions,
    ) -> BoxFut<'_, Result<CommandsResult, EditError>>;
    /// Render the composited frame at `time_us` as PNG bytes. Blocking.
    fn render_png(&self, time_us: i64, preset: ExportPreset) -> Result<Vec<u8>, String>;
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

    fn revision(&self) -> u64 {
        self.app.state::<AppState>().revision_value()
    }

    fn playhead_us(&self) -> i64 {
        self.app.state::<AppState>().playhead_us()
    }

    fn active_sequence(&self) -> Option<Id> {
        *self.app.state::<AppState>().active_sequence.lock()
    }

    fn apply_write(
        &self,
        commands: Vec<Value>,
        options: WriteOptions,
    ) -> BoxFut<'_, Result<CommandsResult, EditError>> {
        let app = self.app.clone();
        Box::pin(async move {
            let state = app.state::<AppState>();
            // The in-app agent is an automated caller: the deletion guard applies.
            state
                .apply_commands_inner(&app, commands, None, None, options, Caller::Agent)
                .await
        })
    }

    fn render_png(&self, time_us: i64, preset: ExportPreset) -> Result<Vec<u8>, String> {
        self.app
            .state::<AppState>()
            .render_live_frame_png(time_us, preset)
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
    /// The project_revision from your last timeline read. If the project changed since, the edit
    /// is rejected with revision_conflict and nothing is applied.
    #[serde(default)]
    pub expected_revision: Option<u64>,
    /// Check the commands and report what would change, without changing anything.
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Idempotency key. Retrying the same request_id with the same commands returns the original
    /// result instead of applying them again.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Required to delete clips, tracks, media or sequences: a reason plus the exact full ids a
    /// dry_run lists under changed.deleted.
    #[serde(default)]
    pub allow_deletion: Option<AllowDeletionArg>,
}

/// `allow_deletion` as an agent sends it. Ids are full UUIDs (the error message lists them).
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
pub struct AllowDeletionArg {
    /// Why these deletions are intended. Must not be empty.
    pub reason: String,
    /// Every id the write would delete.
    pub ids: Vec<String>,
}

impl EditRequest {
    /// The envelope fields as the shared contract's options.
    pub fn write_options(&self) -> Result<WriteOptions, EditError> {
        options_from(
            self.expected_revision,
            self.dry_run.unwrap_or(false),
            self.request_id.clone(),
            self.allow_deletion.as_ref(),
        )
    }
}

/// The edit-contract envelope from an agent's edit arguments (shared by the MCP `edit` tool and
/// the Ollama `edit` tool).
pub fn options_from(
    expected_revision: Option<u64>,
    dry_run: bool,
    request_id: Option<String>,
    allow_deletion: Option<&AllowDeletionArg>,
) -> Result<WriteOptions, EditError> {
    let allow_deletion = match allow_deletion {
        None => None,
        Some(arg) => {
            let ids = arg
                .ids
                .iter()
                .map(|raw| {
                    Id::parse_str(raw.trim()).map_err(|e| {
                        EditError::invalid(format!(
                            "allow_deletion.ids contains '{raw}', which is not a full id: {e}"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Some(AllowDeletion {
                reason: arg.reason.clone(),
                ids,
            })
        }
    };
    Ok(WriteOptions {
        expected_revision,
        dry_run,
        request_id,
        allow_deletion,
        deprecations: Vec::new(),
    })
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
pub struct TranscriptRequest {
    /// Optional transcript id or media id (short ids work). Defaults to every transcript.
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub struct LookRequest {
    /// Timeline time to render: "1.5s", "90f" or integer microseconds. Defaults to the playhead.
    #[serde(default)]
    pub time: Option<renderly_core::time::Time>,
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

/// A contract error as a tool error: `isError` set, text is the `{code, message, hint}` JSON.
fn edit_error_result(err: &EditError) -> CallToolResult {
    error_result(serde_json::to_string_pretty(err).unwrap_or_else(|_| edit_error_text(err)))
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

/// `edit`: apply commands through the edit contract as one undo step and summarise each one in
/// plain language, with the revision, what changed, and any warnings. A dry run reports the same
/// text without changing anything.
pub async fn run_edit(
    host: &dyn ToolHost,
    mut commands: Vec<Value>,
    options: WriteOptions,
) -> Result<String, EditError> {
    if commands.is_empty() {
        return Err(EditError::invalid("no commands given"));
    }
    let before = host.project().map_err(EditError::internal)?;
    for value in commands.iter_mut() {
        outline::resolve_short_ids(value, &before).map_err(EditError::invalid)?;
    }
    let parsed: Vec<Command> = commands
        .iter()
        .map(|v| {
            serde_json::from_value::<Command>(v.clone()).map_err(|e| {
                EditError::invalid(format!(
                    "invalid command {}: {e}",
                    v.get("command").unwrap_or(&Value::Null)
                ))
            })
        })
        .collect::<Result<_, _>>()?;

    let dry_run = options.dry_run;
    let applied = host.apply_write(commands, options).await?;
    let result = &applied.write;
    let after = host.project().map_err(EditError::internal)?;

    let mut lines = Vec::new();
    let mut created = Vec::new();
    for (index, cmd) in parsed.iter().enumerate() {
        match applied.typed_outcomes.get(index) {
            Some(outcome) if result.committed => {
                lines.push(format!(
                    "- {}",
                    outline::summarize(cmd, &before, &after, outcome)
                ));
                if let Some(c) = created_line(outcome) {
                    created.push(c);
                }
            }
            _ => lines.push(format!("- {}", command_label(cmd))),
        }
    }

    let header = if result.committed {
        format!(
            "Applied {} command(s) as one undo step. project_revision is now {}.",
            parsed.len(),
            result.revision
        )
    } else if dry_run {
        format!(
            "Dry run of {} command(s): nothing was changed. project_revision is still {}.",
            parsed.len(),
            result.revision
        )
    } else {
        format!(
            "No change: the commands left the project as it was. project_revision is {}.",
            result.revision
        )
    };
    let mut text = format!("{header}\n{}", lines.join("\n"));
    if result.committed && !created.is_empty() {
        text.push_str(&format!("\nCreated: {}", created.join("; ")));
    }
    if !result.changed.is_empty() {
        text.push_str(&format!("\nChanged: {}", changed_line(&result.changed)));
    }
    for warning in &result.warnings {
        text.push_str(&format!("\nWarning: {warning}"));
    }
    if dry_run {
        text.push_str("\nSend the same commands without dry_run to apply them.");
    }
    Ok(text)
}

/// A command's name in words, e.g. `DeleteClip` -> `Delete clip`, for lines that have no outline.
fn command_label(cmd: &Command) -> String {
    let name = renderly_core::edit_contract::command_name(cmd);
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// `created`, `modified` and `deleted` as short ids, for the edit summary.
fn changed_line(changed: &renderly_core::edit_contract::Changed) -> String {
    let short = |ids: &[Id]| {
        ids.iter()
            .map(|id| outline::short_id(*id))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut parts = Vec::new();
    if !changed.created.is_empty() {
        parts.push(format!("created {}", short(&changed.created)));
    }
    if !changed.modified.is_empty() {
        parts.push(format!("modified {}", short(&changed.modified)));
    }
    if !changed.deleted.is_empty() {
        parts.push(format!("deleted {}", short(&changed.deleted)));
    }
    parts.join("; ")
}

/// A contract error as the text an agent reads: `code: message` and the hint on the next line.
pub fn edit_error_text(err: &EditError) -> String {
    format!("{}: {}\nHint: {}", err.code.as_str(), err.message, err.hint)
}

/// `timeline`: the outline of the root timeline, or of one sequence.
pub fn run_timeline(host: &dyn ToolHost, sequence_id: Option<&str>) -> Result<String, String> {
    let project = host.project()?;
    let sequence = match sequence_id {
        Some(raw) => Some(resolve_sequence(&project, raw)?),
        None => host.active_sequence(),
    };
    Ok(format!(
        "project_revision: {}\n{}",
        host.revision(),
        outline::timeline_outline(&project, sequence)
    ))
}

/// `transcript`: the project's word-level transcripts, compact (see [`outline::transcript_text`]).
pub fn run_transcript(host: &dyn ToolHost, id: Option<&str>) -> Result<String, String> {
    let project = host.project()?;
    outline::transcript_text(&project, id)
}

/// `assets`: the effect, transition and caption catalog (refs, params) and loaded plugins.
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
    time_us: Option<i64>,
    max_width: Option<u32>,
) -> Result<LookFrame, String> {
    let project = host.project()?;
    let time = time_us.unwrap_or_else(|| host.playhead_us()).max(0);
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
        description = "Apply one or more edit commands to the open project as ONE undo step. Each command is a JSON object tagged by \"command\" (see the schema). Ids may be given as the first 8 hex characters shown by timeline. Pass expected_revision from your last timeline read; use dry_run first for large or destructive edits; deleting clips, tracks, media or sequences needs allow_deletion with the exact full ids a dry run lists. Returns one plain-language line per command, the new project_revision, and what changed."
    )]
    async fn edit(&self, Parameters(req): Parameters<EditRequest>) -> CallToolResult {
        let options = match req.write_options() {
            Ok(options) => options,
            Err(e) => return edit_error_result(&e),
        };
        match run_edit(self.host.as_ref(), req.commands, options).await {
            Ok(text) => text_result(text),
            Err(e) => edit_error_result(&e),
        }
    }

    #[tool(
        description = "Compact outline of the open timeline: project_revision (pass it as expected_revision to edit), settings, tracks in z-order (index 0 is the bottom), and clips with short ids, spans, source ranges, media names, speed, effects and transitions. Use it to find ids before editing."
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
        let fps = self
            .host
            .project()
            .map(|p| p.settings.fps)
            .unwrap_or_default();
        let time_us = req.time.map(|t| t.resolve(fps));
        match run_look(Arc::clone(&self.host), time_us, req.max_width).await {
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
        description = "The project's word-level transcripts (made by GenerateCaptions): per transcript its id, media, and the words as w-<n>:word tokens with source-media times at the start of each line; ~word~ is deleted, (filler) a detection tag. Use the word ids with ApplyTranscriptCuts / RestoreTranscriptWords."
    )]
    async fn transcript(&self, Parameters(req): Parameters<TranscriptRequest>) -> CallToolResult {
        match run_transcript(self.host.as_ref(), req.id.as_deref()) {
            Ok(text) => text_result(text),
            Err(e) => error_result(e),
        }
    }

    #[tool(
        description = "The asset catalog: every effect, filter, transition and LUT you can apply by ref (builtins and installed packs) with parameter ranges and defaults, caption and text styles, and loaded plugins."
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
                "timeline",
                "transcript"
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
        assert!(text.contains("SplitCaptions"), "text commands missing");
        assert!(text.contains("oneOf"));
        eprintln!("edit schema: {} bytes", text.len());
        // Compact enough for a tool listing.
        assert!(text.len() < 60_000, "edit schema is {} bytes", text.len());
    }

    use renderly_core::edit_contract::EditErrorCode;

    /// An in-memory host that commits through the same core contract the app uses.
    struct MemHost {
        state: std::sync::Mutex<(Project, u64, renderly_core::edit_contract::WriteLedger)>,
    }

    impl MemHost {
        fn new() -> Self {
            Self {
                state: std::sync::Mutex::new((
                    Project::new("contract", renderly_core::project::Settings::default()),
                    0,
                    renderly_core::edit_contract::WriteLedger::new(),
                )),
            }
        }
    }

    impl ToolHost for MemHost {
        fn project(&self) -> Result<Arc<Project>, String> {
            Ok(Arc::new(self.state.lock().unwrap().0.clone()))
        }
        fn revision(&self) -> u64 {
            self.state.lock().unwrap().1
        }
        fn playhead_us(&self) -> i64 {
            0
        }
        fn active_sequence(&self) -> Option<Id> {
            None
        }
        fn apply_write(
            &self,
            commands: Vec<Value>,
            options: WriteOptions,
        ) -> BoxFut<'_, Result<CommandsResult, EditError>> {
            Box::pin(async move {
                let cmds: Vec<Command> = commands
                    .into_iter()
                    .map(|c| {
                        serde_json::from_value(c).map_err(|e| EditError::invalid(e.to_string()))
                    })
                    .collect::<Result<_, _>>()?;
                let mut guard = self.state.lock().unwrap();
                let (project, revision, ledger) = &mut *guard;
                let mut plan = renderly_core::edit_contract::plan_write(
                    project,
                    *revision,
                    ledger,
                    &options,
                    Caller::Agent,
                    cmds,
                )?;
                let typed = plan.outcomes().to_vec();
                let committed_at = plan.take_next().map(|next| {
                    *project = next;
                    *revision += 1;
                    *revision
                });
                let write = plan.finish(committed_at, ledger);
                Ok(CommandsResult {
                    write,
                    outcomes: Vec::new(),
                    typed_outcomes: typed,
                })
            })
        }
        fn render_png(&self, _time_us: i64, _preset: ExportPreset) -> Result<Vec<u8>, String> {
            Err("no rendering in tests".into())
        }
    }

    fn add_track(name: &str) -> Value {
        serde_json::json!({ "command": "AddTrack", "kind": "caption", "name": name })
    }

    #[test]
    fn edit_reports_the_new_revision_and_what_changed() {
        let host = MemHost::new();
        let text = tauri::async_runtime::block_on(run_edit(
            &host,
            vec![add_track("Subs")],
            WriteOptions::default(),
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(text.contains("project_revision is now 1"), "{text}");
        assert!(text.contains("Changed: created"), "{text}");
        // The warning nudges the model to send expected_revision next time.
        assert!(text.contains("Warning:"), "{text}");
        assert!(run_timeline(&host, None)
            .unwrap_or_default()
            .starts_with("project_revision: 1"));
    }

    #[test]
    fn edit_rejects_a_stale_revision_and_applies_nothing() {
        let host = MemHost::new();
        tauri::async_runtime::block_on(run_edit(
            &host,
            vec![add_track("A")],
            WriteOptions::default(),
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        let stale = WriteOptions {
            expected_revision: Some(0),
            ..WriteOptions::default()
        };
        let err = tauri::async_runtime::block_on(run_edit(&host, vec![add_track("B")], stale))
            .err()
            .unwrap_or_else(|| panic!("stale edit was accepted"));
        assert_eq!(err.code, EditErrorCode::RevisionConflict);
        assert_eq!(host.project().map(|p| p.tracks.len()).unwrap_or(0), 1);
        assert_eq!(host.revision(), 1);
    }

    #[test]
    fn dry_run_edit_says_nothing_changed() {
        let host = MemHost::new();
        let dry = WriteOptions {
            dry_run: true,
            ..WriteOptions::default()
        };
        let text = tauri::async_runtime::block_on(run_edit(&host, vec![add_track("Preview")], dry))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(text.contains("nothing was changed"), "{text}");
        assert!(text.contains("project_revision is still 0"), "{text}");
        assert_eq!(host.project().map(|p| p.tracks.len()).unwrap_or(1), 0);
    }

    #[test]
    fn agent_deletion_is_refused_until_allowed_with_exact_ids() {
        let host = MemHost::new();
        tauri::async_runtime::block_on(run_edit(
            &host,
            vec![add_track("Trash")],
            WriteOptions::default(),
        ))
        .unwrap_or_else(|e| panic!("{e}"));
        let track = host.project().map(|p| p.tracks[0].id).unwrap_or_default();
        let delete = serde_json::json!({ "command": "DeleteTrack", "track_id": track });

        let err = tauri::async_runtime::block_on(run_edit(
            &host,
            vec![delete.clone()],
            WriteOptions::default(),
        ))
        .err()
        .unwrap_or_else(|| panic!("deletion without allow_deletion was accepted"));
        assert_eq!(err.code, EditErrorCode::DeletionNotAllowed);
        assert!(edit_error_text(&err).contains("deletion_not_allowed"));

        let allowed = options_from(
            None,
            false,
            None,
            Some(&AllowDeletionArg {
                reason: "cleanup".into(),
                ids: vec![track.to_string()],
            }),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        tauri::async_runtime::block_on(run_edit(&host, vec![delete], allowed))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(host.project().map(|p| p.tracks.is_empty()).unwrap_or(false));
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
