//! Project schema v6 — matches docs/project-schema.md exactly.
//! If you change a type here, update that doc in the same change.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub mod anim;

pub use anim::{
    evaluate_speed, evaluate_transform, evaluate_volume_db, integrate_speed, source_time_at,
    timeline_duration_secs,
};

pub type Id = uuid::Uuid;

/// Current on-disk schema. Loaders accept `1`..=`6` (older files get serde defaults for
/// new fields); new projects and saves write `6`.
pub const SCHEMA_VERSION: u32 = 6;
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
    pub tracks: Vec<Track>,
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
            asset_pack_paths: Vec::new(),
            wasm_plugin_paths: Vec::new(),
            multicam_groups: Vec::new(),
            segmentation_model_path: None,
        }
    }

    pub fn find_media(&self, id: Id) -> Option<&MediaItem> {
        self.media.iter().find(|m| m.id == id)
    }

    pub fn find_track(&self, id: Id) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn find_track_mut(&mut self, id: Id) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }
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
}

impl Clip {
    pub fn id(&self) -> Id {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.id,
            Clip::Caption(c) => c.id,
        }
    }

    pub fn position_secs(&self) -> f64 {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.position_secs,
            Clip::Caption(c) => c.position_secs,
        }
    }

    pub fn duration_secs(&self) -> f64 {
        match self {
            Clip::Video(c) | Clip::Audio(c) => c.timeline_duration_secs(),
            Clip::Caption(c) => c.duration_secs,
        }
    }

    pub fn end_secs(&self) -> f64 {
        self.position_secs() + self.duration_secs()
    }

    pub fn as_media(&self) -> Option<&MediaClip> {
        match self {
            Clip::Video(c) | Clip::Audio(c) => Some(c),
            Clip::Caption(_) => None,
        }
    }

    pub fn as_media_mut(&mut self) -> Option<&mut MediaClip> {
        match self {
            Clip::Video(c) | Clip::Audio(c) => Some(c),
            Clip::Caption(_) => None,
        }
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
    /// `heuristic` (builtin), or `cli` / `rvm` / `birefnet` when `RENDERLY_SEG_CLI` is set.
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
}
