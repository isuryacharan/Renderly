//! Compact, token-cheap views of the open project for the in-app agent: the timeline outline
//! (`timeline` tool and the per-turn context block), plain-language summaries of applied edits,
//! short-id resolution, the asset catalog, and media discovery for `find_media`.
//!
//! Everything here is read-only except [`resolve_short_ids`], which rewrites a command's JSON
//! before the normal command dispatch. Edits themselves still go through `apply_commands`.

use renderly_core::project::{
    BlendMode, Clip, EffectInstance, EffectStatus, Id, MediaKind, Project, Track, TrackKind,
};
use renderly_core::{Command, CommandOutcome};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

/// Characters of outline pushed with every user turn before the rest is left for `timeline`.
pub const CONTEXT_OUTLINE_CHARS: usize = 1800;

/// First eight hex digits of an id: the short form the agent sees and may send back.
pub fn short_id(id: Id) -> String {
    id.to_string()[..8].to_string()
}

/// `m:ss.s` for a time in seconds (`0:05.0`, `1:02.5`).
pub fn fmt_time(us: i64) -> String {
    let secs = us as f64 / 1e6;
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
        Some(Clip::Caption(c)) => format!("caption '{}'", truncate(&c.body.text, 40)),
        Some(Clip::Text(t)) => format!("text '{}'", truncate(&t.body.text, 40)),
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

/// Effects and transitions that are not `ok`, by effect instance id (or clip id for a
/// transition): the outline marks them so the agent sees a missing effect.
type Problems = HashMap<Id, EffectStatus>;

fn problems_of(project: &Project) -> Problems {
    renderly_core::packs::status::installed_problem_effects(project)
        .into_iter()
        .map(|p| (p.instance_id.unwrap_or(p.clip_id), p.status))
        .collect()
}

/// `blur`, `print` (a pack item shows as `<pack>/<item>`): the ref without prefix and version.
fn short_ref(item_ref: &str) -> String {
    let rest = item_ref
        .strip_prefix("builtin:")
        .or_else(|| item_ref.strip_prefix("pack:"))
        .unwrap_or(item_ref);
    rest.split('@').next().unwrap_or(rest).to_string()
}

fn effect_label(e: &EffectInstance, problems: &Problems) -> String {
    let mut label = format!("{}:{}", short_ref(&e.item_ref), short_id(e.id));
    if !e.enabled {
        label.push_str(" off");
    }
    if let Some(status) = problems.get(&e.id) {
        label.push_str(&format!(" {}", status.as_str()));
    }
    label
}

/// ` fx=[filter print:ab12cd34, key chroma_key:..., blur:..., ...]`: slots in render order,
/// then the effects list.
fn effect_list(stack: &[(&'static str, &EffectInstance)], problems: &Problems) -> String {
    if stack.is_empty() {
        return String::new();
    }
    let names: Vec<String> = stack
        .iter()
        .map(|(slot, e)| match *slot {
            "effects" => effect_label(e, problems),
            slot => format!("{slot} {}", effect_label(e, problems)),
        })
        .collect();
    format!(" fx=[{}]", names.join(", "))
}

fn blend_note(mode: BlendMode) -> String {
    if mode.is_normal() {
        String::new()
    } else {
        format!(" blend {}", mode.as_str())
    }
}

/// ` style tiktok-box`, with the status when the style is not installed. `id` is the clip (or
/// track) the problem entry is keyed by.
fn style_note(item_ref: &str, id: Id, problems: &Problems) -> String {
    let mut note = format!(" style {}", short_ref(item_ref));
    if let Some(status) = problems.get(&id) {
        note.push_str(&format!(" {}", status.as_str()));
    }
    note
}

/// ` anim in:fade-up,active:word-highlight` (stored; rendered from the captions phase).
fn animation_note(animations: &renderly_core::project::TextAnimations) -> String {
    let parts: Vec<String> = animations
        .entries()
        .into_iter()
        .map(|(phase, a)| format!("{phase}:{}", short_ref(&a.item_ref)))
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" anim {}", parts.join(","))
    }
}

/// One line per clip, with the span on the timeline and the source range for media clips.
fn clip_line(project: &Project, clip: &Clip, problems: &Problems) -> String {
    match clip {
        Clip::Video(c) | Clip::Audio(c) => {
            let len = ((c.source_out_us - c.source_in_us) as f64 / c.speed.max(f64::EPSILON))
                .round() as i64;
            let mut line = format!(
                "  clip {} {} {}-{} src {}-{} '{}'",
                short_id(c.id),
                if matches!(clip, Clip::Video(_)) {
                    "video"
                } else {
                    "audio"
                },
                fmt_time(c.start_us),
                fmt_time(c.start_us + len),
                fmt_time(c.source_in_us),
                fmt_time(c.source_out_us),
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
                    short_ref(&t.item_ref),
                    renderly_core::time::us_to_seconds(t.duration_us)
                ));
                if let Some(status) = problems.get(&c.id) {
                    line.push_str(&format!(" {}", status.as_str()));
                }
            }
            if c.mask.is_some() {
                line.push_str(" mask");
            }
            if c.background_removal.is_some() {
                line.push_str(" bg-removal");
            }
            line.push_str(&blend_note(c.blend_mode));
            line.push_str(&effect_list(&c.slot_entries(), problems));
            line
        }
        Clip::Caption(c) => {
            let mut line = format!(
                "  clip {} caption {}-{} '{}'",
                short_id(c.id),
                fmt_time(c.start_us),
                fmt_time(c.start_us + c.duration_us),
                truncate(&c.body.text, 60),
            );
            if !c.body.words.is_empty() {
                let marked = c.body.words.iter().filter(|w| w.emphasis).count();
                line.push_str(&format!(" words {}", c.body.words.len()));
                if marked > 0 {
                    line.push_str(&format!(" ({marked} emphasised)"));
                }
            }
            match &c.style {
                Some(style) => line.push_str(&style_note(&style.item_ref, c.id, problems)),
                None => line.push_str(" style inherit"),
            }
            line.push_str(&animation_note(&c.animations));
            line
        }
        Clip::Text(t) => {
            let mut line = format!(
                "  clip {} text {}-{} '{}'",
                short_id(t.id),
                fmt_time(t.start_us),
                fmt_time(t.start_us + t.duration_us),
                truncate(&t.body.text, 60),
            );
            line.push_str(&style_note(&t.style.item_ref, t.id, problems));
            line.push_str(&animation_note(&t.animations));
            if !t.enabled {
                line.push_str(" disabled");
            }
            if let Some(tr) = &t.outgoing_transition {
                line.push_str(&format!(
                    " transition {} {}s",
                    short_ref(&tr.item_ref),
                    renderly_core::time::us_to_seconds(tr.duration_us)
                ));
            }
            if !t.transform.is_identity() {
                line.push_str(" transformed");
            }
            if t.mask.is_some() {
                line.push_str(" mask");
            }
            line.push_str(&blend_note(t.blend_mode));
            line.push_str(&effect_list(&t.slot_entries(), problems));
            line
        }
        Clip::Adjustment(c) => format!(
            "  clip {} adjustment {}-{} opacity {}{}{}",
            short_id(c.id),
            fmt_time(c.start_us),
            fmt_time(c.start_us + c.duration_us),
            c.opacity,
            blend_note(c.blend_mode),
            effect_list(&c.slot_entries(), problems),
        ),
    }
}

fn track_block(project: &Project, index: usize, track: &Track, problems: &Problems) -> String {
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
    if track.magnetic {
        flags.push("magnetic");
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
    if let Some(style) = &track.caption_style {
        head.push_str(&style_note(&style.item_ref, track.id, problems).replacen(
            " style",
            " caption style",
            1,
        ));
    }
    let mut lines = vec![head];
    let mut clips: Vec<&Clip> = track.clips.iter().collect();
    clips.sort_by(|a, b| {
        clip_start(a)
            .partial_cmp(&clip_start(b))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for clip in clips {
        lines.push(clip_line(project, clip, problems));
    }
    if track.clips.is_empty() {
        lines.push("  (empty)".into());
    }
    lines.join("\n")
}

fn clip_start(clip: &Clip) -> i64 {
    match clip {
        Clip::Video(c) | Clip::Audio(c) => c.start_us,
        Clip::Caption(c) => c.start_us,
        Clip::Text(c) => c.start_us,
        Clip::Adjustment(c) => c.start_us,
    }
}

/// Duration of the root timeline (the latest clip end), in µs.
pub fn timeline_duration(tracks: &[Track]) -> i64 {
    tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| match c {
            Clip::Video(m) | Clip::Audio(m) => {
                m.start_us
                    + (((m.source_out_us - m.source_in_us) as f64) / m.speed.max(f64::EPSILON))
                        .round() as i64
            }
            Clip::Caption(c) => c.start_us + c.duration_us,
            Clip::Text(c) => c.start_us + c.duration_us,
            Clip::Adjustment(c) => c.start_us + c.duration_us,
        })
        .fold(0, i64::max)
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
        s.fps.as_f64(),
        fmt_time(timeline_duration(tracks)),
        tracks.len(),
    )];
    let problems = problems_of(project);
    for (i, track) in tracks.iter().enumerate() {
        out.push(track_block(project, i, track, &problems));
    }
    if !project.sequences.is_empty() && sequence.is_none() {
        let seqs: Vec<String> = project
            .sequences
            .iter()
            .map(|s| format!("'{}' {}", s.name, short_id(s.id)))
            .collect();
        out.push(format!("sequences: {}", seqs.join(", ")));
    }
    if sequence.is_none() {
        if let Some(line) = markers_line(project) {
            out.push(line);
        }
        out.extend(transcript_lines(project));
    }
    out.join("\n")
}

/// `markers: 0:01.0 'drop', 0:02.5 beat, ...` (at most 12, then a count).
fn markers_line(project: &Project) -> Option<String> {
    if project.markers.is_empty() {
        return None;
    }
    const SHOWN: usize = 12;
    let mut items: Vec<String> = project
        .markers
        .iter()
        .take(SHOWN)
        .map(|m| {
            if m.label.is_empty() {
                format!("{} {}", fmt_time(m.time_us), m.kind.as_str())
            } else {
                format!("{} '{}'", fmt_time(m.time_us), truncate(&m.label, 24))
            }
        })
        .collect();
    if project.markers.len() > SHOWN {
        items.push(format!("... {} more", project.markers.len() - SHOWN));
    }
    Some(format!(
        "markers ({}): {}",
        project.markers.len(),
        items.join(", ")
    ))
}

/// `transcript 3f2a... of 'interview': 340 words, 12 deleted` per transcript; the words are in
/// `get_project` / `get_transcript`, not here.
fn transcript_lines(project: &Project) -> Vec<String> {
    project
        .transcripts
        .iter()
        .map(|t| {
            let deleted = t.words.iter().filter(|w| w.deleted).count();
            let mut line = format!(
                "transcript {} of '{}': {} words",
                short_id(t.id),
                media_name(project, t.media_id),
                t.words.len()
            );
            if deleted > 0 {
                line.push_str(&format!(", {deleted} deleted"));
            }
            line
        })
        .collect()
}

/// The per-turn context block prepended to the user's message. Keeps the outline short: when
/// it is too long the block ends with a pointer to the `timeline` tool.
pub fn context_block(
    project: &Project,
    playhead_us: i64,
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
    let mut lines = vec![format!("playhead {}", fmt_time(playhead_us))];
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
                start,
                ..
            },
            _,
        ) => format!(
            "Added '{}' to {} at {}",
            media_name(after, *media_id),
            track_name(after, *track_id),
            fmt_time(start.resolve(after.settings.fps))
        ),
        (Command::SplitClip { clip_id, at, .. }, _) => {
            format!(
                "Split {} at {}",
                named(*clip_id),
                fmt_time(at.resolve(after.settings.fps))
            )
        }
        (Command::TrimClip { clip_id, .. }, _) => {
            let range = find_clip(after, *clip_id).and_then(|c| match c {
                Clip::Video(m) | Clip::Audio(m) => Some(format!(
                    "{}-{}",
                    fmt_time(m.source_in_us),
                    fmt_time(m.source_out_us)
                )),
                _ => None,
            });
            match range {
                Some(r) => format!("Trimmed {} to {r}", named(*clip_id)),
                None => format!("Trimmed {}", named(*clip_id)),
            }
        }
        (Command::MoveClip { clip_id, start, .. }, _) => {
            format!(
                "Moved {} to {}",
                name(*clip_id),
                fmt_time(start.resolve(after.settings.fps))
            )
        }
        (Command::DeleteClip { clip_id, .. }, _) => format!("Deleted {}", named(*clip_id)),
        (Command::AddCaption { body, start, .. }, _) => format!(
            "Added caption '{}' at {}",
            truncate(&body.text, 60),
            fmt_time(start.resolve(after.settings.fps))
        ),
        (Command::SetCaption { clip_id, .. }, _) => format!("Updated {}", named(*clip_id)),
        (Command::AddText { body, start, .. }, _) => format!(
            "Added text '{}' at {}",
            truncate(&body.text, 60),
            fmt_time(start.resolve(after.settings.fps))
        ),
        (
            Command::SetTextBody {
                clip_id, targets, ..
            }
            | Command::SetTextStyle {
                clip_id, targets, ..
            }
            | Command::SetTextAnimation {
                clip_id, targets, ..
            },
            _,
        ) => format!(
            "Updated text of {}",
            target_label(*clip_id, targets, &named)
        ),
        (Command::SplitCaptions { .. }, _) => "Re-split captions into pages".into(),
        (Command::AddMarkers { markers }, _) => format!("Added {} marker(s)", markers.len()),
        (Command::ApplyTranscriptCuts { .. }, _) => "Cut the deleted transcript words".into(),
        (Command::RestoreTranscriptWords { .. }, _) => "Restored transcript words".into(),
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
        (
            Command::SetClipTransform {
                clip_id, targets, ..
            },
            _,
        ) => format!(
            "Changed position and scale of {}",
            target_label(*clip_id, targets, &named)
        ),
        (
            Command::SetClipEffects {
                clip_id, targets, ..
            },
            _,
        ) => format!(
            "Updated effects on {}",
            target_label(*clip_id, targets, &named)
        ),
        (
            Command::SetClipTransition {
                clip_id,
                targets,
                item_ref,
                ..
            },
            _,
        ) => match item_ref {
            Some(r) => format!(
                "Set {} transition on {}",
                short_ref(r).replace('_', " "),
                target_label(*clip_id, targets, &named)
            ),
            None => format!(
                "Removed transition from {}",
                target_label(*clip_id, targets, &named)
            ),
        },
        (Command::SetKeyframeEasing { clip_id, .. }, _) => {
            format!("Changed keyframe easing on {}", named(*clip_id))
        }
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

/// Names the clip a single-clip command targets, or counts the clips of a batch.
fn target_label(
    clip_id: Option<Id>,
    targets: &[renderly_core::commands::ClipRef],
    named: &dyn Fn(Id) -> String,
) -> String {
    match (clip_id, targets) {
        (Some(id), []) => named(id),
        (_, [one]) => named(one.clip_id),
        (_, many) if !many.is_empty() => format!("{} clips", many.len()),
        _ => "the clip".into(),
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

/// The `transcript` tool's text. Each transcript has a header, then its words as
/// `w-<n>:word` tokens, ten to a line, each line starting with the source-media time of its first
/// word; a deleted word is `~word~` and a detection tag follows in parentheses. `id` selects one
/// transcript by transcript id or media id (a prefix of at least 4 hex digits works).
pub fn transcript_text(project: &Project, id: Option<&str>) -> Result<String, String> {
    if project.transcripts.is_empty() {
        return Ok(
            "The project has no transcripts. GenerateCaptions on a media item makes one.".into(),
        );
    }
    let wanted = id.map(str::to_ascii_lowercase);
    let picked: Vec<&renderly_core::project::Transcript> = project
        .transcripts
        .iter()
        .filter(|t| match &wanted {
            None => true,
            Some(w) => {
                t.id.to_string().starts_with(w.as_str())
                    || t.media_id.to_string().starts_with(w.as_str())
            }
        })
        .collect();
    if picked.is_empty() {
        return Err(format!(
            "no transcript matches '{}'",
            id.unwrap_or_default()
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for t in picked {
        let deleted = t.words.iter().filter(|w| w.deleted).count();
        out.push(format!(
            "transcript {} of '{}' (media {}): {} words{}{}",
            short_id(t.id),
            media_name(project, t.media_id),
            short_id(t.media_id),
            t.words.len(),
            if deleted > 0 {
                format!(", {deleted} deleted")
            } else {
                String::new()
            },
            if t.language.is_empty() {
                String::new()
            } else {
                format!(", {}", t.language)
            },
        ));
        for line in t.words.chunks(10) {
            let words: Vec<String> = line
                .iter()
                .map(|w| {
                    let mut text = if w.deleted {
                        format!("~{}~", w.text)
                    } else {
                        w.text.clone()
                    };
                    if !w.tags.is_empty() {
                        text.push_str(&format!("({})", w.tags.join(",")));
                    }
                    format!("{}:{text}", w.id)
                })
                .collect();
            out.push(format!(
                "{} {}",
                fmt_time(line[0].start_us),
                words.join(" ")
            ));
        }
    }
    Ok(out.join("\n"))
}

/// The asset catalog for `assets`: every effect, filter, transition and LUT that can be applied
/// by ref (builtins and the installed packs) with its params, caption styles, and the WASM
/// plugins this project loaded.
pub fn assets_text(project: &Project) -> String {
    let installed = renderly_core::packs::load_installed_packs();
    let mut out = renderly_core::packs::describe::catalogue_text(&installed);
    out.push_str(
        "Apply by ref: SetClipEffects {filter | key | adjust | effects:[{ref, params}]}, \
         SetClipTransition {ref, duration}.\n",
    );
    out.push_str(&format!(
        "Caption and text styles (style.ref builtin:<id>@1): {}\n",
        renderly_core::captions::BUILTIN_STYLES.join(", ")
    ));
    let plugins: Vec<String> = project
        .legacy_pack_paths
        .iter()
        .filter(|p| p.join("plugin.json").is_file())
        .filter_map(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
        .collect();
    out.push_str(&format!(
        "Loaded WASM plugins (wasm:<id>): {}\n",
        if plugins.is_empty() {
            "none".to_string()
        } else {
            plugins.join(", ")
        }
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
                let dur = p.duration_us.map(fmt_time).unwrap_or_else(|| "?".into());
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
    use renderly_core::time::Time;

    fn project_with_clip() -> (Project, Id, Id) {
        let mut p = Project::new("Demo", Settings::default());
        let media_id = Id::new_v4();
        p.media.push(MediaItem {
            id: media_id,
            path: PathBuf::from("C:/clips/Gameplay 1.mp4"),
            kind: MediaKind::Video,
            duration_us: Some(10_000_000),
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
            start_us: 1000000,
            source_in_us: 0,
            source_out_us: 5000000,
            gain_db: 0.0,
            enabled: true,
            fade_in_us: 0,
            fade_out_us: 0,
            speed: 1.0,
            transform: Default::default(),
            keyframes: vec![],
            filter: None,
            key: None,
            adjust: None,
            effects: vec![],
            blend_mode: BlendMode::Normal,
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
    fn outline_shows_text_markers_magnetic_tracks_and_transcripts() {
        use renderly_core::project::{
            CaptionWord, Marker, MarkerKind, TextAnimationRef, TextBody, TextClip, TextStyleRef,
            Transcript, TranscriptWord,
        };
        let (mut p, media_id, _) = project_with_clip();
        p.tracks[0].magnetic = true;
        // A caption track with a track style, one caption with words and an animation.
        let mut caps = Track::new(TrackKind::Caption, "Subs");
        caps.caption_style = Some(TextStyleRef::builtin("tiktok-box"));
        let mut cap =
            renderly_core::project::CaptionClip::new(1_000_000, 2_000_000, "Let's build it");
        cap.body.words = vec![
            CaptionWord {
                text: "Let's".into(),
                start_us: 0,
                end_us: 300_000,
                emphasis: false,
                word_id: None,
            },
            CaptionWord {
                text: "build".into(),
                start_us: 400_000,
                end_us: 800_000,
                emphasis: true,
                word_id: None,
            },
        ];
        cap.animations.in_ = Some(TextAnimationRef {
            item_ref: "builtin:fade-up@1".into(),
            hash: None,
            params: Default::default(),
            duration_us: None,
        });
        caps.clips.push(Clip::Caption(cap));
        p.tracks.push(caps);
        // A title on its own video track with an unknown pack style.
        let mut titles = Track::new(TrackKind::Video, "Titles");
        let mut title = TextClip::new(0, 3_000_000, TextBody::plain("EPISODE 12"));
        title.style = TextStyleRef::new("pack:mira.titles/neon@2");
        titles.clips.push(Clip::Text(title));
        p.tracks.push(titles);
        p.markers.push(Marker {
            id: Id::new_v4(),
            time_us: 4_000_000,
            label: "drop".into(),
            color: String::new(),
            kind: MarkerKind::User,
        });
        p.transcripts.push(Transcript {
            id: Id::new_v4(),
            media_id,
            language: "en".into(),
            model: "base".into(),
            words: vec![
                TranscriptWord {
                    id: "w-1".into(),
                    text: "So".into(),
                    start_us: 1_200_000,
                    end_us: 1_400_000,
                    deleted: false,
                    tags: vec![],
                },
                TranscriptWord {
                    id: "w-2".into(),
                    text: "um".into(),
                    start_us: 1_500_000,
                    end_us: 1_700_000,
                    deleted: true,
                    tags: vec!["filler".into()],
                },
            ],
        });

        let outline = timeline_outline(&p, None);
        assert!(outline.contains("[magnetic]"), "{outline}");
        assert!(outline.contains("caption style tiktok-box"), "{outline}");
        assert!(
            outline.contains("caption 0:01.0-0:03.0 'Let's build it' words 2 (1 emphasised) style inherit anim in:fade-up"),
            "{outline}"
        );
        assert!(
            outline.contains("text 0:00.0-0:03.0 'EPISODE 12' style mira.titles/neon"),
            "{outline}"
        );
        assert!(
            outline.contains("missing"),
            "an uninstalled style is marked: {outline}"
        );
        assert!(outline.contains("markers (1): 0:04.0 'drop'"), "{outline}");
        assert!(
            outline.contains("transcript") && outline.contains("2 words, 1 deleted"),
            "{outline}"
        );

        let words = transcript_text(&p, None).unwrap();
        assert!(words.contains("w-1:So w-2:~um~(filler)"), "{words}");
        assert!(words.starts_with("transcript "), "{words}");
        assert!(transcript_text(&p, Some("ffffffff")).is_err());
        let empty = Project::new("x", renderly_core::project::Settings::default());
        assert!(transcript_text(&empty, None)
            .unwrap()
            .contains("no transcripts"));
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
        let mut cmd = serde_json::json!({"command": "TrimClip", "track_id": p.tracks[0].id.to_string(), "clip_id": short, "source_out": "3s"});
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
                ..renderly_core::project::AdjustmentClip::new(0, 1_000_000)
            }));
        other.tracks[0]
            .clips
            .push(Clip::Adjustment(renderly_core::project::AdjustmentClip {
                id: b,
                ..renderly_core::project::AdjustmentClip::new(2_000_000, 1_000_000)
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
            source_in: None,
            source_out: Some(Time::Us(5_000_000)),
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
        let block = context_block(&p, 1_500_000, &[], None);
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
                    start_us: 6_000_000 + i as i64 * 4_000_000,
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
                    ..renderly_core::project::CaptionClip::new(i as i64 * 2_000_000, 2000000, *text)
                        .with_style("tiktok-box")
                }));
        }
        p.tracks.push(second);
        p.tracks.push(captions);
        let block = context_block(&p, 2_500_000, &[], None);
        eprintln!("typical context block: {} chars", block.len());
        assert!(block.len() < 1200, "context block is {} chars", block.len());
        assert!(block.contains("track 2 caption 'Captions'"));
    }

    #[test]
    fn assets_lists_builtin_effects_and_transitions() {
        let (p, _, _) = project_with_clip();
        let text = assets_text(&p);
        assert!(text.contains("builtin:blur@1"));
        assert!(text.contains("builtin:crossfade@1"));
    }

    #[test]
    fn fmt_time_formats_minutes_and_fraction() {
        assert_eq!(fmt_time(5_000_000), "0:05.0");
        assert_eq!(fmt_time(62_500_000), "1:02.5");
        assert_eq!(fmt_time(-3_000_000), "0:00.0");
    }
}
