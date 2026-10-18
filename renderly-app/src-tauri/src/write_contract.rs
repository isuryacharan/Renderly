//! The desktop app's write path for the agent edit contract (docs/command-api.md "Edit contract").
//!
//! Every project write from the webview (Tauri `apply_command` / `apply_commands`), the live
//! agent bridge and the in-app agent tools comes through `write_commands` here. It runs
//! `renderly_core::edit_contract::plan_write` under the project's edit lock, so the revision
//! check, the apply, the deletion guard and the commit see one consistent project. The core owns
//! the rules; this module only commits the plan (session, history, change event, side effects).

use crate::{AppState, CommandResult, CommandsResult};
use renderly_core::edit_contract::{command_name, plan_write, Caller, EditError, WriteOptions};
use renderly_core::project::{Id, Project};
use renderly_core::{Command, CommandOutcome};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::AppHandle;

/// One committed or planned write: the contract result plus the typed outcome of each command,
/// which in-app callers use to name what was created.
pub(crate) struct WriteOutput {
    pub result: renderly_core::edit_contract::WriteResult,
    pub outcomes: Vec<CommandOutcome>,
}

impl AppState {
    /// One command: the Tauri `apply_command` handler, the live bridge and the agent tools.
    pub(crate) async fn apply_command_inner(
        &self,
        app: &AppHandle,
        command: Value,
        mutation_id: Option<String>,
        merge_key: Option<String>,
        options: WriteOptions,
        caller: Caller,
    ) -> Result<CommandResult, EditError> {
        // The in-process `rvm` matte bake takes minutes: run it off the project lock with progress
        // and cancel, and only apply its small result through the edit lock. A dry run never bakes.
        if !options.dry_run {
            if let Some((track_id, clip_id)) = crate::ml::rvm_matte_target(self, &command) {
                // Boxed: `generate_rvm_matte` calls back into this function for the attach step.
                return Box::pin(self.generate_rvm_matte(app, track_id, clip_id, mutation_id))
                    .await
                    .map_err(EditError::internal);
            }
        }
        let cmd: Command = serde_json::from_value(command)
            .map_err(|e| EditError::invalid(format!("invalid command: {e}")))?;
        let out = self
            .write_commands(app, vec![cmd], options, caller, mutation_id, merge_key)
            .await?;
        let outcome = out
            .outcomes
            .first()
            .map(|o| format!("{o:?}"))
            .unwrap_or_default();
        Ok(CommandResult {
            write: out.result,
            outcome,
        })
    }

    /// A batch of commands, applied all or nothing as one undo step.
    pub(crate) async fn apply_commands_inner(
        &self,
        app: &AppHandle,
        commands: Vec<Value>,
        mutation_id: Option<String>,
        merge_key: Option<String>,
        options: WriteOptions,
        caller: Caller,
    ) -> Result<CommandsResult, EditError> {
        let cmds: Vec<Command> = commands
            .into_iter()
            .map(|c| {
                serde_json::from_value(c)
                    .map_err(|e| EditError::invalid(format!("invalid command: {e}")))
            })
            .collect::<Result<_, _>>()?;
        let out = self
            .write_commands(app, cmds, options, caller, mutation_id, merge_key)
            .await?;
        Ok(CommandsResult {
            outcomes: out.outcomes.iter().map(|o| format!("{o:?}")).collect(),
            typed_outcomes: out.outcomes,
            write: out.result,
        })
    }

    /// Check, apply and (unless dry run or no-op) commit a batch of typed commands.
    pub(crate) async fn write_commands(
        &self,
        app: &AppHandle,
        cmds: Vec<Command>,
        options: WriteOptions,
        caller: Caller,
        mutation_id: Option<String>,
        merge_key: Option<String>,
    ) -> Result<WriteOutput, EditError> {
        let _edit_guard = self.edit_lock.lock().await;
        let before = self
            .with_session(|s| Ok(Arc::clone(&s.project)))
            .map_err(EditError::internal)?;
        let revision = self.revision_value();
        let names: Vec<String> = cmds.iter().map(command_name).collect();
        let prewarm = denoise_prewarm_targets(&cmds);
        let voiceover_paths: Vec<Option<PathBuf>> = cmds
            .iter()
            .map(|c| match c {
                Command::GenerateVoiceover { output_path, .. } => Some(PathBuf::from(output_path)),
                _ => None,
            })
            .collect();

        // The plan (apply to a scratch copy, possibly a whole Whisper run) is CPU/IO heavy: run it
        // off the async runtime. The ledger lock is held only for this closure.
        let ledger = Arc::clone(&self.write_ledger);
        let plan_input = Arc::clone(&before);
        let planned = tauri::async_runtime::spawn_blocking(move || {
            let ledger = ledger.lock();
            plan_write(&plan_input, revision, &ledger, &options, caller, cmds)
        })
        .await
        .map_err(|e| EditError::internal(e.to_string()))?;

        let mut plan = match planned {
            Ok(plan) => plan,
            Err(err) => {
                // Commands before the failing one may already have written voiceover files.
                let failed = err
                    .data
                    .as_ref()
                    .and_then(|d| d.get("failed_index"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                for path in voiceover_paths.iter().take(failed).flatten() {
                    let _ = std::fs::remove_file(path);
                }
                return Err(err);
            }
        };
        let outcomes = plan.outcomes().to_vec();

        let Some(next) = plan.take_next() else {
            // Dry run, replay, or nothing changed: nothing to commit, no history, no event.
            let result = plan.finish(None, &mut self.write_ledger.lock());
            return Ok(WriteOutput { result, outcomes });
        };
        let next = Arc::new(next);

        // Side effects that need the new project, gathered before the commit.
        let imported: Vec<(Id, PathBuf, renderly_core::project::MediaKind)> = outcomes
            .iter()
            .filter_map(|o| match o {
                CommandOutcome::MediaImported { media_id } => next.find_media(*media_id),
                _ => None,
            })
            .map(|item| (item.id, item.path.clone(), item.kind))
            .collect();

        self.record_change(Arc::clone(&before), names, merge_key.as_deref());
        self.commit_project(Arc::clone(&next))
            .await
            .map_err(EditError::internal)?;

        for (target_track, target_clip, strength) in prewarm {
            prewarm_denoise(&next, target_track, target_clip, strength);
        }
        for (media_id, path, kind) in imported {
            request_media_assets(self, app, media_id, path, kind);
        }

        let revision = self.emit_project_changed(app, mutation_id);
        let result = plan.finish(Some(revision), &mut self.write_ledger.lock());
        Ok(WriteOutput { result, outcomes })
    }
}

/// `(track, clip, strength)` for every RNNoise denoise the batch turns on.
fn denoise_prewarm_targets(cmds: &[Command]) -> Vec<(Id, Id, f64)> {
    cmds.iter()
        .filter_map(|cmd| match cmd {
            Command::SetClipAudioDenoise {
                track_id,
                clip_id,
                config: Some(cfg),
            } if cfg.enabled && cfg.backend == "rnnoise" => {
                Some((*track_id, *clip_id, cfg.strength))
            }
            _ => None,
        })
        .collect()
}

/// RNNoise denoise is rendered into a cache the first time it is mixed; start that now, off the
/// UI/command path, so the next playback or export finds it ready.
fn prewarm_denoise(project: &Project, track_id: Id, clip_id: Id, strength: f64) {
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

/// Imported media: request its thumbnail/waveform assets and allow the webview to read them.
fn request_media_assets(
    state: &AppState,
    app: &AppHandle,
    media_id: Id,
    path: PathBuf,
    kind: renderly_core::project::MediaKind,
) {
    let session_path = state.with_session(|s| Ok(s.path.clone())).ok();
    let assets_dir = crate::media_assets::resolve_cache_dir(app, session_path.as_deref());
    if let Ok(dir) = &assets_dir {
        crate::allow_media_assets_dir(app, dir);
    }
    if let Ok(dir) = assets_dir {
        crate::media_assets::request_assets(
            app.clone(),
            media_id.to_string(),
            path.clone(),
            kind,
            dir,
        );
    }
    crate::allow_media_assets(app, std::iter::once(path.as_path()));
}
