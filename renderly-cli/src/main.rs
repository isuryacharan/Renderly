//! Thin CLI over renderly-core's command API (docs/command-api.md). Every subcommand here
//! must go through `renderly_core::apply_command` — see AGENTS.md §0.1.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use renderly_core::commands::{Command, ExportPreset};
use renderly_core::edit_contract::{
    plan_write, AllowDeletion, Caller, WriteLedger, WriteOptions, WriteResult,
};
use renderly_core::export::export_project_with_progress;
use renderly_core::otio::{
    export_otio_with_options, import_otio, probe_media, ExportOptions, ImportOptions,
};
use renderly_core::project::{Project, Settings};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

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
        #[command(flatten)]
        write: WriteFlags,
    },
    /// Apply a JSON array of commands from a script file, in order, and save the result. All or
    /// nothing: if any command fails, the file is not written.
    ApplyScript {
        /// Path to the project JSON file (modified in place).
        path: PathBuf,
        /// Path to a JSON file containing an array of Command objects.
        script: PathBuf,
        #[command(flatten)]
        write: WriteFlags,
    },
    /// Print the project as pretty JSON, with its project_revision.
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
    /// Work with declarative effect/asset packs (docs/effect-pack-format.md) and the user
    /// library they install into (docs/community-store.md).
    Pack {
        /// The library folder to use. Default: the user library, or RENDERLY_LIBRARY_DIR.
        #[arg(long, global = true, value_name = "DIR")]
        library: Option<PathBuf>,
        #[command(subcommand)]
        action: PackCommand,
    },
}

#[derive(Subcommand)]
enum PackCommand {
    /// Load a pack folder (the one holding pack.json) and report every item as ok, skipped,
    /// disabled or error, with its reason and content hash. Exits non-zero if the pack is
    /// refused or any item failed to load.
    Validate {
        /// The pack folder.
        dir: PathBuf,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Build a pack folder into a reproducible .rpack archive (same source, same bytes). Refuses
    /// a pack that does not load or has an item that fails to load.
    Build {
        /// The pack folder (the one holding pack.json).
        dir: PathBuf,
        /// The .rpack file to write, or a folder to write <pack-id>-<release>.rpack into.
        /// Default: the current folder.
        #[arg(short, long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install a .rpack file or a pack folder into the library. Exits non-zero when refused: a
    /// pack that does not load, a different file under the same release, or an item version
    /// already published with other content. Items that fail are installed and reported.
    Install {
        /// A .rpack file or a pack folder.
        source: PathBuf,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Remove an installed pack: every release, or only --release. Copies are deleted; a linked
    /// dev folder is only forgotten, never deleted.
    Uninstall {
        /// The pack id, such as mira.film-looks or local.starter.
        pack_id: String,
        /// Only this release.
        #[arg(long)]
        release: Option<String>,
        /// Print the removed releases as JSON.
        #[arg(long)]
        json: bool,
    },
    /// List the installed packs (and the builtin pack) with each one's items and their states.
    List {
        /// Print the list as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Link a pack folder for development: nothing is copied, and every load re-reads the folder.
    Link {
        /// The pack folder (the one holding pack.json).
        dir: PathBuf,
        /// Print the report as JSON.
        #[arg(long)]
        json: bool,
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

fn open_library(library: &Option<PathBuf>) -> Result<renderly_core::library::Library> {
    let root = library
        .clone()
        .unwrap_or_else(renderly_core::library::default_root);
    Ok(renderly_core::library::Library::open(&root)?)
}

fn print_install(
    report: &renderly_core::library::InstallReport,
    verb: &str,
    json: bool,
) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    let headline = if report.already_installed {
        "already installed"
    } else {
        verb
    };
    println!(
        "{headline} {} {} \"{}\" (format {}, from {})",
        report.pack_id, report.release, report.name, report.format, report.source
    );
    if !report.items_added.is_empty() {
        let counts: Vec<String> = report
            .items_added
            .iter()
            .map(|(kind, n)| format!("{n} {kind}"))
            .collect();
        println!("  items: {}", counts.join(", "));
    }
    for item in report.items.iter().filter(|i| i.state != "ok") {
        let mut line = format!(
            "  {:<9} {}/{}@{}",
            item.state, item.kind, item.id, item.version
        );
        if let Some(reason) = &item.reason {
            line.push_str(": ");
            line.push_str(reason);
        }
        println!("{line}");
    }
    if !report.other_releases.is_empty() {
        println!(
            "  other releases kept: {}",
            report.other_releases.join(", ")
        );
    }
    Ok(())
}

fn print_library(installs: &[renderly_core::library::Install]) {
    for install in installs {
        let state = if install.enabled {
            "enabled"
        } else {
            "disabled"
        };
        let source = if install.is_dev() {
            "dev link"
        } else {
            install.source.as_str()
        };
        println!(
            "{} {} {state} ({source}, format {})",
            install.pack_id, install.release, install.format
        );
        if let Some(error) = &install.error {
            println!("  error: {error}");
        }
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for item in &install.items {
            *counts.entry(item.state.as_str()).or_insert(0) += 1;
        }
        let summary: Vec<String> = counts.iter().map(|(s, n)| format!("{n} {s}")).collect();
        if !summary.is_empty() {
            println!("  items: {}", summary.join(", "));
        }
        for item in install.items.iter().filter(|i| i.state != "ok") {
            println!(
                "    {:<9} {}/{}@{}: {}",
                item.state,
                item.kind,
                item.id,
                item.version,
                item.reason.as_deref().unwrap_or("")
            );
        }
    }
}

fn run_pack(library: Option<PathBuf>, action: PackCommand) -> Result<()> {
    match action {
        PackCommand::Validate { dir, json } => {
            let pack = renderly_core::packs::load_pack(&dir)
                .map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
            let report = renderly_core::packs::describe::pack_report(&pack);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "pack {} \"{}\" (format {}, release {}, license {})",
                    report.id,
                    report.name,
                    report.format,
                    report.release.as_deref().unwrap_or("-"),
                    report.license.as_deref().unwrap_or("-")
                );
                for item in &report.items {
                    let mut line = format!(
                        "  {:<9} {:<13} {}@{} \"{}\"",
                        item.state, item.kind, item.id, item.version, item.label
                    );
                    if let Some(reason) = &item.reason {
                        line.push_str(": ");
                        line.push_str(reason);
                    }
                    println!("{line}");
                    if let Some(hash) = &item.hash {
                        println!("{:>14}sha256 {hash}", "");
                    }
                }
                let count = |state: &str| report.items.iter().filter(|i| i.state == state).count();
                println!(
                    "{} items: {} ok, {} disabled, {} skipped, {} error",
                    report.items.len(),
                    count("ok"),
                    count("disabled"),
                    count("skipped"),
                    count("error")
                );
            }
            let failed = report.items.iter().filter(|i| i.state == "error").count();
            if failed > 0 {
                bail!("{failed} item(s) failed to load");
            }
            Ok(())
        }
        PackCommand::Build { dir, out, json } => {
            let out = out.unwrap_or_else(|| PathBuf::from("."));
            let report = renderly_core::packs::build(&dir, &out)
                .map_err(|e| anyhow::anyhow!("{}: {e}", dir.display()))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!(
                    "wrote {} ({} bytes, {} files)",
                    report.path.display(),
                    report.bytes,
                    report.files
                );
                println!("sha256 {}", report.sha256);
            }
            Ok(())
        }
        PackCommand::Install { source, json } => {
            let mut lib = open_library(&library)?;
            let report = lib.install(&source)?;
            print_install(&report, "installed", json)
        }
        PackCommand::Link { dir, json } => {
            let mut lib = open_library(&library)?;
            let report = lib.link_dev(&dir)?;
            print_install(&report, "linked", json)
        }
        PackCommand::Uninstall {
            pack_id,
            release,
            json,
        } => {
            let mut lib = open_library(&library)?;
            let removed = lib.uninstall(&pack_id, release.as_deref())?;
            if json {
                println!("{}", serde_json::to_string_pretty(&removed)?);
            } else {
                for pair in &removed {
                    println!("removed {pair}");
                }
            }
            Ok(())
        }
        PackCommand::List { json } => {
            let lib = open_library(&library)?;
            let installs = lib.list();
            if json {
                println!("{}", serde_json::to_string_pretty(&installs)?);
            } else {
                print_library(&installs);
            }
            Ok(())
        }
    }
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

/// The edit-contract envelope flags, shared by `apply` and `apply-script`.
#[derive(clap::Args, Debug, Clone, Default)]
struct WriteFlags {
    /// Check the command(s) and report what would change, without writing the project.
    #[arg(long)]
    dry_run: bool,
    /// The project_revision you last read (`renderly show` prints it). If the project has moved
    /// on, nothing is applied and the error says what the current revision is.
    #[arg(long, value_name = "REVISION")]
    expected_revision: Option<u64>,
    /// Idempotency key. Retrying the same id with the same command(s) returns the original result.
    #[arg(long, value_name = "ID")]
    request_id: Option<String>,
    /// Why clips, tracks, media or sequences may be deleted. Required with --allow-deletion-id.
    #[arg(long, value_name = "TEXT")]
    allow_deletion_reason: Option<String>,
    /// A full id this write may delete. Repeat for each id; the list must match a --dry-run's
    /// `changed.deleted` exactly.
    #[arg(long = "allow-deletion-id", value_name = "UUID")]
    allow_deletion_ids: Vec<String>,
}

impl WriteFlags {
    fn options(&self) -> Result<WriteOptions> {
        let ids = self
            .allow_deletion_ids
            .iter()
            .map(|raw| {
                Uuid::parse_str(raw.trim())
                    .with_context(|| format!("--allow-deletion-id '{raw}' is not a full id"))
            })
            .collect::<Result<Vec<_>>>()?;
        let allow_deletion = match (&self.allow_deletion_reason, ids.is_empty()) {
            (None, true) => None,
            (reason, _) => Some(AllowDeletion {
                reason: reason.clone().unwrap_or_default(),
                ids,
            }),
        };
        Ok(WriteOptions {
            expected_revision: self.expected_revision,
            dry_run: self.dry_run,
            request_id: self.request_id.clone(),
            allow_deletion,
        })
    }
}

/// The headless state kept next to a project file: its revision and recent request ids. The
/// project JSON itself has no revision, so each CLI call reads this to continue the count.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CliState {
    revision: u64,
    ledger: WriteLedger,
}

fn state_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".state.json");
    PathBuf::from(name)
}

fn load_state(path: &std::path::Path) -> Result<CliState> {
    let file = state_path(path);
    match fs::read_to_string(&file) {
        Ok(data) => serde_json::from_str(&data)
            .with_context(|| format!("reading revision state {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(CliState::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

fn save_state(path: &std::path::Path, state: &CliState) -> Result<()> {
    let file = state_path(path);
    let data = serde_json::to_string_pretty(state)?;
    fs::write(&file, data).with_context(|| format!("writing {}", file.display()))
}

/// Check and apply `commands` to the project at `path` through the edit contract (the same one
/// the app and MCP use), then save the project when something changed.
/// Returns the contract result and the outcome of each command (for example the ids created).
fn run_write(
    path: &PathBuf,
    commands: Vec<Command>,
    flags: &WriteFlags,
) -> Result<(WriteResult, Vec<String>)> {
    let options = flags.options()?;
    let project = load_project(path)?;
    let mut state = load_state(path)?;
    // CLI calls are automation, so the deletion guard applies (`Caller::Agent`).
    let mut plan = plan_write(
        &project,
        state.revision,
        &state.ledger,
        &options,
        Caller::Agent,
        commands,
    )
    .map_err(|e| anyhow::anyhow!("{}", serde_json::to_string_pretty(&e).unwrap_or_default()))?;

    let outcomes: Vec<String> = plan.outcomes().iter().map(|o| format!("{o:?}")).collect();
    let committed_at = match plan.take_next() {
        Some(next) => {
            save_project(path, &next)?;
            state.revision += 1;
            Some(state.revision)
        }
        None => None,
    };
    let result = plan.finish(committed_at, &mut state.ledger);
    if committed_at.is_some() {
        save_state(path, &state)?;
    }
    Ok((result, outcomes))
}

fn print_write_result(result: &WriteResult, outcomes: &[String]) -> Result<()> {
    let mut value = serde_json::to_value(result)?;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("outcomes".into(), serde_json::json!(outcomes));
    }
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        CliCommand::Models { action } => run_models(action)?,
        CliCommand::Pack { library, action } => run_pack(library, action)?,
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
        CliCommand::Apply {
            path,
            command_json,
            write,
        } => {
            let cmd: Command =
                serde_json::from_str(&command_json).context("parsing command JSON")?;
            let (result, outcomes) = run_write(&path, vec![cmd], &write)?;
            print_write_result(&result, &outcomes)?;
        }
        CliCommand::ApplyScript {
            path,
            script,
            write,
        } => {
            let script_data = fs::read_to_string(&script)
                .with_context(|| format!("reading {}", script.display()))?;
            let cmds: Vec<Command> =
                serde_json::from_str(&script_data).context("parsing script JSON")?;
            // The whole script is one batch: any failing command applies nothing, and the file
            // is only written when every command succeeded (docs/command-api.md).
            let (result, outcomes) = run_write(&path, cmds, &write)?;
            print_write_result(&result, &outcomes)?;
        }
        CliCommand::Show { path } => {
            let project = load_project(&path)?;
            let state = load_state(&path)?;
            // The project JSON, plus `project_revision` at its top level (as renderly-mcp returns).
            let mut shown = serde_json::to_value(&project)?;
            if let Some(obj) = shown.as_object_mut() {
                obj.insert("project_revision".into(), serde_json::json!(state.revision));
            }
            println!("{}", serde_json::to_string_pretty(&shown)?);
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

    fn fixture_pack(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../renderly-core/tests/fixtures/packs")
            .join(name)
    }

    #[test]
    fn pack_validate_passes_a_clean_pack_and_fails_one_with_a_broken_item() {
        // starter-v1 is format 1 and loads cleanly.
        assert!(run_pack(
            None,
            PackCommand::Validate {
                dir: fixture_pack("starter-v1"),
                json: true,
            }
        )
        .is_ok());
        // looks-v2 has a WGSL item that fails to load, so validation exits non-zero.
        assert!(run_pack(
            None,
            PackCommand::Validate {
                dir: fixture_pack("looks-v2"),
                json: false,
            }
        )
        .is_err());
        assert!(run_pack(
            None,
            PackCommand::Validate {
                dir: fixture_pack("no-such-pack"),
                json: false,
            }
        )
        .is_err());
    }

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

    fn cmd(json: &str) -> Command {
        serde_json::from_str(json).unwrap()
    }

    fn fresh_project(tag: &str) -> PathBuf {
        let path = temp_project_path(tag);
        save_project(&path, &test_project()).unwrap();
        path
    }

    fn cleanup(path: &PathBuf) {
        std::fs::remove_file(path).ok();
        std::fs::remove_file(state_path(path)).ok();
    }

    #[test]
    fn apply_command_json_matches_apply_subcommand_path() {
        // Same deserialization the `apply` subcommand uses: JSON string -> Command.
        let path = fresh_project("apply");
        let (result, _) = run_write(
            &path,
            vec![cmd(r#"{"command":"AddTrack","kind":"video","name":"V1"}"#)],
            &WriteFlags::default(),
        )
        .unwrap();
        assert!(result.committed);
        assert_eq!(result.revision, 1);
        let reloaded = load_project(&path).unwrap();
        assert_eq!(reloaded.tracks.len(), 1);
        assert_eq!(reloaded.tracks[0].name, "V1");
        cleanup(&path);
    }

    #[test]
    fn dry_run_prints_the_result_and_leaves_the_file_and_revision_alone() {
        let path = fresh_project("dry");
        let before = std::fs::read(&path).unwrap();
        let flags = WriteFlags {
            dry_run: true,
            ..WriteFlags::default()
        };
        let (result, _) = run_write(
            &path,
            vec![cmd(r#"{"command":"AddTrack","kind":"video","name":"V1"}"#)],
            &flags,
        )
        .unwrap();
        assert!(!result.committed && result.dry_run);
        assert!(!result.patch.0.is_empty());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(load_state(&path).unwrap().revision, 0);
        cleanup(&path);
    }

    #[test]
    fn expected_revision_must_match_the_saved_revision() {
        let path = fresh_project("expected");
        let add = || cmd(r#"{"command":"AddTrack","kind":"video","name":"V1"}"#);
        run_write(&path, vec![add()], &WriteFlags::default()).unwrap();
        // Revision is now 1; a write that read revision 0 is stale.
        let stale = WriteFlags {
            expected_revision: Some(0),
            ..WriteFlags::default()
        };
        let err = run_write(&path, vec![add()], &stale).unwrap_err();
        assert!(err.to_string().contains("revision_conflict"), "{err}");
        assert_eq!(load_project(&path).unwrap().tracks.len(), 1);
        let fresh = WriteFlags {
            expected_revision: Some(1),
            ..WriteFlags::default()
        };
        assert!(run_write(&path, vec![add()], &fresh).is_ok());
        cleanup(&path);
    }

    #[test]
    fn request_id_retry_is_not_applied_twice() {
        let path = fresh_project("request");
        let flags = WriteFlags {
            request_id: Some("r-1".into()),
            ..WriteFlags::default()
        };
        let add = || cmd(r#"{"command":"AddTrack","kind":"video","name":"V1"}"#);
        run_write(&path, vec![add()], &flags).unwrap();
        let (replay, _) = run_write(&path, vec![add()], &flags).unwrap();
        assert!(replay.replayed);
        assert_eq!(load_project(&path).unwrap().tracks.len(), 1);
        cleanup(&path);
    }

    #[test]
    fn deletion_needs_reason_and_exact_ids() {
        let path = fresh_project("delete");
        run_write(
            &path,
            vec![cmd(r#"{"command":"CreateSequence","name":"Inner"}"#)],
            &WriteFlags::default(),
        )
        .unwrap();
        let project = load_project(&path).unwrap();
        let sequence = project.sequences[0].id.to_string();
        let delete = || {
            cmd(&format!(
                r#"{{"command":"DeleteSequence","sequence_id":"{sequence}"}}"#
            ))
        };

        // No allow flags: refused, with the error naming the deletion rule.
        let err = run_write(&path, vec![delete()], &WriteFlags::default()).unwrap_err();
        assert!(err.to_string().contains("deletion_not_allowed"), "{err}");

        // The sequence's own id alone misses its media item: still refused.
        let partial = WriteFlags {
            allow_deletion_reason: Some("cleanup".into()),
            allow_deletion_ids: vec![sequence.clone()],
            ..WriteFlags::default()
        };
        assert!(run_write(&path, vec![delete()], &partial).is_err());

        // The exact set (the sequence and its media item) is accepted.
        let media = project
            .media
            .iter()
            .find(|m| m.sequence_id.is_some())
            .unwrap()
            .id;
        let exact = WriteFlags {
            allow_deletion_reason: Some("cleanup".into()),
            allow_deletion_ids: vec![sequence.clone(), media.to_string()],
            ..WriteFlags::default()
        };
        let (result, _) = run_write(&path, vec![delete()], &exact).unwrap();
        assert!(result.committed);
        assert!(load_project(&path).unwrap().sequences.is_empty());
        cleanup(&path);
    }

    #[test]
    fn write_flags_reject_a_malformed_deletion_id() {
        let flags = WriteFlags {
            allow_deletion_ids: vec!["not-an-id".into()],
            ..WriteFlags::default()
        };
        assert!(flags.options().is_err());
    }

    #[test]
    fn load_project_reports_bad_json() {
        let path = temp_project_path("badjson");
        std::fs::write(&path, "not json").unwrap();
        assert!(load_project(&path).is_err());
        std::fs::remove_file(&path).ok();
    }
}
