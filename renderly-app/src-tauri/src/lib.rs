mod media_assets;
mod ml;
mod playback;
mod preview_assets;

mod bridge;

use parking_lot::Mutex;
use playback::PlaybackEngine;
use renderly_core::{
    apply_command as apply_core_command, commands::ExportPreset, encode_rgba_png,
    export_project_with_settings, project::Project, Command, CommandOutcome, DecodeOptions,
    ExportError, ExportPhase, ExportProgress, ExportSettings, FrameRenderer,
    VideoEncoderPreference,
};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

/// P1 webview preview migration (docs/preview-webview.md item 11): the webview loads
/// media files directly (`<video src>`/`<img src>` via `convertFileSrc`), so every media
/// item's real filesystem path must be allow-listed on the Tauri asset protocol scope —
/// it starts out scoped to only `$APPCACHE/media-cache/*` (see tauri.conf.json). Called
/// whenever the session's `Project` is set or gains media (open/create/quick-start,
/// import). Errors are logged, not propagated — a missing scope entry means "that one
/// asset won't load," not "the project failed to open."
fn allow_media_assets<'a>(app: &AppHandle, paths: impl Iterator<Item = &'a std::path::Path>) {
    let scope = app.asset_protocol_scope();
    for path in paths {
        if let Err(e) = scope.allow_file(path) {
            eprintln!("asset protocol: failed to allow {}: {e}", path.display());
        }
    }
}

/// Generated thumbnail strips/waveform-adjacent files now live in a project-local sidecar
/// directory (`media_assets::assets_dir_for_project`) rather than only the single
/// `$APPCACHE/media-cache` dir statically scoped in tauri.conf.json, so that directory must
/// be allow-listed too, recursively (new files land in it continuously as generation
/// completes). Idempotent — safe to call on every project open/create/quick-start and every
/// import, same as `allow_media_assets`.
fn allow_media_assets_dir(app: &AppHandle, dir: &std::path::Path) {
    let scope = app.asset_protocol_scope();
    if let Err(e) = scope.allow_directory(dir, true) {
        eprintln!("asset protocol: failed to allow dir {}: {e}", dir.display());
    }
}

struct Session {
    path: PathBuf,
    /// Shared snapshot bumped on every commit — play/seek/refresh take `Arc::clone`
    /// instead of deep-cloning the whole `Project` (improvement-plan B3).
    project: Arc<Project>,
}

/// Undo/redo stack over full `Project` snapshots — session-layer state management, not a
/// second edit path: every entry is a project state that only ever arose from a successful
/// `apply_command` call (or a prior undo/redo), so `apply_command` remains the sole way a
/// project's *contents* change. See docs/architecture.md "Undo/redo" for the full
/// rationale required by AGENTS.md. Entries are `Arc` so undoing reuses the pre-edit
/// snapshot without an extra deep clone at push time.
struct History {
    undo: Vec<Arc<Project>>,
    redo: Vec<Arc<Project>>,
}

const HISTORY_CAP: usize = 100;

impl History {
    fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Push onto the bounded undo stack, evicting the oldest entry past `HISTORY_CAP`.
    fn push_undo_bounded(&mut self, project: Arc<Project>) {
        self.undo.push(project);
        if self.undo.len() > HISTORY_CAP {
            self.undo.remove(0);
        }
    }

    /// Push a pre-mutation snapshot and drop the (now-stale) redo branch — call this for
    /// a genuinely new edit, not for `redo()`'s own bookkeeping (see `push_undo_bounded`).
    fn push_undo(&mut self, project: Arc<Project>) {
        self.push_undo_bounded(project);
        self.redo.clear();
    }

    fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    fn status(&self) -> HistoryStatus {
        HistoryStatus {
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(crate) struct HistoryStatus {
    pub can_undo: bool,
    pub can_redo: bool,
}

/// Result of a single `apply_command` — revision + RFC-6902 patch so the frontend can
/// update without a full `get_project` round-trip (improvement-plan B3). App IPC only;
/// not part of the core `Command` enum.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CommandResult {
    pub revision: u64,
    pub patch: json_patch::Patch,
    pub outcome: String,
}

/// Result of an atomic `apply_commands` batch (one undo step, one patch).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CommandsResult {
    pub revision: u64,
    pub patch: json_patch::Patch,
    pub outcomes: Vec<String>,
}

/// Undo/redo response — same history flags as before, plus revision + patch (B3).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct HistoryResult {
    pub can_undo: bool,
    pub can_redo: bool,
    pub revision: u64,
    pub patch: json_patch::Patch,
}

/// Full project fetch with the session revision counter (so the client can set
/// `clientRevision` without waiting for a `project:changed` event).
#[derive(Debug, Clone, Serialize)]
struct ProjectSnapshot {
    project: Project,
    revision: u64,
}

fn empty_patch() -> json_patch::Patch {
    json_patch::Patch(Vec::new())
}

fn project_json(project: &Project) -> Result<serde_json::Value, String> {
    serde_json::to_value(project).map_err(|e| format!("serialize project: {e}"))
}

#[derive(Debug, Clone, serde::Serialize)]
struct ProjectChanged {
    revision: u64,
    can_undo: bool,
    can_redo: bool,
    /// Echoes the `mutation_id` the frontend passed into `apply_command`/`apply_commands`/
    /// `undo`/`redo`. The frontend applies the patch from the invoke result itself (B3);
    /// without this echo, the `project:changed` listener had no way to tell "this is the
    /// event for the edit I just made" apart from "an external writer (MCP, another
    /// window) changed the project" and would full-fetch again for every edit — see
    /// improvement-plan B2. `None` for edits with no client-supplied id (bridge/MCP).
    mutation_id: Option<String>,
}

pub struct AppState {
    session: Mutex<Option<Session>>,
    pub(crate) playback: PlaybackEngine,
    history: Mutex<History>,
    revision: AtomicU64,
    /// Cooperative cancel flag for the in-flight export (M6). Cleared at export start;
    /// `cancel_export` sets it so the progress callback returns `false`.
    pub(crate) export_cancel: Arc<AtomicBool>,
    /// Cancel flag / single-flight guard of the in-flight `rvm` background-matte bake (see
    /// `ml.rs`); the bake runs off the project lock.
    pub(crate) matte_cancel: Arc<AtomicBool>,
    pub(crate) matte_running: AtomicBool,
    /// Serializes every whole-project mutation (`apply_command`, `apply_commands`, `undo`,
    /// `redo`, and project open/create/quick-start) end-to-end — snapshot, compute,
    /// history push, session write-back, and save all happen while this is held. Without
    /// it, two overlapping calls (e.g. a double-tapped Ctrl+Z firing two `undo` invokes
    /// before the first resolves) could each read the same pre-mutation project, race on
    /// which write-back lands last, and silently corrupt the undo/redo stacks. A
    /// `tokio`-backed async mutex (via `tauri::async_runtime`) rather than `parking_lot`,
    /// since it must be held across `.await` points (the `spawn_blocking` compute step).
    edit_lock: tauri::async_runtime::Mutex<()>,
    /// Last known playhead (secs) for bridge `get_editor_status` / seek bookkeeping.
    playhead: Mutex<f64>,
    /// Frontend selection mirrored for bridge status (`{ primary, all }` or null).
    selection: Mutex<Option<serde_json::Value>>,
    /// Sequence open in the frontend timeline (schema v7). While set, audio playback and
    /// scrub audio render that sequence's mix instead of the root timeline's; see
    /// `AppState::playback_project`.
    active_sequence: Mutex<Option<renderly_core::project::Id>>,
}

impl AppState {
    fn new() -> Self {
        Self {
            session: Mutex::new(None),
            playback: PlaybackEngine::new(),
            history: Mutex::new(History::new()),
            revision: AtomicU64::new(0),
            export_cancel: Arc::new(AtomicBool::new(false)),
            matte_cancel: Arc::new(AtomicBool::new(false)),
            matte_running: AtomicBool::new(false),
            edit_lock: tauri::async_runtime::Mutex::new(()),
            playhead: Mutex::new(0.0),
            selection: Mutex::new(None),
            active_sequence: Mutex::new(None),
        }
    }

    pub(crate) fn playhead_secs(&self) -> f64 {
        *self.playhead.lock()
    }

    pub(crate) fn set_playhead_secs(&self, t: f64) {
        *self.playhead.lock() = t.max(0.0);
    }

    pub(crate) fn selection_json(&self) -> Option<serde_json::Value> {
        self.selection.lock().clone()
    }

    pub(crate) fn set_selection_json(&self, value: Option<serde_json::Value>) {
        *self.selection.lock() = value;
    }

    pub(crate) fn revision_value(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    /// E3: render a PNG of the live session project via its own `FrameRenderer` (headless,
    /// independent of playback/preview — the webview owns preview rendering entirely, so
    /// this is the MCP live-agent bridge's only render path; see `bridge.rs`'s
    /// `render_frame` handler). Renders at `preset_for_aspect`'s export resolution; the P4
    /// removal of the native preview surface also removed the preview-panel-size decode
    /// hint this used to honor (`PlaybackEngine::target_height`), so this now always
    /// renders at full preset resolution.
    pub(crate) fn render_live_frame_png(
        &self,
        time_secs: f64,
        preset_for_aspect: ExportPreset,
    ) -> Result<Vec<u8>, String> {
        let project = self.with_session(|s| Ok(Arc::clone(&s.project)))?;
        let settings = ExportSettings::from_preset(&preset_for_aspect, &project);
        let decode_opts = DecodeOptions {
            target_height: None,
            output_fps: None,
        };
        let mut renderer = FrameRenderer::new(settings, decode_opts).map_err(|e| e.to_string())?;
        let rgba = renderer
            .render(&project, time_secs)
            .map_err(|e| e.to_string())?;
        encode_rgba_png(&rgba, settings.width, settings.height).map_err(|e| e.to_string())
    }

    /// Shared by the Tauri `apply_command` handler and the live agent bridge.
    pub(crate) async fn apply_command_inner(
        &self,
        app: &AppHandle,
        command: serde_json::Value,
        mutation_id: Option<String>,
    ) -> Result<CommandResult, String> {
        // The in-process `rvm` matte bake takes minutes: run it off the project lock with
        // progress + cancel, and only apply its small result through the edit lock below.
        if let Some((track_id, clip_id)) = ml::rvm_matte_target(self, &command) {
            // Boxed: `generate_rvm_matte` calls back into this function for the attach step.
            return Box::pin(self.generate_rvm_matte(app, track_id, clip_id, mutation_id)).await;
        }
        let _edit_guard = self.edit_lock.lock().await;
        let cmd: Command =
            serde_json::from_value(command).map_err(|e| format!("invalid command: {e}"))?;
        let before_arc = self.with_session(|s| Ok(Arc::clone(&s.project)))?;
        let before_json = project_json(&before_arc)?;
        let mut project = (*before_arc).clone();

        let denoise_prewarm = match &cmd {
            Command::SetClipAudioDenoise {
                track_id,
                clip_id,
                config: Some(cfg),
            } if cfg.enabled && cfg.backend == "rnnoise" => {
                Some((*track_id, *clip_id, cfg.strength))
            }
            _ => None,
        };

        let (outcome, project) = tauri::async_runtime::spawn_blocking(move || {
            let outcome = apply_core_command(&mut project, cmd);
            (outcome, project)
        })
        .await
        .map_err(|e| e.to_string())?;
        let outcome = outcome.map_err(|e| e.to_string())?;

        // RNNoise denoise is rendered into a cache the first time it is mixed; start that
        // now, off the UI/command path, so the next playback or export finds it ready.
        if let Some((track_id, clip_id, strength)) = denoise_prewarm {
            let source = project
                .find_track(track_id)
                .and_then(|t| t.clips.iter().find(|c| c.id() == clip_id))
                .and_then(|c| c.as_media())
                .and_then(|c| project.find_media(c.media_id))
                .map(|m| m.path.clone());
            if let Some(path) = source {
                let strength = strength.clamp(0.0, 1.0);
                tauri::async_runtime::spawn_blocking(move || {
                    let _ = renderly_core::audio::denoise::denoised_wav_cached(&path, strength);
                });
            }
        }

        if let CommandOutcome::MediaImported { media_id } = &outcome {
            if let Some(item) = project.find_media(*media_id) {
                let session_path = self.with_session(|s| Ok(s.path.clone())).ok();
                let assets_dir = media_assets::resolve_cache_dir(app, session_path.as_deref());
                if let Ok(dir) = &assets_dir {
                    allow_media_assets_dir(app, dir);
                }
                if let Ok(dir) = assets_dir {
                    media_assets::request_assets(
                        app.clone(),
                        media_id.to_string(),
                        item.path.clone(),
                        item.kind,
                        dir,
                    );
                }
                allow_media_assets(app, std::iter::once(item.path.as_path()));
            }
        }

        let after_json = project_json(&project)?;
        let patch = json_patch::diff(&before_json, &after_json);
        self.history.lock().push_undo(before_arc);
        self.commit_project(Arc::new(project)).await?;
        let revision = self.emit_project_changed(app, mutation_id);
        Ok(CommandResult {
            revision,
            patch,
            outcome: format!("{outcome:?}"),
        })
    }

    pub(crate) async fn apply_commands_inner(
        &self,
        app: &AppHandle,
        commands: Vec<serde_json::Value>,
        mutation_id: Option<String>,
    ) -> Result<CommandsResult, String> {
        let _edit_guard = self.edit_lock.lock().await;
        let cmds: Vec<Command> = commands
            .into_iter()
            .map(|c| serde_json::from_value(c).map_err(|e| format!("invalid command: {e}")))
            .collect::<Result<_, _>>()?;

        let voiceover_paths: Vec<Option<PathBuf>> = cmds
            .iter()
            .map(|c| match c {
                Command::GenerateVoiceover { output_path, .. } => Some(PathBuf::from(output_path)),
                _ => None,
            })
            .collect();

        let before_arc = self.with_session(|s| Ok(Arc::clone(&s.project)))?;
        let before_json = project_json(&before_arc)?;
        let mut project = (*before_arc).clone();

        let (result, project) = tauri::async_runtime::spawn_blocking(move || {
            let mut outcomes = Vec::with_capacity(cmds.len());
            for cmd in cmds {
                match apply_core_command(&mut project, cmd) {
                    Ok(outcome) => outcomes.push(outcome),
                    Err(e) => return (Err((outcomes.len(), e.to_string())), project),
                }
            }
            (Ok(outcomes), project)
        })
        .await
        .map_err(|e| e.to_string())?;

        let outcomes = match result {
            Ok(outcomes) => outcomes,
            Err((succeeded, message)) => {
                for path in voiceover_paths.iter().take(succeeded).flatten() {
                    let _ = std::fs::remove_file(path);
                }
                return Err(message);
            }
        };

        let after_json = project_json(&project)?;
        let patch = json_patch::diff(&before_json, &after_json);
        self.history.lock().push_undo(before_arc);
        self.commit_project(Arc::new(project)).await?;
        let revision = self.emit_project_changed(app, mutation_id);
        Ok(CommandsResult {
            revision,
            patch,
            outcomes: outcomes.into_iter().map(|o| format!("{o:?}")).collect(),
        })
    }

    pub(crate) async fn undo_inner(
        &self,
        app: &AppHandle,
        mutation_id: Option<String>,
    ) -> Result<HistoryResult, String> {
        let _edit_guard = self.edit_lock.lock().await;
        let popped = self.history.lock().undo.pop();
        let Some(prev) = popped else {
            let status = self.history.lock().status();
            return Ok(HistoryResult {
                can_undo: status.can_undo,
                can_redo: status.can_redo,
                revision: self.revision_value(),
                patch: empty_patch(),
            });
        };

        let current = self.with_session(|s| Ok(Arc::clone(&s.project)))?;
        let before_json = project_json(&current)?;
        let after_json = project_json(&prev)?;
        let patch = json_patch::diff(&before_json, &after_json);
        self.history.lock().redo.push(current);
        self.commit_project(prev).await?;
        let revision = self.emit_project_changed(app, mutation_id);
        let status = self.history.lock().status();
        Ok(HistoryResult {
            can_undo: status.can_undo,
            can_redo: status.can_redo,
            revision,
            patch,
        })
    }

    pub(crate) async fn redo_inner(
        &self,
        app: &AppHandle,
        mutation_id: Option<String>,
    ) -> Result<HistoryResult, String> {
        let _edit_guard = self.edit_lock.lock().await;
        let popped = self.history.lock().redo.pop();
        let Some(next) = popped else {
            let status = self.history.lock().status();
            return Ok(HistoryResult {
                can_undo: status.can_undo,
                can_redo: status.can_redo,
                revision: self.revision_value(),
                patch: empty_patch(),
            });
        };

        let current = self.with_session(|s| Ok(Arc::clone(&s.project)))?;
        let before_json = project_json(&current)?;
        let after_json = project_json(&next)?;
        let patch = json_patch::diff(&before_json, &after_json);
        self.history.lock().push_undo_bounded(current);
        self.commit_project(next).await?;
        let revision = self.emit_project_changed(app, mutation_id);
        let status = self.history.lock().status();
        Ok(HistoryResult {
            can_undo: status.can_undo,
            can_redo: status.can_redo,
            revision,
            patch,
        })
    }

    /// Clone/mutate the project under a short-lived lock only — never hold this across
    /// file I/O, media decode, or other blocking work (see docs/architecture.md
    /// "Playback engine" for why the old sync-command design froze the UI thread).
    /// The project snapshot playback should use: the session's project, or — when the
    /// frontend has a nested sequence open — a copy whose root timeline is that sequence
    /// (`Project::with_sequence_as_root`), so audio follows the open container. A sequence
    /// that no longer exists falls back to the root.
    pub(crate) fn playback_project(&self) -> Result<Arc<Project>, String> {
        let project = self.with_session(|s| Ok(Arc::clone(&s.project)))?;
        let active = *self.active_sequence.lock();
        Ok(
            match active.and_then(|id| project.with_sequence_as_root(id)) {
                Some(view) => Arc::new(view),
                None => project,
            },
        )
    }

    pub(crate) fn with_session<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&mut Session) -> Result<T, String>,
    {
        let mut guard = self.session.lock();
        let session = guard
            .as_mut()
            .ok_or_else(|| "no project open".to_string())?;
        f(session)
    }

    /// Write `project` into the session and persist it to disk. The `session` lock is
    /// held only long enough to swap in the new `Arc` and read the save path — the
    /// actual (blocking) serialize+write runs in `spawn_blocking`, outside any lock. This
    /// used to run `std::fs::write` synchronously while still holding `session`'s lock
    /// (directly contradicting `with_session`'s own documented invariant above), which
    /// stalled every other session-locking command (`play`/`seek`/`scrub_audio`/
    /// `get_project`) behind a disk write on every single edit.
    async fn commit_project(&self, project: Arc<Project>) -> Result<(), String> {
        let path = {
            let mut guard = self.session.lock();
            let session = guard
                .as_mut()
                .ok_or_else(|| "no project open".to_string())?;
            let path = session.path.clone();
            session.project = Arc::clone(&project);
            path
        };
        tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
            let data = serde_json::to_string_pretty(&*project).map_err(|e| e.to_string())?;
            std::fs::write(&path, data).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }

    /// Bump the revision counter, emit `project:changed`, and return the new revision
    /// (every mutating command, undo, and redo call this once).
    fn emit_project_changed(&self, app: &AppHandle, mutation_id: Option<String>) -> u64 {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let status = self.history.lock().status();
        let _ = app.emit(
            "project:changed",
            ProjectChanged {
                revision,
                can_undo: status.can_undo,
                can_redo: status.can_redo,
                mutation_id,
            },
        );
        revision
    }

    /// Reset the revision counter on open/close/create so the frontend treats the new
    /// session as a full load (clientRevision starts from the subsequent emit / get_project).
    fn reset_revision(&self) {
        self.revision.store(0, Ordering::SeqCst);
    }
}

/// Stops any active playback session, joining its worker thread from `spawn_blocking`
/// rather than the calling async command's own worker thread. `PlaybackEngine::stop()`
/// can block for as long as an in-flight audio premix takes (multi-second, on a long
/// timeline) if called right after `play()` starts — running that wait inline in an async
/// command handler stalls whichever tokio worker thread is executing it, and everything
/// else queued behind that same worker.
async fn stop_playback_blocking(app: &AppHandle) {
    let app = app.clone();
    let _ = tauri::async_runtime::spawn_blocking(move || {
        app.state::<AppState>().playback.stop();
    })
    .await;
}

fn default_projects_dir() -> Result<PathBuf, String> {
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE")
    } else {
        std::env::var("HOME")
    }
    .map_err(|e| format!("home directory: {e}"))?;
    Ok(PathBuf::from(home).join("Documents").join("Renderly"))
}

/// `foo.renderly.json` → `Some("foo")`. Mirrors `project_thumb_path`'s stripping (strip the
/// full `.renderly.json` suffix, not just the last extension — `Path::file_stem` would leave
/// a trailing `.renderly` on a name like `Jul 18, 2026.renderly.json`).
fn project_name_stem(file_name: &str) -> Option<&str> {
    file_name.strip_suffix(".renderly.json")
}

/// CapCut-style default project name: the current local date, e.g. `Jul 18, 2026`. Windows-
/// safe (no colons/slashes) since it's comma+space punctuated only.
fn today_date_label() -> String {
    chrono::Local::now().format("%b %-d, %Y").to_string()
}

/// Pure next-free-name logic (B1): given the set of existing project-file name stems (i.e.
/// `foo.renderly.json` minus the `.renderly.json` suffix, see `project_name_stem`) and
/// today's date label, pick `date`, or `date (2)`, `date (3)`, … — the first suffix not
/// already present. Scans sequentially from 2 so it fills gaps (e.g. if `(2)` and `(4)`
/// exist but `(3)` doesn't, `(3)` is returned) rather than always appending past the max.
/// Fs-free and side-effect-free so it's unit-testable without touching disk.
fn next_free_project_name(
    existing_stems: &std::collections::HashSet<String>,
    date: &str,
) -> String {
    if !existing_stems.contains(date) {
        return date.to_string();
    }
    let mut n = 2u32;
    loop {
        let candidate = format!("{date} ({n})");
        if !existing_stems.contains(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Scan `dir` for `*.renderly.json` files and return their name stems (see
/// `project_name_stem`). Missing dir reads as "no existing projects" rather than an error —
/// mirrors `list_projects`' own `!dir.is_dir()` early-return.
fn existing_project_name_stems(dir: &std::path::Path) -> std::collections::HashSet<String> {
    let mut stems = std::collections::HashSet::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return stems;
    };
    for entry in entries.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if let Some(stem) = project_name_stem(name) {
                stems.insert(stem.to_string());
            }
        }
    }
    stems
}

#[tauri::command]
async fn quick_start_project(app: AppHandle, state: State<'_, AppState>) -> Result<String, String> {
    use renderly_core::project::Settings;

    let _edit_guard = state.edit_lock.lock().await;
    stop_playback_blocking(&app).await;

    let dir = default_projects_dir()?;
    let existing = existing_project_name_stems(&dir);
    let name = next_free_project_name(&existing, &today_date_label());
    let path_buf = dir.join(format!("{name}.renderly.json"));
    let project = Project::new(
        name,
        Settings {
            fps: 60.0,
            width: 1080,
            height: 1920,
            sample_rate: 48000,
            duck_db: -12.0,
        },
    );

    let write_path = path_buf.clone();
    let write_project = project.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let data = serde_json::to_string_pretty(&write_project).map_err(|e| e.to_string())?;
        std::fs::write(&write_path, data).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    if let Ok(dir) = media_assets::resolve_cache_dir(&app, Some(path_buf.as_path())) {
        allow_media_assets_dir(&app, &dir);
    }

    *state.session.lock() = Some(Session {
        path: path_buf.clone(),
        project: Arc::new(project),
    });
    state.history.lock().clear();
    state.reset_revision();
    state.emit_project_changed(&app, None);
    bridge::sync_discovery_project(&app, Some(path_buf.clone()));
    Ok(path_buf.to_string_lossy().into_owned())
}

#[tauri::command]
async fn new_project(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
    name: String,
) -> Result<(), String> {
    use renderly_core::project::Settings;

    let _edit_guard = state.edit_lock.lock().await;
    stop_playback_blocking(&app).await;

    let path_buf = PathBuf::from(&path);
    let project = Project::new(
        name,
        Settings {
            fps: 60.0,
            width: 1080,
            height: 1920,
            sample_rate: 48000,
            duck_db: -12.0,
        },
    );

    let write_path = path_buf.clone();
    let write_project = project.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let data = serde_json::to_string_pretty(&write_project).map_err(|e| e.to_string())?;
        std::fs::write(&write_path, data).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())??;

    if let Ok(dir) = media_assets::resolve_cache_dir(&app, Some(path_buf.as_path())) {
        allow_media_assets_dir(&app, &dir);
    }

    *state.session.lock() = Some(Session {
        path: path_buf.clone(),
        project: Arc::new(project),
    });
    state.history.lock().clear();
    state.reset_revision();
    state.emit_project_changed(&app, None);
    bridge::sync_discovery_project(&app, Some(path_buf));
    Ok(())
}

#[tauri::command]
async fn open_project(
    app: AppHandle,
    state: State<'_, AppState>,
    path: String,
) -> Result<(), String> {
    let _edit_guard = state.edit_lock.lock().await;
    stop_playback_blocking(&app).await;

    let path_buf = PathBuf::from(&path);
    let read_path = path_buf.clone();
    let project: Project =
        tauri::async_runtime::spawn_blocking(move || -> Result<Project, String> {
            let data = std::fs::read_to_string(&read_path).map_err(|e| e.to_string())?;
            Project::from_json(&data).map_err(|e| format!("{}: {e}", read_path.display()))
        })
        .await
        .map_err(|e| e.to_string())??;

    // Kick off (cache-hit-cheap) asset generation for every media item already in this
    // project — not just newly-imported ones — so reopening a project shows filmstrips/
    // waveforms without the user re-triggering anything. Resolved once per open (not per
    // item): the project-local sidecar dir next to `path_buf`, see
    // `media_assets::resolve_cache_dir`.
    let assets_dir = media_assets::resolve_cache_dir(&app, Some(path_buf.as_path()))?;
    allow_media_assets_dir(&app, &assets_dir);
    for item in &project.media {
        media_assets::request_assets(
            app.clone(),
            item.id.to_string(),
            item.path.clone(),
            item.kind,
            assets_dir.clone(),
        );
    }
    allow_media_assets(
        &app,
        project
            .media
            .iter()
            .filter(|m| m.kind != renderly_core::project::MediaKind::Sequence)
            .map(|m| m.path.as_path()),
    );

    *state.session.lock() = Some(Session {
        path: path_buf.clone(),
        project: Arc::new(project),
    });
    state.history.lock().clear();
    state.reset_revision();
    state.emit_project_changed(&app, None);
    bridge::sync_discovery_project(&app, Some(path_buf));
    Ok(())
}

#[tauri::command]
async fn save_project(state: State<'_, AppState>) -> Result<(), String> {
    let (path, project) = state.with_session(|s| Ok((s.path.clone(), Arc::clone(&s.project))))?;
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let data = serde_json::to_string_pretty(&*project).map_err(|e| e.to_string())?;
        std::fs::write(&path, data).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// G1: CapCut-style home gallery card.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectSummary {
    path: String,
    name: String,
    duration_secs: f64,
    modified_ms: u64,
    width: u32,
    height: u32,
    fps: f64,
    /// Absolute filesystem path to a cached PNG thumb (frontend turns into asset URL).
    thumb_path: Option<String>,
}

fn project_thumb_path(project_path: &std::path::Path) -> PathBuf {
    let mut p = project_path.to_path_buf();
    // `foo.renderly.json` → `foo.renderly.thumb.png`
    let stem = project_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    p.set_file_name(format!("{stem}.thumb.png"));
    p
}

#[tauri::command]
async fn list_projects(app: AppHandle) -> Result<Vec<ProjectSummary>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Vec<ProjectSummary>, String> {
        let dir = default_projects_dir()?;
        // Gallery-card thumbs live next to the project files in the projects dir — outside
        // the statically scoped $APPCACHE/media-cache — so the asset protocol must be told
        // about them or the webview's <img> fetch 403s and cards render blank.
        allow_media_assets_dir(&app, &dir);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if !name.ends_with(".renderly.json") {
                continue;
            }
            let meta = entry.metadata().map_err(|e| e.to_string())?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let project: Project =
                serde_json::from_str(&data).map_err(|e| format!("{}: {e}", path.display()))?;
            let thumb = project_thumb_path(&path);
            out.push(ProjectSummary {
                path: path.to_string_lossy().into_owned(),
                name: project.name.clone(),
                duration_secs: renderly_core::timeline_duration(&project),
                modified_ms,
                width: project.settings.width,
                height: project.settings.height,
                fps: project.settings.fps,
                thumb_path: thumb
                    .is_file()
                    .then(|| thumb.to_string_lossy().into_owned()),
            });
        }
        out.sort_by_key(|b| std::cmp::Reverse(b.modified_ms));
        Ok(out)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn rename_project(path: String, new_name: String) -> Result<(), String> {
    let name = new_name.trim().to_string();
    if name.is_empty() {
        return Err("project name cannot be empty".into());
    }
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let path = PathBuf::from(&path);
        let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut project = Project::from_json(&data).map_err(|e| e.to_string())?;
        project.name = name;
        let out = serde_json::to_string_pretty(&project).map_err(|e| e.to_string())?;
        std::fs::write(&path, out).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn delete_project(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let path = PathBuf::from(&path);
        let thumb = project_thumb_path(&path);
        let assets_dir = media_assets::assets_dir_for_project(&path);
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(thumb);
        // Generated thumbnail-strip/waveform sidecar assets are project-local (see
        // media_assets module doc) and useless once the project is gone.
        let _ = std::fs::remove_dir_all(assets_dir);
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn project_thumbnail(app: AppHandle, path: String) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<String>, String> {
        let path = PathBuf::from(&path);
        let thumb = project_thumb_path(&path);
        // See list_projects: the thumb lives outside the static asset-protocol scope.
        // Allowed here too (not just there) so a freshly (re)generated thumb loads even if
        // this command ever runs before/without a list_projects call.
        allow_media_assets(&app, std::iter::once(thumb.as_path()));
        // Regenerate when missing OR stale (project file mtime newer than the cached thumb)
        // — a card that's just missing its thumb and one whose project changed since the
        // last render both need a fresh render; only an up-to-date thumb short-circuits.
        if thumb_is_fresh(&path, &thumb) {
            return Ok(Some(thumb.to_string_lossy().into_owned()));
        }
        let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let project = Project::from_json(&data).map_err(|e| e.to_string())?;
        // Small preview for gallery cards — Custom keeps aspect, scales height down.
        let h = 180u32;
        let w = ((project.settings.width as f64 / project.settings.height as f64) * h as f64)
            .round()
            .max(2.0) as u32;
        let w = (w / 2) * 2;
        // t=0 is frequently black (empty leader / fade-in); sample a representative frame
        // instead. Falls back to 0.0 for an empty/content-free project, which still renders
        // (a black frame) rather than erroring — an empty project card should show *something*.
        let frame_time = renderly_core::perceive::representative_frame_time(&project);
        let png = renderly_core::perceive::render_frame_png(
            &project,
            frame_time,
            ExportPreset::Custom {
                width: w.max(2),
                height: h,
                fps: project.settings.fps,
            },
        )
        .map_err(|e| e.to_string())?;
        std::fs::write(&thumb, png).map_err(|e| e.to_string())?;
        Ok(Some(thumb.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// A cached thumb is fresh iff it exists and its mtime is at least as new as the project
/// file's. Any I/O failure (either file's metadata/mtime unreadable) is treated as "not
/// fresh" so callers fall through to regenerating rather than silently keeping a possibly
/// stale/corrupt cache.
fn thumb_is_fresh(project_path: &std::path::Path, thumb_path: &std::path::Path) -> bool {
    let (Ok(project_meta), Ok(thumb_meta)) = (
        std::fs::metadata(project_path),
        std::fs::metadata(thumb_path),
    ) else {
        return false;
    };
    let (Ok(project_mtime), Ok(thumb_mtime)) = (project_meta.modified(), thumb_meta.modified())
    else {
        return false;
    };
    thumb_mtime >= project_mtime
}

#[tauri::command]
async fn close_project(app: AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let _edit_guard = state.edit_lock.lock().await;
    stop_playback_blocking(&app).await;
    *state.session.lock() = None;
    state.history.lock().clear();
    state.reset_revision();
    state.set_selection_json(None);
    state.set_playhead_secs(0.0);
    bridge::sync_discovery_project(&app, None);
    Ok(())
}

/// Explicitly (re)trigger thumbnail/waveform generation for one media item — the normal
/// path is automatic (on import, and for every item on project open), so this exists for
/// the frontend to retry after a generation failure without requiring a full reopen.
#[tauri::command]
async fn request_media_assets(
    app: AppHandle,
    state: State<'_, AppState>,
    media_id: String,
) -> Result<(), String> {
    let id: uuid::Uuid = media_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let (item, session_path) = state.with_session(|s| {
        let item = s
            .project
            .find_media(id)
            .cloned()
            .ok_or_else(|| format!("media not found: {media_id}"))?;
        Ok((item, s.path.clone()))
    })?;
    let dir = media_assets::resolve_cache_dir(&app, Some(session_path.as_path()))?;
    allow_media_assets_dir(&app, &dir);
    media_assets::request_assets(app, media_id, item.path, item.kind, dir);
    Ok(())
}

/// Synchronously return whatever's already cached for a media item — no generation
/// triggered. Used by the frontend on mount/selection to show a filmstrip/waveform
/// immediately if a prior session (or the background worker) already produced one.
#[tauri::command]
async fn get_media_assets(
    app: AppHandle,
    state: State<'_, AppState>,
    media_id: String,
) -> Result<media_assets::MediaAssetsPayload, String> {
    let id: uuid::Uuid = media_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let (path, session_path) = state.with_session(|s| {
        let path = s
            .project
            .find_media(id)
            .map(|m| m.path.clone())
            .ok_or_else(|| format!("media not found: {media_id}"))?;
        Ok((path, s.path.clone()))
    })?;
    let dir = media_assets::resolve_cache_dir(&app, Some(session_path.as_path()))?;
    media_assets::get_cached(&app, &media_id, &path, &dir)
}

#[derive(Clone, serde::Serialize)]
struct PackStickerInfo {
    id: String,
    label: String,
    default_duration_secs: f64,
}

#[derive(Clone, serde::Serialize)]
struct PackSfxInfo {
    id: String,
    label: String,
}

#[derive(Clone, serde::Serialize)]
struct PackLutInfo {
    id: String,
    label: String,
}

#[derive(Clone, serde::Serialize)]
struct PackTransitionInfo {
    id: String,
    label: String,
    kind: String,
    default_duration_secs: f64,
}

#[derive(Clone, serde::Serialize)]
struct PackTemplateInfo {
    id: String,
    label: String,
}

#[derive(Clone, serde::Serialize)]
struct LoadedPackInfo {
    id: String,
    name: String,
    path: String,
    stickers: Vec<PackStickerInfo>,
    sfx: Vec<PackSfxInfo>,
    luts: Vec<PackLutInfo>,
    transitions: Vec<PackTransitionInfo>,
    templates: Vec<PackTemplateInfo>,
}

#[derive(Clone, serde::Serialize)]
struct LoadedPluginInfo {
    id: String,
    name: String,
    path: String,
    has_frame: bool,
    has_audio: bool,
}

#[derive(Clone, serde::Serialize)]
struct ExtensionCatalog {
    packs: Vec<LoadedPackInfo>,
    plugins: Vec<LoadedPluginInfo>,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct RegistryEntry {
    id: String,
    kind: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    git_url: Option<String>,
    summary: String,
    schema_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolved_path: Option<String>,
}

#[tauri::command]
async fn list_extensions(state: State<'_, AppState>) -> Result<ExtensionCatalog, String> {
    let project = state.with_session(|s| Ok(Arc::clone(&s.project)))?;
    let packs = renderly_core::packs::load_project_packs(&project)
        .into_iter()
        .map(|p| LoadedPackInfo {
            id: p.manifest.id.clone(),
            name: p.manifest.name.clone(),
            path: p.root.display().to_string(),
            stickers: p
                .manifest
                .stickers
                .iter()
                .map(|s| PackStickerInfo {
                    id: s.id.clone(),
                    label: s.label.clone(),
                    default_duration_secs: s.default_duration_secs,
                })
                .collect(),
            sfx: p
                .manifest
                .sfx
                .iter()
                .map(|s| PackSfxInfo {
                    id: s.id.clone(),
                    label: s.label.clone(),
                })
                .collect(),
            luts: p
                .manifest
                .luts
                .iter()
                .map(|l| PackLutInfo {
                    id: l.id.clone(),
                    label: l.label.clone(),
                })
                .collect(),
            transitions: p
                .manifest
                .transitions
                .iter()
                .map(|t| PackTransitionInfo {
                    id: t.id.clone(),
                    label: t.label.clone(),
                    kind: t.kind.clone(),
                    default_duration_secs: t.default_duration_secs,
                })
                .collect(),
            templates: p
                .manifest
                .templates
                .iter()
                .map(|t| PackTemplateInfo {
                    id: t.id.clone(),
                    label: t.label.clone(),
                })
                .collect(),
        })
        .collect();

    let mut plugins = Vec::new();
    for path in &project.wasm_plugin_paths {
        let Ok(manifest) = renderly_core::plugins::load_plugin_manifest(path) else {
            continue;
        };
        let caps = renderly_core::plugins::plugin_capabilities(path).unwrap_or_default();
        plugins.push(LoadedPluginInfo {
            id: manifest.id,
            name: manifest.name,
            path: path.display().to_string(),
            has_frame: caps.has_frame,
            has_audio: caps.has_audio,
        });
    }

    Ok(ExtensionCatalog { packs, plugins })
}

#[tauri::command]
async fn list_registry() -> Result<Vec<RegistryEntry>, String> {
    // Prefer repo-relative seed when running from a checkout; otherwise empty.
    let candidates = [
        std::path::PathBuf::from("examples/registry/index.json"),
        std::path::PathBuf::from("../examples/registry/index.json"),
        std::path::PathBuf::from("../../examples/registry/index.json"),
    ];
    let mut path = None;
    for c in &candidates {
        if c.is_file() {
            path = Some(c.clone());
            break;
        }
    }
    let Some(index_path) = path else {
        return Ok(Vec::new());
    };
    let text = std::fs::read_to_string(&index_path).map_err(|e| e.to_string())?;
    let mut entries: Vec<RegistryEntry> =
        serde_json::from_str(&text).map_err(|e| format!("registry index: {e}"))?;
    let base = index_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));
    for entry in &mut entries {
        if let Some(rel) = &entry.path {
            let resolved = base.join(rel);
            if resolved.exists() {
                entry.resolved_path = Some(
                    resolved
                        .canonicalize()
                        .unwrap_or(resolved)
                        .display()
                        .to_string(),
                );
            }
        }
    }
    Ok(entries)
}

#[tauri::command]
async fn get_project(state: State<'_, AppState>) -> Result<ProjectSnapshot, String> {
    let project = state.with_session(|session| Ok(Arc::clone(&session.project)))?;
    Ok(ProjectSnapshot {
        project: (*project).clone(),
        revision: state.revision_value(),
    })
}

#[tauri::command]
async fn apply_command(
    app: AppHandle,
    state: State<'_, AppState>,
    command: serde_json::Value,
    mutation_id: Option<String>,
) -> Result<CommandResult, String> {
    state.apply_command_inner(&app, command, mutation_id).await
}

/// Apply a batch of commands atomically: all-or-nothing against a single project clone,
/// one undo snapshot, one save, one `project:changed` emit — used for gestures that are
/// logically a single edit but need more than one `Command` (e.g. CapCut-style
/// auto-track-on-drop: `AddTrack` + `AddClip` should undo together, not as two steps).
#[tauri::command]
async fn apply_commands(
    app: AppHandle,
    state: State<'_, AppState>,
    commands: Vec<serde_json::Value>,
    mutation_id: Option<String>,
) -> Result<CommandsResult, String> {
    state
        .apply_commands_inner(&app, commands, mutation_id)
        .await
}

/// Restore the previous project snapshot, moving the current one onto the redo stack.
#[tauri::command]
async fn undo(
    app: AppHandle,
    state: State<'_, AppState>,
    mutation_id: Option<String>,
) -> Result<HistoryResult, String> {
    state.undo_inner(&app, mutation_id).await
}

/// Re-apply the most recently undone snapshot, moving the current one onto the undo stack.
#[tauri::command]
async fn redo(
    app: AppHandle,
    state: State<'_, AppState>,
    mutation_id: Option<String>,
) -> Result<HistoryResult, String> {
    state.redo_inner(&app, mutation_id).await
}

#[derive(Debug, Clone, Serialize)]
struct ExportProgressEvent {
    phase: ExportPhase,
    frame: u64,
    total_frames: u64,
    /// 0.0–1.0 overall progress (video frames dominate; audio/mux sit at 1.0).
    fraction: f64,
}

pub(crate) fn parse_export_preset(preset: &serde_json::Value) -> Result<ExportPreset, String> {
    match preset {
        serde_json::Value::String(s) => match s.as_str() {
            "tiktok" => Ok(ExportPreset::TikTok9x16),
            "youtube" => Ok(ExportPreset::Youtube16x9),
            other => {
                serde_json::from_str(other).map_err(|e| format!("unknown preset '{other}': {e}"))
            }
        },
        other => serde_json::from_value(other.clone())
            .map_err(|e| format!("invalid export preset JSON: {e}")),
    }
}

fn parse_video_encoder(raw: &str) -> Result<VideoEncoderPreference, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "auto" => Ok(VideoEncoderPreference::Auto),
        "software" => Ok(VideoEncoderPreference::Software),
        "nvenc" => Ok(VideoEncoderPreference::Nvenc),
        "qsv" => Ok(VideoEncoderPreference::Qsv),
        other => Err(format!(
            "unknown video encoder '{other}' (expected auto|software|nvenc|qsv)"
        )),
    }
}

/// Merge optional encode overrides (camelCase or snake_case) into preset-derived settings.
pub(crate) fn merge_encode_into_settings(
    settings: &mut ExportSettings,
    encode: &serde_json::Value,
) -> Result<(), String> {
    let Some(obj) = encode.as_object() else {
        return Err("encode must be an object".into());
    };

    if let Some(v) = obj
        .get("videoEncoder")
        .or_else(|| obj.get("video_encoder"))
        .and_then(|v| v.as_str())
    {
        settings.video_encoder = parse_video_encoder(v)?;
    }

    if let Some(v) = obj.get("crf") {
        let n = v
            .as_u64()
            .or_else(|| v.as_f64().map(|f| f.round() as u64))
            .ok_or_else(|| "encode.crf must be a number".to_string())?;
        if n > 51 {
            return Err("encode.crf must be 0–51".into());
        }
        settings.crf = n as u8;
    }

    if let Some(v) = obj
        .get("audioBitrateK")
        .or_else(|| obj.get("audio_bitrate_k"))
    {
        let n = v
            .as_u64()
            .or_else(|| v.as_f64().map(|f| f.round() as u64))
            .ok_or_else(|| "encode.audioBitrateK must be a number".to_string())?;
        if n == 0 || n > 512 {
            return Err("encode.audioBitrateK must be 1–512".into());
        }
        settings.audio_bitrate_k = n as u32;
    }

    Ok(())
}

/// Render the open project to `output_path`. Clones the project and never holds the
/// session/`edit_lock` during encode. Emits `export:progress` (~10 Hz) while running;
/// call `cancel_export` to cooperatively abort (temp dir cleaned up).
#[tauri::command]
async fn export_project(
    app: AppHandle,
    state: State<'_, AppState>,
    output_path: String,
    preset: serde_json::Value,
    encode: Option<serde_json::Value>,
) -> Result<(), String> {
    let preset = parse_export_preset(&preset)?;
    // Clone under a short lock — export must not hold session/edit_lock for the duration.
    let project = state.with_session(|s| Ok(Arc::clone(&s.project)))?;
    let mut settings = ExportSettings::from_preset(&preset, &project);
    if let Some(ref enc) = encode {
        merge_encode_into_settings(&mut settings, enc)?;
    }
    state.export_cancel.store(false, Ordering::SeqCst);
    let cancel = Arc::clone(&state.export_cancel);

    tauri::async_runtime::spawn_blocking(move || {
        let mut last_emit = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        let result = export_project_with_settings(
            &project,
            std::path::Path::new(&output_path),
            settings,
            &mut |p: ExportProgress| {
                if cancel.load(Ordering::SeqCst) {
                    return false;
                }
                let force =
                    p.phase != ExportPhase::Video || p.frame + 1 >= p.total_frames || p.frame == 0;
                if force || last_emit.elapsed() >= Duration::from_millis(100) {
                    last_emit = Instant::now();
                    let fraction = if p.total_frames == 0 {
                        0.0
                    } else {
                        (p.frame as f64 / p.total_frames as f64).clamp(0.0, 1.0)
                    };
                    let _ = app.emit(
                        "export:progress",
                        ExportProgressEvent {
                            phase: p.phase,
                            frame: p.frame,
                            total_frames: p.total_frames,
                            fraction,
                        },
                    );
                }
                true
            },
        );
        match result {
            Ok(()) => Ok(()),
            Err(ExportError::Cancelled) => Err("export cancelled".into()),
            Err(e) => Err(e.to_string()),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Ask the in-flight export (if any) to stop at the next progress checkpoint.
#[tauri::command]
async fn cancel_export(state: State<'_, AppState>) -> Result<(), String> {
    state.export_cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// Start (or resume) playback from `time_secs`. Non-blocking: hands an `Arc<Project>`
/// off to the playback worker thread and returns immediately — see playback.rs. The
/// webview owns all video presentation; this only drives the audio clock and
/// `playback:tick` emission (see playback.rs's module doc comment).
#[tauri::command]
async fn play(app: AppHandle, state: State<'_, AppState>, time_secs: f64) -> Result<(), String> {
    state.set_playhead_secs(time_secs);
    let project = state.playback_project()?;
    state.playback.play(app, project, time_secs);
    Ok(())
}

/// Stop playback and return the time to resume from. Joins the playback thread from
/// `spawn_blocking` rather than inline — see `stop_playback_blocking`'s doc comment; the
/// same in-flight-premix blocking risk applies here since `pause()` can be called the
/// instant after `play()` starts.
#[tauri::command]
async fn pause(app: AppHandle, state: State<'_, AppState>) -> Result<f64, String> {
    let t = tauri::async_runtime::spawn_blocking(move || app.state::<AppState>().playback.pause())
        .await
        .map_err(|e| e.to_string())?;
    state.set_playhead_secs(t);
    Ok(t)
}

/// Jump the playhead to `time_secs`. While playing, this coalesces into the running
/// playback loop (audio/decoders restart from the new position without a pause/resume
/// round trip). While paused, the webview canvas repaints itself
/// (`WebviewPreviewEngine.seek`) — there is no backend-side paused-state render anymore.
#[tauri::command]
async fn seek(state: State<'_, AppState>, time_secs: f64) -> Result<(), String> {
    state.set_playhead_secs(time_secs);
    state.playback.seek_while_playing(time_secs);
    Ok(())
}

/// Refresh playback after an edit that didn't go through `seek` (e.g. `apply_command`,
/// `apply_commands`, `undo`, `redo`). Deliberately distinct from `seek`: while playing,
/// `seek` restarts the play loop from a new position (stops the audio sink, re-premixes,
/// reopens decoders) — appropriate for an actual scrub, wildly disruptive for "the
/// inspector's opacity slider moved" or "a clip got trimmed". This instead live-swaps the
/// project into the running session (see `PlaybackEngine::update_live_project`) so the
/// next already-scheduled frame's audio reflects the new state; the webview canvas
/// repaints itself from the same store patch (`WebviewPreviewEngine.notifyProjectPatched`)
/// regardless of play state.
#[tauri::command]
async fn refresh_frame(state: State<'_, AppState>) -> Result<(), String> {
    let project = state.playback_project()?;
    state.playback.update_live_project(project);
    Ok(())
}

/// Render a short audio blip at `time_secs` (timeline scrub feedback). Non-blocking and
/// coalesced — safe to call on every pointermove during a drag. The visual scrub redraw
/// is entirely the webview canvas's own doing; this only drives the audio cue.
#[tauri::command]
async fn scrub_audio(state: State<'_, AppState>, time_secs: f64) -> Result<(), String> {
    let project = state.playback_project()?;
    state.playback.request_scrub_audio(project, time_secs);
    Ok(())
}

/// Tell the backend which nested sequence the timeline has open (`None` = root) so playback
/// and scrub audio follow it. The frontend stops playback before switching containers.
#[tauri::command]
async fn set_active_sequence(
    state: State<'_, AppState>,
    sequence_id: Option<String>,
) -> Result<(), String> {
    let id = match sequence_id {
        None => None,
        Some(s) => Some(
            s.parse::<renderly_core::project::Id>()
                .map_err(|e| format!("invalid sequence id: {e}"))?,
        ),
    };
    *state.active_sequence.lock() = id;
    Ok(())
}

/// Mirror frontend timeline selection into AppState for bridge `get_editor_status` (E2).
#[tauri::command]
async fn set_editor_selection(
    state: State<'_, AppState>,
    selection: Option<serde_json::Value>,
) -> Result<(), String> {
    state.set_selection_json(selection);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::new())
        .setup(|app| {
            // Point renderly-core at the ffmpeg/ffprobe bundled beside the app (fetched at
            // build time into the Tauri resource dir — see scripts/fetch-ffmpeg.mjs and
            // ffmpeg_cli.rs's `which_tool`). Absent in `tauri dev` (resources aren't fetched
            // for dev), where core falls back to PATH. Set only when the binaries exist so a
            // dev machine's PATH ffmpeg still wins if the resource dir is empty.
            if let Ok(res_dir) = app.path().resource_dir() {
                let ffmpeg_dir = res_dir.join("ffmpeg");
                let exe = if cfg!(windows) {
                    "ffmpeg.exe"
                } else {
                    "ffmpeg"
                };
                if ffmpeg_dir.join(exe).is_file() {
                    // SAFETY: single-threaded setup, before any worker spawns ffmpeg.
                    std::env::set_var("RENDERLY_FFMPEG_DIR", &ffmpeg_dir);
                }
            }
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                match bridge::run_bridge(handle.clone()).await {
                    Ok(bridge_handle) => {
                        handle.manage(bridge_handle);
                    }
                    Err(e) => {
                        eprintln!("renderly bridge failed to start: {e}");
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            quick_start_project,
            new_project,
            open_project,
            save_project,
            list_projects,
            rename_project,
            delete_project,
            project_thumbnail,
            close_project,
            get_project,
            apply_command,
            apply_commands,
            undo,
            redo,
            export_project,
            cancel_export,
            play,
            pause,
            seek,
            refresh_frame,
            scrub_audio,
            set_editor_selection,
            set_active_sequence,
            request_media_assets,
            get_media_assets,
            list_extensions,
            list_registry,
            preview_assets::read_preview_matte,
            preview_assets::preview_matte_stamps,
            preview_assets::read_preview_pack_luts,
            ml::ml_model_status,
            ml::ml_download_model,
            ml::cancel_background_matte,
            ml::background_matte_status,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Renderly");
}

#[cfg(test)]
mod history_tests {
    use super::*;
    use renderly_core::project::Settings;

    fn dummy_project(tag: &str) -> Arc<Project> {
        Arc::new(Project::new(tag, Settings::default()))
    }

    #[test]
    fn push_undo_bounds_stack_at_history_cap() {
        let mut history = History::new();
        for i in 0..(HISTORY_CAP + 20) {
            history.push_undo(dummy_project(&format!("edit-{i}")));
        }
        assert_eq!(history.undo.len(), HISTORY_CAP);
        // Oldest entries should have been evicted — the surviving bottom entry is the
        // 21st push (edits 0..19 evicted), not edit-0.
        assert_eq!(history.undo.first().unwrap().name, "edit-20");
        assert_eq!(history.undo.last().unwrap().name, "edit-119");
    }

    #[test]
    fn push_undo_clears_redo_but_push_undo_bounded_does_not() {
        let mut history = History::new();
        history.push_undo(dummy_project("a"));
        history.redo.push(dummy_project("stale-redo"));

        // A genuinely new edit invalidates the redo branch.
        history.push_undo(dummy_project("b"));
        assert!(history.redo.is_empty());

        // Redo's own bookkeeping push must NOT clear a redo branch that still has
        // further-forward entries below the one just popped.
        history.redo.push(dummy_project("still-redoable"));
        history.push_undo_bounded(dummy_project("c"));
        assert_eq!(history.redo.len(), 1);
    }

    #[test]
    fn clear_empties_both_stacks() {
        let mut history = History::new();
        history.push_undo(dummy_project("a"));
        history.redo.push(dummy_project("b"));
        history.clear();
        assert!(!history.status().can_undo);
        assert!(!history.status().can_redo);
    }

    #[test]
    fn rename_diff_is_non_empty() {
        let before = Project::new("before", Settings::default());
        let before_json = serde_json::to_value(&before).expect("serialize before");
        let mut after = before;
        after.name = "after".into();
        let after_json = serde_json::to_value(&after).expect("serialize after");
        let patch = json_patch::diff(&before_json, &after_json);
        assert!(
            !patch.0.is_empty(),
            "rename should produce a non-empty JSON Patch"
        );
    }
}

#[cfg(test)]
mod naming_tests {
    use super::*;
    use std::collections::HashSet;

    fn stems(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn next_free_project_name_no_collision() {
        let existing = stems(&["Jul 17, 2026", "Jul 16, 2026"]);
        assert_eq!(
            next_free_project_name(&existing, "Jul 18, 2026"),
            "Jul 18, 2026"
        );
    }

    #[test]
    fn next_free_project_name_single_collision() {
        let existing = stems(&["Jul 18, 2026"]);
        assert_eq!(
            next_free_project_name(&existing, "Jul 18, 2026"),
            "Jul 18, 2026 (2)"
        );
    }

    #[test]
    fn next_free_project_name_multiple_collisions() {
        let existing = stems(&["Jul 18, 2026", "Jul 18, 2026 (2)", "Jul 18, 2026 (3)"]);
        assert_eq!(
            next_free_project_name(&existing, "Jul 18, 2026"),
            "Jul 18, 2026 (4)"
        );
    }

    #[test]
    fn next_free_project_name_fills_gap() {
        // (2) and (4) exist but (3) doesn't — the next free suffix must be the gap, not
        // one past the highest existing suffix.
        let existing = stems(&["Jul 18, 2026", "Jul 18, 2026 (2)", "Jul 18, 2026 (4)"]);
        assert_eq!(
            next_free_project_name(&existing, "Jul 18, 2026"),
            "Jul 18, 2026 (3)"
        );
    }

    #[test]
    fn project_name_stem_strips_full_suffix() {
        assert_eq!(
            project_name_stem("Jul 18, 2026.renderly.json"),
            Some("Jul 18, 2026")
        );
        assert_eq!(project_name_stem("not-a-project.txt"), None);
    }
}
