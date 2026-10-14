//! OTIO -> a new project. See docs/otio.md for the mapping table and the rules below.
//!
//! The native OTIO structure is authoritative (positions, source ranges, speed, enabled,
//! transitions); `metadata["renderly"]`, when present, supplies everything OTIO cannot
//! express *and* the exact (un-snapped) times: a stored time is used only while it still
//! rounds to the OTIO value, so an edit made in another tool always wins.

use super::time::{clean, read_range, read_rt, Range, Rt};
use super::url::{self, file_url_to_path, is_absolute_path, path_to_file_url};
use super::{
    ImportOptions, OtioError, OtioWarning, WarningKind, CAPTION_GENERATOR_KIND,
    RENDERLY_METADATA_KEY,
};
use crate::project::{
    clamp_clip_speed, AdjustmentClip, AnimProperty, CaptionClip, Clip, ClipTransition,
    EffectInstance, Id, MediaClip, MediaItem, MediaKind, MulticamGroup, Project, Sequence,
    Settings, Track, TrackAudioRole, TrackKind, TransitionKind, MAX_SEQUENCE_DEPTH, SCHEMA_VERSION,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

const DEFAULT_WIDTH: u32 = 1920;
const DEFAULT_HEIGHT: u32 = 1080;
const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const EPS: f64 = 1e-9;

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"];
const AUDIO_EXTS: &[&str] = &[
    "wav", "mp3", "aac", "flac", "ogg", "m4a", "aif", "aiff", "opus",
];

/// Build a new [`Project`] from OTIO JSON. Returns the project (already validated) and every
/// warning collected on the way. See [`ImportOptions`].
pub fn import_otio(
    json: &str,
    options: &ImportOptions,
) -> Result<(Project, Vec<OtioWarning>), OtioError> {
    let root = parse_json(json)?;
    let source = find_timeline(&root)?;
    let mut warnings = Vec::new();

    let renderly = source.renderly.as_ref();
    let meta_settings: Option<Settings> = renderly
        .and_then(|m| m.get("settings"))
        .and_then(|s| serde_json::from_value(s.clone()).ok());

    let rates = tally_rates(&source.stack);
    let fps = choose_fps(
        options,
        meta_settings.as_ref().map(|s| s.fps),
        source.global_start.map(|g| g.rate),
        &rates,
        &mut warnings,
    );
    if let Some(g) = source.global_start {
        if g.value.abs() > EPS {
            warnings.push(OtioWarning {
                kind: WarningKind::Other,
                path: "global_start_time".into(),
                message: format!(
                    "global_start_time of {:.3}s is ignored; the project timeline starts at 0",
                    g.secs()
                ),
            });
        }
    }

    let mut imp = Importer {
        opts: options,
        warnings,
        fps,
        meta_media: HashMap::new(),
        meta_seq_media: HashMap::new(),
        media: Vec::new(),
        media_ids: HashSet::new(),
        media_by_key: HashMap::new(),
        sequences: Vec::new(),
        seq_by_orig: HashMap::new(),
        used_track_ids: HashSet::new(),
        used_clip_ids: HashSet::new(),
        used_seq_ids: HashSet::new(),
        track_counts: HashMap::new(),
    };
    if let Some(list) = renderly
        .and_then(|m| m.get("media"))
        .and_then(Value::as_array)
    {
        for v in list {
            if let Ok(item) = serde_json::from_value::<MediaItem>(v.clone()) {
                match (item.kind, item.sequence_id) {
                    (MediaKind::Sequence, Some(seq)) => {
                        imp.meta_seq_media.insert(seq, item);
                    }
                    (MediaKind::Sequence, None) => {}
                    _ => {
                        imp.meta_media.insert(item.id, item);
                    }
                }
            }
        }
    }

    let tracks = imp.tracks_from_stack(&source.stack, "tracks", 0);

    // Sequences that exist in the project but are not placed anywhere have no OTIO
    // structure; Renderly stores them whole in the timeline metadata.
    if let Some(list) = renderly
        .and_then(|m| m.get("unplaced_sequences"))
        .and_then(Value::as_array)
    {
        for v in list {
            let Ok(seq) = serde_json::from_value::<Sequence>(v.clone()) else {
                continue;
            };
            if imp.used_seq_ids.contains(&seq.id) {
                continue;
            }
            imp.used_seq_ids.insert(seq.id);
            let media_id = imp.sequence_media_id(Some(seq.id), seq.id);
            let _ = media_id;
            imp.sequences.push(seq);
        }
    }
    // Restore the rest of the media bin (items no clip uses).
    let mut leftovers: Vec<MediaItem> = imp
        .meta_media
        .values()
        .filter(|m| !imp.media_ids.contains(&m.id))
        .cloned()
        .collect();
    let order: HashMap<Id, usize> = renderly
        .and_then(|m| m.get("media"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .enumerate()
                .filter_map(|(i, v)| {
                    serde_json::from_value::<Id>(v.get("id")?.clone())
                        .ok()
                        .map(|id| (id, i))
                })
                .collect()
        })
        .unwrap_or_default();
    leftovers.sort_by_key(|m| order.get(&m.id).copied().unwrap_or(usize::MAX));
    for item in leftovers {
        imp.media_ids.insert(item.id);
        imp.media.push(item);
    }

    let settings = Settings {
        fps,
        width: options
            .width
            .or(meta_settings.map(|s| s.width))
            .unwrap_or(DEFAULT_WIDTH)
            .max(1),
        height: options
            .height
            .or(meta_settings.map(|s| s.height))
            .unwrap_or(DEFAULT_HEIGHT)
            .max(1),
        sample_rate: options
            .sample_rate
            .or(meta_settings.map(|s| s.sample_rate))
            .unwrap_or(DEFAULT_SAMPLE_RATE)
            .max(1),
        duck_db: meta_settings.map(|s| s.duck_db).unwrap_or(-12.0),
    };

    let name = options
        .name
        .clone()
        .filter(|n| !n.is_empty())
        .or_else(|| (!source.name.is_empty()).then(|| source.name.clone()))
        .unwrap_or_else(|| "untitled".to_string());
    let mut project = Project::new(name, settings);
    if let Some(id) = renderly
        .and_then(|m| m.get("project_id"))
        .and_then(|v| serde_json::from_value::<Id>(v.clone()).ok())
    {
        project.id = id;
    }
    project.schema_version = SCHEMA_VERSION;
    project.media = imp.media;
    project.tracks = tracks;
    project.sequences = imp.sequences;
    // Restore the original ordering of the (unordered-by-structure) media bin and sequence
    // list: media follows the stored list, sequences follow their media items.
    project
        .media
        .sort_by_key(|m| order.get(&m.id).copied().unwrap_or(usize::MAX));
    let media_order: HashMap<Id, usize> = project
        .media
        .iter()
        .enumerate()
        .filter_map(|(i, m)| m.sequence_id.map(|s| (s, i)))
        .collect();
    project
        .sequences
        .sort_by_key(|s| media_order.get(&s.id).copied().unwrap_or(usize::MAX));
    if let Some(m) = renderly {
        project.asset_pack_paths = field(m, "asset_pack_paths").unwrap_or_default();
        project.wasm_plugin_paths = field(m, "wasm_plugin_paths").unwrap_or_default();
        project.multicam_groups =
            field::<Vec<MulticamGroup>>(m, "multicam_groups").unwrap_or_default();
        project.segmentation_model_path = field(m, "segmentation_model_path").unwrap_or(None);
    }
    project.validate()?;
    Ok((project, imp.warnings))
}

/// Fill `duration_secs` / `width` / `height` / `fps` of imported media from the files that
/// exist on disk (ffprobe). Missing files are left untouched. Relative paths resolve against
/// `base_dir`. Returns one warning per file that could not be probed.
#[cfg(not(target_arch = "wasm32"))]
pub fn probe_media(project: &mut Project, base_dir: Option<&std::path::Path>) -> Vec<OtioWarning> {
    let mut warnings = Vec::new();
    for (i, item) in project.media.iter_mut().enumerate() {
        if item.kind == MediaKind::Sequence {
            continue;
        }
        let mut path = item.path.clone();
        if path.is_relative() {
            if let Some(base) = base_dir {
                path = base.join(&path);
            }
        }
        if !path.is_file() {
            continue;
        }
        match crate::media::probe(&path) {
            Ok(p) => {
                if p.duration_secs.is_some() {
                    item.duration_secs = p.duration_secs;
                }
                if item.kind != MediaKind::Audio {
                    item.width = p.width.or(item.width);
                    item.height = p.height.or(item.height);
                    item.fps = p.fps.or(item.fps);
                }
            }
            Err(e) => warnings.push(OtioWarning {
                kind: WarningKind::Other,
                path: format!("media[{i}]"),
                message: format!("could not probe {}: {e}", path.display()),
            }),
        }
    }
    warnings
}

fn field<T: serde::de::DeserializeOwned>(meta: &Value, key: &str) -> Option<T> {
    serde_json::from_value(meta.get(key)?.clone()).ok()
}

// ---- document-level helpers -----------------------------------------------------------

fn parse_json(json: &str) -> Result<Value, OtioError> {
    match serde_json::from_str::<Value>(json) {
        Ok(v) => Ok(v),
        Err(e) => {
            // OTIO allows bare `NaN` / `Infinity` / `-Inf` doubles, which JSON does not.
            let fixed = replace_nonfinite(json);
            if fixed != json {
                if let Ok(v) = serde_json::from_str::<Value>(&fixed) {
                    return Ok(v);
                }
            }
            Err(OtioError::Parse(e))
        }
    }
}

/// Replace bare `NaN`, `Inf`, `Infinity` (optionally signed) tokens outside strings by `null`.
fn replace_nonfinite(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    let mut start = 0;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            i += 1;
            continue;
        }
        if matches!(c, b'N' | b'I' | b'-') {
            let rest = &s[i..];
            let token = ["NaN", "-Infinity", "Infinity", "-Inf", "Inf"]
                .iter()
                .find(|t| rest.starts_with(**t));
            if let Some(t) = token {
                let before_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
                let after = b.get(i + t.len()).copied();
                let after_ok = after.is_none_or(|a| !a.is_ascii_alphanumeric());
                if before_ok && after_ok {
                    out.push_str(&s[start..i]);
                    out.push_str("null");
                    i += t.len();
                    start = i;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.push_str(&s[start..]);
    out
}

fn schema_name(v: &Value) -> Option<&str> {
    let s = v.get("OTIO_SCHEMA")?.as_str()?;
    Some(s.rsplit_once('.').map_or(s, |(name, _)| name))
}

fn renderly_of(v: &Value) -> Option<&Value> {
    v.get("metadata")?.get(RENDERLY_METADATA_KEY)
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

struct Source {
    name: String,
    renderly: Option<Value>,
    stack: Value,
    global_start: Option<Rt>,
}

fn find_timeline(root: &Value) -> Result<Source, OtioError> {
    match schema_name(root) {
        Some("Timeline") => {
            let stack = root.get("tracks").cloned().unwrap_or(Value::Null);
            if !stack.is_object() {
                return Err(OtioError::Invalid(
                    "Timeline has no `tracks` stack".to_string(),
                ));
            }
            Ok(Source {
                name: str_of(root, "name").to_string(),
                renderly: renderly_of(root).cloned(),
                stack,
                global_start: root.get("global_start_time").and_then(read_rt),
            })
        }
        Some("SerializableCollection") => root
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find_map(|c| match schema_name(c) {
                Some("Timeline" | "SerializableCollection") => find_timeline(c).ok(),
                _ => None,
            })
            .ok_or_else(|| OtioError::NotATimeline("collection holds no Timeline".into())),
        Some("Stack") => Ok(Source {
            name: str_of(root, "name").to_string(),
            renderly: None,
            stack: root.clone(),
            global_start: None,
        }),
        Some("Track" | "Sequence" | "Clip") => Ok(Source {
            name: str_of(root, "name").to_string(),
            renderly: None,
            stack: json!({ "OTIO_SCHEMA": "Stack.1", "children": [root.clone()] }),
            global_start: None,
        }),
        Some(other) => Err(OtioError::NotATimeline(format!(
            "top-level object is a {other}"
        ))),
        None => Err(OtioError::NotATimeline(
            "top-level value has no OTIO_SCHEMA".into(),
        )),
    }
}

/// Count how many times each rate appears in the `source_range`s and transition offsets of
/// the tracks tree.
fn tally_rates(v: &Value) -> BTreeMap<u64, usize> {
    fn rate_bits(rt: &Value, out: &mut BTreeMap<u64, usize>) {
        if let Some(r) = read_rt(rt) {
            *out.entry(r.rate.to_bits()).or_insert(0) += 1;
        }
    }
    fn walk(v: &Value, out: &mut BTreeMap<u64, usize>) {
        if let Some(r) = v.get("source_range").filter(|r| r.is_object()) {
            for k in ["start_time", "duration"] {
                if let Some(t) = r.get(k) {
                    rate_bits(t, out);
                }
            }
        }
        if schema_name(v) == Some("Transition") {
            for k in ["in_offset", "out_offset"] {
                if let Some(t) = v.get(k) {
                    rate_bits(t, out);
                }
            }
        }
        for c in v
            .get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            walk(c, out);
        }
    }
    let mut out = BTreeMap::new();
    walk(v, &mut out);
    out
}

fn choose_fps(
    options: &ImportOptions,
    meta_fps: Option<f64>,
    global_rate: Option<f64>,
    rates: &BTreeMap<u64, usize>,
    warnings: &mut Vec<OtioWarning>,
) -> f64 {
    let usable = |r: f64| r.is_finite() && r > 0.0 && r <= 1000.0;
    if let Some(f) = options.fps {
        if usable(f) {
            return f;
        }
        warnings.push(OtioWarning {
            kind: WarningKind::Rate,
            path: String::new(),
            message: format!("ignoring unusable fps option {f}"),
        });
    }
    let counted: Vec<(f64, usize)> = rates
        .iter()
        .map(|(b, n)| (f64::from_bits(*b), *n))
        .filter(|(r, _)| usable(*r))
        .collect();
    if let Some(f) = meta_fps.filter(|f| usable(*f)) {
        // The file's own project rate wins while some range still uses it.
        if counted.is_empty() || counted.iter().any(|(r, _)| (r - f).abs() < 1e-9) {
            return f;
        }
    }
    let Some(max) = counted.iter().map(|(_, n)| *n).max() else {
        return global_rate.filter(|r| usable(*r)).unwrap_or(24.0);
    };
    let tied: Vec<f64> = counted
        .iter()
        .filter(|(_, n)| *n == max)
        .map(|(r, _)| *r)
        .collect();
    let chosen = global_rate
        .filter(|g| tied.iter().any(|r| (r - g).abs() < 1e-9))
        .or_else(|| {
            tied.iter()
                .copied()
                .fold(None, |a: Option<f64>, r| Some(a.map_or(r, |x| x.max(r))))
        })
        .unwrap_or(24.0);
    if counted.len() > 1 {
        let total: usize = counted.iter().map(|(_, n)| *n).sum();
        let list: Vec<String> = counted.iter().map(|(r, n)| format!("{r} ({n}x)")).collect();
        warnings.push(OtioWarning {
            kind: WarningKind::Rate,
            path: String::new(),
            message: format!(
                "mixed frame rates [{}]: project fps set to {chosen} (used by {max} of {total} \
                 times); every time was converted with its own rate",
                list.join(", ")
            ),
        });
    }
    chosen
}

// ---- intermediate representation -------------------------------------------------------

#[derive(Debug, Clone)]
enum Body {
    Gap,
    Adjustment(Box<AdjustmentClip>),
    Caption {
        text: String,
        style_id: String,
        snap: Option<CaptionClip>,
    },
    /// A media clip or (when `media_id` is a sequence's media item) a nested sequence.
    Clip {
        media_id: Id,
        /// Seconds into the source, file-relative.
        src_in: f64,
        speed: f64,
        enabled: bool,
        snap: Option<Box<MediaClip>>,
        effects: Vec<EffectInstance>,
    },
}

#[derive(Debug, Clone)]
struct TransitionIr {
    kind: TransitionKind,
    /// `in_offset + out_offset`, seconds.
    total: f64,
    snap: Option<ClipTransition>,
}

#[derive(Debug, Clone)]
struct Item {
    start: f64,
    dur: f64,
    /// Unrounded end of the item, so positions accumulate without rounding drift.
    end_raw: f64,
    body: Body,
    transition: Option<TransitionIr>,
    path: String,
}

impl Item {
    fn is_clip(&self) -> bool {
        matches!(self.body, Body::Clip { .. })
    }
}

struct Importer<'a> {
    opts: &'a ImportOptions,
    warnings: Vec<OtioWarning>,
    fps: f64,
    meta_media: HashMap<Id, MediaItem>,
    meta_seq_media: HashMap<Id, MediaItem>,
    media: Vec<MediaItem>,
    media_ids: HashSet<Id>,
    media_by_key: HashMap<String, Id>,
    sequences: Vec<Sequence>,
    /// Sequence id recorded in metadata -> (sequence id, media id) already built.
    seq_by_orig: HashMap<Id, (Id, Id)>,
    used_track_ids: HashSet<Id>,
    used_clip_ids: HashSet<Id>,
    used_seq_ids: HashSet<Id>,
    track_counts: HashMap<(usize, bool), usize>,
}

impl Importer<'_> {
    fn warn(&mut self, kind: WarningKind, path: &str, message: impl Into<String>) {
        self.warnings.push(OtioWarning {
            kind,
            path: path.to_string(),
            message: message.into(),
        });
    }

    /// `exact` when it still rounds to `native` (within `tol_frames`), else `native`.
    fn pick(&self, native: f64, exact: Option<f64>, tol_frames: f64) -> f64 {
        match exact {
            Some(e) if e.is_finite() && (e - native).abs() * self.fps <= tol_frames + 1e-6 => e,
            _ => native,
        }
    }

    fn unique_clip_id(&mut self, wanted: Option<Id>) -> Id {
        let id = wanted
            .filter(|i| !self.used_clip_ids.contains(i))
            .unwrap_or_else(Id::new_v4);
        self.used_clip_ids.insert(id);
        id
    }

    // ---- tracks ----

    fn tracks_from_stack(&mut self, stack: &Value, path: &str, depth: usize) -> Vec<Track> {
        let mut out = Vec::new();
        let children = stack.get("children").and_then(Value::as_array);
        for (i, child) in children.into_iter().flatten().enumerate() {
            let p = format!("{path}.children[{i}]");
            match schema_name(child) {
                Some("Track" | "Sequence") => {
                    if let Some(t) = self.track(child, &p, depth, true) {
                        out.push(t);
                    }
                }
                Some("Stack" | "Clip") => {
                    // A bare item in a stack is a layer of its own.
                    let wrapped = json!({
                        "OTIO_SCHEMA": "Track.1", "kind": "Video", "name": str_of(child, "name"),
                        "children": [child.clone()],
                    });
                    if let Some(t) = self.track(&wrapped, &p, depth, false) {
                        out.push(t);
                    }
                }
                Some("Gap" | "Filler" | "Transition") => {}
                other => self.warn(
                    WarningKind::UnknownSchema,
                    &p,
                    format!(
                        "skipped a stack child with unsupported schema {:?}",
                        other.unwrap_or("(none)")
                    ),
                ),
            }
        }
        out
    }

    fn track(&mut self, v: &Value, path: &str, depth: usize, apply_trim: bool) -> Option<Track> {
        let rmeta = renderly_of(v).and_then(|m| m.get("track"));
        let otio_kind = if str_of(v, "kind").eq_ignore_ascii_case("audio") {
            TrackKind::Audio
        } else {
            TrackKind::Video
        };
        if !str_of(v, "kind").is_empty()
            && !str_of(v, "kind").eq_ignore_ascii_case("audio")
            && !str_of(v, "kind").eq_ignore_ascii_case("video")
        {
            self.warn(
                WarningKind::Other,
                path,
                format!(
                    "unknown track kind {:?}; treated as video",
                    str_of(v, "kind")
                ),
            );
        }
        // A track imported as the body of a nested item had its effects and markers handled
        // (and reported) with the item.
        if apply_trim
            && v.get("markers")
                .and_then(Value::as_array)
                .is_some_and(|m| !m.is_empty())
        {
            self.warn(
                WarningKind::MarkersIgnored,
                path,
                "track markers are not imported",
            );
        }
        if apply_trim && has_effects(v) {
            self.warn(
                WarningKind::UnsupportedEffect,
                path,
                "track effects are not imported",
            );
        }

        let mut items: Vec<Item> = Vec::new();
        let mut cursor = 0.0_f64;
        let children = v.get("children").and_then(Value::as_array);
        for (i, child) in children.into_iter().flatten().enumerate() {
            let p = format!("{path}.children[{i}]");
            match schema_name(child) {
                Some("Transition") => {
                    let Some(tr) = self.transition(child, &p) else {
                        continue;
                    };
                    match items.last_mut() {
                        Some(prev) if prev.is_clip() && prev.transition.is_none() => {
                            prev.transition = Some(tr);
                        }
                        _ => self.warn(
                            WarningKind::Transition,
                            &p,
                            "transition has no clip before it; dropped",
                        ),
                    }
                }
                _ => {
                    let item = self.item(child, &p, cursor, otio_kind, depth);
                    if let Some(item) = item {
                        cursor = item.end_raw;
                        items.push(item);
                    }
                }
            }
        }

        if apply_trim {
            if let Some(r) = v.get("source_range").and_then(read_range) {
                let trimmed = trim_items(&items, r.start, r.start + r.dur);
                if trimmed.is_empty() && !items.is_empty() {
                    // A window that misses all content (some exporters write a record
                    // timecode here) would silently empty the track: keep the content.
                    self.warn(
                        WarningKind::TrackTrimmed,
                        path,
                        "track source_range does not overlap its content; ignored",
                    );
                } else {
                    if r.start.abs() > EPS {
                        self.warn(
                            WarningKind::TrackTrimmed,
                            path,
                            format!(
                                "track source_range starts at {:.3}s: content is shifted {} by that amount",
                                r.start,
                                if r.start > 0.0 { "earlier (head cut)" } else { "later" }
                            ),
                        );
                    }
                    items = trimmed;
                }
            }
        }

        let snap_kind: Option<TrackKind> = rmeta
            .and_then(|m| m.get("kind"))
            .and_then(|k| serde_json::from_value(k.clone()).ok());
        let has_caption = items.iter().any(|i| matches!(i.body, Body::Caption { .. }));
        let has_visual = items
            .iter()
            .any(|i| matches!(i.body, Body::Clip { .. } | Body::Adjustment(_)));
        let kind = match snap_kind {
            Some(TrackKind::Caption) => TrackKind::Caption,
            _ if has_caption && !has_visual => TrackKind::Caption,
            _ => otio_kind,
        };

        let clips = self.build_clips(items, kind, path);

        let enabled = v.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        let flag = |key: &str| rmeta.and_then(|m| m.get(key)).and_then(Value::as_bool);
        let mut muted = flag("muted").unwrap_or(false);
        let mut hidden = flag("hidden").unwrap_or(false);
        if !enabled {
            if kind == TrackKind::Audio {
                muted = true;
            } else {
                hidden = true;
            }
        }
        let audio_role: Option<TrackAudioRole> = rmeta
            .and_then(|m| m.get("audio_role"))
            .and_then(|r| serde_json::from_value(r.clone()).ok());
        let wanted_id: Option<Id> = rmeta
            .and_then(|m| m.get("id"))
            .and_then(|r| serde_json::from_value(r.clone()).ok());
        let id = wanted_id
            .filter(|i| !self.used_track_ids.contains(i))
            .unwrap_or_else(Id::new_v4);
        self.used_track_ids.insert(id);
        let mut name = str_of(v, "name").to_string();
        if name.is_empty() {
            let n = self
                .track_counts
                .entry((depth, kind == TrackKind::Audio))
                .or_insert(0);
            *n += 1;
            name = match kind {
                TrackKind::Audio => format!("A{n}"),
                TrackKind::Caption => format!("Captions {n}"),
                TrackKind::Video => format!("V{n}"),
            };
        }
        Some(Track {
            id,
            kind,
            name,
            clips,
            audio_role: if kind == TrackKind::Audio {
                audio_role
            } else {
                None
            },
            muted,
            locked: flag("locked").unwrap_or(false),
            hidden,
        })
    }

    // ---- items ----

    fn transition(&mut self, v: &Value, path: &str) -> Option<TransitionIr> {
        if v.get("enabled").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let secs = |k: &str| v.get(k).and_then(read_rt).map_or(0.0, Rt::secs);
        let total = clean(secs("in_offset") + secs("out_offset"));
        let snap: Option<ClipTransition> = renderly_of(v)
            .and_then(|m| m.get("transition"))
            .and_then(|t| serde_json::from_value(t.clone()).ok());
        let ttype = str_of(v, "transition_type");
        let kind = match &snap {
            Some(s) => s.kind,
            None => {
                if !ttype.eq_ignore_ascii_case("SMPTE_Dissolve")
                    && !ttype.to_ascii_lowercase().contains("dissolve")
                {
                    self.warn(
                        WarningKind::Transition,
                        path,
                        format!("transition type {ttype:?} approximated as a crossfade"),
                    );
                }
                TransitionKind::Crossfade
            }
        };
        Some(TransitionIr { kind, total, snap })
    }

    fn item(
        &mut self,
        v: &Value,
        path: &str,
        start: f64,
        parent: TrackKind,
        depth: usize,
    ) -> Option<Item> {
        let kind = schema_name(v);
        let range = v.get("source_range").and_then(read_range);
        let mk = |dur: f64, body: Body| {
            (dur > EPS).then(|| Item {
                start: clean(start),
                dur: clean(dur),
                end_raw: start + dur,
                body,
                transition: None,
                path: path.to_string(),
            })
        };
        if v.get("markers")
            .and_then(Value::as_array)
            .is_some_and(|m| !m.is_empty())
        {
            self.warn(
                WarningKind::MarkersIgnored,
                path,
                "markers are not imported",
            );
        }
        match kind {
            Some("Gap" | "Filler") => {
                let dur = range.map_or(0.0, |r| r.dur);
                if let Some(adj) = renderly_of(v)
                    .and_then(|m| m.get("adjustment"))
                    .and_then(|a| serde_json::from_value::<AdjustmentClip>(a.clone()).ok())
                {
                    return mk(dur, Body::Adjustment(Box::new(adj)));
                }
                mk(dur, Body::Gap)
            }
            Some("Clip") => self.clip_item(v, path, range, start, parent, mk),
            Some("Stack" | "Track" | "Sequence") => {
                let dur = range.map_or_else(|| composition_duration(v), |r| r.dur);
                let src_in = range.map_or(0.0, |r| r.start);
                let (speed, _) = self.time_warp(v, path);
                if depth >= MAX_SEQUENCE_DEPTH {
                    self.warn(
                        WarningKind::Other,
                        path,
                        format!(
                            "nested deeper than {MAX_SEQUENCE_DEPTH} levels; imported as a gap"
                        ),
                    );
                    return mk(dur, Body::Gap);
                }
                let (_, media_id) = self.nested_sequence(v, path, depth + 1);
                let snap = clip_snapshot(v);
                mk(
                    dur,
                    Body::Clip {
                        media_id,
                        src_in: clean(src_in),
                        speed,
                        enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                        snap,
                        effects: Vec::new(),
                    },
                )
            }
            other => {
                self.warn(
                    WarningKind::UnknownSchema,
                    path,
                    format!(
                        "unsupported track child {:?}; imported as a gap",
                        other.unwrap_or("(none)")
                    ),
                );
                mk(range.map_or(0.0, |r| r.dur), Body::Gap)
            }
        }
    }

    fn clip_item(
        &mut self,
        v: &Value,
        path: &str,
        range: Option<Range>,
        _start: f64,
        parent: TrackKind,
        mk: impl Fn(f64, Body) -> Option<Item>,
    ) -> Option<Item> {
        let name = str_of(v, "name").to_string();
        let reference = active_reference(v);
        let ref_schema = reference.and_then(schema_name);
        let avail = reference
            .and_then(|r| r.get("available_range"))
            .and_then(read_range);
        let dur = match (range, avail) {
            (Some(r), _) => r.dur,
            (None, Some(a)) => a.dur,
            (None, None) => 0.0,
        };

        // Renderly captions: a generator reference of our own kind.
        if ref_schema == Some("GeneratorReference") {
            let r = reference?;
            if str_of(r, "generator_kind") == CAPTION_GENERATOR_KIND {
                let params = r.get("parameters");
                let text = params
                    .and_then(|p| p.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_string();
                let style_id = params
                    .and_then(|p| p.get("style_id"))
                    .and_then(Value::as_str)
                    .unwrap_or("tiktok-bold-yellow")
                    .to_string();
                let snap = renderly_of(v)
                    .and_then(|m| m.get("caption"))
                    .and_then(|c| serde_json::from_value::<CaptionClip>(c.clone()).ok());
                return mk(
                    dur,
                    Body::Caption {
                        text,
                        style_id,
                        snap,
                    },
                );
            }
            self.warn(
                WarningKind::MissingMedia,
                path,
                format!(
                    "clip {name:?} uses a generator ({:?}) Renderly cannot render; imported as a gap",
                    str_of(r, "generator_kind")
                ),
            );
            return mk(dur, Body::Gap);
        }

        let gap = |me: &mut Self, why: String| {
            me.warn(WarningKind::MissingMedia, path, why);
            mk(dur, Body::Gap)
        };
        let url = match ref_schema {
            Some("ExternalReference") => reference
                .and_then(|r| r.get("target_url"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            Some("MissingReference") | None => {
                return gap(
                    self,
                    format!("clip {name:?} has no media (MissingReference); imported as a gap"),
                );
            }
            Some("ImageSequenceReference") => {
                return gap(
                    self,
                    format!(
                        "clip {name:?} is an image sequence (not supported); imported as a gap"
                    ),
                );
            }
            Some(other) => {
                self.warn(
                    WarningKind::UnknownSchema,
                    path,
                    format!("media reference schema {other:?} is not supported"),
                );
                return mk(dur, Body::Gap);
            }
        };
        if url.is_empty() {
            return gap(
                self,
                format!("clip {name:?} has an empty target_url; imported as a gap"),
            );
        }

        let snap = clip_snapshot(v);
        let snap_media = snap.as_ref().map(|s| s.media_id);
        let Some(media_id) = self.resolve_media(&url, parent, avail, snap_media, path, &name)
        else {
            return mk(dur, Body::Gap);
        };

        // File-relative source start: OTIO source times include the reference's own start
        // (timecode) offset.
        let mut src_in = match (range, avail) {
            (Some(r), Some(a)) => r.start - a.start,
            (Some(r), None) => r.start,
            (None, _) => 0.0,
        };
        if src_in < -EPS {
            self.warn(
                WarningKind::Other,
                path,
                format!(
                    "source range starts {:.3}s before the available media; clamped to 0",
                    -src_in
                ),
            );
            src_in = 0.0;
        }
        let (speed, effects) = self.time_warp(v, path);
        mk(
            dur,
            Body::Clip {
                media_id,
                src_in: clean(src_in.max(0.0)),
                speed,
                enabled: v.get("enabled").and_then(Value::as_bool).unwrap_or(true),
                snap,
                effects,
            },
        )
    }

    /// Product of the item's enabled time warps (clamped to Renderly's range) and its
    /// importable native effects. Everything else is dropped with a warning.
    fn time_warp(&mut self, v: &Value, path: &str) -> (f64, Vec<EffectInstance>) {
        let mut speed = 1.0_f64;
        let mut effects = Vec::new();
        let mut dropped: Vec<String> = Vec::new();
        for e in v
            .get("effects")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if e.get("enabled").and_then(Value::as_bool) == Some(false) {
                continue;
            }
            match schema_name(e) {
                Some("LinearTimeWarp") => {
                    let s = e.get("time_scalar").and_then(Value::as_f64).unwrap_or(1.0);
                    if s.is_finite() && s > 0.0 {
                        speed *= s;
                    } else {
                        self.warn(
                            WarningKind::TimeWarp,
                            path,
                            format!(
                                "time_scalar {s} cannot be represented; played at normal speed"
                            ),
                        );
                    }
                }
                Some("FreezeFrame") => self.warn(
                    WarningKind::TimeWarp,
                    path,
                    "FreezeFrame is not supported; the clip plays at normal speed",
                ),
                Some("TimeEffect") => dropped.push("TimeEffect".into()),
                _ => {
                    // Our own effect entries carry their parameters; they only matter when
                    // the clip's metadata snapshot is gone.
                    let name = str_of(e, "effect_name");
                    if name.starts_with("builtin:") {
                        if let Some(rm) = renderly_of(e) {
                            effects.push(EffectInstance {
                                id: field(rm, "id").unwrap_or_else(Id::new_v4),
                                effect_id: name.to_string(),
                                enabled: true,
                                params: field(rm, "params").unwrap_or_default(),
                            });
                            continue;
                        }
                    }
                    dropped.push(if name.is_empty() {
                        schema_name(e).unwrap_or("effect").to_string()
                    } else {
                        name.to_string()
                    });
                }
            }
        }
        if !dropped.is_empty() {
            self.warn(
                WarningKind::UnsupportedEffect,
                path,
                format!(
                    "dropped effects with no Renderly equivalent: {}",
                    dropped.join(", ")
                ),
            );
        }
        if (speed - 1.0).abs() < 1e-9 {
            speed = 1.0;
        }
        let clamped = clamp_clip_speed(speed);
        if (clamped - speed).abs() > 1e-9 {
            self.warn(
                WarningKind::SpeedClamped,
                path,
                format!("speed {speed:.4} is outside 0.25..4; clamped to {clamped}"),
            );
        }
        (clamped, effects)
    }

    // ---- media ----

    fn resolve_media(
        &mut self,
        url: &str,
        track: TrackKind,
        avail: Option<Range>,
        snap_media: Option<Id>,
        path: &str,
        name: &str,
    ) -> Option<Id> {
        let Some(raw) = file_url_to_path(url) else {
            self.warn(
                WarningKind::MissingMedia,
                path,
                format!("clip {name:?} points at a non-file URL ({url}); imported as a gap"),
            );
            return None;
        };
        let abs = match &self.opts.base_dir {
            Some(base) if !is_absolute_path(&raw) => url::join(&base.to_string_lossy(), &raw),
            _ => raw,
        };
        let Some(kind) = kind_for(&abs, track) else {
            self.warn(
                WarningKind::MissingMedia,
                path,
                format!(
                    "clip {name:?} references {abs:?}, which cannot be placed on a {track:?} track; \
                     imported as a gap"
                ),
            );
            return None;
        };
        let key_url = path_to_file_url(&abs);
        let media_key = format!("{kind:?}|{key_url}");

        // Bind to the media item recorded in Renderly metadata while it still points at the
        // same file (a relinked clip gets a fresh item instead).
        if let Some(id) = snap_media {
            if let Some(item) = self.meta_media.get(&id) {
                let meta_path = item.path.to_string_lossy().into_owned();
                let meta_abs = match &self.opts.base_dir {
                    Some(base) if !is_absolute_path(&meta_path) => {
                        url::join(&base.to_string_lossy(), &meta_path)
                    }
                    _ => meta_path.clone(),
                };
                if item.kind == kind
                    && (path_to_file_url(&meta_abs) == key_url
                        || path_to_file_url(&meta_path) == key_url)
                {
                    let item = item.clone();
                    if self.media_ids.insert(item.id) {
                        self.media_by_key.insert(media_key, item.id);
                        self.media.push(item);
                    }
                    return Some(id);
                }
            }
        }
        if let Some(&id) = self.media_by_key.get(&media_key) {
            if let (Some(a), Some(m)) = (avail, self.media.iter_mut().find(|m| m.id == id)) {
                if m.duration_secs.is_some_and(|d| a.dur > d) {
                    m.duration_secs = Some(clean(a.dur));
                }
            }
            return Some(id);
        }
        let id = Id::new_v4();
        self.media_ids.insert(id);
        self.media_by_key.insert(media_key, id);
        self.media.push(MediaItem {
            id,
            path: PathBuf::from(abs),
            kind,
            duration_secs: avail.map(|a| clean(a.dur)).filter(|d| *d > 0.0),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        Some(id)
    }

    // ---- nested sequences ----

    /// The media item id for a sequence (the one recorded in metadata when still free).
    fn sequence_media_id(&mut self, orig: Option<Id>, seq_id: Id) -> Id {
        let recorded = orig.and_then(|o| self.meta_seq_media.get(&o).cloned());
        let id = recorded
            .as_ref()
            .map(|m| m.id)
            .filter(|i| !self.media_ids.contains(i) && !self.meta_media.contains_key(i))
            .unwrap_or_else(Id::new_v4);
        self.media_ids.insert(id);
        self.media.push(MediaItem {
            id,
            path: PathBuf::new(),
            kind: MediaKind::Sequence,
            duration_secs: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(seq_id),
        });
        id
    }

    fn nested_sequence(&mut self, v: &Value, path: &str, depth: usize) -> (Id, Id) {
        let seq_meta = renderly_of(v).and_then(|m| m.get("sequence"));
        let orig: Option<Id> = seq_meta
            .and_then(|m| m.get("id"))
            .and_then(|i| serde_json::from_value(i.clone()).ok());
        if let Some(done) = orig.and_then(|o| self.seq_by_orig.get(&o)) {
            return *done;
        }
        let seq_id = orig
            .filter(|i| !self.used_seq_ids.contains(i))
            .unwrap_or_else(Id::new_v4);
        self.used_seq_ids.insert(seq_id);
        let tracks = match schema_name(v) {
            Some("Stack") => self.tracks_from_stack(v, path, depth),
            _ => self.track(v, path, depth, false).into_iter().collect(),
        };
        let name = Some(str_of(v, "name"))
            .filter(|n| !n.is_empty())
            .or_else(|| seq_meta.and_then(|m| m.get("name")).and_then(Value::as_str))
            .unwrap_or("Nested")
            .to_string();
        let dim = |k: &str| {
            seq_meta
                .and_then(|m| m.get(k))
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok())
        };
        self.sequences.push(Sequence {
            id: seq_id,
            name,
            width: dim("width"),
            height: dim("height"),
            tracks,
        });
        let media_id = self.sequence_media_id(orig, seq_id);
        if let Some(o) = orig {
            self.seq_by_orig.insert(o, (seq_id, media_id));
        }
        (seq_id, media_id)
    }

    // ---- items -> clips ----

    fn build_clips(&mut self, items: Vec<Item>, kind: TrackKind, track_path: &str) -> Vec<Clip> {
        let mut clips: Vec<Clip> = Vec::new();
        for (i, it) in items.iter().enumerate() {
            let adjacent_next = items.get(i + 1).filter(|n| {
                n.is_clip()
                    && kind == TrackKind::Video
                    && (n.start - (it.start + it.dur)).abs() < 1e-6
            });
            match &it.body {
                Body::Gap => {}
                Body::Adjustment(a) => {
                    if kind != TrackKind::Video {
                        self.warn(
                            WarningKind::Other,
                            &it.path,
                            "adjustment layers only exist on video tracks; dropped",
                        );
                        continue;
                    }
                    let mut adj = (**a).clone();
                    adj.id = self.unique_clip_id(Some(adj.id));
                    adj.position_secs = self.pick(it.start, Some(a.position_secs), 0.5);
                    adj.duration_secs = self.pick(it.dur, Some(a.duration_secs), 1.0);
                    clips.push(Clip::Adjustment(adj));
                }
                Body::Caption {
                    text,
                    style_id,
                    snap,
                } => {
                    if kind != TrackKind::Caption {
                        self.warn(
                            WarningKind::Other,
                            &it.path,
                            "caption clip on a non-caption track; dropped",
                        );
                        continue;
                    }
                    let id = self.unique_clip_id(snap.as_ref().map(|s| s.id));
                    clips.push(Clip::Caption(CaptionClip {
                        id,
                        text: text.clone(),
                        position_secs: self.pick(
                            it.start,
                            snap.as_ref().map(|s| s.position_secs),
                            0.5,
                        ),
                        duration_secs: self.pick(
                            it.dur,
                            snap.as_ref().map(|s| s.duration_secs),
                            1.0,
                        ),
                        style_id: style_id.clone(),
                    }));
                }
                Body::Clip {
                    media_id,
                    src_in,
                    speed,
                    enabled,
                    snap,
                    effects,
                } => {
                    if kind == TrackKind::Caption {
                        self.warn(
                            WarningKind::Other,
                            &it.path,
                            "media clip on a caption track; dropped",
                        );
                        continue;
                    }
                    let mut c =
                        self.media_clip(it, *media_id, *src_in, *speed, *enabled, snap, effects);
                    // Transitions: the native item wins; the stored one survives only where
                    // OTIO has no way to express it (no touching clip after this one).
                    match (&it.transition, adjacent_next) {
                        (Some(tr), Some(next)) => {
                            let max = (it.dur.min(next.dur)) / 2.0;
                            let mut dur = tr.total;
                            if let Some(s) = &tr.snap {
                                if (s.duration_secs - dur).abs() * self.fps <= 0.5 + 1e-6 {
                                    dur = s.duration_secs;
                                }
                            }
                            if dur > max + EPS {
                                self.warn(
                                    WarningKind::Transition,
                                    &it.path,
                                    format!(
                                        "transition of {dur:.3}s exceeds half the adjacent clips; \
                                         shortened to {max:.3}s"
                                    ),
                                );
                                dur = max;
                            }
                            c.outgoing_transition = (dur > EPS).then_some(ClipTransition {
                                kind: tr.kind,
                                duration_secs: dur,
                            });
                        }
                        (Some(_), None) => {
                            c.outgoing_transition = None;
                            self.warn(
                                WarningKind::Transition,
                                &it.path,
                                "transition is not between two adjacent video clips; dropped",
                            );
                        }
                        (None, Some(_)) => c.outgoing_transition = None,
                        (None, None) => {}
                    }
                    clips.push(if kind == TrackKind::Audio {
                        c.outgoing_transition = None;
                        Clip::Audio(c)
                    } else {
                        Clip::Video(c)
                    });
                }
            }
        }
        let _ = track_path;
        clips
    }

    #[allow(clippy::too_many_arguments)]
    fn media_clip(
        &mut self,
        it: &Item,
        media_id: Id,
        src_in: f64,
        speed: f64,
        enabled: bool,
        snap: &Option<Box<MediaClip>>,
        effects: &[EffectInstance],
    ) -> MediaClip {
        let mut c = match snap {
            Some(s) => (**s).clone(),
            None => MediaClip {
                effects: effects.to_vec(),
                ..MediaClip::default()
            },
        };
        c.id = self.unique_clip_id(snap.as_ref().map(|s| s.id));
        c.media_id = media_id;
        c.enabled = enabled;
        let src_out = src_in + it.dur * speed;

        let mut keep_speed_keys = true;
        c.speed = match snap {
            Some(s) => {
                let keyed = s
                    .keyframes
                    .iter()
                    .any(|k| k.property == AnimProperty::Speed && !k.keys.is_empty());
                if keyed {
                    let tl = s.timeline_duration_secs();
                    let avg = if tl > 0.0 {
                        (s.source_out_secs - s.source_in_secs) / tl
                    } else {
                        1.0
                    };
                    if (avg - speed).abs() < 1e-6 {
                        s.speed
                    } else {
                        keep_speed_keys = false;
                        speed
                    }
                } else if (s.speed_factor() - speed).abs() < 1e-6 {
                    s.speed
                } else {
                    speed
                }
            }
            None => speed,
        };
        if !keep_speed_keys {
            c.keyframes.retain(|k| k.property != AnimProperty::Speed);
            self.warn(
                WarningKind::TimeWarp,
                &it.path,
                "speed keyframes dropped: the time warp was changed outside Renderly",
            );
        }
        c.position_secs = self.pick(it.start, snap.as_ref().map(|s| s.position_secs), 0.5);
        c.source_in_secs = self.pick(src_in, snap.as_ref().map(|s| s.source_in_secs), 0.5);
        c.source_out_secs = self.pick(
            src_out,
            snap.as_ref().map(|s| s.source_out_secs),
            1.5 + speed,
        );
        if c.source_out_secs <= c.source_in_secs {
            c.source_out_secs = src_out.max(c.source_in_secs + EPS);
        }
        c
    }
}

// ---- free helpers ------------------------------------------------------------------------

fn clip_snapshot(v: &Value) -> Option<Box<MediaClip>> {
    renderly_of(v)
        .and_then(|m| m.get("clip"))
        .and_then(|c| serde_json::from_value::<MediaClip>(c.clone()).ok())
        .map(Box::new)
}

fn has_effects(v: &Value) -> bool {
    v.get("effects")
        .and_then(Value::as_array)
        .is_some_and(|e| !e.is_empty())
}

/// The media reference a clip uses: `Clip.2`'s active entry of `media_references`, or
/// `Clip.1`'s single `media_reference`.
fn active_reference(v: &Value) -> Option<&Value> {
    if let Some(refs) = v.get("media_references").and_then(Value::as_object) {
        let key = v
            .get("active_media_reference_key")
            .and_then(Value::as_str)
            .unwrap_or("DEFAULT_MEDIA");
        if let Some(r) = refs.get(key).filter(|r| r.is_object()) {
            return Some(r);
        }
        return refs.values().find(|r| r.is_object());
    }
    v.get("media_reference").filter(|r| r.is_object())
}

/// Duration of an item that has no `source_range`.
fn composition_duration(v: &Value) -> f64 {
    let children = || {
        v.get("children")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    };
    let item_dur = |c: &Value| -> f64 {
        if schema_name(c) == Some("Transition") {
            0.0
        } else if let Some(r) = c.get("source_range").and_then(read_range) {
            r.dur
        } else {
            match schema_name(c) {
                Some("Clip") => active_reference(c)
                    .and_then(|r| r.get("available_range"))
                    .and_then(read_range)
                    .map_or(0.0, |a| a.dur),
                Some("Track" | "Sequence" | "Stack") => composition_duration(c),
                _ => 0.0,
            }
        }
    };
    match schema_name(v) {
        Some("Stack") => children().map(item_dur).fold(0.0, f64::max),
        _ => children().map(item_dur).sum(),
    }
}

fn kind_for(path: &str, track: TrackKind) -> Option<MediaKind> {
    let ext = path
        .rsplit('/')
        .next()
        .and_then(|f| f.rsplit_once('.'))
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let image = IMAGE_EXTS.contains(&ext.as_str());
    let audio = AUDIO_EXTS.contains(&ext.as_str());
    match track {
        TrackKind::Video if audio => None,
        TrackKind::Video if image => Some(MediaKind::Image),
        TrackKind::Video => Some(MediaKind::Video),
        TrackKind::Audio if image => None,
        TrackKind::Audio => Some(MediaKind::Audio),
        TrackKind::Caption => None,
    }
}

/// Keep the part of `items` inside `[from, to)` and move it so `from` becomes 0.
fn trim_items(items: &[Item], from: f64, to: f64) -> Vec<Item> {
    let mut out = Vec::with_capacity(items.len());
    for src in items {
        let mut it = src.clone();
        let a = it.start;
        let b = it.start + it.dur;
        let na = a.max(from);
        let nb = b.min(to);
        if nb - na <= EPS {
            continue;
        }
        let delta = na - a;
        if delta > EPS {
            if let Body::Clip { src_in, speed, .. } = &mut it.body {
                *src_in = clean(*src_in + delta * *speed);
            }
        }
        if nb < b - EPS {
            it.transition = None;
        }
        it.start = clean(na - from);
        it.dur = clean(nb - na);
        out.push(it);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonfinite_tokens_become_null_but_strings_are_untouched() {
        let s = r#"{"a": NaN, "b": "NaN Infinity", "c": -Infinity, "d": [Inf, -Inf], "NaNo": 1}"#;
        let fixed = replace_nonfinite(s);
        let v: Value = serde_json::from_str(&fixed).unwrap();
        assert!(v["a"].is_null() && v["c"].is_null());
        assert_eq!(v["b"], "NaN Infinity");
        assert!(v["d"][0].is_null() && v["d"][1].is_null());
        assert_eq!(v["NaNo"], 1);
    }

    #[test]
    fn kind_for_follows_track_kind_and_extension() {
        assert_eq!(
            kind_for("a/b.MOV", TrackKind::Video),
            Some(MediaKind::Video)
        );
        assert_eq!(
            kind_for("a/b.png", TrackKind::Video),
            Some(MediaKind::Image)
        );
        assert_eq!(kind_for("a/b.wav", TrackKind::Video), None);
        assert_eq!(
            kind_for("a/b.mov", TrackKind::Audio),
            Some(MediaKind::Audio)
        );
        assert_eq!(kind_for("a/b.png", TrackKind::Audio), None);
    }

    #[test]
    fn trim_cuts_head_and_tail() {
        let mk = |start: f64, dur: f64| Item {
            start,
            dur,
            end_raw: start + dur,
            body: Body::Clip {
                media_id: Id::new_v4(),
                src_in: 0.0,
                speed: 2.0,
                enabled: true,
                snap: None,
                effects: vec![],
            },
            transition: None,
            path: String::new(),
        };
        let out = trim_items(&[mk(0.0, 4.0), mk(4.0, 4.0), mk(8.0, 4.0)], 2.0, 9.0);
        assert_eq!(out.len(), 3);
        assert_eq!((out[0].start, out[0].dur), (0.0, 2.0));
        assert_eq!((out[1].start, out[1].dur), (2.0, 4.0));
        assert_eq!((out[2].start, out[2].dur), (6.0, 1.0));
        // The head was cut by 2s of timeline = 4s of source at 2x.
        match &out[0].body {
            Body::Clip { src_in, .. } => assert_eq!(*src_in, 4.0),
            _ => unreachable!(),
        }
        // A negative start shifts everything later.
        let shifted = trim_items(&[mk(0.0, 4.0)], -3.0, 5.0);
        assert_eq!((shifted[0].start, shifted[0].dur), (3.0, 4.0));
    }

    #[test]
    fn fps_is_the_most_used_rate_with_the_global_rate_breaking_ties() {
        let mut w = Vec::new();
        let mut rates = BTreeMap::new();
        rates.insert(24.0_f64.to_bits(), 5);
        rates.insert(30.0_f64.to_bits(), 9);
        let o = ImportOptions::default();
        assert_eq!(choose_fps(&o, None, None, &rates, &mut w), 30.0);
        assert_eq!(w.len(), 1, "mixed rates are reported");
        rates.insert(24.0_f64.to_bits(), 9);
        assert_eq!(choose_fps(&o, None, Some(24.0), &rates, &mut w), 24.0);
        assert_eq!(choose_fps(&o, None, None, &rates, &mut w), 30.0);
        // Metadata fps wins while a range still uses it; the option wins over everything.
        assert_eq!(choose_fps(&o, Some(24.0), None, &rates, &mut w), 24.0);
        let forced = ImportOptions {
            fps: Some(25.0),
            ..Default::default()
        };
        assert_eq!(choose_fps(&forced, Some(24.0), None, &rates, &mut w), 25.0);
        assert_eq!(
            choose_fps(&o, None, Some(25.0), &BTreeMap::new(), &mut w),
            25.0
        );
        assert_eq!(choose_fps(&o, None, None, &BTreeMap::new(), &mut w), 24.0);
    }

    #[test]
    fn rejects_non_timelines_and_bad_json() {
        let o = ImportOptions::default();
        assert!(matches!(import_otio("{", &o), Err(OtioError::Parse(_))));
        assert!(matches!(
            import_otio(
                r#"{"OTIO_SCHEMA":"RationalTime.1","value":1,"rate":24}"#,
                &o
            ),
            Err(OtioError::NotATimeline(_))
        ));
        assert!(matches!(
            import_otio("[]", &o),
            Err(OtioError::NotATimeline(_))
        ));
        assert!(matches!(
            import_otio(r#"{"OTIO_SCHEMA":"Timeline.1","name":"x"}"#, &o),
            Err(OtioError::Invalid(_))
        ));
    }
}
