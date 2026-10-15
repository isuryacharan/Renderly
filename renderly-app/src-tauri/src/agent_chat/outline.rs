//! Compact, token-cheap views of the open project for the in-app agent: the timeline outline
//! (`timeline` tool and the per-turn context block), plain-language summaries of applied edits,
//! short-id resolution, the asset catalog, and media discovery for `find_media`.
//!
//! Everything here is read-only except [`resolve_short_ids`], which rewrites a command's JSON
//! before the normal command dispatch. Edits themselves still go through `apply_commands`.

use renderly_core::project::{Clip, Id, MediaKind, Project, Track, TrackKind};
use renderly_core::{Command, CommandOutcome};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Characters of outline pushed with every user turn before the rest is left for `timeline`.
pub const CONTEXT_OUTLINE_CHARS: usize = 1800;

/// First eight hex digits of an id: the short form the agent sees and may send back.
pub fn short_id(id: Id) -> String {
    id.to_string()[..8].to_string()
}

/// `m:ss.s` for a time in seconds (`0:05.0`, `1:02.5`).
pub fn fmt_time(secs: f64) -> String {
    let t = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
    let minutes = (t / 60.0).floor();
    let seconds = t - minutes * 60.0;
    format!("{}:{:04.1}", minutes as u64, seconds)
}

fn kind_label(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "video",
        TrackKind::Audio => "audio",
        TrackKind::Caption => "caption",
    }
}

fn media_name(project: &Project, media_id: Id) -> String {
    project
        .find_media(media_id)
        .and_then(|m| m.path.file_stem())
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "missing media".into())
}

/// Finds a clip by id on the root timeline or inside any sequence.
pub fn find_clip(project: &Project, clip_id: Id) -> Option<&Clip> {
    project
        .tracks
        .iter()
        .chain(project.sequences.iter().flat_map(|s| s.tracks.iter()))
        .flat_map(|t| t.clips.iter())
        .find(|c| c.id() == clip_id)
}

fn clip_label(project: &Project, clip_id: Id) -> String {
    match find_clip(project, clip_id) {
        Some(Clip::Caption(c)) => format!("caption '{}'", truncate(&c.text, 40)),
        Some(Clip::Adjustment(_)) => "adjustment layer".into(),
        Some(Clip::Video(c)) | Some(Clip::Audio(c)) => {
            format!("'{}'", media_name(project, c.media_id))
        }
        None => format!("clip {}", short_id(clip_id)),
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn effect_list(effects: &[renderly_core::project::EffectInstance]) -> String {
    if effects.is_empty() {
        return String::new();
    }
    let names: Vec<String> = effects
        .iter()
        .map(|e| {
            let name = e.effect_id.strip_prefix("builtin:").unwrap_or(&e.effect_id);
            let off = if e.enabled { "" } else { " off" };
            format!("{name}:{}{off}", short_id(e.id))
        })
        .collect();
    format!(" fx=[{}]", names.join(", "))
}

/// One line per clip, with the span on the timeline and the source range for media clips.
fn clip_line(project: &Project, clip: &Clip) -> String {
    match clip {
        Clip::Video(c) | Clip::Audio(c) => {
            let len = (c.source_out_secs - c.source_in_secs) / c.speed.max(f64::EPSILON);
            let mut line = format!(
                "  clip {} {} {}-{} src {}-{} '{}'",
                short_id(c.id),
                if matches!(clip, Clip::Video(_)) {
                    "video"
                } else {
                    "audio"
                },
                fmt_time(c.position_secs),
                fmt_time(c.position_secs + len),
                fmt_time(c.source_in_secs),
                fmt_time(c.source_out_secs),
                media_name(project, c.media_id),
            );
            if (c.speed - 1.0).abs() > 1e-9 {
                line.push_str(&format!(" speed {}x", c.speed));
            }
            if !c.enabled {
                line.push_str(" disabled");
            }
            if (c.gain_db).abs() > 1e-9 {
                line.push_str(&format!(" gain {:+.1}dB", c.gain_db));
            }
            if let Some(t) = &c.outgoing_transition {
                line.push_str(&format!(
                    " transition {} {}s",
                    t.kind.as_str(),
                    t.duration_secs
                ));
            }
            if c.mask.is_some() {
                line.push_str(" mask");
            }
            if c.background_removal.is_some() {
                line.push_str(" bg-removal");
            }
            line.push_str(&effect_list(&c.effects));
            line
        }
        Clip::Caption(c) => format!(
            "  clip {} caption {}-{} '{}' style {}",
            short_id(c.id),
            fmt_time(c.position_secs),
            fmt_time(c.position_secs + c.duration_secs),
            truncate(&c.text, 60),
            c.style_id,
        ),
        Clip::Adjustment(c) => format!(
            "  clip {} adjustment {}-{} opacity {}{}",
            short_id(c.id),
            fmt_time(c.position_secs),
            fmt_time(c.position_secs + c.duration_secs),
            c.opacity,
            effect_list(&c.effects),
        ),
    }
}

fn track_block(project: &Project, index: usize, track: &Track) -> String {
    let mut flags = Vec::new();
    if track.muted {
        flags.push("muted");
    }
    if track.locked {
        flags.push("locked");
    }
    if track.hidden {
        flags.push("hidden");
    }
    let mut head = format!(
        "track {} {} '{}' {}",
        index,
        kind_label(track.kind),
        track.name,
        short_id(track.id)
    );
    if !flags.is_empty() {
        head.push_str(&format!(" [{}]", flags.join(",")));
    }
    let mut lines = vec![head];
    let mut clips: Vec<&Clip> = track.clips.iter().collect();
    clips.sort_by(|a, b| {
        clip_start(a)
            .partial_cmp(&clip_start(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for clip in clips {
        lines.push(clip_line(project, clip));
    }
    if track.clips.is_empty() {
        lines.push("  (empty)".into());
    }
    lines.join("\n")
}

fn clip_start(clip: &Clip) -> f64 {
    match clip {
        Clip::Video(c) | Clip::Audio(c) => c.position_secs,
        Clip::Caption(c) => c.position_secs,
        Clip::Adjustment(c) => c.position_secs,
    }
}

/// Duration of the root timeline (the latest clip end), in seconds.
pub fn timeline_duration(tracks: &[Track]) -> f64 {
    tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| match c {
            Clip::Video(m) | Clip::Audio(m) => {
                m.position_secs + (m.source_out_secs - m.source_in_secs) / m.speed.max(f64::EPSILON)
            }
            Clip::Caption(c) => c.position_secs + c.duration_secs,
            Clip::Adjustment(c) => c.position_secs + c.duration_secs,
        })
        .fold(0.0, f64::max)
}

/// The full timeline outline: header, then one block per track in z-order (index 0 is the
/// bottom track). `sequence` selects a nested sequence instead of the root timeline.
pub fn timeline_outline(project: &Project, sequence: Option<Id>) -> String {
    let (tracks, label) =
        match sequence.and_then(|id| project.sequences.iter().find(|s| s.id == id)) {
            Some(seq) => (
                seq.tracks.as_slice(),
                format!("sequence '{}' {}", seq.name, short_id(seq.id)),
            ),
            None => (project.tracks.as_slice(), "root timeline".to_string()),
        };
    let s = &project.settings;
    let mut out = vec![format!(
        "{} ({}): {}x{} {} fps, duration {}, {} tracks",
        project.name,
        label,
        s.width,
        s.height,
        s.fps,
        fmt_time(timeline_duration(tracks)),
        tracks.len(),
    )];
    for (i, track) in tracks.iter().enumerate() {
        out.push(track_block(project, i, track));
    }
    if !project.sequences.is_empty() && sequence.is_none() {
        let seqs: Vec<String> = project
            .sequences
            .iter()
            .map(|s| format!("'{}' {}", s.name, short_id(s.id)))
            .collect();
        out.push(format!("sequences: {}", seqs.join(", ")));
    }
    out.join("\n")
}

/// The per-turn context block prepended to the user's message. Keeps the outline short: when
/// it is too long the block ends with a pointer to the `timeline` tool.
pub fn context_block(
    project: &Project,
    playhead_secs: f64,
    selected_clip_ids: &[String],
    sequence: Option<Id>,
) -> String {
    let outline = timeline_outline(project, sequence);
    let body = if outline.chars().count() > CONTEXT_OUTLINE_CHARS {
        let mut cut = String::new();
        let mut kept_lines = 0usize;
        let total_lines = outline.lines().count();
        for line in outline.lines() {
            if cut.chars().count() + line.chars().count() > CONTEXT_OUTLINE_CHARS {
                break;
            }
            cut.push_str(line);
            cut.push('\n');
            kept_lines += 1;
        }
        format!(
            "{}(outline truncated: {} of {} lines shown; call timeline for the full outline)",
            cut, kept_lines, total_lines
        )
    } else {
        outline
    };
    let mut lines = vec![format!("playhead {}", fmt_time(playhead_secs))];
    if !selected_clip_ids.is_empty() {
        let shorts: Vec<String> = selected_clip_ids
            .iter()
            .map(|id| id.chars().take(8).collect())
            .collect();
        lines.push(format!("selected clips: {}", shorts.join(", ")));
    }
    format!(
        "[Renderly context: live project state, not a message from the user]\n{}\n{}\n[end context]\n\n",
        lines.join("\n"),
        body.trim_end()
    )
}

/// Replaces unique 8+ character hex prefixes of project ids, under keys that hold ids
/// (`*_id`, `*_ids`, `id`), with the full id. Ambiguous prefixes are an error. Anything that
/// matches no id is left alone, so the command parser reports it with its own message.
pub fn resolve_short_ids(value: &mut Value, project: &Project) -> Result<(), String> {
    let ids = project_id_strings(project);
    resolve_in(value, None, &ids)
}

fn project_id_strings(project: &Project) -> Vec<String> {
    let mut set = BTreeSet::new();
    if let Ok(v) = serde_json::to_value(project) {
        collect_uuids(&v, &mut set);
    }
    set.into_iter().collect()
}

fn collect_uuids(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::String(s) if s.len() == 36 && uuid::Uuid::parse_str(s).is_ok() => {
            out.insert(s.to_ascii_lowercase());
        }
        Value::Array(items) => items.iter().for_each(|v| collect_uuids(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_uuids(v, out)),
        _ => {}
    }
}

fn is_id_key(key: Option<&str>) -> bool {
    match key {
        Some(k) => k == "id" || k.ends_with("_id") || k.ends_with("_ids"),
        None => false,
    }
}

fn is_short_id(s: &str) -> bool {
    (8..36).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

fn resolve_in(value: &mut Value, key: Option<&str>, ids: &[String]) -> Result<(), String> {
    match value {
        Value::String(s) if is_id_key(key) && is_short_id(s) => {
            let prefix = s.to_ascii_lowercase();
            let matches: Vec<&String> = ids.iter().filter(|id| id.starts_with(&prefix)).collect();
            match matches.as_slice() {
                [one] => *s = (*one).clone(),
                [] => {}
                many => {
                    return Err(format!(
                        "id prefix '{s}' is ambiguous ({} matches); use more characters",
                        many.len()
                    ))
                }
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                resolve_in(item, key, ids)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (k, v) in map.iter_mut() {
                resolve_in(v, Some(k.as_str()), ids)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn humanize(variant: &str) -> String {
    let mut out = String::new();
    for (i, ch) in variant.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

/// A one-line, plain-language description of an applied command. `before` is the project
/// before the edit, `after` the project after it (names and ranges are read from `after`, falling
/// back to `before` for removed items).
pub fn summarize(
    cmd: &Command,
    before: &Project,
    after: &Project,
    outcome: &CommandOutcome,
) -> String {
    let name = |id: Id| clip_label(after, id).replace("clip ", "");
    let named = |id: Id| {
        let l = clip_label(after, id);
        if l.starts_with("clip ") {
            clip_label(before, id)
        } else {
            l
        }
    };
    match (cmd, outcome) {
        (Command::ImportMedia { path }, _) => {
            let file = Path::new(path)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            format!("Imported '{file}'")
        }
        (Command::AddTrack { kind, name, .. }, _) => {
            format!("Added {} track '{name}'", kind_label(*kind))
        }
        (
            Command::AddClip {
                track_id,
                media_id,
                position_secs,
                ..
            },
            _,
        ) => format!(
            "Added '{}' to {} at {}",
            media_name(after, *media_id),
            track_name(after, *track_id),
            fmt_time(*position_secs)
        ),
        (
            Command::SplitClip {
                clip_id, at_secs, ..
            },
            _,
        ) => {
            format!("Split {} at {}", named(*clip_id), fmt_time(*at_secs))
        }
        (Command::TrimClip { clip_id, .. }, _) => {
            let range = find_clip(after, *clip_id).and_then(|c| match c {
                Clip::Video(m) | Clip::Audio(m) => Some(format!(
                    "{}-{}",
                    fmt_time(m.source_in_secs),
                    fmt_time(m.source_out_secs)
                )),
                _ => None,
            });
            match range {
                Some(r) => format!("Trimmed {} to {r}", named(*clip_id)),
                None => format!("Trimmed {}", named(*clip_id)),
            }
        }
        (
            Command::MoveClip {
                clip_id,
                new_position_secs,
                ..
            },
            _,
        ) => {
            format!(
                "Moved {} to {}",
                name(*clip_id),
                fmt_time(*new_position_secs)
            )
        }
        (Command::DeleteClip { clip_id, .. }, _) => format!("Deleted {}", named(*clip_id)),
        (
            Command::AddCaption {
                text,
                position_secs,
                ..
            },
            _,
        ) => format!(
            "Added caption '{}' at {}",
            truncate(text, 60),
            fmt_time(*position_secs)
        ),
        (Command::SetCaption { clip_id, .. }, _) => format!("Updated {}", named(*clip_id)),
        (
            Command::SetAudioGain {
                clip_id, gain_db, ..
            },
            _,
        ) => {
            format!("Set {} gain to {gain_db:+.1} dB", named(*clip_id))
        }
        (Command::SetClipSpeed { clip_id, speed, .. }, _) => {
            format!("Set {} speed to {speed}x", named(*clip_id))
        }
        (Command::SetClipTransform { clip_id, .. }, _) => {
            format!("Changed position and scale of {}", named(*clip_id))
        }
        (Command::SetClipEffects { clip_id, .. }, _) => {
            format!("Updated effects on {}", named(*clip_id))
        }
        (
            Command::SetClipTransition {
                clip_id,
                transition,
                ..
            },
            _,
        ) => match transition {
            Some(t) => format!(
                "Set {} transition on {}",
                t.kind.as_str().replace('_', " "),
                named(*clip_id)
            ),
            None => format!("Removed transition from {}", named(*clip_id)),
        },
        (Command::SetProjectSettings { .. }, _) => "Changed project size or frame rate".into(),
        (Command::RenameTrack { track_id, name, .. }, _) => {
            format!("Renamed track to '{name}' ({})", short_id(*track_id))
        }
        (other, _) => {
            let json = serde_json::to_value(other).unwrap_or(Value::Null);
            let variant = json
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("Edit");
            format!("Applied {}", humanize(variant))
        }
    }
}

fn track_name(project: &Project, track_id: Id) -> String {
    project
        .tracks
        .iter()
        .chain(project.sequences.iter().flat_map(|s| s.tracks.iter()))
        .find(|t| t.id == track_id)
        .map(|t| format!("'{}'", t.name))
        .unwrap_or_else(|| "a track".into())
}

/// The asset catalog for `assets`: built-in effects with defaults, transitions, caption styles
/// and the packs and plugins this project has loaded.
pub fn assets_text(project: &Project) -> String {
    let mut out = String::from("Built-in effects (id: default params):\n");
    for id in renderly_core::compose::BUILTIN_EFFECT_IDS {
        let params =
            serde_json::to_string(&renderly_core::compose::default_params(id)).unwrap_or_default();
        out.push_str(&format!("- {id} {params}\n"));
    }
    let transitions: Vec<&str> = renderly_core::project::TransitionKind::ALL
        .iter()
        .map(|k| k.as_str())
        .collect();
    out.push_str(&format!("Transitions: {}\n", transitions.join(", ")));
    out.push_str(&format!(
        "Caption styles: {}\n",
        renderly_core::captions::BUILTIN_STYLES.join(", ")
    ));
    let list = |paths: &[PathBuf]| -> String {
        if paths.is_empty() {
            "none".into()
        } else {
            paths
                .iter()
                .map(|p| {
                    p.file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(", ")
        }
    };
    out.push_str(&format!(
        "Loaded asset packs: {}\n",
        list(&project.asset_pack_paths)
    ));
    out.push_str(&format!(
        "Loaded WASM plugins: {}\n",
        list(&project.wasm_plugin_paths)
    ));
    out
}

/// Media files under `dir` with size; durations are probed for the first 20 files only.
pub fn find_media_text(dir: &Path, recursive: bool, max: usize) -> Result<String, String> {
    const VIDEO: &[&str] = &["mp4", "mov", "mkv", "avi", "webm"];
    const AUDIO: &[&str] = &["wav", "mp3", "aac", "flac", "ogg", "m4a"];
    const IMAGE: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];
    if !dir.is_dir() {
        return Err(format!("not a folder: {}", dir.display()));
    }
    let mut found: Vec<PathBuf> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if recursive {
                    stack.push(path);
                }
                continue;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if VIDEO.contains(&ext.as_str())
                || AUDIO.contains(&ext.as_str())
                || IMAGE.contains(&ext.as_str())
            {
                found.push(path);
            }
        }
    }
    found.sort();
    let total = found.len();
    let mut out = format!("{total} media file(s) in {}\n", dir.display());
    for (i, path) in found.iter().take(max).enumerate() {
        let size_mb = std::fs::metadata(path)
            .map(|m| m.len() as f64 / 1_048_576.0)
            .unwrap_or(0.0);
        let mut line = format!("- {} ({size_mb:.1} MB)", path.display());
        if i < 20 {
            if let Ok(p) = renderly_core::media::probe(path) {
                let kind = match p.kind {
                    Some(MediaKind::Video) => "video",
                    Some(MediaKind::Audio) => "audio",
                    Some(MediaKind::Image) => "image",
                    _ => "other",
                };
                let dur = p.duration_secs.map(fmt_time).unwrap_or_else(|| "?".into());
                line.push_str(&format!(" {kind} {dur}"));
                if let (Some(w), Some(h)) = (p.width, p.height) {
                    line.push_str(&format!(" {w}x{h}"));
                }
            }
        }
        out.push_str(&line);
        out.push('\n');
    }
    if total > max {
        out.push_str(&format!("... {} more (raise max)\n", total - max));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderly_core::project::{MediaClip, MediaItem, Settings};

    fn project_with_clip() -> (Project, Id, Id) {
        let mut p = Project::new("Demo", Settings::default());
        let media_id = Id::new_v4();
        p.media.push(MediaItem {
            id: media_id,
            path: PathBuf::from("C:/clips/Gameplay 1.mp4"),
            kind: MediaKind::Video,
            duration_secs: Some(10.0),
            width: Some(1920),
            height: Some(1080),
            fps: Some(60.0),
            sequence_id: None,
        });
        let mut track = Track::new(TrackKind::Video, "V1");
        let clip_id = Id::new_v4();
        track.clips.push(Clip::Video(MediaClip {
            id: clip_id,
            media_id,
            position_secs: 1.0,
            source_in_secs: 0.0,
            source_out_secs: 5.0,
            gain_db: 0.0,
            enabled: true,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            speed: 1.0,
            transform: Default::default(),
            keyframes: vec![],
            effects: vec![],
            outgoing_transition: None,
            mask: None,
            background_removal: None,
            audio_denoise: None,
            multicam_group_id: None,
        }));
        p.tracks.push(track);
        (p, media_id, clip_id)
    }

    #[test]
    fn outline_lists_tracks_clips_and_short_ids() {
        let (p, _, clip_id) = project_with_clip();
        let text = timeline_outline(&p, None);
        assert!(text.contains("track 0 video 'V1'"));
        assert!(text.contains(&format!(
            "clip {} video 0:01.0-0:06.0 src 0:00.0-0:05.0 'Gameplay 1'",
            short_id(clip_id)
        )));
        assert!(text.contains("1920x1080") || text.contains("1080x1920"));
    }

    #[test]
    fn short_id_resolves_when_unique_and_errors_when_ambiguous() {
        let (p, _, clip_id) = project_with_clip();
        let short = short_id(clip_id);
        let mut cmd = serde_json::json!({"command": "TrimClip", "track_id": p.tracks[0].id.to_string(), "clip_id": short, "new_source_out_secs": 3.0});
        resolve_short_ids(&mut cmd, &p).expect("unique prefix resolves");
        assert_eq!(cmd["clip_id"], clip_id.to_string());

        // Two different ids sharing an 8-character prefix are ambiguous.
        let mut other = Project::new("x", Settings::default());
        let a = Id::from_u128(0x1234_5678_0000_0000_0000_0000_0000_0001);
        let b = Id::from_u128(0x1234_5678_0000_0000_0000_0000_0000_0002);
        other.tracks.push(Track::new(TrackKind::Video, "a"));
        other.tracks[0]
            .clips
            .push(Clip::Adjustment(renderly_core::project::AdjustmentClip {
                id: a,
                ..renderly_core::project::AdjustmentClip::new(0.0, 1.0)
            }));
        other.tracks[0]
            .clips
            .push(Clip::Adjustment(renderly_core::project::AdjustmentClip {
                id: b,
                ..renderly_core::project::AdjustmentClip::new(2.0, 1.0)
            }));
        let mut amb = serde_json::json!({"clip_id": "12345678"});
        let err = resolve_short_ids(&mut amb, &other).unwrap_err();
        assert!(err.contains("ambiguous"));
    }

    #[test]
    fn unknown_short_id_is_left_for_the_parser() {
        let (p, _, _) = project_with_clip();
        let mut cmd = serde_json::json!({"clip_id": "deadbeef"});
        resolve_short_ids(&mut cmd, &p).unwrap();
        assert_eq!(cmd["clip_id"], "deadbeef");
    }

    #[test]
    fn non_id_keys_are_never_rewritten() {
        let (p, _, _) = project_with_clip();
        let id = p.tracks[0].id.to_string();
        let prefix = &id[..8];
        let mut cmd = serde_json::json!({"name": prefix, "text": prefix});
        resolve_short_ids(&mut cmd, &p).unwrap();
        assert_eq!(cmd["name"], prefix);
    }

    #[test]
    fn summaries_name_the_clip_and_its_new_range() {
        let (p, _, clip_id) = project_with_clip();
        let cmd = Command::TrimClip {
            track_id: p.tracks[0].id,
            clip_id,
            new_source_in_secs: None,
            new_source_out_secs: Some(5.0),
        };
        let s = summarize(&cmd, &p, &p, &CommandOutcome::Applied);
        assert_eq!(s, "Trimmed 'Gameplay 1' to 0:00.0-0:05.0");
    }

    #[test]
    fn context_block_truncates_long_outlines_with_a_pointer() {
        let mut p = Project::new("Big", Settings::default());
        for i in 0..200 {
            p.tracks
                .push(Track::new(TrackKind::Video, format!("Track {i}")));
        }
        let block = context_block(&p, 1.5, &[], None);
        assert!(block.contains("call timeline for the full outline"));
        assert!(block.starts_with("[Renderly context"));
        assert!(block.chars().count() < CONTEXT_OUTLINE_CHARS + 400);
    }

    #[test]
    fn typical_project_context_block_is_compact() {
        // Two video clips, a caption track with three captions: a small real-world edit.
        let (mut p, _, _) = project_with_clip();
        let media_id = p.media[0].id;
        let mut second = Track::new(TrackKind::Video, "V2");
        for i in 0..2 {
            second
                .clips
                .push(Clip::Video(renderly_core::project::MediaClip {
                    position_secs: 6.0 + i as f64 * 4.0,
                    ..match p.tracks[0].clips[0].clone() {
                        Clip::Video(c) => c,
                        _ => unreachable!("fixture clip is a video clip"),
                    }
                }));
            if let Some(Clip::Video(m)) = second.clips.last_mut() {
                m.id = Id::new_v4();
                m.media_id = media_id;
            }
        }
        let mut captions = Track::new(TrackKind::Caption, "Captions");
        for (i, text) in ["Hello there", "Watch this", "Subscribe"]
            .iter()
            .enumerate()
        {
            captions
                .clips
                .push(Clip::Caption(renderly_core::project::CaptionClip {
                    id: Id::new_v4(),
                    text: (*text).into(),
                    position_secs: i as f64 * 2.0,
                    duration_secs: 2.0,
                    style_id: "tiktok-box".into(),
                }));
        }
        p.tracks.push(second);
        p.tracks.push(captions);
        let block = context_block(&p, 2.5, &[], None);
        eprintln!("typical context block: {} chars", block.len());
        assert!(block.len() < 1200, "context block is {} chars", block.len());
        assert!(block.contains("track 2 caption 'Captions'"));
    }

    #[test]
    fn assets_lists_builtin_effects_and_transitions() {
        let (p, _, _) = project_with_clip();
        let text = assets_text(&p);
        assert!(text.contains("builtin:"));
        assert!(text.contains("crossfade"));
    }

    #[test]
    fn fmt_time_formats_minutes_and_fraction() {
        assert_eq!(fmt_time(5.0), "0:05.0");
        assert_eq!(fmt_time(62.5), "1:02.5");
        assert_eq!(fmt_time(-3.0), "0:00.0");
    }
}
