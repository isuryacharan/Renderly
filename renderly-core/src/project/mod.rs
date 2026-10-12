//! Project schema v7 — matches docs/project-schema.md exactly.
//! If you change a type here, update that doc in the same change.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub mod anim;

pub use anim::{
    evaluate_adjustment_opacity, evaluate_speed, evaluate_transform, evaluate_volume_db,
    integrate_speed, source_time_at, timeline_duration_secs,
};

pub type Id = uuid::Uuid;

/// Current on-disk schema. Loaders accept `1`..=`7` (older files get serde defaults for
/// new fields); new projects and saves write `7`.
pub const SCHEMA_VERSION: u32 = 7;

/// Deepest allowed chain of nested sequences (a sequence placed inside a sequence inside ...).
/// Evaluation fails with an error beyond this instead of overflowing the stack; commands
/// reject cycles up front, so this is the backstop for hand-edited files.
pub const MAX_SEQUENCE_DEPTH: usize = 8;
pub const MIN_LOADABLE_SCHEMA_VERSION: u32 = 1;

/// Clamp / sanitize clip playback speed (Phase 3).
pub fn clamp_clip_speed(speed: f64) -> f64 {
    if !speed.is_finite() || speed <= 0.0 {
        1.0
    } else {
        speed.clamp(0.25, 4.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub schema_version: u32,
    pub id: Id,
    pub name: String,
    pub settings: Settings,
    pub media: Vec<MediaItem>,
    /// The root timeline. Nested timelines live in [`Project::sequences`].
    pub tracks: Vec<Track>,
    /// Nested sequences (schema v7). Each is placed on a timeline through its
    /// [`MediaKind::Sequence`] media item. Empty on older projects.
    #[serde(default)]
    pub sequences: Vec<Sequence>,
    /// Absolute or project-relative paths to loaded asset-pack roots (Phase 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub asset_pack_paths: Vec<PathBuf>,
    /// Absolute or project-relative paths to loaded WASM plugin roots (Phase 3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wasm_plugin_paths: Vec<PathBuf>,
    /// Multicam sync groups (Phase 4). Empty on older projects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub multicam_groups: Vec<MulticamGroup>,
    /// Optional local segmentation model directory / marker (Phase 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segmentation_model_path: Option<PathBuf>,
}

impl Project {
    pub fn new(name: impl Into<String>, settings: Settings) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: Id::new_v4(),
            name: name.into(),
            settings,
            media: Vec::new(),
            tracks: Vec::new(),
            sequences: Vec::new(),
            asset_pack_paths: Vec::new(),
            wasm_plugin_paths: Vec::new(),
            multicam_groups: Vec::new(),
            segmentation_model_path: None,
        }
    }

    pub fn find_media(&self, id: Id) -> Option<&MediaItem> {
        self.media.iter().find(|m| m.id == id)
    }

    pub fn find_sequence(&self, id: Id) -> Option<&Sequence> {
        self.sequences.iter().find(|s| s.id == id)
    }

    pub fn find_sequence_mut(&mut self, id: Id) -> Option<&mut Sequence> {
        self.sequences.iter_mut().find(|s| s.id == id)
    }

    /// The media item that places `sequence_id` on a timeline.
    pub fn sequence_media(&self, sequence_id: Id) -> Option<&MediaItem> {
        self.media
            .iter()
            .find(|m| m.kind == MediaKind::Sequence && m.sequence_id == Some(sequence_id))
    }

    /// Track lookup across the root timeline and every sequence (track ids are globally
    /// unique, so no container needs to be named).
    pub fn find_track(&self, id: Id) -> Option<&Track> {
        self.all_tracks().find(|t| t.id == id)
    }

    pub fn find_track_mut(&mut self, id: Id) -> Option<&mut Track> {
        self.all_tracks_mut().find(|t| t.id == id)
    }

    /// Every track in the project: the root timeline first, then each sequence in order.
    pub fn all_tracks(&self) -> impl Iterator<Item = &Track> {
        self.tracks
            .iter()
            .chain(self.sequences.iter().flat_map(|s| s.tracks.iter()))
    }

    pub fn all_tracks_mut(&mut self) -> impl Iterator<Item = &mut Track> {
        self.tracks
            .iter_mut()
            .chain(self.sequences.iter_mut().flat_map(|s| s.tracks.iter_mut()))
    }

    /// The container that owns `track_id`: `Some(None)` for the root timeline,
    /// `Some(Some(sequence_id))` for a sequence, `None` when no such track exists.
    pub fn container_of_track(&self, track_id: Id) -> Option<ContainerId> {
        if self.tracks.iter().any(|t| t.id == track_id) {
            return Some(None);
        }
        self.sequences
            .iter()
            .find(|s| s.tracks.iter().any(|t| t.id == track_id))
            .map(|s| Some(s.id))
    }

    /// The track list of a container (`None` = root timeline).
    pub fn container_tracks(&self, container: ContainerId) -> Option<&Vec<Track>> {
        match container {
            None => Some(&self.tracks),
            Some(id) => self.find_sequence(id).map(|s| &s.tracks),
        }
    }

    pub fn container_tracks_mut(&mut self, container: ContainerId) -> Option<&mut Vec<Track>> {
        match container {
            None => Some(&mut self.tracks),
            Some(id) => self.find_sequence_mut(id).map(|s| &mut s.tracks),
        }
    }

    /// Output size of a sequence: its own `width`/`height`, each inheriting the project's
    /// when unset.
    pub fn sequence_size(&self, sequence: &Sequence) -> (u32, u32) {
        (
            sequence.width.unwrap_or(self.settings.width).max(1),
            sequence.height.unwrap_or(self.settings.height).max(1),
        )
    }

    /// End of the last enabled video/audio/adjustment clip in a sequence (captions are not
    /// rendered inside sequences). `0.0` for an unknown or empty sequence.
    pub fn sequence_duration(&self, sequence_id: Id) -> f64 {
        let Some(seq) = self.find_sequence(sequence_id) else {
            return 0.0;
        };
        seq.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter(|c| c.is_enabled() && !matches!(c, Clip::Caption(_)))
            .map(|c| c.end_secs())
            .fold(0.0_f64, f64::max)
    }

    /// A copy of the project whose root timeline is `sequence_id`'s tracks, for hosts that
    /// preview or play one container as if it were the whole timeline (the desktop app while
    /// a sequence is open). Everything else — media, other sequences, settings — is kept, so
    /// nested sequences inside the opened one still resolve. `None` for an unknown sequence.
    /// Output size stays the project's: a sequence with its own size is previewed at the
    /// project canvas while it is being edited.
    pub fn with_sequence_as_root(&self, sequence_id: Id) -> Option<Project> {
        let tracks = self.find_sequence(sequence_id)?.tracks.clone();
        let mut view = self.clone();
        view.tracks = tracks;
        Some(view)
    }

    /// Duration of a media item. Sequence media is derived from the inner timeline so it can
    /// never go stale; everything else is the probed `duration_secs`.
    pub fn media_duration(&self, media_id: Id) -> Option<f64> {
        let media = self.find_media(media_id)?;
        match (media.kind, media.sequence_id) {
            (MediaKind::Sequence, Some(seq)) => Some(self.sequence_duration(seq)),
            (MediaKind::Sequence, None) => None,
            _ => media.duration_secs,
        }
    }

    /// Pixel size of a media item (sequence media derives it from the sequence).
    pub fn media_size(&self, media_id: Id) -> Option<(u32, u32)> {
        let media = self.find_media(media_id)?;
        match (media.kind, media.sequence_id) {
            (MediaKind::Sequence, Some(seq)) => {
                self.find_sequence(seq).map(|s| self.sequence_size(s))
            }
            (MediaKind::Sequence, None) => None,
            _ => media.width.zip(media.height),
        }
    }

    /// True when placing a clip of `candidate_sequence` on a timeline owned by `container`
    /// would make a sequence contain itself, directly or through other sequences.
    pub fn would_create_cycle(&self, container: ContainerId, candidate_sequence: Id) -> bool {
        let Some(owner) = container else {
            return false;
        };
        owner == candidate_sequence || self.sequence_reaches(candidate_sequence, owner)
    }

    /// True when `from` (transitively) places a sequence clip referencing `target`.
    fn sequence_reaches(&self, from: Id, target: Id) -> bool {
        let mut stack = vec![from];
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            for next in self.sequences_placed_in(id) {
                if next == target {
                    return true;
                }
                stack.push(next);
            }
        }
        false
    }

    /// Ids of the sequences that sequence `id` places on its own timeline.
    fn sequences_placed_in(&self, id: Id) -> Vec<Id> {
        let Some(seq) = self.find_sequence(id) else {
            return Vec::new();
        };
        seq.tracks
            .iter()
            .flat_map(|t| t.clips.iter())
            .filter_map(Clip::as_media)
            .filter_map(|m| self.find_media(m.media_id))
            .filter_map(|media| media.sequence_id)
            .collect()
    }

    /// Parse a project file: accepts schema versions `MIN_LOADABLE_SCHEMA_VERSION..=SCHEMA_VERSION`,
    /// runs [`Self::validate`], and stamps the current schema version so the next save writes
    /// it. Every loader (CLI, MCP, the app) goes through this, so a broken or too-new file
    /// fails with a clear error instead of rendering wrongly.
    pub fn from_json(data: &str) -> Result<Self, ProjectLoadError> {
        let mut project: Project = serde_json::from_str(data).map_err(ProjectLoadError::Parse)?;
        if project.schema_version < MIN_LOADABLE_SCHEMA_VERSION
            || project.schema_version > SCHEMA_VERSION
        {
            return Err(ProjectLoadError::UnsupportedVersion(project.schema_version));
        }
        project.validate().map_err(ProjectLoadError::Invalid)?;
        project.schema_version = SCHEMA_VERSION;
        Ok(project)
    }

    /// Structural checks that serde cannot express; run after loading a project file.
    /// Errors name the offending id so a broken file fails loudly rather than rendering
    /// wrongly.
    pub fn validate(&self) -> Result<(), ProjectError> {
        let mut track_ids = std::collections::HashSet::new();
        for track in self.all_tracks() {
            if !track_ids.insert(track.id) {
                return Err(ProjectError::DuplicateTrackId(track.id));
            }
        }
        let mut seq_ids = std::collections::HashSet::new();
        for seq in &self.sequences {
            if !seq_ids.insert(seq.id) {
                return Err(ProjectError::DuplicateSequenceId(seq.id));
            }
            let n = self
                .media
                .iter()
                .filter(|m| m.kind == MediaKind::Sequence && m.sequence_id == Some(seq.id))
                .count();
            if n != 1 {
                return Err(ProjectError::SequenceMediaCount(seq.id, n));
            }
        }
        for media in &self.media {
            match (media.kind, media.sequence_id) {
                (MediaKind::Sequence, None) => {
                    return Err(ProjectError::SequenceMediaUnbound(media.id));
                }
                (MediaKind::Sequence, Some(seq)) if !seq_ids.contains(&seq) => {
                    return Err(ProjectError::UnknownSequence(seq));
                }
                (MediaKind::Sequence, Some(_)) => {}
                (_, Some(_)) => return Err(ProjectError::SequenceIdOnNonSequenceMedia(media.id)),
                (_, None) => {}
            }
        }
        // Cycles / depth: DFS from every sequence along placed-sequence edges.
        for seq in &self.sequences {
            self.check_sequence_depth(seq.id, &mut vec![seq.id])?;
        }
        Ok(())
    }

    fn check_sequence_depth(&self, id: Id, path: &mut Vec<Id>) -> Result<(), ProjectError> {
        for next in self.sequences_placed_in(id) {
            if path.contains(&next) {
                return Err(ProjectError::SequenceCycle(next));
            }
            if path.len() >= MAX_SEQUENCE_DEPTH {
                return Err(ProjectError::SequenceTooDeep(next));
            }
            path.push(next);
            self.check_sequence_depth(next, path)?;
            path.pop();
        }
        Ok(())
    }
}

/// Why a project file could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ProjectLoadError {
    #[error("parsing project JSON: {0}")]
    Parse(serde_json::Error),
    #[error(
        "unsupported project schema version {0} (this build reads          {MIN_LOADABLE_SCHEMA_VERSION}..={SCHEMA_VERSION})"
    )]
    UnsupportedVersion(u32),
    #[error("invalid project: {0}")]
    Invalid(ProjectError),
}

/// Which timeline a track belongs to: `None` is the root timeline (`Project::tracks`),
/// `Some(id)` is the sequence with that id.
pub type ContainerId = Option<Id>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProjectError {
    #[error("track id {0} appears more than once (track ids must be unique across the root timeline and all sequences)")]
    DuplicateTrackId(Id),
    #[error("sequence id {0} appears more than once")]
    DuplicateSequenceId(Id),
    #[error("sequence {0} must have exactly one media item (found {1})")]
    SequenceMediaCount(Id, usize),
    #[error("media item {0} has kind \"sequence\" but no sequence_id")]
    SequenceMediaUnbound(Id),
    #[error("media item {0} sets sequence_id but its kind is not \"sequence\"")]
    SequenceIdOnNonSequenceMedia(Id),
    #[error("sequence {0} is referenced but does not exist")]
    UnknownSequence(Id),
    #[error("sequence {0} contains itself (directly or through other sequences)")]
    SequenceCycle(Id),
    #[error("sequence {0} is nested deeper than the limit of {MAX_SEQUENCE_DEPTH}")]
    SequenceTooDeep(Id),
}

/// A nested timeline (schema v7). Placed on another timeline through the sequence's
/// [`MediaItem`] (`kind == Sequence`), like any other media.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub id: Id,
    pub name: String,
    /// Output width; `None` inherits `settings.width`.
    #[serde(default)]
    pub width: Option<u32>,
    /// Output height; `None` inherits `settings.height`.
    #[serde(default)]
    pub height: Option<u32>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Settings {
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub sample_rate: u32,
    /// Music ducking under voice/dialog tracks during export (dB). Default -12; set to 0 to disable.
    #[serde(default = "default_duck_db")]
    pub duck_db: f64,
}

fn default_duck_db() -> f64 {
    -12.0
}

impl Default for Settings {
    /// TikTok/shorts-friendly vertical default — matches the primary Ultra Bruno workflow.
    fn default() -> Self {
        Self {
            fps: 60.0,
            width: 1080,
            height: 1920,
            sample_rate: 48000,
            duck_db: default_duck_db(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
    /// A nested [`Sequence`] (schema v7); `MediaItem::sequence_id` names it.
    Sequence,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: Id,
    pub path: PathBuf,
    pub kind: MediaKind,
    /// Known only for kinds/formats the prober supports today; see docs/project-schema.md
    /// and renderly-core::media for current coverage.
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
    /// Present iff `kind == Sequence` (schema v7). For sequence media `path` is empty and
    /// `duration_secs`/`width`/`height`/`fps` are `None`: derive them with
    /// [`Project::media_duration`] / [`Project::media_size`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence_id: Option<Id>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrackKind {
    Video,
    Audio,
    Caption,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub id: Id,
    pub kind: TrackKind,
    pub name: String,
    pub clips: Vec<Clip>,
    /// Mix role for audio ducking (Phase 1). Only meaningful on audio tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_role: Option<TrackAudioRole>,
    /// Excluded from the audio mix on export/playback. GUI-facing; `apply_command`
    /// itself doesn't gate on it (see project-schema.md v1 note).
    #[serde(default)]
    pub muted: bool,
    /// GUI-honored only: `apply_command` deliberately does not reject edits to a locked
    /// track (CLI/MCP agents may still edit it) — the GUI's timeline interactions are
    /// responsible for refusing mouse edits when this is set.
    #[serde(default)]
    pub locked: bool,
    /// Excluded from composited video layers / burned-in captions on export/playback.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackAudioRole {
    Voiceover,
    Dialog,
    Music,
    Ambience,
}

impl Track {
    pub fn new(kind: TrackKind, name: impl Into<String>) -> Self {
        Self {
            id: Id::new_v4(),
            kind,
            name: name.into(),
            clips: Vec::new(),
            audio_role: None,
            muted: false,
            locked: false,
            hidden: false,
        }
    }

    pub fn find_clip(&self, id: Id) -> Option<&Clip> {
        self.clips.iter().find(|c| c.id() == id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Clip {
    Video(MediaClip),
    Audio(MediaClip),
    Caption(CaptionClip),
    /// Effects applied to everything composited beneath it (schema v7). Video tracks only.
    Adjustment(AdjustmentClip),
}

impl Clip {
    pub fn id(&self) -> Id {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.id,
            Clip::Caption(c) => c.id,
            Clip::Adjustment(c) => c.id,
        }
    }

    pub fn position_secs(&self) -> f64 {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.position_secs,
            Clip::Caption(c) => c.position_secs,
            Clip::Adjustment(c) => c.position_secs,
        }
    }

    pub fn duration_secs(&self) -> f64 {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.timeline_duration_secs(),
            Clip::Caption(c) => c.duration_secs,
            Clip::Adjustment(c) => c.duration_secs,
        }
    }

    pub fn end_secs(&self) -> f64 {
        self.position_secs() + self.duration_secs()
    }

    pub fn set_position_secs(&mut self, position_secs: f64) {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.position_secs = position_secs,
            Clip::Caption(c) => c.position_secs = position_secs,
            Clip::Adjustment(c) => c.position_secs = position_secs,
        }
    }

    /// Whether the clip contributes to the render. Caption clips have no enabled flag and
    /// always count.
    pub fn is_enabled(&self) -> bool {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.enabled,
            Clip::Caption(_) => true,
            Clip::Adjustment(c) => c.enabled,
        }
    }

    pub fn as_media(&self) -> Option<&MediaClip> {
        match self {
            Clip::Video(c) | Clip::Audio(c) => Some(c),
            Clip::Caption(_) | Clip::Adjustment(_) => None,
        }
    }

    pub fn as_media_mut(&mut self) -> Option<&mut MediaClip> {
        match self {
            Clip::Video(c) | Clip::Audio(c) => Some(c),
            Clip::Caption(_) | Clip::Adjustment(_) => None,
        }
    }

    pub fn as_adjustment(&self) -> Option<&AdjustmentClip> {
        match self {
            Clip::Adjustment(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_adjustment_mut(&mut self) -> Option<&mut AdjustmentClip> {
        match self {
            Clip::Adjustment(c) => Some(c),
            _ => None,
        }
    }
}

/// An adjustment layer (schema v7): carries effects and applies them to the picture
/// composited from every lower video track for its duration, then mixes the result with the
/// unadjusted picture by `opacity` (and `mask`). It has no pixels of its own, so none of
/// `MediaClip`'s source/speed/transform fields apply.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdjustmentClip {
    pub id: Id,
    pub position_secs: f64,
    pub duration_secs: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// 0..=1 mix between the unadjusted and adjusted picture.
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
    /// Limits where the adjustment applies. UVs are output-frame coordinates (0..1 across
    /// the canvas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<ClipMask>,
    /// Only `AnimProperty::Opacity` is meaningful on an adjustment layer.
    #[serde(default)]
    pub keyframes: Vec<KeyframeTrack>,
}

impl AdjustmentClip {
    pub fn new(position_secs: f64, duration_secs: f64) -> Self {
        Self {
            id: Id::new_v4(),
            position_secs,
            duration_secs,
            enabled: true,
            opacity: 1.0,
            effects: Vec::new(),
            mask: None,
            keyframes: Vec::new(),
        }
    }

    /// Effective mix opacity at absolute time `t` (keyframes override the static value).
    pub fn opacity_at(&self, t: f64) -> f64 {
        anim::evaluate_adjustment_opacity(self, t)
    }
}

/// Static spatial + opacity transform for a media clip (Phase 3.1).
/// Keyframes override these when present for a given property.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ClipTransform {
    /// NDC offset from canvas center (−1 ≈ left/bottom edge, +1 ≈ right/top).
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
    /// 1.0 = cover-fit size after aspect crop.
    #[serde(default = "default_scale")]
    pub scale_x: f64,
    #[serde(default = "default_scale")]
    pub scale_y: f64,
    #[serde(default)]
    pub rotation_deg: f64,
    /// 0..1
    #[serde(default = "default_opacity")]
    pub opacity: f64,
}

fn default_scale() -> f64 {
    1.0
}

fn default_opacity() -> f64 {
    1.0
}

impl Default for ClipTransform {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            scale_x: 1.0,
            scale_y: 1.0,
            rotation_deg: 0.0,
            opacity: 1.0,
        }
    }
}

impl ClipTransform {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    pub fn clamp_opacity(self) -> Self {
        Self {
            opacity: self.opacity.clamp(0.0, 1.0),
            ..self
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimProperty {
    PosX,
    PosY,
    ScaleX,
    ScaleY,
    Rotation,
    Opacity,
    Volume,
    /// Playback rate over clip-local timeline time (Phase 3 deferred / schema v5).
    Speed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
    Bezier,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Time relative to the clip's timeline start (`position_secs`).
    pub time_secs: f64,
    pub value: f64,
    #[serde(default)]
    pub easing: Easing,
    /// Bezier handle (time_offset, value_offset) relative to this key.
    /// Used when easing is Bezier (Phase 4 / C7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_left: Option<(f64, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_right: Option<(f64, f64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyframeTrack {
    pub property: AnimProperty,
    pub keys: Vec<Keyframe>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectInstance {
    pub id: Id,
    /// Builtin id (Phase 3.4): `builtin:color_adjust`, `builtin:blur`,
    /// `builtin:lut_contrast`, `builtin:lut_warm`.
    pub effect_id: String,
    #[serde(default = "default_effect_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
}

fn default_effect_enabled() -> bool {
    true
}

/// Outgoing transition at the end of a media clip (Phase 3.5). Video tracks only for now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipTransition {
    pub kind: TransitionKind,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Crossfade,
    FadeBlack,
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    SlideLeft,
    SlideRight,
    Iris,
    BlurDissolve,
}

impl TransitionKind {
    pub const ALL: &[TransitionKind] = &[
        Self::Crossfade,
        Self::FadeBlack,
        Self::WipeLeft,
        Self::WipeRight,
        Self::WipeUp,
        Self::WipeDown,
        Self::SlideLeft,
        Self::SlideRight,
        Self::Iris,
        Self::BlurDissolve,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Crossfade => "crossfade",
            Self::FadeBlack => "fade_black",
            Self::WipeLeft => "wipe_left",
            Self::WipeRight => "wipe_right",
            Self::WipeUp => "wipe_up",
            Self::WipeDown => "wipe_down",
            Self::SlideLeft => "slide_left",
            Self::SlideRight => "slide_right",
            Self::Iris => "iris",
            Self::BlurDissolve => "blur_dissolve",
        }
    }

    /// Uniform `kind` id for `transition.wgsl`.
    pub fn shader_id(self) -> u32 {
        match self {
            Self::Crossfade => 0,
            Self::FadeBlack => 1,
            Self::WipeLeft => 2,
            Self::WipeRight => 3,
            Self::WipeUp => 4,
            Self::WipeDown => 5,
            Self::SlideLeft => 6,
            Self::SlideRight => 7,
            Self::Iris => 8,
            Self::BlurDissolve => 9,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaClip {
    pub id: Id,
    pub media_id: Id,
    pub position_secs: f64,
    pub source_in_secs: f64,
    pub source_out_secs: f64,
    pub gain_db: f64,
    pub enabled: bool,
    /// Fade-in duration at the clip start (audio export, Phase 1).
    #[serde(default)]
    pub fade_in_secs: f64,
    /// Fade-out duration at the clip end (audio export, Phase 1).
    #[serde(default)]
    pub fade_out_secs: f64,
    /// Timeline playback rate (Phase 3). `1.0` = realtime; timeline length =
    /// `(source_out - source_in) / speed`.
    #[serde(default = "default_speed")]
    pub speed: f64,
    /// Static transform (Phase 3.1). Overridden per-property by `keyframes` when present.
    #[serde(default)]
    pub transform: ClipTransform,
    #[serde(default)]
    pub keyframes: Vec<KeyframeTrack>,
    /// Effect slots executed by the compositor (Phase 3.4 builtins).
    #[serde(default)]
    pub effects: Vec<EffectInstance>,
    /// Transition into the next clip on the same track (Phase 3.5). Renderer-only overlap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing_transition: Option<ClipTransition>,
    /// Shared alpha mask / matte (Phase 4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<ClipMask>,
    /// Local background-removal config (Phase 4); result feeds `mask`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_removal: Option<BackgroundRemoval>,
    /// Local audio denoise (Phase 4). Audio-track clips only in v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_denoise: Option<AudioDenoise>,
    /// Optional multicam group this clip belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multicam_group_id: Option<Id>,
}

fn default_speed() -> f64 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipMask {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub invert: bool,
    /// Soft edge 0..=1.
    #[serde(default)]
    pub feather: f64,
    pub kind: ClipMaskKind,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClipMaskKind {
    None,
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
    },
    Raster {
        path: PathBuf,
    },
    Generated {
        cache_dir: PathBuf,
    },
    /// In-memory grayscale matte (Phase 4 GPU transition).
    #[serde(skip)]
    Luma {
        pixels: Vec<u8>,
        width: u32,
        height: u32,
    },
}

/// Convenience alias used by mask application helpers.
#[derive(Debug, Clone, PartialEq)]
pub enum MaskShape {
    Rect {
        x: f64,
        y: f64,
        width: f64,
        height: f64,
    },
    Ellipse {
        cx: f64,
        cy: f64,
        rx: f64,
        ry: f64,
    },
}

impl ClipMaskKind {
    pub fn as_shape(&self) -> Option<MaskShape> {
        match self {
            Self::Rect {
                x,
                y,
                width,
                height,
            } => Some(MaskShape::Rect {
                x: *x,
                y: *y,
                width: *width,
                height: *height,
            }),
            Self::Ellipse { cx, cy, rx, ry } => Some(MaskShape::Ellipse {
                cx: *cx,
                cy: *cy,
                rx: *rx,
                ry: *ry,
            }),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackgroundRemoval {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `heuristic` (builtin), `rvm` (Robust Video Matting in-process; per-frame matte, needs the
    /// downloaded weights), or `cli` / `birefnet` when `RENDERLY_SEG_CLI` is set.
    #[serde(default = "default_bg_model")]
    pub model_id: String,
    #[serde(default = "default_bg_threshold")]
    pub threshold: f64,
    #[serde(default)]
    pub feather: f64,
    #[serde(default)]
    pub temporal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_cache_dir: Option<PathBuf>,
    /// Present when `matte_cache_dir` holds a per-frame matte sequence (the in-process `rvm`
    /// model): the index needed to map a source time to a matte frame without touching the
    /// filesystem. Absent for single-image mattes (`heuristic`, `RENDERLY_SEG_CLI`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_frames: Option<MatteFrames>,
}

/// Index of a per-frame matte sequence stored in a background-removal cache dir as
/// `f000000.png`, `f000001.png`, ... (grayscale PNG, 255 = subject). Matte frame `i` belongs to
/// *source* time `start_secs + i / fps`, so a matte is valid for every clip that shows that part
/// of the source regardless of speed or timeline position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatteFrames {
    /// Source time (seconds) of matte frame 0.
    pub start_secs: f64,
    pub fps: f64,
    pub count: u32,
    pub width: u32,
    pub height: u32,
    /// Milliseconds since the epoch when the sequence was written; changes on every regeneration
    /// so hosts can invalidate frames cached under the same path.
    #[serde(default)]
    pub stamp: u64,
}

impl MatteFrames {
    /// Nearest matte frame for `source_time` (clamped to the sequence).
    pub fn index_at(&self, source_time: f64) -> u32 {
        if self.count == 0 || !source_time.is_finite() || !(self.fps.is_finite() && self.fps > 0.0)
        {
            return 0;
        }
        let i = ((source_time - self.start_secs) * self.fps).round();
        i.clamp(0.0, f64::from(self.count - 1)) as u32
    }

    /// File name of matte frame `index` inside the cache dir.
    pub fn file_name(index: u32) -> String {
        format!("f{index:06}.png")
    }
}

fn default_bg_model() -> String {
    "heuristic".into()
}

fn default_bg_threshold() -> f64 {
    0.15
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioDenoise {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// `afftdn` (FFmpeg builtin spectral filter) or `rnnoise` (RNNoise network via
    /// `nnnoiseless`). A missing field in a saved project stays `afftdn`; the GUI picks
    /// `rnnoise` for newly created configs.
    #[serde(default = "default_denoise_backend")]
    pub backend: String,
    /// 0..=1. `afftdn`: mapped into noise-reduction strength. `rnnoise`: wet/dry mix
    /// (1.0 = fully denoised, 0.0 = original).
    #[serde(default = "default_denoise_strength")]
    pub strength: f64,
}

fn default_denoise_backend() -> String {
    "afftdn".into()
}

fn default_denoise_strength() -> f64 {
    0.5
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MulticamGroup {
    pub id: Id,
    pub name: String,
    /// Clip ids (one per angle) participating in the group.
    pub angle_clip_ids: Vec<Id>,
    /// Index into `angle_clip_ids` that is currently active for export/preview.
    #[serde(default)]
    pub active_angle: usize,
    /// Sync offset per angle in seconds (same length as angles, optional).
    #[serde(default)]
    pub sync_offsets_secs: Vec<f64>,
}

impl Default for MediaClip {
    fn default() -> Self {
        Self {
            id: Id::new_v4(),
            media_id: Id::new_v4(),
            position_secs: 0.0,
            source_in_secs: 0.0,
            source_out_secs: 0.0,
            gain_db: 0.0,
            enabled: true,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            speed: 1.0,
            transform: ClipTransform::default(),
            keyframes: Vec::new(),
            effects: Vec::new(),
            outgoing_transition: None,
            mask: None,
            background_removal: None,
            audio_denoise: None,
            multicam_group_id: None,
        }
    }
}

impl MediaClip {
    pub fn speed_factor(&self) -> f64 {
        clamp_clip_speed(self.speed)
    }

    /// Timeline span occupied by this clip (honors Speed keyframes when present).
    pub fn timeline_duration_secs(&self) -> f64 {
        anim::timeline_duration_secs(self)
    }

    /// Media source time corresponding to absolute timeline time `t`.
    pub fn source_time_at(&self, t: f64) -> f64 {
        anim::source_time_at(self, t)
    }

    /// Instantaneous speed at absolute timeline time `t`.
    pub fn speed_at(&self, t: f64) -> f64 {
        anim::evaluate_speed(self, t)
    }
}

/// Split keyframe tracks at `split_offset` (seconds from clip start). Left keeps keys
/// with `t < split_offset`; right remaps surviving keys to `t - split_offset`.
pub fn split_keyframes(
    tracks: &[KeyframeTrack],
    split_offset: f64,
) -> (Vec<KeyframeTrack>, Vec<KeyframeTrack>) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    for track in tracks {
        let mut left_keys = Vec::new();
        let mut right_keys = Vec::new();
        for key in &track.keys {
            if key.time_secs < split_offset {
                left_keys.push(key.clone());
            } else {
                right_keys.push(Keyframe {
                    time_secs: key.time_secs - split_offset,
                    value: key.value,
                    easing: key.easing,
                    handle_left: key.handle_left,
                    handle_right: key.handle_right,
                });
            }
        }
        if !left_keys.is_empty() {
            left.push(KeyframeTrack {
                property: track.property,
                keys: left_keys,
            });
        }
        if !right_keys.is_empty() {
            right.push(KeyframeTrack {
                property: track.property,
                keys: right_keys,
            });
        }
    }
    (left, right)
}

/// Clone effects for the right half of a split, minting fresh instance ids.
pub fn clone_effects_with_new_ids(effects: &[EffectInstance]) -> Vec<EffectInstance> {
    effects
        .iter()
        .map(|e| EffectInstance {
            id: Id::new_v4(),
            effect_id: e.effect_id.clone(),
            enabled: e.enabled,
            params: e.params.clone(),
        })
        .collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptionClip {
    pub id: Id,
    pub text: String,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub style_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_media_clip_json_deserializes_with_transform_defaults() {
        let json = r#"{
            "id": "00000000-0000-4000-8000-000000000001",
            "media_id": "00000000-0000-4000-8000-000000000002",
            "position_secs": 0.0,
            "source_in_secs": 0.0,
            "source_out_secs": 5.0,
            "gain_db": 0.0,
            "enabled": true
        }"#;
        let clip: MediaClip = serde_json::from_str(json).unwrap();
        assert!(clip.transform.is_identity());
        assert!(clip.keyframes.is_empty());
        assert!(clip.effects.is_empty());
    }

    #[test]
    fn split_keyframes_remaps_right() {
        let tracks = vec![KeyframeTrack {
            property: AnimProperty::Opacity,
            keys: vec![
                Keyframe {
                    time_secs: 0.0,
                    value: 1.0,
                    easing: Easing::Linear,
                    handle_left: None,
                    handle_right: None,
                },
                Keyframe {
                    time_secs: 2.0,
                    value: 0.5,
                    easing: Easing::Linear,
                    handle_left: None,
                    handle_right: None,
                },
                Keyframe {
                    time_secs: 4.0,
                    value: 0.0,
                    easing: Easing::Linear,
                    handle_left: None,
                    handle_right: None,
                },
            ],
        }];
        let (left, right) = split_keyframes(&tracks, 2.5);
        assert_eq!(left[0].keys.len(), 2);
        assert_eq!(right[0].keys.len(), 1);
        assert!((right[0].keys[0].time_secs - 1.5).abs() < 1e-9);
    }

    // ---- schema v7 --------------------------------------------------------------------

    /// A minimal real-world v6 file: no `sequences`, no `sequence_id`, no adjustment clips.
    const V6_PROJECT_JSON: &str = r#"{
        "schema_version": 6,
        "id": "00000000-0000-4000-8000-0000000000a0",
        "name": "old",
        "settings": { "fps": 30.0, "width": 1920, "height": 1080, "sample_rate": 48000, "duck_db": -12.0 },
        "media": [
            { "id": "00000000-0000-4000-8000-0000000000b1", "path": "a.mp4", "kind": "video",
              "duration_secs": 10.0, "width": 1920, "height": 1080, "fps": 30.0 }
        ],
        "tracks": [
            { "id": "00000000-0000-4000-8000-0000000000c1", "kind": "video", "name": "V1",
              "clips": [
                { "type": "video", "id": "00000000-0000-4000-8000-0000000000d1",
                  "media_id": "00000000-0000-4000-8000-0000000000b1",
                  "position_secs": 0.0, "source_in_secs": 0.0, "source_out_secs": 5.0,
                  "gain_db": 0.0, "enabled": true }
              ] }
        ]
    }"#;

    #[test]
    fn v6_project_loads_unchanged_under_v7() {
        let project: Project = serde_json::from_str(V6_PROJECT_JSON).unwrap();
        assert_eq!(
            project.schema_version, 6,
            "the file's version is preserved on load"
        );
        assert!(project.sequences.is_empty());
        assert_eq!(project.media[0].sequence_id, None);
        assert_eq!(project.media[0].kind, MediaKind::Video);
        assert!(project.validate().is_ok());
        const {
            assert!(MIN_LOADABLE_SCHEMA_VERSION <= 6 && 6 <= SCHEMA_VERSION);
        }
        assert_eq!(SCHEMA_VERSION, 7);
        // Re-saving writes no v7-only keys for features the project does not use.
        let json = serde_json::to_value(&project).unwrap();
        assert!(json["media"][0].get("sequence_id").is_none());
        assert_eq!(json["sequences"], serde_json::json!([]));
    }

    #[test]
    fn from_json_loads_old_versions_validates_and_stamps_the_current_version() {
        let project = Project::from_json(V6_PROJECT_JSON).unwrap();
        assert_eq!(project.schema_version, SCHEMA_VERSION);
        assert_eq!(project.tracks[0].clips.len(), 1);

        // Newer than this build.
        let future = V6_PROJECT_JSON.replace("\"schema_version\": 6", "\"schema_version\": 99");
        assert!(matches!(
            Project::from_json(&future),
            Err(ProjectLoadError::UnsupportedVersion(99))
        ));
        // Structurally broken: two tracks with one id.
        let mut v: serde_json::Value = serde_json::from_str(V6_PROJECT_JSON).unwrap();
        let track = v["tracks"][0].clone();
        v["tracks"].as_array_mut().unwrap().push(track);
        let err = Project::from_json(&v.to_string()).unwrap_err();
        assert!(matches!(
            err,
            ProjectLoadError::Invalid(ProjectError::DuplicateTrackId(_))
        ));
        assert!(err
            .to_string()
            .contains("00000000-0000-4000-8000-0000000000c1"));
        assert!(matches!(
            Project::from_json("{"),
            Err(ProjectLoadError::Parse(_))
        ));
    }

    fn seq_project() -> (Project, Id, Id, Id) {
        let mut p = Project::new("v7", Settings::default());
        let video = Id::new_v4();
        p.media.push(MediaItem {
            id: video,
            path: "a.mp4".into(),
            kind: MediaKind::Video,
            duration_secs: Some(10.0),
            width: Some(100),
            height: Some(100),
            fps: Some(30.0),
            sequence_id: None,
        });
        let seq_id = Id::new_v4();
        let seq_media = Id::new_v4();
        let mut inner = Track::new(TrackKind::Video, "inner");
        inner.clips.push(Clip::Video(MediaClip {
            media_id: video,
            position_secs: 1.0,
            source_in_secs: 0.0,
            source_out_secs: 3.0,
            ..Default::default()
        }));
        inner
            .clips
            .push(Clip::Adjustment(AdjustmentClip::new(0.0, 6.0)));
        p.sequences.push(Sequence {
            id: seq_id,
            name: "S".into(),
            width: Some(640),
            height: None,
            tracks: vec![inner],
        });
        p.media.push(MediaItem {
            id: seq_media,
            path: PathBuf::new(),
            kind: MediaKind::Sequence,
            duration_secs: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(seq_id),
        });
        let mut root = Track::new(TrackKind::Video, "root");
        root.clips.push(Clip::Video(MediaClip {
            media_id: seq_media,
            position_secs: 0.0,
            source_in_secs: 0.0,
            source_out_secs: 6.0,
            ..Default::default()
        }));
        p.tracks.push(root);
        (p, seq_id, seq_media, video)
    }

    #[test]
    fn v7_project_round_trips_and_validates() {
        let (p, seq_id, seq_media, _) = seq_project();
        assert!(p.validate().is_ok());
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.schema_version, SCHEMA_VERSION);
        assert_eq!(back.sequences.len(), 1);
        assert_eq!(back.sequences[0].id, seq_id);
        assert_eq!(back.sequences[0].width, Some(640));
        assert_eq!(back.sequences[0].height, None);
        assert_eq!(
            back.find_media(seq_media).unwrap().kind,
            MediaKind::Sequence
        );
        assert!(matches!(
            back.sequences[0].tracks[0].clips[1],
            Clip::Adjustment(_)
        ));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["media"][1]["kind"], "sequence");
        assert_eq!(
            v["sequences"][0]["tracks"][0]["clips"][1]["type"],
            "adjustment"
        );
    }

    #[test]
    fn derived_duration_and_size_for_sequence_media() {
        let (mut p, seq_id, seq_media, video) = seq_project();
        // Last enabled clip ends at max(1+3, 0+6) = 6 (the adjustment layer counts).
        assert!((p.sequence_duration(seq_id) - 6.0).abs() < 1e-9);
        assert!((p.media_duration(seq_media).unwrap() - 6.0).abs() < 1e-9);
        // Own width, inherited height.
        assert_eq!(p.media_size(seq_media), Some((640, p.settings.height)));
        // Plain media keeps its probed values.
        assert_eq!(p.media_duration(video), Some(10.0));
        assert_eq!(p.media_size(video), Some((100, 100)));
        // Disabled clips do not extend it.
        if let Clip::Adjustment(a) = &mut p.sequences[0].tracks[0].clips[1] {
            a.enabled = false;
        }
        assert!((p.sequence_duration(seq_id) - 4.0).abs() < 1e-9);
        assert_eq!(p.media_duration(Id::new_v4()), None);
    }

    #[test]
    fn with_sequence_as_root_swaps_tracks_only() {
        let (p, seq_id, seq_media, _) = seq_project();
        let view = p.with_sequence_as_root(seq_id).unwrap();
        assert_eq!(view.tracks.len(), p.sequences[0].tracks.len());
        assert_eq!(view.tracks[0].id, p.sequences[0].tracks[0].id);
        assert_eq!(
            view.tracks[0].clips.len(),
            p.sequences[0].tracks[0].clips.len()
        );
        // Media and the sequence list are untouched, so nested lookups still work.
        assert_eq!(view.media.len(), p.media.len());
        assert!(view
            .sequence_media(seq_id)
            .is_some_and(|m| m.id == seq_media));
        assert!(p.with_sequence_as_root(Id::new_v4()).is_none());
    }

    #[test]
    fn find_track_searches_root_and_sequences_and_container_lookup() {
        let (mut p, seq_id, _, _) = seq_project();
        let inner = p.sequences[0].tracks[0].id;
        let root = p.tracks[0].id;
        assert_eq!(p.find_track(inner).unwrap().name, "inner");
        assert_eq!(p.find_track(root).unwrap().name, "root");
        p.find_track_mut(inner).unwrap().name = "renamed".into();
        assert_eq!(p.sequences[0].tracks[0].name, "renamed");
        assert_eq!(p.container_of_track(root), Some(None));
        assert_eq!(p.container_of_track(inner), Some(Some(seq_id)));
        assert_eq!(p.container_of_track(Id::new_v4()), None);
        assert_eq!(p.all_tracks().count(), 2);
    }

    #[test]
    fn validate_rejects_broken_files_naming_the_offender() {
        // Duplicate track id across containers.
        let (mut p, _, _, _) = seq_project();
        let dup = p.tracks[0].id;
        p.sequences[0].tracks[0].id = dup;
        assert_eq!(p.validate(), Err(ProjectError::DuplicateTrackId(dup)));

        // A sequence with no media item.
        let (mut p, seq_id, seq_media, _) = seq_project();
        p.media.retain(|m| m.id != seq_media);
        assert_eq!(
            p.validate(),
            Err(ProjectError::SequenceMediaCount(seq_id, 0))
        );

        // Two media items for one sequence.
        let (mut p, seq_id, _, _) = seq_project();
        let extra = p.media.last().unwrap().clone();
        p.media.push(MediaItem {
            id: Id::new_v4(),
            ..extra
        });
        assert_eq!(
            p.validate(),
            Err(ProjectError::SequenceMediaCount(seq_id, 2))
        );

        // sequence_id pointing nowhere.
        let (mut p, seq_id, seq_media, _) = seq_project();
        let ghost = Id::new_v4();
        p.sequences.retain(|s| s.id != seq_id);
        p.media.push(MediaItem {
            id: Id::new_v4(),
            path: PathBuf::new(),
            kind: MediaKind::Sequence,
            duration_secs: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(ghost),
        });
        p.media.retain(|m| m.id != seq_media);
        assert_eq!(p.validate(), Err(ProjectError::UnknownSequence(ghost)));

        // sequence_id on non-sequence media.
        let (mut p, _, _, video) = seq_project();
        p.media
            .iter_mut()
            .find(|m| m.id == video)
            .unwrap()
            .sequence_id = Some(Id::new_v4());
        assert_eq!(
            p.validate(),
            Err(ProjectError::SequenceIdOnNonSequenceMedia(video))
        );

        // A clip pointing at missing media is NOT a load error: v1-v6 files may carry
        // dangling references and still load (it surfaces as a render error instead).
        let (mut p, _, _, _) = seq_project();
        if let Clip::Video(m) = &mut p.tracks[0].clips[0] {
            m.media_id = Id::new_v4();
        }
        assert!(p.validate().is_ok());
    }

    /// Place sequence media `media` as a clip on `track` of `p`.
    fn place(p: &mut Project, container: Option<Id>, media: Id) {
        let track = match container {
            None => &mut p.tracks[0],
            Some(id) => &mut p.find_sequence_mut(id).unwrap().tracks[0],
        };
        track.clips.push(Clip::Video(MediaClip {
            media_id: media,
            position_secs: 100.0,
            source_in_secs: 0.0,
            source_out_secs: 1.0,
            ..Default::default()
        }));
    }

    #[test]
    fn validate_detects_cycles_and_excess_depth() {
        // A contains itself.
        let (mut p, seq_id, seq_media, _) = seq_project();
        place(&mut p, Some(seq_id), seq_media);
        assert_eq!(p.validate(), Err(ProjectError::SequenceCycle(seq_id)));
        assert!(p.would_create_cycle(Some(seq_id), seq_id));

        // A -> B -> A.
        let (mut p, a, a_media, _) = seq_project();
        let b = Id::new_v4();
        let b_media = Id::new_v4();
        p.sequences.push(Sequence {
            id: b,
            name: "B".into(),
            width: None,
            height: None,
            tracks: vec![Track::new(TrackKind::Video, "b")],
        });
        p.media.push(MediaItem {
            id: b_media,
            path: PathBuf::new(),
            kind: MediaKind::Sequence,
            duration_secs: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(b),
        });
        place(&mut p, Some(a), b_media);
        assert!(p.validate().is_ok(), "A containing B is fine");
        assert!(
            p.would_create_cycle(Some(b), a),
            "B containing A would close the loop"
        );
        assert!(!p.would_create_cycle(Some(a), b));
        assert!(
            !p.would_create_cycle(None, a),
            "the root can contain anything"
        );
        place(&mut p, Some(b), a_media);
        assert!(matches!(p.validate(), Err(ProjectError::SequenceCycle(_))));

        // A chain deeper than the cap.
        let mut p = Project::new("deep", Settings::default());
        p.tracks.push(Track::new(TrackKind::Video, "root"));
        let mut prev_media: Option<Id> = None;
        let mut first: Option<Id> = None;
        for i in 0..(MAX_SEQUENCE_DEPTH + 2) {
            let id = Id::new_v4();
            let media = Id::new_v4();
            let mut t = Track::new(TrackKind::Video, format!("t{i}"));
            if let Some(pm) = prev_media {
                // Each sequence places the previous (deeper) one.
                t.clips.push(Clip::Video(MediaClip {
                    media_id: pm,
                    source_out_secs: 1.0,
                    ..Default::default()
                }));
            }
            p.sequences.push(Sequence {
                id,
                name: format!("s{i}"),
                width: None,
                height: None,
                tracks: vec![t],
            });
            p.media.push(MediaItem {
                id: media,
                path: PathBuf::new(),
                kind: MediaKind::Sequence,
                duration_secs: None,
                width: None,
                height: None,
                fps: None,
                sequence_id: Some(id),
            });
            first.get_or_insert(id);
            prev_media = Some(media);
        }
        assert!(matches!(
            p.validate(),
            Err(ProjectError::SequenceTooDeep(_))
        ));
    }
}
