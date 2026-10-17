//! Local ML models in the app: status / explicit consented download, and the long-running
//! in-process background-matte bake (progress + cancel, off the project lock).
//!
//! Nothing here downloads implicitly: `ml_download_model` is only ever invoked by the consent
//! dialog's Download button (or an agent through the MCP `download_model` tool / CLI), and the
//! matte bake fails with `ModelNotAvailable` instead of fetching weights.

use crate::{AppState, CommandResult};
use renderly_core::commands::{prepare_rvm_matte_job, segmentation_error};
use renderly_core::ml::{self, ModelStatus};
use renderly_core::project::Id;
use renderly_core::segmentation::{self, MatteProgress, MatteStatus};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Clone, Serialize)]
struct DownloadProgressEvent {
    id: String,
    done: u64,
    total: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
struct MatteProgressEvent {
    track_id: Id,
    clip_id: Id,
    #[serde(flatten)]
    progress: MatteProgress,
}

#[derive(Debug, Clone, Serialize)]
struct MatteFinishedEvent {
    track_id: Id,
    clip_id: Id,
    /// `ok`, `cancelled` or `error`.
    status: &'static str,
    message: Option<String>,
}

/// Whether the model's weights are installed, plus what the consent dialog must show (licence,
/// size, download host).
#[tauri::command]
pub async fn ml_model_status(id: String) -> Result<ModelStatus, String> {
    tauri::async_runtime::spawn_blocking(move || ml::model_status(&id).map_err(|e| e.to_string()))
        .await
        .map_err(|e| e.to_string())?
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);

struct Flag<'a>(&'a AtomicBool);
impl Drop for Flag<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Download model `id` from its pinned upstream host (SHA-256 verified, atomic install).
/// The caller (the consent dialog) must have obtained the user's agreement first. Emits
/// `ml:download-progress` `{ id, done, total }` (~10 Hz).
#[tauri::command]
pub async fn ml_download_model(app: AppHandle, id: String) -> Result<ModelStatus, String> {
    if DOWNLOADING.swap(true, Ordering::SeqCst) {
        return Err("a model download is already running".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let _flag = Flag(&DOWNLOADING);
        let mut last = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        let model_id = id.clone();
        ml::download_model(&id, &mut |done, total| {
            if last.elapsed() >= Duration::from_millis(100) || Some(done) == total {
                last = Instant::now();
                let _ = app.emit(
                    "ml:download-progress",
                    DownloadProgressEvent {
                        id: model_id.clone(),
                        done,
                        total,
                    },
                );
            }
        })
        .map_err(|e| e.to_string())?;
        ml::model_status(&id).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Ask the running matte bake (if any) to stop after the current frame.
#[tauri::command]
pub async fn cancel_background_matte(state: State<'_, AppState>) -> Result<(), String> {
    state.matte_cancel.store(true, Ordering::SeqCst);
    Ok(())
}

/// State of a clip's baked background-removal matte (`not_generated` / `ready` / `stale`).
#[tauri::command]
pub async fn background_matte_status(
    state: State<'_, AppState>,
    track_id: Id,
    clip_id: Id,
) -> Result<MatteStatus, String> {
    let project = state.with_session(|s| Ok(Arc::clone(&s.project)))?;
    tauri::async_runtime::spawn_blocking(move || {
        let clip = project
            .find_track(track_id)
            .and_then(|t| t.clips.iter().find(|c| c.id() == clip_id))
            .and_then(|c| c.as_media())
            .ok_or_else(|| format!("media clip {clip_id} not found"))?;
        Ok(segmentation::matte_status(&project, clip))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// `(track, clip)` when `command` is a `GenerateBackgroundMatte` for a clip whose model is the
/// in-process `rvm` (the only matte bake that takes long enough to need progress and cancel).
pub(crate) fn rvm_matte_target(state: &AppState, command: &serde_json::Value) -> Option<(Id, Id)> {
    if command.get("command")?.as_str()? != "GenerateBackgroundMatte" {
        return None;
    }
    let track_id: Id = command.get("track_id")?.as_str()?.parse().ok()?;
    let clip_id: Id = command.get("clip_id")?.as_str()?.parse().ok()?;
    let project = state.with_session(|s| Ok(Arc::clone(&s.project))).ok()?;
    match prepare_rvm_matte_job(&project, track_id, clip_id) {
        Ok(Some(_)) => Some((track_id, clip_id)),
        _ => None,
    }
}

impl AppState {
    /// `GenerateBackgroundMatte` for the `rvm` model, without holding the project: bake against
    /// a snapshot on a blocking thread (emitting `matte:progress`, cancellable via
    /// `cancel_background_matte`), then apply the small `AttachBackgroundMatte` command through
    /// the normal edit path (brief edit lock, undo entry, `project:changed`). Emits
    /// `matte:finished` however it ends.
    pub(crate) async fn generate_rvm_matte(
        &self,
        app: &AppHandle,
        track_id: Id,
        clip_id: Id,
        mutation_id: Option<String>,
    ) -> Result<CommandResult, String> {
        let finish = |status: &'static str, message: Option<String>| {
            let _ = app.emit(
                "matte:finished",
                MatteFinishedEvent {
                    track_id,
                    clip_id,
                    status,
                    message,
                },
            );
        };
        let result = self
            .generate_rvm_matte_inner(app, track_id, clip_id, mutation_id)
            .await;
        match &result {
            Ok(_) => finish("ok", None),
            Err(m) if m == "cancelled" => finish("cancelled", None),
            Err(m) => finish("error", Some(m.clone())),
        }
        result
    }

    async fn generate_rvm_matte_inner(
        &self,
        app: &AppHandle,
        track_id: Id,
        clip_id: Id,
        mutation_id: Option<String>,
    ) -> Result<CommandResult, String> {
        if self.matte_running.swap(true, Ordering::SeqCst) {
            return Err("another background matte is already being generated".into());
        }
        let _running = Flag(&self.matte_running);
        self.matte_cancel.store(false, Ordering::SeqCst);

        let project = self.with_session(|s| Ok(Arc::clone(&s.project)))?;
        let job = prepare_rvm_matte_job(&project, track_id, clip_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "clip does not use the rvm background-removal model".to_string())?;
        drop(project);

        let cancel = Arc::clone(&self.matte_cancel);
        let progress_app = app.clone();
        let baked = tauri::async_runtime::spawn_blocking(move || {
            let mut last = Instant::now()
                .checked_sub(Duration::from_secs(1))
                .unwrap_or_else(Instant::now);
            segmentation::bake_rvm_matte(&job, &mut |p| {
                if cancel.load(Ordering::SeqCst) {
                    return false;
                }
                if last.elapsed() >= Duration::from_millis(100) || p.frame >= p.total {
                    last = Instant::now();
                    let _ = progress_app.emit(
                        "matte:progress",
                        MatteProgressEvent {
                            track_id,
                            clip_id,
                            progress: p,
                        },
                    );
                }
                true
            })
            .map(|_| job.cache_dir.clone())
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| segmentation_error(e).to_string())?;

        let attach = serde_json::json!({
            "command": "AttachBackgroundMatte",
            "track_id": track_id,
            "clip_id": clip_id,
            "cache_dir": baked,
        });
        self.apply_command_inner(app, attach, mutation_id, None)
            .await
    }
}
