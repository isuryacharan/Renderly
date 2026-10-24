//! Decode-free timeline evaluation: "which layers are active at time `t`, with what source
//! times, keyframed params, and transition progress" — the parity-critical logic shared by
//! the native export/preview path (`export::FrameRenderer`) and the wasm preview compositor
//! (`renderly-wasm`). See docs/preview-webview.md P2. Schema v7 adds adjustment layers
//! ([`ComposeStep::Adjustment`]) and nested sequences ([`ActiveSource::Sequence`], evaluated
//! recursively with [`sequence_steps`]).
//!
//! This module deliberately has **no** decode/IO dependency: it only reads `Project` state
//! (paths, keyframes, transitions) and produces [`ComposeStep`]/[`ActiveCaption`] descriptors.
//! Callers are responsible for turning an [`ActiveSource`] into actual pixels
//! (FFmpeg decode natively, `<video>`/`<img>` elements in the browser) — never re-derive which
//! layers are active or their transform/transition params independently, or preview and
//! export will drift apart.

use super::FxTiming;
use crate::project::{
    evaluate_effect_stack, evaluate_text_transform, evaluate_transform, BlendMode, Clip, ClipMask,
    ClipTransform, EffectInstance, Id, MediaClip, MediaKind, Project, TextClip, Track, TrackKind,
    MAX_SEQUENCE_DEPTH,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum EvalError {
    #[error("media not found: {0}")]
    MediaNotFound(uuid::Uuid),
    #[error("media {0} is not video")]
    NotVideo(uuid::Uuid),
    #[error("sequence not found: {0}")]
    SequenceNotFound(uuid::Uuid),
    #[error("sequence {0} is nested deeper than the limit of {MAX_SEQUENCE_DEPTH}")]
    SequenceTooDeep(uuid::Uuid),
}

/// Identity of one layer instance for decoder / `<video>` element reuse: the chain of
/// decoder keys from the root timeline down to the layer — one entry per enclosing nested
/// sequence layer, then the layer's own decoder key. The same media can be needed at two
/// different times in one frame (a sequence placed twice, or a clip both nested and on the
/// root); each such use has a distinct key, so decoders never fight over one reader.
///
/// A root-level layer's key is just `[decoder_key]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct InstanceKey(Vec<Id>);

impl InstanceKey {
    /// The key of a layer at the root timeline.
    pub fn root(decoder_key: Id) -> Self {
        Self(vec![decoder_key])
    }

    /// This prefix extended by one more link.
    pub fn with(&self, id: Id) -> Self {
        let mut v = self.0.clone();
        v.push(id);
        Self(v)
    }

    pub fn depth(&self) -> usize {
        self.0.len()
    }

    /// True for a layer inside at least one nested sequence.
    pub fn is_nested(&self) -> bool {
        self.0.len() > 1
    }

    pub fn ids(&self) -> &[Id] {
        &self.0
    }
}

impl std::fmt::Display for InstanceKey {
    /// Stable string form (`id/id/...`), the key the wasm preview and its JS host exchange.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, id) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("/")?;
            }
            write!(f, "{id}")?;
        }
        Ok(())
    }
}

/// What an [`ActiveLayer`] samples.
#[derive(Debug, Clone)]
pub enum ActiveSource {
    /// A decoded video frame or still image. `media_id` is what the wasm preview keys its
    /// `<video>`/`<img>` pool by; the native path decodes `path` at `source_time`.
    Media {
        media_id: Id,
        path: PathBuf,
        source_time: i64,
    },
    /// A nested sequence rendered at `time_us` (sequence time). `instance` is the key
    /// prefix for the layers inside it: pass it to [`sequence_steps`] to evaluate them.
    Sequence {
        sequence_id: Id,
        time_us: i64,
        instance: InstanceKey,
    },
    /// A text clip: its picture is rendered text (see [`ActiveText`]), not decoded.
    Text(ActiveText),
}

/// What a text clip draws, resolved from the clip: the displayed text and the style the caption
/// renderer is given. `style_id` is the item part of the style ref (`tiktok-box`); `overrides`
/// are the clip's style overrides (the renderer reads `fill`, `size` and `anchor`).
#[derive(Debug, Clone, PartialEq)]
pub struct ActiveText {
    pub clip_id: Id,
    pub text: String,
    pub style_id: String,
    pub overrides: BTreeMap<String, serde_json::Value>,
}

impl ActiveText {
    fn of(clip: &TextClip) -> Self {
        Self {
            clip_id: clip.id,
            text: clip.body.text.clone(),
            style_id: clip.style.render_style_id(),
            overrides: clip.style.overrides.clone(),
        }
    }
}

/// One layer to be composited at a given time, bottom-to-top by track order (see
/// `compose::Compositor::composite`'s doc comment). Decode-free: `source` describes *what*
/// to draw, not decoded pixels.
#[derive(Debug, Clone)]
pub struct ActiveLayer {
    /// Decoder/element cache key within its container (usually the track id; the incoming
    /// side of a transition uses a derived key so two decoders/elements can be open at once).
    pub decoder_key: uuid::Uuid,
    /// Container-qualified key: unique per simultaneously active layer in the whole frame,
    /// including layers inside nested sequences.
    pub instance: InstanceKey,
    pub source: ActiveSource,
    pub transform: ClipTransform,
    /// The effect stack in render order (filter, key, adjust, `effects[]`), with effect-param
    /// keyframes evaluated at the frame time.
    pub effects: Vec<EffectInstance>,
    pub transition: Option<crate::compose::LayerTransition>,
    pub mask: Option<ClipMask>,
    pub background_removal: Option<crate::project::BackgroundRemoval>,
    pub blend_mode: BlendMode,
    pub timing: FxTiming,
}

impl ActiveLayer {
    /// `(media_id, path, source_time)` when this layer samples media rather than a sequence.
    pub fn media(&self) -> Option<(Id, &std::path::Path, i64)> {
        match &self.source {
            ActiveSource::Media {
                media_id,
                path,
                source_time,
            } => Some((*media_id, path.as_path(), *source_time)),
            ActiveSource::Sequence { .. } | ActiveSource::Text(_) => None,
        }
    }

    /// The text this layer draws, when it is a text clip.
    pub fn text(&self) -> Option<&ActiveText> {
        match &self.source {
            ActiveSource::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// One step of the compositing stack at time `t`, bottom to top.
// `Layer` carries the whole evaluated layer (it is built once per active layer per frame and
// consumed immediately), so boxing it would only add an allocation per layer.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ComposeStep {
    Layer(ActiveLayer),
    /// Apply `effects` to everything composited so far, then mix with the unadjusted
    /// picture by `opacity` (and `mask`).
    Adjustment {
        effects: Vec<EffectInstance>,
        opacity: f32,
        mask: Option<ClipMask>,
        blend_mode: BlendMode,
        timing: FxTiming,
    },
}

#[derive(Debug, Clone)]
pub struct ActiveCaption {
    pub text: String,
    /// The style the renderer is given: the item part of the resolved style ref.
    pub style_id: String,
    /// The style's overrides (the renderer reads `fill`, `size` and `anchor`).
    pub overrides: BTreeMap<String, serde_json::Value>,
}

/// Evaluate the root timeline at `t`: which video-track layers and adjustment layers are
/// active, with per-layer source time, keyframed transform/opacity, and transition progress.
/// Tracks are iterated in project order (first = bottom); within a transition window both
/// the outgoing and incoming clip are emitted so the caller can blend them.
pub fn active_steps(project: &Project, t: i64) -> Result<Vec<ComposeStep>, EvalError> {
    container_steps(project, &project.tracks, t, &InstanceKey::default())
}

/// Evaluate the inside of a nested sequence layer: `time_us` and `instance` come from the
/// layer's [`ActiveSource::Sequence`]. Fails with [`EvalError::SequenceTooDeep`] beyond
/// [`MAX_SEQUENCE_DEPTH`] nested levels.
pub fn sequence_steps(
    project: &Project,
    sequence_id: Id,
    time_us: i64,
    instance: &InstanceKey,
) -> Result<Vec<ComposeStep>, EvalError> {
    if instance.depth() > MAX_SEQUENCE_DEPTH {
        return Err(EvalError::SequenceTooDeep(sequence_id));
    }
    let seq = project
        .find_sequence(sequence_id)
        .ok_or(EvalError::SequenceNotFound(sequence_id))?;
    container_steps(project, &seq.tracks, time_us, instance)
}

/// Every media-sampling layer active at root time `t`, including those inside nested
/// sequences (flattened, bottom to top per container). For hosts that must open a decoder /
/// element per leaf regardless of how the picture is assembled (prefetch, the preview's
/// element manager).
pub fn leaf_media_layers(project: &Project, t: i64) -> Result<Vec<ActiveLayer>, EvalError> {
    let mut out = Vec::new();
    collect_leaves(project, active_steps(project, t)?, &mut out)?;
    Ok(out)
}

fn collect_leaves(
    project: &Project,
    steps: Vec<ComposeStep>,
    out: &mut Vec<ActiveLayer>,
) -> Result<(), EvalError> {
    for step in steps {
        let ComposeStep::Layer(layer) = step else {
            continue;
        };
        match &layer.source {
            ActiveSource::Media { .. } => out.push(layer),
            ActiveSource::Text(_) => {}
            ActiveSource::Sequence {
                sequence_id,
                time_us,
                instance,
            } => {
                let inner = sequence_steps(project, *sequence_id, *time_us, instance)?;
                collect_leaves(project, inner, out)?;
            }
        }
    }
    Ok(())
}

/// Resolve a media id to what a layer samples at `source_time`. `child_prefix` is the
/// instance-key prefix for layers inside it when the media is a sequence.
fn resolve_source(
    project: &Project,
    media_id: Id,
    source_time: i64,
    child_prefix: InstanceKey,
) -> Result<ActiveSource, EvalError> {
    let media = project
        .find_media(media_id)
        .ok_or(EvalError::MediaNotFound(media_id))?;
    match media.kind {
        MediaKind::Video | MediaKind::Image => Ok(ActiveSource::Media {
            media_id,
            path: media.path.clone(),
            source_time,
        }),
        MediaKind::Sequence => {
            let sequence_id = media.sequence_id.ok_or(EvalError::NotVideo(media_id))?;
            if child_prefix.depth() > MAX_SEQUENCE_DEPTH {
                return Err(EvalError::SequenceTooDeep(sequence_id));
            }
            Ok(ActiveSource::Sequence {
                sequence_id,
                time_us: source_time,
                instance: child_prefix,
            })
        }
        MediaKind::Audio => Err(EvalError::NotVideo(media_id)),
    }
}

/// A clip that is a layer of a video track: a media clip, or a text clip (a title).
#[derive(Clone, Copy)]
enum LayerClip<'a> {
    Media(&'a MediaClip),
    Text(&'a TextClip),
}

impl<'a> LayerClip<'a> {
    fn start_us(&self) -> i64 {
        match self {
            LayerClip::Media(v) => v.start_us,
            LayerClip::Text(t) => t.start_us,
        }
    }

    fn duration_us(&self) -> i64 {
        match self {
            LayerClip::Media(v) => v.timeline_duration_us(),
            LayerClip::Text(t) => t.duration_us,
        }
    }

    fn end_us(&self) -> i64 {
        self.start_us() + self.duration_us()
    }

    fn transition(&self) -> Option<&'a crate::project::ClipTransition> {
        match self {
            LayerClip::Media(v) => v.outgoing_transition.as_ref(),
            LayerClip::Text(t) => t.outgoing_transition.as_ref(),
        }
    }

    fn transform_at(&self, t: i64) -> ClipTransform {
        match self {
            LayerClip::Media(v) => evaluate_transform(v, t),
            LayerClip::Text(x) => evaluate_text_transform(x, t),
        }
    }

    fn effects_at(&self, t: i64) -> Vec<EffectInstance> {
        match self {
            LayerClip::Media(v) => clip_effects(v, t),
            LayerClip::Text(x) => {
                evaluate_effect_stack(&x.effect_stack(), &x.keyframes, (t - x.start_us).max(0))
            }
        }
    }

    fn mask(&self) -> Option<ClipMask> {
        match self {
            LayerClip::Media(v) => v.mask.clone(),
            LayerClip::Text(t) => t.mask.clone(),
        }
    }

    fn background_removal(&self) -> Option<crate::project::BackgroundRemoval> {
        match self {
            LayerClip::Media(v) => v.background_removal.clone(),
            LayerClip::Text(_) => None,
        }
    }

    fn blend_mode(&self) -> BlendMode {
        match self {
            LayerClip::Media(v) => v.blend_mode,
            LayerClip::Text(t) => t.blend_mode,
        }
    }

    /// What the layer shows at timeline time `t`.
    fn source_at(
        &self,
        project: &Project,
        t: i64,
        instance: InstanceKey,
    ) -> Result<ActiveSource, EvalError> {
        match self {
            LayerClip::Media(v) => {
                resolve_source(project, v.media_id, v.source_time_at(t), instance)
                    .map_err(|e| not_video_for(e, v.media_id))
            }
            LayerClip::Text(x) => Ok(ActiveSource::Text(ActiveText::of(x))),
        }
    }

    /// What the incoming side of a transition shows `elapsed` timeline µs into the window.
    fn incoming_source(
        &self,
        project: &Project,
        elapsed: i64,
        instance: InstanceKey,
    ) -> Result<ActiveSource, EvalError> {
        match self {
            LayerClip::Media(v) => resolve_source(
                project,
                v.media_id,
                v.source_in_us + (elapsed as f64 * v.speed_factor()).round() as i64,
                instance,
            )
            .map_err(|e| not_video_for(e, v.media_id)),
            LayerClip::Text(x) => Ok(ActiveSource::Text(ActiveText::of(x))),
        }
    }

    /// The effect clock at timeline time `t`.
    fn timing_at(&self, project: &Project, t: i64) -> FxTiming {
        match self {
            LayerClip::Media(v) => clip_timing(project, v, t),
            LayerClip::Text(x) => text_timing(project, x, (t - x.start_us).max(0)),
        }
    }

    /// The effect clock of the incoming side, `elapsed` timeline µs into the window.
    fn incoming_timing(&self, project: &Project, elapsed: i64) -> FxTiming {
        match self {
            LayerClip::Media(v) => {
                let in_elapsed = ((elapsed as f64) * v.speed_factor()).round() as i64;
                timing_for(project, v, in_elapsed, v.speed_factor())
            }
            LayerClip::Text(x) => text_timing(project, x, elapsed),
        }
    }
}

fn text_timing(project: &Project, clip: &TextClip, elapsed_us: i64) -> FxTiming {
    FxTiming {
        time: elapsed_us as f64 / 1e6,
        duration: clip.duration_us.max(0) as f64 / 1e6,
        frame: project.settings.fps.frame_at(elapsed_us).max(0) as u32,
        speed: 1.0,
    }
}

fn container_steps(
    project: &Project,
    tracks: &[Track],
    t: i64,
    prefix: &InstanceKey,
) -> Result<Vec<ComposeStep>, EvalError> {
    let mut steps = Vec::new();

    for track in tracks {
        if track.kind != TrackKind::Video || track.hidden {
            continue;
        }

        let mut video_clips: Vec<LayerClip> = track
            .clips
            .iter()
            .filter_map(|c| match c {
                Clip::Video(v) if v.enabled && multicam_angle_active(project, v) => {
                    Some(LayerClip::Media(v))
                }
                Clip::Text(x) if x.enabled => Some(LayerClip::Text(x)),
                _ => None,
            })
            .collect();
        video_clips.sort_by_key(LayerClip::start_us);

        // Transition window: during [cut-d, cut) emit outgoing + incoming for WGSL blend.
        let mut handled = false;
        for (i, v) in video_clips.iter().enumerate() {
            let end = v.end_us();
            let Some(tr) = v.transition() else {
                continue;
            };
            let d = tr.duration_us;
            if d <= 0 {
                continue;
            }
            let window_start = end - d;
            if t < window_start || t >= end {
                continue;
            }
            let Some(incoming) = video_clips.get(i + 1).copied() else {
                continue;
            };
            // Incoming must abut (or nearly abut) this cut for renderer-only overlap.
            if (incoming.start_us() - end).abs() > 50_000 && incoming.start_us() < end - 1 {
                continue;
            }

            let u = (((t - window_start) as f64) / (d as f64)).clamp(0.0, 1.0) as f32;

            let out_xf = v.transform_at(t);
            let in_xf = incoming.transform_at(incoming.start_us());
            let out_effects = v.effects_at(t);
            // The incoming clip is held at its first frame for animated values, like its
            // transform; its effect clock runs from the start of the window.
            let in_effects = incoming.effects_at(incoming.start_us());
            let out_timing = v.timing_at(project, t);
            let in_timing = incoming.incoming_timing(project, t - window_start);
            let out_transition = |is_incoming: bool| crate::compose::LayerTransition {
                item_ref: tr.item_ref.clone(),
                hash: tr.hash.clone(),
                params: tr.params.clone(),
                progress: match tr.easing {
                    Some(e) => e.apply(f64::from(u)) as f32,
                    None => u,
                },
                is_incoming,
                default_easing: tr.easing.is_none(),
            };

            // Distinct from track.id so decoders/elements keep a second stream open.
            let incoming_key = uuid::Uuid::from_u128(track.id.as_u128() ^ 0xC0_FF_EE_51_u128);
            let out_instance = prefix.with(track.id);
            let in_instance = prefix.with(incoming_key);
            // Resolve both sources before pushing either, so an error leaves no half pair.
            let out_source = v.source_at(project, t, out_instance.clone())?;
            let in_source =
                incoming.incoming_source(project, t - window_start, in_instance.clone())?;

            steps.push(ComposeStep::Layer(ActiveLayer {
                decoder_key: track.id,
                instance: out_instance,
                source: out_source,
                transform: out_xf,
                effects: out_effects,
                transition: Some(out_transition(false)),
                mask: v.mask(),
                background_removal: v.background_removal(),
                blend_mode: v.blend_mode(),
                timing: out_timing,
            }));
            steps.push(ComposeStep::Layer(ActiveLayer {
                decoder_key: incoming_key,
                instance: in_instance,
                source: in_source,
                transform: in_xf,
                effects: in_effects,
                transition: Some(out_transition(true)),
                mask: incoming.mask(),
                background_removal: incoming.background_removal(),
                blend_mode: incoming.blend_mode(),
                timing: in_timing,
            }));
            handled = true;
            break;
        }
        if handled {
            continue;
        }

        for clip in &track.clips {
            match clip {
                Clip::Video(_) | Clip::Text(_) => {
                    let layer = match clip {
                        Clip::Video(v) if v.enabled && multicam_angle_active(project, v) => {
                            LayerClip::Media(v)
                        }
                        Clip::Text(x) if x.enabled => LayerClip::Text(x),
                        _ => continue,
                    };
                    if t >= layer.start_us() && t < layer.end_us() {
                        let instance = prefix.with(track.id);
                        let source = layer.source_at(project, t, instance.clone())?;
                        steps.push(ComposeStep::Layer(ActiveLayer {
                            decoder_key: track.id,
                            instance,
                            source,
                            transform: layer.transform_at(t),
                            effects: layer.effects_at(t),
                            transition: None,
                            mask: layer.mask(),
                            background_removal: layer.background_removal(),
                            blend_mode: layer.blend_mode(),
                            timing: layer.timing_at(project, t),
                        }));
                        break;
                    }
                }
                Clip::Adjustment(a) => {
                    if a.enabled && t >= a.start_us && t < a.start_us + a.duration_us {
                        let local = (t - a.start_us).max(0);
                        steps.push(ComposeStep::Adjustment {
                            effects: evaluate_effect_stack(&a.effect_stack(), &a.keyframes, local),
                            opacity: a.opacity_at(t) as f32,
                            mask: a.mask.clone(),
                            blend_mode: a.blend_mode,
                            timing: FxTiming {
                                time: local as f64 / 1e6,
                                duration: a.duration_us as f64 / 1e6,
                                frame: project.settings.fps.frame_at(local).max(0) as u32,
                                speed: 1.0,
                            },
                        });
                        break;
                    }
                }
                Clip::Audio(_) | Clip::Caption(_) => {}
            }
        }
    }

    Ok(steps)
}

/// The clip's effect stack in render order at absolute time `t`, animated params evaluated.
fn clip_effects(clip: &MediaClip, t: i64) -> Vec<EffectInstance> {
    evaluate_effect_stack(
        &clip.effect_stack(),
        &clip.keyframes,
        (t - clip.start_us).max(0),
    )
}

/// The effect clock of a clip at absolute time `t`: source time elapsed since the clip's first
/// frame, so motion follows clip speed.
fn clip_timing(project: &Project, clip: &MediaClip, t: i64) -> FxTiming {
    let elapsed = (clip.source_time_at(t) - clip.source_in_us).max(0);
    timing_for(project, clip, elapsed, clip.speed_at(t))
}

fn timing_for(project: &Project, clip: &MediaClip, elapsed_us: i64, speed: f64) -> FxTiming {
    FxTiming {
        time: elapsed_us as f64 / 1e6,
        duration: (clip.source_out_us - clip.source_in_us).max(0) as f64 / 1e6,
        frame: project.settings.fps.frame_at(elapsed_us).max(0) as u32,
        speed,
    }
}

/// A non-video media kind in a transition/clip surfaces as `NotVideo` carrying the clip's
/// media id (matches the pre-v7 error shape).
fn not_video_for(e: EvalError, media_id: Id) -> EvalError {
    match e {
        EvalError::NotVideo(_) => EvalError::NotVideo(media_id),
        other => other,
    }
}

/// True when the clip is not in a multicam group, or is the group's active angle.
pub fn multicam_angle_active(project: &Project, clip: &crate::project::MediaClip) -> bool {
    let Some(group_id) = clip.multicam_group_id else {
        return true;
    };
    let Some(group) = project.multicam_groups.iter().find(|g| g.id == group_id) else {
        return true;
    };
    group
        .angle_clip_ids
        .get(group.active_angle)
        .copied()
        .map(|id| id == clip.id)
        .unwrap_or(false)
}

/// Evaluate which caption-track clips are active (visible, burned-in) at `t`.
pub fn active_captions(project: &Project, t: i64) -> Vec<ActiveCaption> {
    let mut caps = Vec::new();
    for track in &project.tracks {
        if track.kind != TrackKind::Caption || track.hidden {
            continue;
        }
        for clip in &track.clips {
            let Clip::Caption(c) = clip else { continue };
            let end = c.start_us + c.duration_us;
            if t >= c.start_us && t < end {
                let style = Project::caption_style_for(track, c);
                caps.push(ActiveCaption {
                    text: c.body.text.clone(),
                    style_id: style.render_style_id(),
                    overrides: style.overrides.clone(),
                });
            }
        }
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Clip, ClipTransition, MediaClip, MediaItem, Project, Settings, Track};

    /// Root-level media/sequence layers only (adjustment steps are asserted on separately).
    fn layers_at(project: &Project, t: i64) -> Result<Vec<ActiveLayer>, EvalError> {
        Ok(active_steps(project, t)?
            .into_iter()
            .filter_map(|s| match s {
                ComposeStep::Layer(l) => Some(l),
                ComposeStep::Adjustment { .. } => None,
            })
            .collect())
    }

    fn base_project() -> Project {
        let mut project = Project::new("eval-test", Settings::default());
        project.tracks.push(Track::new(TrackKind::Video, "V1"));
        project
    }

    fn add_media(project: &mut Project, kind: MediaKind, path: &str) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        project.media.push(MediaItem {
            id,
            path: PathBuf::from(path),
            kind,
            duration_us: Some(10000000),
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            sequence_id: None,
        });
        id
    }

    #[test]
    fn active_layers_empty_project_has_no_layers() {
        let project = base_project();
        let layers = layers_at(&project, 0).unwrap();
        assert!(layers.is_empty());
    }

    #[test]
    fn active_layers_picks_clip_covering_time_and_computes_source_time() {
        let mut project = base_project();
        let media_id = add_media(&mut project, MediaKind::Video, "a.mp4");
        let clip = MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: 2000000,
            source_in_us: 0,
            source_out_us: 5000000,
            gain_db: 0.0,
            enabled: true,
            ..Default::default()
        };
        project.tracks[0].clips.push(Clip::Video(clip));

        let layers = layers_at(&project, 3000000).unwrap();
        assert_eq!(layers.len(), 1);
        let (_, path, source_time) = layers[0].media().unwrap();
        assert_eq!(path, std::path::Path::new("a.mp4"));
        // source_time_at(3.0) for a clip starting at 2.0 with source_in 0.0 and speed 1x = 1.0.
        assert!((source_time - 1000000).abs() < 1);
        assert!(layers[0].transition.is_none());
    }

    #[test]
    fn active_layers_outside_clip_range_yields_nothing() {
        let mut project = base_project();
        let media_id = add_media(&mut project, MediaKind::Video, "a.mp4");
        let clip = MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: 2000000,
            source_in_us: 0,
            source_out_us: 5000000,
            enabled: true,
            ..Default::default()
        };
        project.tracks[0].clips.push(Clip::Video(clip));

        assert!(layers_at(&project, 500000).unwrap().is_empty());
        assert!(layers_at(&project, 8000000).unwrap().is_empty());
    }

    #[test]
    fn active_layers_missing_media_is_an_eval_error() {
        let mut project = base_project();
        let missing_id = uuid::Uuid::new_v4();
        let clip = MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id: missing_id,
            start_us: 0,
            source_in_us: 0,
            source_out_us: 5000000,
            enabled: true,
            ..Default::default()
        };
        project.tracks[0].clips.push(Clip::Video(clip));

        let err = layers_at(&project, 0).unwrap_err();
        assert_eq!(err, EvalError::MediaNotFound(missing_id));
    }

    #[test]
    fn active_layers_hidden_track_is_skipped() {
        let mut project = base_project();
        let media_id = add_media(&mut project, MediaKind::Video, "a.mp4");
        let clip = MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: 0,
            source_in_us: 0,
            source_out_us: 5000000,
            enabled: true,
            ..Default::default()
        };
        project.tracks[0].clips.push(Clip::Video(clip));
        project.tracks[0].hidden = true;

        assert!(layers_at(&project, 1000000).unwrap().is_empty());
    }

    #[test]
    fn active_layers_transition_window_emits_outgoing_and_incoming() {
        let mut project = base_project();
        let media_a = add_media(&mut project, MediaKind::Video, "a.mp4");
        let media_b = add_media(&mut project, MediaKind::Video, "b.mp4");

        let clip_a = MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id: media_a,
            start_us: 0,
            source_in_us: 0,
            source_out_us: 4000000,
            enabled: true,
            outgoing_transition: Some(ClipTransition::new("builtin:crossfade@1", 1000000)),
            ..Default::default()
        };
        let clip_b = MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id: media_b,
            start_us: 4000000,
            source_in_us: 0,
            source_out_us: 4000000,
            enabled: true,
            ..Default::default()
        };

        project.tracks[0].clips.push(Clip::Video(clip_a));
        project.tracks[0].clips.push(Clip::Video(clip_b));

        // Mid-transition: t = 3.5 is inside [3.0, 4.0) window.
        let layers = layers_at(&project, 3500000).unwrap();
        assert_eq!(layers.len(), 2);
        assert!(layers[0].transition.as_ref().unwrap().progress > 0.0);
        assert!(!layers[0].transition.as_ref().unwrap().is_incoming);
        assert!(layers[1].transition.as_ref().unwrap().is_incoming);
    }

    #[test]
    fn active_captions_only_returns_captions_covering_time() {
        let mut project = base_project();
        project
            .tracks
            .push(Track::new(TrackKind::Caption, "Captions"));
        let track_idx = project.tracks.len() - 1;
        project.tracks[track_idx]
            .clips
            .push(Clip::Caption(crate::project::CaptionClip {
                id: uuid::Uuid::new_v4(),
                ..crate::project::CaptionClip::new(1000000, 2000000, "hello").with_style("default")
            }));

        assert!(active_captions(&project, 500000).is_empty());
        let caps = active_captions(&project, 2000000);
        assert_eq!(caps.len(), 1);
        assert_eq!(caps[0].text, "hello");
    }

    // ---- schema v7: adjustment layers and nested sequences -------------------------------

    use crate::project::{AdjustmentClip, Sequence};

    /// Register a sequence (one video track holding `clips`) and its media item; returns the
    /// sequence id and its media id.
    fn add_sequence(project: &mut Project, clips: Vec<Clip>) -> (uuid::Uuid, uuid::Uuid) {
        let id = uuid::Uuid::new_v4();
        let media_id = uuid::Uuid::new_v4();
        let mut track = Track::new(TrackKind::Video, "inner");
        track.clips = clips;
        project.sequences.push(Sequence {
            id,
            name: "S".into(),
            width: None,
            height: None,
            tracks: vec![track],
        });
        project.media.push(MediaItem {
            id: media_id,
            path: PathBuf::new(),
            kind: MediaKind::Sequence,
            duration_us: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(id),
        });
        (id, media_id)
    }

    fn media_clip(media_id: uuid::Uuid, position: f64, dur: f64) -> MediaClip {
        MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: (position * 1e6).round() as i64,
            source_in_us: 0,
            source_out_us: (dur * 1e6).round() as i64,
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn adjustment_steps_interleave_with_layers_in_track_order() {
        let mut project = base_project();
        let media = add_media(&mut project, MediaKind::Video, "a.mp4");
        project.tracks[0]
            .clips
            .push(Clip::Video(media_clip(media, 0.0, 5.0)));
        let mut adj_track = Track::new(TrackKind::Video, "adj");
        let mut adj = AdjustmentClip::new(2000000, 2000000);
        adj.opacity = 0.5;
        adj_track.clips.push(Clip::Adjustment(adj));
        project.tracks.push(adj_track);
        let mut top = Track::new(TrackKind::Video, "top");
        top.clips.push(Clip::Video(media_clip(media, 0.0, 5.0)));
        project.tracks.push(top);

        // Before the adjustment starts: two layers, no adjustment step.
        let steps = active_steps(&project, 1000000).unwrap();
        assert_eq!(steps.len(), 2);
        // During: layer, adjustment, layer (bottom to top).
        let steps = active_steps(&project, 2500000).unwrap();
        assert_eq!(steps.len(), 3);
        assert!(matches!(steps[0], ComposeStep::Layer(_)));
        match &steps[1] {
            ComposeStep::Adjustment { opacity, .. } => assert!((opacity - 0.5).abs() < 1e-6),
            other => panic!("expected adjustment, got {other:?}"),
        }
        assert!(matches!(steps[2], ComposeStep::Layer(_)));
        // The end is exclusive, and a disabled or hidden adjustment contributes nothing.
        assert_eq!(active_steps(&project, 4000000).unwrap().len(), 2);
        if let Clip::Adjustment(a) = &mut project.tracks[1].clips[0] {
            a.enabled = false;
        }
        assert_eq!(active_steps(&project, 2500000).unwrap().len(), 2);
        if let Clip::Adjustment(a) = &mut project.tracks[1].clips[0] {
            a.enabled = true;
        }
        project.tracks[1].hidden = true;
        assert_eq!(active_steps(&project, 2500000).unwrap().len(), 2);
    }

    #[test]
    fn adjustment_opacity_follows_keyframes() {
        use crate::project::{AnimProperty, Keyframe, KeyframeTrack};
        let mut project = base_project();
        let mut adj = AdjustmentClip::new(1000000, 4000000);
        adj.keyframes.push(KeyframeTrack {
            property: AnimProperty::Opacity.into(),
            keys: vec![Keyframe::new(0, 1.0), Keyframe::new(2000000, 0.0)],
        });
        project.tracks[0].clips.push(Clip::Adjustment(adj));
        let opacity_at = |t: i64| match &active_steps(&project, t).unwrap()[0] {
            ComposeStep::Adjustment { opacity, .. } => *opacity,
            other => panic!("expected adjustment, got {other:?}"),
        };
        assert!((opacity_at(1000000) - 1.0).abs() < 1e-6);
        assert!(
            (opacity_at(2000000) - 0.5).abs() < 1e-6,
            "local t=1 of a 0..2 fade"
        );
        assert!(opacity_at(4000000).abs() < 1e-6, "held after the last key");
    }

    #[test]
    fn sequence_clip_maps_time_and_exposes_inner_layers() {
        let mut project = base_project();
        let media = add_media(&mut project, MediaKind::Video, "inner.mp4");
        // Inside the sequence, the clip runs 2..6 of the sequence timeline.
        let (seq_id, seq_media) =
            add_sequence(&mut project, vec![Clip::Video(media_clip(media, 2.0, 4.0))]);
        // Outer clip plays sequence time 1.0.. at 2x, placed at root t=10.
        let mut outer = media_clip(seq_media, 10.0, 0.0);
        outer.source_in_us = 1_000_000;
        outer.source_out_us = 5_000_000;
        outer.speed = 2.0;
        project.tracks[0].clips.push(Clip::Video(outer));

        let steps = active_steps(&project, 11000000).unwrap();
        assert_eq!(steps.len(), 1);
        let ComposeStep::Layer(layer) = &steps[0] else {
            panic!()
        };
        let ActiveSource::Sequence {
            sequence_id,
            time_us,
            instance,
        } = &layer.source
        else {
            panic!("expected a sequence layer")
        };
        assert_eq!(*sequence_id, seq_id);
        // 1 s into the outer clip at 2x -> sequence time 1.0 + 2.0.
        assert!((time_us - 3000000).abs() < 1);
        assert_eq!(instance.depth(), 1);

        let inner = sequence_steps(&project, *sequence_id, *time_us, instance).unwrap();
        assert_eq!(inner.len(), 1);
        let ComposeStep::Layer(inner_layer) = &inner[0] else {
            panic!()
        };
        let (_, path, source_time) = inner_layer.media().unwrap();
        assert_eq!(path, std::path::Path::new("inner.mp4"));
        // Sequence time 3.0 is 1 s into the inner clip (which starts at 2.0).
        assert!((source_time - 1000000).abs() < 1);
        assert_eq!(inner_layer.instance.depth(), 2);
        assert!(inner_layer.instance.is_nested());
        assert!(!layer.instance.is_nested());

        // Past the end of the inner timeline the sequence layer is still emitted (it renders
        // transparent), but evaluates to no inner layers.
        let late = sequence_steps(&project, seq_id, 9000000, instance).unwrap();
        assert!(late.is_empty());
    }

    #[test]
    fn same_sequence_twice_gets_distinct_instance_keys() {
        let mut project = base_project();
        let media = add_media(&mut project, MediaKind::Video, "inner.mp4");
        let (_, seq_media) =
            add_sequence(&mut project, vec![Clip::Video(media_clip(media, 0.0, 4.0))]);
        project.tracks[0]
            .clips
            .push(Clip::Video(media_clip(seq_media, 0.0, 4.0)));
        let mut second = Track::new(TrackKind::Video, "V2");
        let mut shifted = media_clip(seq_media, 0.0, 2.0);
        shifted.source_in_us = 2_000_000;
        shifted.source_out_us = 4_000_000;
        second.clips.push(Clip::Video(shifted));
        project.tracks.push(second);

        let leaves = leaf_media_layers(&project, 1000000).unwrap();
        assert_eq!(leaves.len(), 2, "one inner layer per placement");
        assert_ne!(leaves[0].instance, leaves[1].instance);
        assert_ne!(
            leaves[0].instance.to_string(),
            leaves[1].instance.to_string()
        );
        // Same media, two different source times in one frame.
        let times: Vec<f64> = leaves
            .iter()
            .map(|l| l.media().unwrap().2 as f64 / 1e6)
            .collect();
        assert!(
            (times[0] - 1.0).abs() < 1e-9 && (times[1] - 3.0).abs() < 1e-9,
            "{times:?}"
        );
        assert!(leaves.iter().all(|l| l.instance.is_nested()));
    }

    #[test]
    fn eval_fails_cleanly_on_cycles_and_excess_depth() {
        // A sequence placing itself (a hand-edited file): bounded by the depth cap.
        let mut project = base_project();
        let (seq_id, seq_media) = add_sequence(&mut project, vec![]);
        project.sequences[0].tracks[0]
            .clips
            .push(Clip::Video(media_clip(seq_media, 0.0, 4.0)));
        project.tracks[0]
            .clips
            .push(Clip::Video(media_clip(seq_media, 0.0, 4.0)));
        let err = leaf_media_layers(&project, 1000000).unwrap_err();
        assert_eq!(err, EvalError::SequenceTooDeep(seq_id));

        // Unknown sequence referenced by sequence media.
        let mut project = base_project();
        let ghost = uuid::Uuid::new_v4();
        let media_id = uuid::Uuid::new_v4();
        project.media.push(MediaItem {
            id: media_id,
            path: PathBuf::new(),
            kind: MediaKind::Sequence,
            duration_us: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(ghost),
        });
        project.tracks[0]
            .clips
            .push(Clip::Video(media_clip(media_id, 0.0, 4.0)));
        let steps = active_steps(&project, 1000000).unwrap();
        let ComposeStep::Layer(l) = &steps[0] else {
            panic!()
        };
        let ActiveSource::Sequence {
            sequence_id,
            time_us,
            instance,
        } = &l.source
        else {
            panic!()
        };
        assert_eq!(
            sequence_steps(&project, *sequence_id, *time_us, instance).unwrap_err(),
            EvalError::SequenceNotFound(ghost)
        );
    }

    #[test]
    fn transition_between_sequence_clip_and_media_clip_pairs_the_layers() {
        let mut project = base_project();
        let media = add_media(&mut project, MediaKind::Video, "b.mp4");
        let inner_media = add_media(&mut project, MediaKind::Video, "inner.mp4");
        let (_, seq_media) = add_sequence(
            &mut project,
            vec![Clip::Video(media_clip(inner_media, 0.0, 4.0))],
        );
        let mut seq_clip = media_clip(seq_media, 0.0, 4.0);
        seq_clip.outgoing_transition = Some(ClipTransition::new("builtin:crossfade@1", 1000000));
        project.tracks[0].clips.push(Clip::Video(seq_clip));
        project.tracks[0]
            .clips
            .push(Clip::Video(media_clip(media, 4.0, 4.0)));

        let steps = active_steps(&project, 3500000).unwrap();
        assert_eq!(steps.len(), 2);
        let (ComposeStep::Layer(out), ComposeStep::Layer(inc)) = (&steps[0], &steps[1]) else {
            panic!()
        };
        assert!(matches!(out.source, ActiveSource::Sequence { .. }));
        assert!(matches!(inc.source, ActiveSource::Media { .. }));
        assert!(!out.transition.as_ref().unwrap().is_incoming);
        assert!(inc.transition.as_ref().unwrap().is_incoming);
        // The two sides never share an instance key (a second decoder/element is needed).
        assert_ne!(out.instance, inc.instance);
        // Sequence on the incoming side works too.
        let mut project2 = base_project();
        let media2 = add_media(&mut project2, MediaKind::Video, "b.mp4");
        let (_, seq_media2) = add_sequence(
            &mut project2,
            vec![Clip::Video(media_clip(media2, 0.0, 4.0))],
        );
        let mut first = media_clip(media2, 0.0, 4.0);
        first.outgoing_transition = Some(ClipTransition::new("builtin:crossfade@1", 1000000));
        project2.tracks[0].clips.push(Clip::Video(first));
        project2.tracks[0]
            .clips
            .push(Clip::Video(media_clip(seq_media2, 4.0, 4.0)));
        let steps = active_steps(&project2, 3500000).unwrap();
        let ComposeStep::Layer(inc) = &steps[1] else {
            panic!()
        };
        assert!(matches!(inc.source, ActiveSource::Sequence { .. }));
    }

    #[test]
    fn instance_key_display_is_stable_and_unique() {
        let a = uuid::Uuid::new_v4();
        let b = uuid::Uuid::new_v4();
        let root = InstanceKey::root(a);
        assert_eq!(root.to_string(), a.to_string());
        let nested = root.with(b);
        assert_eq!(nested.to_string(), format!("{a}/{b}"));
        assert_ne!(root, nested);
        assert_eq!(nested.ids(), &[a, b]);
    }
}
