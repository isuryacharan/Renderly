//! Thin CLI over renderly-core's command API (docs/command-api.md). Every subcommand here
//! must go through `renderly_core::apply_command` — see AGENTS.md §0.1.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use renderly_core::commands::{Command, ExportPreset};
use renderly_core::export::export_project_with_progress;
use renderly_core::otio::{
    export_otio_with_options, import_otio, probe_media, ExportOptions, ImportOptions,
};
use renderly_core::project::{Project, Settings};
use std::fs;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "renderly",
    version,
    about = "Renderly CLI — script an edit through the command API"
)]
struct Cli {
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Create a new, empty project file.
    NewProject {
        /// Path to write the project JSON to.
        path: PathBuf,
        #[arg(long, default_value = "untitled")]
        name: String,
        #[arg(long, default_value_t = 1080)]
        width: u32,
        #[arg(long, default_value_t = 1920)]
        height: u32,
        #[arg(long, default_value_t = 60.0)]
        fps: f64,
        #[arg(long, default_value_t = 48000)]
        sample_rate: u32,
    },
    /// Apply one command (as a JSON object) to a project and save the result.
    Apply {
        /// Path to the project JSON file (modified in place).
        path: PathBuf,
        /// A single Command as a JSON object, e.g. '{"command":"AddTrack","kind":"video","name":"V1"}'
        command_json: String,
    },
    /// Apply a JSON array of commands from a script file, in order, and save the result.
    ApplyScript {
        /// Path to the project JSON file (modified in place).
        path: PathBuf,
        /// Path to a JSON file containing an array of Command objects.
        script: PathBuf,
    },
    /// Print the project state as pretty JSON.
    Show { path: PathBuf },
    /// Render the project to a video file (requires ffmpeg on PATH).
    Export {
        path: PathBuf,
        output: PathBuf,
        #[arg(long, default_value = "tiktok")]
        preset: String,
    },
    /// Write the project as an OpenTimelineIO (.otio) file (docs/otio.md).
    ExportOtio {
        path: PathBuf,
        /// Where to write the .otio file.
        output: PathBuf,
    },
    /// Create a NEW project file from an OpenTimelineIO (.otio) file (docs/otio.md).
    ImportOtio {
        /// The .otio file to read.
        input: PathBuf,
        /// Path to write the new project JSON to.
        output: PathBuf,
        /// Project width when the file carries none of its own.
        #[arg(long)]
        width: Option<u32>,
        /// Project height when the file carries none of its own.
        #[arg(long)]
        height: Option<u32>,
        /// Fill media durations / sizes from the files that exist on disk (needs ffprobe).
        #[arg(long)]
        probe: bool,
    },
    /// List or download the local ML models (never downloaded implicitly).
    Models {
        #[command(subcommand)]
        action: ModelsCommand,
    },
}

#[derive(Subcommand)]
enum ModelsCommand {
    /// Show every known model: licence, size, download host and whether it is installed.
    List,
    /// Download a model from its upstream host into the model cache (SHA-256 verified).
    Download {
        /// Model id, e.g. `rvm-mobilenetv3-fp32` (see `models list`).
        id: String,
        /// Confirm you accept the model's licence and the download from the upstream host.
        #[arg(long)]
        yes: bool,
    },
}

fn describe_model(m: &renderly_core::ml::ModelStatus) -> String {
    format!(
        "{} ({})\n  licence: {} <{}>\n  size: {:.1} MB, from {}\n  stored in: {}\n  status: {}",
        m.name,
        m.id,
        m.license_spdx,
        m.license_url,
        m.size_bytes as f64 / 1_000_000.0,
        m.download_host.as_deref().unwrap_or("(bundled)"),
        m.cache_dir,
        match (&m.installed, &m.path) {
            (true, Some(p)) => format!("installed at {p}"),
            (true, None) => "installed (bundled)".to_string(),
            _ => "not installed".to_string(),
        }
    )
}

fn run_models(action: ModelsCommand) -> Result<()> {
    match action {
        ModelsCommand::List => {
            for m in renderly_core::ml::list_model_status() {
                println!("{}\n", describe_model(&m));
            }
        }
        ModelsCommand::Download { id, yes } => {
            let status = renderly_core::ml::model_status(&id)?;
            println!("{}", describe_model(&status));
            if status.installed && status.path.is_some() {
                println!("already installed");
                return Ok(());
            }
            if !yes {
                bail!(
                    "this downloads {:.1} MB from {} under the {} licence; re-run with --yes to confirm",
                    status.size_bytes as f64 / 1_000_000.0,
                    status
                        .download_host
                        .as_deref()
                        .unwrap_or("the upstream host"),
                    status.license_spdx
                );
            }
            let mut last_pct = u64::MAX;
            let path = renderly_core::ml::download_model(&id, &mut |done, total| {
                let pct = total.map_or(0, |t| done * 100 / t.max(1));
                if pct != last_pct {
                    last_pct = pct;
                    eprint!("\rdownloading {id}: {pct}%   ");
                }
            })?;
            eprintln!();
            println!("installed {}", path.display());
        }
    }
    Ok(())
}

/// Directory a project / .otio file lives in (relative media paths resolve against it).
fn parent_dir(path: &std::path::Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        return std::env::current_dir().ok();
    }
    std::path::absolute(parent).ok()
}

fn load_project(path: &PathBuf) -> Result<Project> {
    let data = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    Project::from_json(&data).with_context(|| format!("loading project {}", path.display()))
}

fn save_project(path: &PathBuf, project: &Project) -> Result<()> {
    let data = serde_json::to_string_pretty(project)?;
    fs::write(path, data).with_context(|| format!("writing {}", path.display()))
}

fn apply_and_report(project: &mut Project, cmd: Command) -> Result<()> {
    match renderly_core::apply_command(project, cmd) {
        Ok(outcome) => {
            println!("{:?}", outcome);
            Ok(())
        }
        Err(e) => bail!("command failed: {e}"),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        CliCommand::Models { action } => run_models(action)?,
        CliCommand::NewProject {
            path,
            name,
            width,
            height,
            fps,
            sample_rate,
        } => {
            let project = Project::new(
                name,
                Settings {
                    fps,
                    width,
                    height,
                    sample_rate,
                    duck_db: -12.0,
                },
            );
            save_project(&path, &project)?;
            println!("created project {}", path.display());
        }
        CliCommand::Apply { path, command_json } => {
            let mut project = load_project(&path)?;
            let cmd: Command =
                serde_json::from_str(&command_json).context("parsing command JSON")?;
            apply_and_report(&mut project, cmd)?;
            save_project(&path, &project)?;
        }
        CliCommand::ApplyScript { path, script } => {
            let mut project = load_project(&path)?;
            let script_data = fs::read_to_string(&script)
                .with_context(|| format!("reading {}", script.display()))?;
            let commands: Vec<Command> =
                serde_json::from_str(&script_data).context("parsing script JSON")?;
            for cmd in commands {
                apply_and_report(&mut project, cmd)?;
            }
            save_project(&path, &project)?;
        }
        CliCommand::Show { path } => {
            let project = load_project(&path)?;
            println!("{}", serde_json::to_string_pretty(&project)?);
        }
        CliCommand::Export {
            path,
            output,
            preset,
        } => {
            // Export is a pure render of a cloned project (no project mutation), so we call
            // `export_project_with_progress` directly for a live frame counter. Scripted
            // `Command::Export` via `apply` still goes through `apply_command`.
            let project = load_project(&path)?;
            let preset = match preset.as_str() {
                "tiktok" => ExportPreset::TikTok9x16,
                "youtube" => ExportPreset::Youtube16x9,
                other => bail!("unknown preset '{other}', expected 'tiktok' or 'youtube'"),
            };
            let mut last_phase = None;
            export_project_with_progress(&project, &output, preset, &mut |p| {
                if last_phase != Some(p.phase) {
                    if last_phase.is_some() {
                        eprintln!();
                    }
                    last_phase = Some(p.phase);
                }
                eprint!("\rexport {:?}: {}/{}    ", p.phase, p.frame, p.total_frames);
                true
            })?;
            eprintln!();
            println!("exported {}", output.display());
        }
        CliCommand::ExportOtio { path, output } => {
            let project = load_project(&path)?;
            let options = ExportOptions {
                base_dir: parent_dir(&path),
            };
            let otio = export_otio_with_options(&project, &options)?;
            fs::write(&output, otio).with_context(|| format!("writing {}", output.display()))?;
            println!("exported {}", output.display());
        }
        CliCommand::ImportOtio {
            input,
            output,
            width,
            height,
            probe,
        } => {
            let json = fs::read_to_string(&input)
                .with_context(|| format!("reading {}", input.display()))?;
            let base_dir = parent_dir(&input);
            let options = ImportOptions {
                base_dir: base_dir.clone(),
                width,
                height,
                ..Default::default()
            };
            let (mut project, mut warnings) = import_otio(&json, &options)
                .with_context(|| format!("importing {}", input.display()))?;
            if probe {
                warnings.extend(probe_media(&mut project, base_dir.as_deref()));
            }
            save_project(&output, &project)?;
            for w in &warnings {
                eprintln!("warning: {w}");
            }
            println!(
                "imported {} ({} tracks, {} media, {} sequences, {} warnings) -> {}",
                input.display(),
                project.tracks.len(),
                project.media.len(),
                project.sequences.len(),
                warnings.len(),
                output.display()
            );
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_project_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "renderly-cli-test-{tag}-{}.renderly.json",
            std::process::id()
        ))
    }

    fn test_project() -> Project {
        Project::new(
            "cli test",
            Settings {
                fps: 30.0,
                width: 1920,
                height: 1080,
                sample_rate: 48000,
                duck_db: -12.0,
            },
        )
    }

    #[test]
    fn save_load_round_trip_preserves_project() {
        let path = temp_project_path("roundtrip");
        let project = test_project();
        save_project(&path, &project).unwrap();
        let loaded = load_project(&path).unwrap();
        assert_eq!(loaded.name, project.name);
        assert_eq!(loaded.schema_version, project.schema_version);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn apply_command_json_matches_apply_subcommand_path() {
        // Same deserialization the `apply` subcommand uses: JSON string -> Command.
        let path = temp_project_path("apply");
        let mut project = test_project();
        let cmd: Command =
            serde_json::from_str(r#"{"command":"AddTrack","kind":"video","name":"V1"}"#).unwrap();
        apply_and_report(&mut project, cmd).unwrap();
        assert_eq!(project.tracks.len(), 1);
        assert_eq!(project.tracks[0].name, "V1");
        save_project(&path, &project).unwrap();
        let reloaded = load_project(&path).unwrap();
        assert_eq!(reloaded.tracks.len(), 1);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn load_project_reports_bad_json() {
        let path = temp_project_path("badjson");
        std::fs::write(&path, "not json").unwrap();
        assert!(load_project(&path).is_err());
        std::fs::remove_file(&path).ok();
    }
}
