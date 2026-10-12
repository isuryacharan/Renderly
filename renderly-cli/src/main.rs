//! Thin CLI over renderly-core's command API (docs/command-api.md). Every subcommand here
//! must go through `renderly_core::apply_command` — see AGENTS.md §0.1.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use renderly_core::commands::{Command, ExportPreset};
use renderly_core::export::export_project_with_progress;
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
