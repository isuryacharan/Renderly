//! Command API — matches docs/command-api.md exactly. This is the *only* sanctioned way
//! to mutate a `Project` (see AGENTS.md §0.1). GUI, CLI, and MCP all dispatch here.

use crate::audio::{synthesize_to_wav, VoiceoverProvider};
use crate::media::{self, MediaError};
use crate::project::{
    clone_effects_with_new_ids, split_keyframes, AnimProperty, AudioDenoise, BackgroundRemoval,
    CaptionClip, Clip, ClipMask, ClipMaskKind, ClipTransform, ClipTransition, EffectInstance, Id,
    KeyframeTrack, MediaClip, MulticamGroup, Project, Track, TrackAudioRole, TrackKind,
    TransitionKind,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod adjustment;
mod motion;
mod reframe;
mod sequences;

pub use sequences::ClipRef;

/// Normalised rectangle (0..1, origin top-left, y down) in the clip's video frame, before any
/// cover-fit crop. Used to pick the area `TrackMotion` follows.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TrackRegion {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "PascalCase")]
pub enum Command {
    ImportMedia {
        path: String,
    },
    AddTrack {
        kind: TrackKind,
        name: String,
        /// Caller-supplied id, for callers that need to reference the new track from a
        /// second command in the same `apply_commands` batch (which can't otherwise see
        /// an earlier command's generated id — e.g. GUI drag-drop auto-track-on-drop:
        /// `AddTrack` + `AddClip{track_id}` atomically). Server-generates one if omitted.
        #[serde(default)]
        id: Option<Id>,
        /// Add the track to this sequence's timeline instead of the root timeline
        /// (schema v7). Caption tracks are root-only.
        #[serde(default)]
        sequence_id: Option<Id>,
    },
    AddClip {
        track_id: Id,
        media_id: Id,
        position_secs: f64,
        source_in_secs: f64,
        source_out_secs: f64,
    },
    SplitClip {
        track_id: Id,
        clip_id: Id,
        at_secs: f64,
    },
    TrimClip {
        track_id: Id,
        clip_id: Id,
        new_source_in_secs: Option<f64>,
        new_source_out_secs: Option<f64>,
    },
    MoveClip {
        track_id: Id,
        clip_id: Id,
        new_position_secs: f64,
        new_track_id: Option<Id>,
    },
    DeleteClip {
        track_id: Id,
        clip_id: Id,
        ripple: bool,
    },
    AddCaption {
        track_id: Id,
        text: String,
        position_secs: f64,
        duration_secs: f64,
        style_id: String,
    },
    /// Update an existing caption clip (Phase 2 caption editor).
    SetCaption {
        track_id: Id,
        clip_id: Id,
        text: Option<String>,
        position_secs: Option<f64>,
        duration_secs: Option<f64>,
        style_id: Option<String>,
    },
    SetAudioGain {
        track_id: Id,
        clip_id: Id,
        gain_db: f64,
    },
    Export {
        output_path: String,
        preset: ExportPreset,
    },
    /// Run local Whisper STT on a media item and add caption clips to a caption track.
    GenerateCaptions {
        media_id: Id,
        track_id: Id,
        style_id: String,
        /// Seconds added to each segment timestamp when placing on the timeline.
        #[serde(default)]
        timeline_offset_secs: f64,
    },
    /// Synthesize narration audio (Piper local or OpenAI BYO) and place on an audio track.
    GenerateVoiceover {
        text: String,
        track_id: Id,
        position_secs: f64,
        output_path: String,
        provider: VoiceoverProvider,
    },
    /// Set fade-in/out on an audio clip (applied during export).
    SetAudioFade {
        track_id: Id,
        clip_id: Id,
        fade_in_secs: f64,
        fade_out_secs: f64,
    },
    /// Assign mix role on an audio track (voiceover/dialog/music/ambience) for ducking.
    SetTrackAudioRole {
        track_id: Id,
        role: Option<TrackAudioRole>,
    },
    /// Change project-level output settings. At least one field must be `Some`.
    SetProjectSettings {
        width: Option<u32>,
        height: Option<u32>,
        fps: Option<f64>,
    },
    /// Set mute/lock/hidden flags on a track. At least one field must be `Some`.
    /// `locked` is GUI-honored only — see `Track::locked` doc comment.
    SetTrackFlags {
        track_id: Id,
        muted: Option<bool>,
        locked: Option<bool>,
        hidden: Option<bool>,
    },
    RenameTrack {
        track_id: Id,
        name: String,
    },
    DeleteTrack {
        track_id: Id,
    },
    /// Reorder a track within `Project::tracks` (the compositing z-order, first = bottom).
    /// `new_index` is clamped to the valid range; a no-op if the position is unchanged.
    MoveTrack {
        track_id: Id,
        new_index: usize,
    },
    /// Soft-enable/disable a media clip (video or audio) without deleting it.
    SetClipEnabled {
        track_id: Id,
        clip_id: Id,
        enabled: bool,
    },
    /// Replace the static transform on a media clip (Phase 3.1).
    SetClipTransform {
        track_id: Id,
        clip_id: Id,
        transform: ClipTransform,
    },
    /// Replace all keyframe tracks on a media clip (Phase 3.1).
    SetClipKeyframes {
        track_id: Id,
        clip_id: Id,
        keyframes: Vec<KeyframeTrack>,
    },
    /// Replace effect instance list (Phase 3.4 executes builtins).
    SetClipEffects {
        track_id: Id,
        clip_id: Id,
        effects: Vec<EffectInstance>,
    },
    /// Set or clear the outgoing transition on a video media clip (Phase 3.5).
    SetClipTransition {
        track_id: Id,
        clip_id: Id,
        transition: Option<ClipTransition>,
    },
    /// Constant clip playback speed (Phase 3). Timeline duration = source span / speed.
    SetClipSpeed {
        track_id: Id,
        clip_id: Id,
        speed: f64,
    },
    /// Load a declarative asset pack from a directory containing `pack.json`.
    LoadAssetPack {
        path: String,
    },
    /// Unload a previously loaded asset pack by id.
    UnloadAssetPack {
        pack_id: String,
    },
    /// Load a WASM frame-effect plugin (directory with `plugin.json` + `.wasm`).
    LoadWasmPlugin {
        path: String,
    },
    /// Unload a WASM plugin by id.
    UnloadWasmPlugin {
        plugin_id: String,
    },
    /// Place a sticker image from a loaded pack onto a video track.
    AddStickerFromPack {
        pack_id: String,
        sticker_id: String,
        track_id: Id,
        position_secs: f64,
    },
    /// Place an SFX audio clip from a loaded pack onto an audio track.
    AddSfxFromPack {
        pack_id: String,
        sfx_id: String,
        track_id: Id,
        position_secs: f64,
    },
    /// Set or clear the clip mask (Phase 4).
    SetClipMask {
        track_id: Id,
        clip_id: Id,
        mask: Option<ClipMask>,
    },
    /// Set or clear background-removal config (Phase 4).
    SetClipBackgroundRemoval {
        track_id: Id,
        clip_id: Id,
        config: Option<BackgroundRemoval>,
    },
    /// Bake a single-frame matte for background removal and wire `mask` (Phase 4).
    GenerateBackgroundMatte {
        track_id: Id,
        clip_id: Id,
    },
    /// Set or clear audio denoise on an audio-track clip (Phase 4).
    SetClipAudioDenoise {
        track_id: Id,
        clip_id: Id,
        config: Option<AudioDenoise>,
    },
    /// Apply a pack template's command sequence at a timeline offset (Phase 4).
    ApplyTemplate {
        pack_id: String,
        template_id: String,
        position_secs: f64,
    },
    /// Generate a simple sticker PNG from a prompt and place it on a video track (Phase 4).
    GenerateSticker {
        prompt: String,
        track_id: Id,
        position_secs: f64,
        output_path: String,
    },
    /// Create a multicam group from existing clip ids (Phase 4).
    CreateMulticamGroup {
        name: String,
        clip_ids: Vec<Id>,
    },
    /// Switch the active angle in a multicam group (Phase 4).
    SetMulticamAngle {
        group_id: Id,
        active_angle: usize,
    },
    /// Track a region of the clip with pyramidal Lucas-Kanade features + RANSAC and write
    /// `PosX`/`PosY` keyframes (the tracked point's position in NDC); optionally also
    /// `Rotation`/`ScaleX`/`ScaleY` (Phase 4).
    TrackMotion {
        track_id: Id,
        clip_id: Id,
        sample_count: u32,
        #[serde(default)]
        region: Option<TrackRegion>,
        #[serde(default)]
        track_rotation_scale: bool,
    },
    /// Feature-based stabilization: estimate global similarity motion, smooth the camera path
    /// and write compensating `PosX`/`PosY`/`Rotation`/`ScaleX`/`ScaleY` keyframes (Phase 4).
    StabilizeClip {
        track_id: Id,
        clip_id: Id,
        #[serde(default)]
        strength: Option<f64>,
        #[serde(default)]
        sample_count: Option<u32>,
    },
    /// Subject-aware reframe for `target_aspect` (Phase 4): detects faces (YuNet), keyframes
    /// `PosX`/`PosY` to keep the subject in frame; centred scale/offset when none is found.
    AutoReframeClip {
        track_id: Id,
        clip_id: Id,
        target_aspect: f64,
    },
    /// Create an empty nested sequence and its media item (schema v7). `width`/`height` of
    /// `None` inherit the project size.
    CreateSequence {
        name: String,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
        /// Caller-supplied sequence id (like `AddTrack`'s `id`); generated if omitted.
        #[serde(default)]
        id: Option<Id>,
    },
    RenameSequence {
        sequence_id: Id,
        name: String,
    },
    /// Set a sequence's output size; `None` resets that axis to the project size.
    SetSequenceSettings {
        sequence_id: Id,
        #[serde(default)]
        width: Option<u32>,
        #[serde(default)]
        height: Option<u32>,
    },
    /// Remove a sequence and its media item. Fails with `SequenceInUse` while any clip
    /// places it.
    DeleteSequence {
        sequence_id: Id,
    },
    /// Move clips (all on one timeline) into a new sequence and replace them with a single
    /// clip (plus one audio clip if any were audio) placing it.
    NestClips {
        clips: Vec<ClipRef>,
        name: String,
    },
    /// Inverse of `NestClips` for an untouched nested clip: copy the sequence's clips back
    /// into the parent timeline and remove the placing clip. The sequence is kept.
    UnnestClip {
        track_id: Id,
        clip_id: Id,
    },
    /// Add an adjustment layer clip to a video track (schema v7).
    AddAdjustmentLayer {
        track_id: Id,
        position_secs: f64,
        duration_secs: f64,
    },
    /// Edit an adjustment layer's span/opacity; at least one field must be `Some`.
    SetAdjustmentLayer {
        track_id: Id,
        clip_id: Id,
        position_secs: Option<f64>,
        duration_secs: Option<f64>,
        opacity: Option<f64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPreset {
    TikTok9x16,
    Youtube16x9,
    Custom { width: u32, height: u32, fps: f64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommandOutcome {
    MediaImported {
        media_id: Id,
    },
    TrackAdded {
        track_id: Id,
    },
    ClipAdded {
        clip_id: Id,
    },
    ClipSplit {
        left_id: Id,
        right_id: Id,
    },
    CaptionsGenerated {
        count: usize,
    },
    VoiceoverGenerated {
        media_id: Id,
        clip_id: Id,
    },
    SequenceCreated {
        sequence_id: Id,
        media_id: Id,
    },
    ClipsNested {
        sequence_id: Id,
        media_id: Id,
        video_clip_id: Option<Id>,
        audio_clip_id: Option<Id>,
    },
    Applied,
}

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("track not found: {0}")]
    TrackNotFound(Id),
    #[error("media not found: {0}")]
    MediaNotFound(Id),
    #[error("clip not found: {0} on track {1}")]
    ClipNotFound(Id, Id),
    #[error("track {0} is kind {1:?}, expected {2:?}")]
    TrackKindMismatch(Id, TrackKind, TrackKind),
    #[error("clip range [{0}, {1}) overlaps an existing clip on track {2}")]
    Overlap(f64, f64, Id),
    #[error("invalid range: source_out_secs ({0}) <= source_in_secs ({1})")]
    InvalidRange(f64, f64),
    #[error("source range exceeds media duration ({0}s)")]
    ExceedsMediaDuration(f64),
    #[error("split point {0} is not strictly inside the clip's span")]
    SplitOutOfBounds(f64),
    #[error("TrimClip requires at least one of new_source_in_secs/new_source_out_secs")]
    TrimRequiresChange,
    #[error("SetCaption requires at least one field to change")]
    SetCaptionRequiresChange,
    #[error("clip has no audio: {0}")]
    NoAudio(Id),
    #[error("{0}")]
    Media(#[from] MediaError),
    #[error("{0}")]
    Export(#[from] crate::export::ExportError),
    #[error("{0}")]
    Perceive(#[from] crate::perceive::PerceiveError),
    #[error("{0}")]
    Tts(#[from] crate::audio::TtsError),
    #[error("invalid fade: fade durations must be >= 0")]
    InvalidFade,
    #[error("SetProjectSettings requires at least one of width/height/fps")]
    SetProjectSettingsRequiresChange,
    #[error("invalid dimensions: width and height must be > 0")]
    InvalidDimensions,
    #[error("SetTrackFlags requires at least one of muted/locked/hidden")]
    SetTrackFlagsRequiresChange,
    #[error("clip {0} is not a media clip (video/audio)")]
    NotMediaClip(Id),
    #[error("track id {0} already exists")]
    DuplicateTrackId(Id),
    #[error("invalid duration: {0} must be > 0")]
    InvalidDuration(f64),
    #[error("invalid fps: {0} must be > 0")]
    InvalidFps(f64),
    #[error("invalid transform: non-finite or out-of-range field")]
    InvalidTransform,
    #[error("invalid keyframes: {0}")]
    InvalidKeyframes(String),
    #[error("invalid effects: {0}")]
    InvalidEffects(String),
    #[error("invalid transition: {0}")]
    InvalidTransition(String),
    #[error("invalid speed: {0}")]
    InvalidSpeed(String),
    #[error("asset pack error: {0}")]
    AssetPack(String),
    #[error("wasm plugin error: {0}")]
    WasmPlugin(String),
    #[error("invalid mask: {0}")]
    InvalidMask(String),
    #[error("invalid denoise: {0}")]
    InvalidDenoise(String),
    #[error("template error: {0}")]
    Template(String),
    #[error("multicam error: {0}")]
    Multicam(String),
    #[error("analysis error: {0}")]
    Analysis(String),
    #[error("not yet implemented: {0}")]
    NotImplemented(&'static str),
    #[error("sequence not found: {0}")]
    SequenceNotFound(Id),
    #[error("sequence id {0} already exists")]
    DuplicateSequenceId(Id),
    #[error("sequence {0} is still placed by at least one clip")]
    SequenceInUse(Id),
    #[error("placing sequence {0} here would make a sequence contain itself")]
    SequenceCycle(Id),
    #[error("not supported for sequences (schema v7): {0}")]
    UnsupportedForSequence(&'static str),
    #[error("cannot unnest: {0}")]
    CannotUnnest(String),
    #[error("cannot nest: {0}")]
    InvalidNest(String),
    #[error("clip {0} is not an adjustment layer")]
    NotAdjustmentClip(Id),
    #[error("invalid adjustment layer: {0}")]
    InvalidAdjustment(String),
    #[error("SetAdjustmentLayer requires at least one of position_secs/duration_secs/opacity")]
    SetAdjustmentLayerRequiresChange,
    #[error(
        "TrimClip does not apply to adjustment layer {0} (it is defined in source time); \
         use SetAdjustmentLayer"
    )]
    TrimAdjustment(Id),
}

pub fn apply_command(project: &mut Project, cmd: Command) -> Result<CommandOutcome, CommandError> {
    match cmd {
        Command::ImportMedia { path } => import_media(project, &path),
        Command::AddTrack {
            kind,
            name,
            id,
            sequence_id,
        } => add_track(project, kind, name, id, sequence_id),
        Command::AddClip {
            track_id,
            media_id,
            position_secs,
            source_in_secs,
            source_out_secs,
        } => add_clip(
            project,
            track_id,
            media_id,
            position_secs,
            source_in_secs,
            source_out_secs,
        ),
        Command::SplitClip {
            track_id,
            clip_id,
            at_secs,
        } => split_clip(project, track_id, clip_id, at_secs),
        Command::TrimClip {
            track_id,
            clip_id,
            new_source_in_secs,
            new_source_out_secs,
        } => trim_clip(
            project,
            track_id,
            clip_id,
            new_source_in_secs,
            new_source_out_secs,
        ),
        Command::MoveClip {
            track_id,
            clip_id,
            new_position_secs,
            new_track_id,
        } => move_clip(project, track_id, clip_id, new_position_secs, new_track_id),
        Command::DeleteClip {
            track_id,
            clip_id,
            ripple,
        } => delete_clip(project, track_id, clip_id, ripple),
        Command::AddCaption {
            track_id,
            text,
            position_secs,
            duration_secs,
            style_id,
        } => add_caption(
            project,
            track_id,
            text,
            position_secs,
            duration_secs,
            style_id,
        ),
        Command::SetCaption {
            track_id,
            clip_id,
            text,
            position_secs,
            duration_secs,
            style_id,
        } => set_caption(
            project,
            track_id,
            clip_id,
            text,
            position_secs,
            duration_secs,
            style_id,
        ),
        Command::SetAudioGain {
            track_id,
            clip_id,
            gain_db,
        } => set_audio_gain(project, track_id, clip_id, gain_db),
        Command::Export {
            output_path,
            preset,
        } => export_project_cmd(project, &output_path, preset),
        Command::GenerateCaptions {
            media_id,
            track_id,
            style_id,
            timeline_offset_secs,
        } => generate_captions(project, media_id, track_id, style_id, timeline_offset_secs),
        Command::GenerateVoiceover {
            text,
            track_id,
            position_secs,
            output_path,
            provider,
        } => generate_voiceover(
            project,
            &text,
            track_id,
            position_secs,
            &output_path,
            provider,
        ),
        Command::SetAudioFade {
            track_id,
            clip_id,
            fade_in_secs,
            fade_out_secs,
        } => set_audio_fade(project, track_id, clip_id, fade_in_secs, fade_out_secs),
        Command::SetTrackAudioRole { track_id, role } => {
            set_track_audio_role(project, track_id, role)
        }
        Command::SetProjectSettings { width, height, fps } => {
            set_project_settings(project, width, height, fps)
        }
        Command::SetTrackFlags {
            track_id,
            muted,
            locked,
            hidden,
        } => set_track_flags(project, track_id, muted, locked, hidden),
        Command::RenameTrack { track_id, name } => rename_track(project, track_id, name),
        Command::DeleteTrack { track_id } => delete_track(project, track_id),
        Command::MoveTrack {
            track_id,
            new_index,
        } => move_track(project, track_id, new_index),
        Command::SetClipEnabled {
            track_id,
            clip_id,
            enabled,
        } => set_clip_enabled(project, track_id, clip_id, enabled),
        Command::SetClipTransform {
            track_id,
            clip_id,
            transform,
        } => set_clip_transform(project, track_id, clip_id, transform),
        Command::SetClipKeyframes {
            track_id,
            clip_id,
            keyframes,
        } => set_clip_keyframes(project, track_id, clip_id, keyframes),
        Command::SetClipEffects {
            track_id,
            clip_id,
            effects,
        } => set_clip_effects(project, track_id, clip_id, effects),
        Command::SetClipTransition {
            track_id,
            clip_id,
            transition,
        } => set_clip_transition(project, track_id, clip_id, transition),
        Command::SetClipSpeed {
            track_id,
            clip_id,
            speed,
        } => set_clip_speed(project, track_id, clip_id, speed),
        Command::LoadAssetPack { path } => load_asset_pack(project, &path),
        Command::UnloadAssetPack { pack_id } => unload_asset_pack(project, &pack_id),
        Command::LoadWasmPlugin { path } => load_wasm_plugin(project, &path),
        Command::UnloadWasmPlugin { plugin_id } => unload_wasm_plugin(project, &plugin_id),
        Command::AddStickerFromPack {
            pack_id,
            sticker_id,
            track_id,
            position_secs,
        } => add_sticker_from_pack(project, &pack_id, &sticker_id, track_id, position_secs),
        Command::AddSfxFromPack {
            pack_id,
            sfx_id,
            track_id,
            position_secs,
        } => add_sfx_from_pack(project, &pack_id, &sfx_id, track_id, position_secs),
        Command::SetClipMask {
            track_id,
            clip_id,
            mask,
        } => set_clip_mask(project, track_id, clip_id, mask),
        Command::SetClipBackgroundRemoval {
            track_id,
            clip_id,
            config,
        } => set_clip_background_removal(project, track_id, clip_id, config),
        Command::GenerateBackgroundMatte { track_id, clip_id } => {
            generate_background_matte(project, track_id, clip_id)
        }
        Command::SetClipAudioDenoise {
            track_id,
            clip_id,
            config,
        } => set_clip_audio_denoise(project, track_id, clip_id, config),
        Command::ApplyTemplate {
            pack_id,
            template_id,
            position_secs,
        } => apply_template(project, &pack_id, &template_id, position_secs),
        Command::GenerateSticker {
            prompt,
            track_id,
            position_secs,
            output_path,
        } => generate_sticker(project, &prompt, track_id, position_secs, &output_path),
        Command::CreateMulticamGroup { name, clip_ids } => {
            create_multicam_group(project, name, clip_ids)
        }
        Command::SetMulticamAngle {
            group_id,
            active_angle,
        } => set_multicam_angle(project, group_id, active_angle),
        Command::TrackMotion {
            track_id,
            clip_id,
            sample_count,
            region,
            track_rotation_scale,
        } => motion::track_motion(
            project,
            track_id,
            clip_id,
            sample_count,
            region,
            track_rotation_scale,
        ),
        Command::StabilizeClip {
            track_id,
            clip_id,
            strength,
            sample_count,
        } => motion::stabilize_clip(project, track_id, clip_id, strength, sample_count),
        Command::AutoReframeClip {
            track_id,
            clip_id,
            target_aspect,
        } => reframe::auto_reframe_clip(project, track_id, clip_id, target_aspect),
        Command::CreateSequence {
            name,
            width,
            height,
            id,
        } => sequences::create_sequence(project, name, width, height, id),
        Command::RenameSequence { sequence_id, name } => {
            sequences::rename_sequence(project, sequence_id, name)
        }
        Command::SetSequenceSettings {
            sequence_id,
            width,
            height,
        } => sequences::set_sequence_settings(project, sequence_id, width, height),
        Command::DeleteSequence { sequence_id } => sequences::delete_sequence(project, sequence_id),
        Command::NestClips { clips, name } => sequences::nest_clips(project, clips, name),
        Command::UnnestClip { track_id, clip_id } => {
            sequences::unnest_clip(project, track_id, clip_id)
        }
        Command::AddAdjustmentLayer {
            track_id,
            position_secs,
            duration_secs,
        } => adjustment::add_adjustment_layer(project, track_id, position_secs, duration_secs),
        Command::SetAdjustmentLayer {
            track_id,
            clip_id,
            position_secs,
            duration_secs,
            opacity,
        } => adjustment::set_adjustment_layer(
            project,
            track_id,
            clip_id,
            position_secs,
            duration_secs,
            opacity,
        ),
    }
}

fn import_media(project: &mut Project, path: &str) -> Result<CommandOutcome, CommandError> {
    use crate::project::MediaItem;
    use std::path::PathBuf;

    let path_buf = PathBuf::from(path);
    let probed = media::probe(&path_buf)?;
    let media_id = Id::new_v4();
    project.media.push(MediaItem {
        id: media_id,
        path: path_buf,
        kind: probed.kind.expect("probe() always sets kind on success"),
        duration_secs: probed.duration_secs,
        width: probed.width,
        height: probed.height,
        fps: probed.fps,
        sequence_id: None,
    });
    Ok(CommandOutcome::MediaImported { media_id })
}

fn add_track(
    project: &mut Project,
    kind: TrackKind,
    name: String,
    id: Option<Id>,
    sequence_id: Option<Id>,
) -> Result<CommandOutcome, CommandError> {
    if let Some(wanted) = id {
        // Track ids are unique across the root timeline and every sequence.
        if project.find_track(wanted).is_some() {
            return Err(CommandError::DuplicateTrackId(wanted));
        }
    }
    if let Some(seq_id) = sequence_id {
        if project.find_sequence(seq_id).is_none() {
            return Err(CommandError::SequenceNotFound(seq_id));
        }
        if kind == TrackKind::Caption {
            return Err(CommandError::UnsupportedForSequence("caption tracks"));
        }
    }
    let mut track = Track::new(kind, name);
    if let Some(id) = id {
        track.id = id;
    }
    let track_id = track.id;
    let tracks = project
        .container_tracks_mut(sequence_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    tracks.push(track);
    Ok(CommandOutcome::TrackAdded { track_id })
}

fn clip_kind_matches(track_kind: TrackKind, clip: &Clip) -> bool {
    matches!(
        (track_kind, clip),
        (TrackKind::Video, Clip::Video(_))
            | (TrackKind::Audio, Clip::Audio(_))
            | (TrackKind::Caption, Clip::Caption(_))
            | (TrackKind::Video, Clip::Adjustment(_))
    )
}

fn clip_track_kind(clip: &Clip) -> TrackKind {
    match clip {
        Clip::Video(_) => TrackKind::Video,
        Clip::Audio(_) => TrackKind::Audio,
        Clip::Caption(_) => TrackKind::Caption,
        Clip::Adjustment(_) => TrackKind::Video,
    }
}

/// Tolerance for overlap comparisons, so floating-point position math (e.g.
/// `generate_captions` placing consecutive Whisper segments back-to-back) doesn't produce
/// spurious overlaps from sub-microsecond rounding error.
const OVERLAP_EPS: f64 = 1e-6;

fn check_no_overlap(
    track: &Track,
    position_secs: f64,
    duration_secs: f64,
    ignore_clip: Option<Id>,
) -> Result<(), CommandError> {
    let new_end = position_secs + duration_secs;
    for clip in &track.clips {
        if Some(clip.id()) == ignore_clip {
            continue;
        }
        let existing_start = clip.position_secs();
        let existing_end = clip.end_secs();
        if position_secs < existing_end - OVERLAP_EPS && existing_start < new_end - OVERLAP_EPS {
            return Err(CommandError::Overlap(position_secs, new_end, track.id));
        }
    }
    Ok(())
}

fn add_clip(
    project: &mut Project,
    track_id: Id,
    media_id: Id,
    position_secs: f64,
    source_in_secs: f64,
    source_out_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    if source_out_secs <= source_in_secs {
        return Err(CommandError::InvalidRange(source_out_secs, source_in_secs));
    }

    let media = project
        .find_media(media_id)
        .ok_or(CommandError::MediaNotFound(media_id))?;
    let media_kind = media.kind;
    let media_sequence = media.sequence_id;
    // Derived for sequence media, so a nested clip can never reach past the inner timeline.
    let media_duration = project.media_duration(media_id);

    if let Some(duration) = media_duration {
        if source_out_secs > duration {
            return Err(CommandError::ExceedsMediaDuration(duration));
        }
    }

    // Placing a sequence must not make it contain itself (checked before the mutable borrow).
    if let Some(seq_id) = media_sequence {
        if let Some(container) = project.container_of_track(track_id) {
            if project.would_create_cycle(container, seq_id) {
                return Err(CommandError::SequenceCycle(seq_id));
            }
        }
    }

    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;

    let expected_track_kind = match media_kind {
        crate::project::MediaKind::Video => TrackKind::Video,
        crate::project::MediaKind::Audio => TrackKind::Audio,
        crate::project::MediaKind::Image => TrackKind::Video,
        // A sequence's picture goes on a video track, its sound on an audio track.
        crate::project::MediaKind::Sequence => {
            if track.kind == TrackKind::Audio {
                TrackKind::Audio
            } else {
                TrackKind::Video
            }
        }
    };
    if track.kind != expected_track_kind {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            expected_track_kind,
        ));
    }

    let duration_secs = source_out_secs - source_in_secs;
    check_no_overlap(track, position_secs, duration_secs, None)?;

    let clip_id = Id::new_v4();
    let media_clip = MediaClip {
        id: clip_id,
        media_id,
        position_secs,
        source_in_secs,
        source_out_secs,
        gain_db: 0.0,
        enabled: true,
        fade_in_secs: 0.0,
        fade_out_secs: 0.0,
        ..Default::default()
    };
    let clip = match track.kind {
        TrackKind::Video => Clip::Video(media_clip),
        TrackKind::Audio => Clip::Audio(media_clip),
        TrackKind::Caption => unreachable!("caption tracks rejected above"),
    };
    track.clips.push(clip);

    Ok(CommandOutcome::ClipAdded { clip_id })
}

fn split_clip(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    at_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;

    let idx = track
        .clips
        .iter()
        .position(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;

    let original = track.clips[idx].clone();
    let start = original.position_secs();
    let end = original.end_secs();
    if at_secs <= start || at_secs >= end {
        return Err(CommandError::SplitOutOfBounds(at_secs));
    }

    let split_offset = at_secs - start;
    let right_id = Id::new_v4();

    let (left, right) = match original {
        Clip::Video(mut m) => {
            let mut right_m = m.clone();
            right_m.id = right_id;
            m.source_out_secs = m.source_in_secs + split_offset;
            right_m.position_secs = at_secs;
            right_m.source_in_secs = m.source_out_secs;
            let (left_kf, right_kf) = split_keyframes(&m.keyframes, split_offset);
            m.keyframes = left_kf;
            right_m.keyframes = right_kf;
            right_m.effects = clone_effects_with_new_ids(&m.effects);
            right_m.outgoing_transition = None;
            (Clip::Video(m), Clip::Video(right_m))
        }
        Clip::Audio(mut m) => {
            let mut right_m = m.clone();
            right_m.id = right_id;
            m.source_out_secs = m.source_in_secs + split_offset;
            right_m.position_secs = at_secs;
            right_m.source_in_secs = m.source_out_secs;
            let (left_kf, right_kf) = split_keyframes(&m.keyframes, split_offset);
            m.keyframes = left_kf;
            right_m.keyframes = right_kf;
            right_m.effects = clone_effects_with_new_ids(&m.effects);
            right_m.outgoing_transition = None;
            (Clip::Audio(m), Clip::Audio(right_m))
        }
        Clip::Caption(mut c) => {
            let mut right_c = c.clone();
            right_c.id = right_id;
            c.duration_secs = split_offset;
            right_c.position_secs = at_secs;
            right_c.duration_secs = end - at_secs;
            (Clip::Caption(c), Clip::Caption(right_c))
        }
        Clip::Adjustment(mut a) => {
            let mut right_a = a.clone();
            right_a.id = right_id;
            a.duration_secs = split_offset;
            right_a.position_secs = at_secs;
            right_a.duration_secs = end - at_secs;
            let (left_kf, right_kf) = split_keyframes(&a.keyframes, split_offset);
            a.keyframes = left_kf;
            right_a.keyframes = right_kf;
            right_a.effects = clone_effects_with_new_ids(&a.effects);
            (Clip::Adjustment(a), Clip::Adjustment(right_a))
        }
    };

    let left_id = left.id();
    track.clips[idx] = left;
    track.clips.push(right);

    Ok(CommandOutcome::ClipSplit { left_id, right_id })
}

fn trim_clip(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    new_source_in_secs: Option<f64>,
    new_source_out_secs: Option<f64>,
) -> Result<CommandOutcome, CommandError> {
    if new_source_in_secs.is_none() && new_source_out_secs.is_none() {
        return Err(CommandError::TrimRequiresChange);
    }

    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .find_clip(clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;

    let media_clip = match clip {
        Clip::Video(m) | Clip::Audio(m) => m,
        Clip::Caption(_) => return Err(CommandError::NotMediaClip(clip_id)),
        Clip::Adjustment(_) => return Err(CommandError::TrimAdjustment(clip_id)),
    };

    let new_in = new_source_in_secs.unwrap_or(media_clip.source_in_secs);
    let new_out = new_source_out_secs.unwrap_or(media_clip.source_out_secs);
    if new_out <= new_in {
        return Err(CommandError::InvalidRange(new_out, new_in));
    }

    let media_id = media_clip.media_id;
    let position_secs = media_clip.position_secs;

    // Same bounds check `AddClip` enforces — trimming shouldn't be able to reach past the
    // media's probed duration just because `AddClip`'s check only ran once, at creation.
    if let Some(duration) = project.media_duration(media_id) {
        if new_out > duration {
            return Err(CommandError::ExceedsMediaDuration(duration));
        }
    }

    // Trimming changes the clip's on-timeline duration (position_secs is untouched), which
    // can newly collide with a neighbor — every other mutator that changes a clip's span
    // enforces this; TrimClip was the one exception.
    let new_duration = new_out - new_in;
    check_no_overlap(track, position_secs, new_duration, Some(clip_id))?;

    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;
    let media_clip = match clip {
        Clip::Video(m) | Clip::Audio(m) => m,
        Clip::Caption(_) | Clip::Adjustment(_) => {
            unreachable!("non-media clips already rejected above")
        }
    };

    media_clip.source_in_secs = new_in;
    media_clip.source_out_secs = new_out;

    Ok(CommandOutcome::Applied)
}

fn move_clip(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    new_position_secs: f64,
    new_track_id: Option<Id>,
) -> Result<CommandOutcome, CommandError> {
    let dest_track_id = new_track_id.unwrap_or(track_id);

    let src_track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = src_track
        .find_clip(clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?
        .clone();

    if dest_track_id != track_id {
        let dest_track = project
            .find_track(dest_track_id)
            .ok_or(CommandError::TrackNotFound(dest_track_id))?;
        if !clip_kind_matches(dest_track.kind, &clip) {
            return Err(CommandError::TrackKindMismatch(
                dest_track_id,
                dest_track.kind,
                clip_track_kind(&clip),
            ));
        }
        // Moving a sequence clip into another timeline must not create a cycle.
        let placed_sequence = clip
            .as_media()
            .and_then(|m| project.find_media(m.media_id))
            .and_then(|media| media.sequence_id);
        if let (Some(seq_id), Some(container)) =
            (placed_sequence, project.container_of_track(dest_track_id))
        {
            if project.would_create_cycle(container, seq_id) {
                return Err(CommandError::SequenceCycle(seq_id));
            }
        }
    }

    let duration = clip.duration_secs();
    {
        let dest_track = project.find_track(dest_track_id).unwrap();
        let ignore = if dest_track_id == track_id {
            Some(clip_id)
        } else {
            None
        };
        check_no_overlap(dest_track, new_position_secs, duration, ignore)?;
    }

    // Remove from source, update position, insert into destination.
    let src_track = project.find_track_mut(track_id).unwrap();
    let idx = src_track
        .clips
        .iter()
        .position(|c| c.id() == clip_id)
        .unwrap();
    let mut moved = src_track.clips.remove(idx);
    moved.set_position_secs(new_position_secs);

    let dest_track = project.find_track_mut(dest_track_id).unwrap();
    dest_track.clips.push(moved);

    Ok(CommandOutcome::Applied)
}

fn delete_clip(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    ripple: bool,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;

    let idx = track
        .clips
        .iter()
        .position(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;

    let removed = track.clips.remove(idx);

    if ripple {
        let gap_start = removed.position_secs();
        let gap = removed.duration_secs();
        for clip in track.clips.iter_mut() {
            if clip.position_secs() >= gap_start {
                clip.set_position_secs(clip.position_secs() - gap);
            }
        }
    }

    Ok(CommandOutcome::Applied)
}

fn add_caption(
    project: &mut Project,
    track_id: Id,
    text: String,
    position_secs: f64,
    duration_secs: f64,
    style_id: String,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;

    if track.kind != TrackKind::Caption {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Caption,
        ));
    }
    if duration_secs <= 0.0 {
        return Err(CommandError::InvalidDuration(duration_secs));
    }

    check_no_overlap(track, position_secs, duration_secs, None)?;

    let clip_id = Id::new_v4();
    track.clips.push(Clip::Caption(CaptionClip {
        id: clip_id,
        text,
        position_secs,
        duration_secs,
        style_id,
    }));

    Ok(CommandOutcome::ClipAdded { clip_id })
}

fn set_caption(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    text: Option<String>,
    position_secs: Option<f64>,
    duration_secs: Option<f64>,
    style_id: Option<String>,
) -> Result<CommandOutcome, CommandError> {
    if text.is_none() && position_secs.is_none() && duration_secs.is_none() && style_id.is_none() {
        return Err(CommandError::SetCaptionRequiresChange);
    }

    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Caption {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Caption,
        ));
    }

    // Resolve the existing clip first: if `clip_id` doesn't exist, report ClipNotFound
    // rather than silently defaulting position/duration to 0.0/0.1 and risking a
    // misleading Overlap error instead.
    let existing = track
        .find_clip(clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;
    let new_position = position_secs.unwrap_or_else(|| existing.position_secs());
    let new_duration = duration_secs.unwrap_or_else(|| existing.duration_secs());
    if new_duration <= 0.0 {
        return Err(CommandError::InvalidDuration(new_duration));
    }

    check_no_overlap(track, new_position, new_duration, Some(clip_id))?;

    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;

    match clip {
        Clip::Caption(c) => {
            if let Some(t) = text {
                c.text = t;
            }
            if let Some(p) = position_secs {
                c.position_secs = p;
            }
            if let Some(d) = duration_secs {
                c.duration_secs = d;
            }
            if let Some(s) = style_id {
                c.style_id = s;
            }
            Ok(CommandOutcome::Applied)
        }
        _ => Err(CommandError::ClipNotFound(clip_id, track_id)),
    }
}

fn export_project_cmd(
    _project: &mut Project,
    output_path: &str,
    preset: ExportPreset,
) -> Result<CommandOutcome, CommandError> {
    use crate::export::export_project;
    use std::path::Path;

    export_project(_project, Path::new(output_path), preset)?;
    Ok(CommandOutcome::Applied)
}

fn generate_captions(
    project: &mut Project,
    media_id: Id,
    track_id: Id,
    style_id: String,
    timeline_offset_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    use crate::captions::BUILTIN_STYLES;
    use crate::perceive::transcribe_media;

    if !BUILTIN_STYLES.contains(&style_id.as_str()) {
        // Allow any style id but warn via default fallback in renderer — still accept custom ids.
    }

    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Caption {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Caption,
        ));
    }
    reject_sequence_media(project, media_id, "GenerateCaptions")?;

    let transcript = transcribe_media(project, media_id)?;
    let mut count = 0usize;

    for seg in transcript.segments {
        let text = seg.text.trim();
        if text.is_empty() {
            continue;
        }
        let duration = (seg.end_secs - seg.start_secs).max(0.1);
        let position = timeline_offset_secs + seg.start_secs;
        // Skip segments that fail to place (e.g. two whisper segments landing on
        // overlapping timestamps) instead of aborting the whole batch via `?` — an error
        // partway through would otherwise still leave the already-added captions mutated
        // into `project` while reporting failure, silently losing the rest of the
        // transcript. Best-effort captioning of everything that fits is more useful to an
        // agent than an all-or-nothing batch.
        if add_caption(
            project,
            track_id,
            text.to_string(),
            position,
            duration,
            style_id.clone(),
        )
        .is_ok()
        {
            count += 1;
        }
    }

    Ok(CommandOutcome::CaptionsGenerated { count })
}

fn set_audio_gain(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    gain_db: f64,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;

    match clip {
        Clip::Audio(m) => {
            m.gain_db = gain_db;
            Ok(CommandOutcome::Applied)
        }
        _ => Err(CommandError::NoAudio(clip_id)),
    }
}

fn set_audio_fade(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    fade_in_secs: f64,
    fade_out_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    if fade_in_secs < 0.0 || fade_out_secs < 0.0 {
        return Err(CommandError::InvalidFade);
    }

    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;

    match clip {
        Clip::Audio(m) => {
            m.fade_in_secs = fade_in_secs;
            m.fade_out_secs = fade_out_secs;
            Ok(CommandOutcome::Applied)
        }
        _ => Err(CommandError::NoAudio(clip_id)),
    }
}

fn set_track_audio_role(
    project: &mut Project,
    track_id: Id,
    role: Option<TrackAudioRole>,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Audio {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Audio,
        ));
    }
    track.audio_role = role;
    Ok(CommandOutcome::Applied)
}

fn set_project_settings(
    project: &mut Project,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<f64>,
) -> Result<CommandOutcome, CommandError> {
    if width.is_none() && height.is_none() && fps.is_none() {
        return Err(CommandError::SetProjectSettingsRequiresChange);
    }
    let new_width = width.unwrap_or(project.settings.width);
    let new_height = height.unwrap_or(project.settings.height);
    if new_width == 0 || new_height == 0 {
        return Err(CommandError::InvalidDimensions);
    }
    if let Some(fps) = fps {
        if fps <= 0.0 {
            return Err(CommandError::InvalidFps(fps));
        }
    }

    project.settings.width = new_width;
    project.settings.height = new_height;
    if let Some(fps) = fps {
        project.settings.fps = fps;
    }
    Ok(CommandOutcome::Applied)
}

fn set_track_flags(
    project: &mut Project,
    track_id: Id,
    muted: Option<bool>,
    locked: Option<bool>,
    hidden: Option<bool>,
) -> Result<CommandOutcome, CommandError> {
    if muted.is_none() && locked.is_none() && hidden.is_none() {
        return Err(CommandError::SetTrackFlagsRequiresChange);
    }
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if let Some(m) = muted {
        track.muted = m;
    }
    if let Some(l) = locked {
        track.locked = l;
    }
    if let Some(h) = hidden {
        track.hidden = h;
    }
    Ok(CommandOutcome::Applied)
}

fn rename_track(
    project: &mut Project,
    track_id: Id,
    name: String,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    track.name = name;
    Ok(CommandOutcome::Applied)
}

fn delete_track(project: &mut Project, track_id: Id) -> Result<CommandOutcome, CommandError> {
    // Acts within the track's own timeline (root or sequence).
    let container = project
        .container_of_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let tracks = project
        .container_tracks_mut(container)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let idx = tracks
        .iter()
        .position(|t| t.id == track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    tracks.remove(idx);
    Ok(CommandOutcome::Applied)
}

fn move_track(
    project: &mut Project,
    track_id: Id,
    new_index: usize,
) -> Result<CommandOutcome, CommandError> {
    // Reorders within the track's own timeline (root or sequence).
    let container = project
        .container_of_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let tracks = project
        .container_tracks_mut(container)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let idx = tracks
        .iter()
        .position(|t| t.id == track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clamped = new_index.min(tracks.len() - 1);
    if clamped == idx {
        return Ok(CommandOutcome::Applied);
    }
    let track = tracks.remove(idx);
    tracks.insert(clamped, track);
    Ok(CommandOutcome::Applied)
}

fn set_clip_enabled(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    enabled: bool,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;

    match clip {
        Clip::Video(m) | Clip::Audio(m) => {
            m.enabled = enabled;
            Ok(CommandOutcome::Applied)
        }
        Clip::Adjustment(a) => {
            a.enabled = enabled;
            Ok(CommandOutcome::Applied)
        }
        Clip::Caption(_) => Err(CommandError::NotMediaClip(clip_id)),
    }
}

fn media_clip_mut(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
) -> Result<&mut MediaClip, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;
    clip.as_media_mut()
        .ok_or(CommandError::NotMediaClip(clip_id))
}

/// Any clip (media, adjustment or caption) by track + id; the caller matches on the kind.
fn any_clip_mut(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
) -> Result<&mut Clip, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))
}

fn validate_transform(t: &ClipTransform) -> Result<ClipTransform, CommandError> {
    if ![t.x, t.y, t.scale_x, t.scale_y, t.rotation_deg, t.opacity]
        .iter()
        .all(|v| v.is_finite())
    {
        return Err(CommandError::InvalidTransform);
    }
    Ok(t.clamp_opacity())
}

fn validate_keyframes(tracks: Vec<KeyframeTrack>) -> Result<Vec<KeyframeTrack>, CommandError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(tracks.len());
    for mut track in tracks {
        if !seen.insert(track.property) {
            return Err(CommandError::InvalidKeyframes(format!(
                "duplicate property {:?}",
                track.property
            )));
        }
        for key in &track.keys {
            if !key.time_secs.is_finite() || key.time_secs < 0.0 || !key.value.is_finite() {
                return Err(CommandError::InvalidKeyframes(
                    "non-finite or negative keyframe".into(),
                ));
            }
            if track.property == AnimProperty::Opacity && !(0.0..=1.0).contains(&key.value) {
                return Err(CommandError::InvalidKeyframes(
                    "opacity keyframe must be in 0..=1".into(),
                ));
            }
            if track.property == AnimProperty::Speed && !(0.25..=4.0).contains(&key.value) {
                return Err(CommandError::InvalidKeyframes(
                    "speed keyframe must be in 0.25..=4".into(),
                ));
            }
        }
        track.keys.sort_by(|a, b| {
            a.time_secs
                .partial_cmp(&b.time_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.push(track);
    }
    Ok(out)
}

fn validate_effects(
    project: &Project,
    effects: Vec<EffectInstance>,
) -> Result<Vec<EffectInstance>, CommandError> {
    use crate::compose::effects::{clamp_effect_params, is_builtin_effect_id};

    let pack_effect_ids = crate::packs::known_effect_ids(project);
    let wasm_effect_ids = crate::plugins::known_effect_ids(project);

    let mut ids = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(effects.len());
    for mut effect in effects {
        if effect.effect_id.trim().is_empty() {
            return Err(CommandError::InvalidEffects("empty effect_id".into()));
        }
        let known = is_builtin_effect_id(&effect.effect_id)
            || pack_effect_ids.contains(&effect.effect_id)
            || wasm_effect_ids.contains(&effect.effect_id);
        if !known {
            return Err(CommandError::InvalidEffects(format!(
                "unknown effect_id '{}'",
                effect.effect_id
            )));
        }
        if !ids.insert(effect.id) {
            return Err(CommandError::InvalidEffects(format!(
                "duplicate effect id {}",
                effect.id
            )));
        }
        for (k, v) in &effect.params {
            if !v.is_finite() {
                return Err(CommandError::InvalidEffects(format!(
                    "non-finite param '{k}'"
                )));
            }
        }
        clamp_effect_params(&effect.effect_id, &mut effect.params);
        out.push(effect);
    }
    Ok(out)
}

fn set_clip_transform(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    transform: ClipTransform,
) -> Result<CommandOutcome, CommandError> {
    let transform = validate_transform(&transform)?;
    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.transform = transform;
    Ok(CommandOutcome::Applied)
}

fn set_clip_keyframes(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    keyframes: Vec<KeyframeTrack>,
) -> Result<CommandOutcome, CommandError> {
    let keyframes = validate_keyframes(keyframes)?;
    match any_clip_mut(project, track_id, clip_id)? {
        Clip::Video(m) | Clip::Audio(m) => m.keyframes = keyframes,
        Clip::Adjustment(a) => {
            if keyframes
                .iter()
                .any(|k| k.property != AnimProperty::Opacity)
            {
                return Err(CommandError::InvalidKeyframes(
                    "adjustment layers only support opacity keyframes".into(),
                ));
            }
            a.keyframes = keyframes;
        }
        Clip::Caption(_) => return Err(CommandError::NotMediaClip(clip_id)),
    }
    Ok(CommandOutcome::Applied)
}

fn set_clip_effects(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    effects: Vec<EffectInstance>,
) -> Result<CommandOutcome, CommandError> {
    let effects = validate_effects(project, effects)?;
    match any_clip_mut(project, track_id, clip_id)? {
        Clip::Video(m) | Clip::Audio(m) => m.effects = effects,
        Clip::Adjustment(a) => {
            // WASM frame plugins run on CPU pixels of a decoded source frame; an adjustment
            // layer's input exists only on the GPU.
            if let Some(e) = effects.iter().find(|e| e.effect_id.starts_with("wasm:")) {
                return Err(CommandError::InvalidEffects(format!(
                    "WASM plugin effect '{}' is not supported on adjustment layers",
                    e.effect_id
                )));
            }
            a.effects = effects;
        }
        Clip::Caption(_) => return Err(CommandError::NotMediaClip(clip_id)),
    }
    Ok(CommandOutcome::Applied)
}

fn set_clip_transition(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    transition: Option<ClipTransition>,
) -> Result<CommandOutcome, CommandError> {
    let track_kind = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?
        .kind;
    if track_kind != TrackKind::Video {
        return Err(CommandError::InvalidTransition(
            "transitions are only supported on video tracks".into(),
        ));
    }

    if let Some(ref t) = transition {
        if !TransitionKind::ALL.contains(&t.kind) {
            return Err(CommandError::InvalidTransition(
                "unsupported transition kind".into(),
            ));
        }
        if !t.duration_secs.is_finite() || t.duration_secs <= 0.0 {
            return Err(CommandError::InvalidTransition(
                "duration_secs must be > 0".into(),
            ));
        }

        let track = project
            .find_track(track_id)
            .ok_or(CommandError::TrackNotFound(track_id))?;
        let clip = track
            .clips
            .iter()
            .find(|c| c.id() == clip_id)
            .ok_or(CommandError::ClipNotFound(clip_id, track_id))?
            .as_media()
            .ok_or(CommandError::NotMediaClip(clip_id))?;
        let clip_dur = clip.timeline_duration_secs();
        if t.duration_secs > clip_dur / 2.0 + 1e-9 {
            return Err(CommandError::InvalidTransition(
                "duration must be <= half the clip duration".into(),
            ));
        }
        let end = clip.position_secs + clip_dur;
        let mut nexts: Vec<&MediaClip> = track
            .clips
            .iter()
            .filter_map(|c| c.as_media())
            .filter(|m| m.id != clip_id && m.enabled && m.position_secs >= end - 1e-6)
            .collect();
        nexts.sort_by(|a, b| {
            a.position_secs
                .partial_cmp(&b.position_secs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let Some(next) = nexts.first() else {
            return Err(CommandError::InvalidTransition(
                "no following clip on this track".into(),
            ));
        };
        let next_dur = next.timeline_duration_secs();
        if t.duration_secs > next_dur / 2.0 + 1e-9 {
            return Err(CommandError::InvalidTransition(
                "duration must be <= half the next clip duration".into(),
            ));
        }
    }

    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.outgoing_transition = transition;
    Ok(CommandOutcome::Applied)
}

fn set_clip_speed(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    speed: f64,
) -> Result<CommandOutcome, CommandError> {
    if !speed.is_finite() || speed <= 0.0 {
        return Err(CommandError::InvalidSpeed(
            "speed must be a finite number > 0".into(),
        ));
    }
    let speed = crate::project::clamp_clip_speed(speed);
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .clips
        .iter()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?
        .as_media()
        .ok_or(CommandError::NotMediaClip(clip_id))?;
    let new_dur = (clip.source_out_secs - clip.source_in_secs).max(0.0) / speed;
    check_no_overlap(track, clip.position_secs, new_dur, Some(clip_id))?;

    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.speed = speed;
    Ok(CommandOutcome::Applied)
}

fn load_asset_pack(project: &mut Project, path: &str) -> Result<CommandOutcome, CommandError> {
    let path_buf = std::path::PathBuf::from(path);
    let pack = crate::packs::load_pack(&path_buf).map_err(CommandError::AssetPack)?;
    if project.asset_pack_paths.iter().any(|p| {
        p == &path_buf || crate::packs::pack_id_at(p).as_deref() == Some(pack.manifest.id.as_str())
    }) {
        // Replace path if same id already loaded from elsewhere.
        project
            .asset_pack_paths
            .retain(|p| crate::packs::pack_id_at(p).as_deref() != Some(pack.manifest.id.as_str()));
    }
    project.asset_pack_paths.push(path_buf);
    Ok(CommandOutcome::Applied)
}

fn unload_asset_pack(project: &mut Project, pack_id: &str) -> Result<CommandOutcome, CommandError> {
    let before = project.asset_pack_paths.len();
    project
        .asset_pack_paths
        .retain(|p| crate::packs::pack_id_at(p).as_deref() != Some(pack_id));
    if project.asset_pack_paths.len() == before {
        return Err(CommandError::AssetPack(format!(
            "pack '{pack_id}' is not loaded"
        )));
    }
    Ok(CommandOutcome::Applied)
}

fn load_wasm_plugin(project: &mut Project, path: &str) -> Result<CommandOutcome, CommandError> {
    let path_buf = std::path::PathBuf::from(path);
    let plugin =
        crate::plugins::load_plugin_manifest(&path_buf).map_err(CommandError::WasmPlugin)?;
    project
        .wasm_plugin_paths
        .retain(|p| crate::plugins::plugin_id_at(p).as_deref() != Some(plugin.id.as_str()));
    // Validate the module loads once at install time.
    crate::plugins::PluginHost::load_from_dir(&path_buf).map_err(CommandError::WasmPlugin)?;
    project.wasm_plugin_paths.push(path_buf);
    Ok(CommandOutcome::Applied)
}

fn unload_wasm_plugin(
    project: &mut Project,
    plugin_id: &str,
) -> Result<CommandOutcome, CommandError> {
    let before = project.wasm_plugin_paths.len();
    project
        .wasm_plugin_paths
        .retain(|p| crate::plugins::plugin_id_at(p).as_deref() != Some(plugin_id));
    if project.wasm_plugin_paths.len() == before {
        return Err(CommandError::WasmPlugin(format!(
            "plugin '{plugin_id}' is not loaded"
        )));
    }
    Ok(CommandOutcome::Applied)
}

fn find_or_import_media(project: &mut Project, path: &std::path::Path) -> Result<Id, CommandError> {
    if let Some(existing) = project.media.iter().find(|m| m.path == path) {
        return Ok(existing.id);
    }
    let outcome = import_media(project, &path.to_string_lossy())?;
    match outcome {
        CommandOutcome::MediaImported { media_id } => Ok(media_id),
        _ => Err(CommandError::AssetPack("import failed".into())),
    }
}

fn add_sticker_from_pack(
    project: &mut Project,
    pack_id: &str,
    sticker_id: &str,
    track_id: Id,
    position_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    let packs = crate::packs::load_project_packs(project);
    let (pack, sticker) =
        crate::packs::find_sticker(&packs, pack_id, sticker_id).ok_or_else(|| {
            CommandError::AssetPack(format!("sticker '{pack_id}/{sticker_id}' not found"))
        })?;
    let asset = pack.root.join(&sticker.path);
    if !asset.is_file() {
        return Err(CommandError::AssetPack(format!(
            "sticker file missing: {}",
            asset.display()
        )));
    }
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Video {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Video,
        ));
    }
    let media_id = find_or_import_media(project, &asset)?;
    let dur = sticker.default_duration_secs.max(0.1);
    add_clip(project, track_id, media_id, position_secs, 0.0, dur)
}

fn add_sfx_from_pack(
    project: &mut Project,
    pack_id: &str,
    sfx_id: &str,
    track_id: Id,
    position_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    let packs = crate::packs::load_project_packs(project);
    let (pack, sfx) = crate::packs::find_sfx(&packs, pack_id, sfx_id)
        .ok_or_else(|| CommandError::AssetPack(format!("sfx '{pack_id}/{sfx_id}' not found")))?;
    let asset = pack.root.join(&sfx.path);
    if !asset.is_file() {
        return Err(CommandError::AssetPack(format!(
            "sfx file missing: {}",
            asset.display()
        )));
    }
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Audio {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Audio,
        ));
    }
    let media_id = find_or_import_media(project, &asset)?;
    let probed = media::probe(&asset)?;
    let dur = probed.duration_secs.unwrap_or(1.0).max(0.1);
    add_clip(project, track_id, media_id, position_secs, 0.0, dur)
}

fn validate_mask(mask: &ClipMask) -> Result<(), CommandError> {
    if !mask.feather.is_finite() || !(0.0..=1.0).contains(&mask.feather) {
        return Err(CommandError::InvalidMask(
            "feather must be finite and in 0..=1".into(),
        ));
    }
    match &mask.kind {
        ClipMaskKind::None
        | ClipMaskKind::Raster { .. }
        | ClipMaskKind::Generated { .. }
        | ClipMaskKind::Luma { .. } => Ok(()),
        ClipMaskKind::Rect {
            x,
            y,
            width,
            height,
        } => {
            if ![x, y, width, height].iter().all(|v| v.is_finite()) {
                return Err(CommandError::InvalidMask(
                    "rect fields must be finite".into(),
                ));
            }
            if *width <= 0.0 || *height <= 0.0 {
                return Err(CommandError::InvalidMask(
                    "rect width/height must be > 0".into(),
                ));
            }
            Ok(())
        }
        ClipMaskKind::Ellipse { cx, cy, rx, ry } => {
            if ![cx, cy, rx, ry].iter().all(|v| v.is_finite()) {
                return Err(CommandError::InvalidMask(
                    "ellipse fields must be finite".into(),
                ));
            }
            if *rx <= 0.0 || *ry <= 0.0 {
                return Err(CommandError::InvalidMask(
                    "ellipse radii must be > 0".into(),
                ));
            }
            Ok(())
        }
    }
}

fn set_clip_mask(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    mask: Option<ClipMask>,
) -> Result<CommandOutcome, CommandError> {
    if let Some(ref m) = mask {
        validate_mask(m)?;
    }
    match any_clip_mut(project, track_id, clip_id)? {
        Clip::Video(m) | Clip::Audio(m) => m.mask = mask,
        Clip::Adjustment(a) => a.mask = mask,
        Clip::Caption(_) => return Err(CommandError::NotMediaClip(clip_id)),
    }
    Ok(CommandOutcome::Applied)
}

fn set_clip_background_removal(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    config: Option<BackgroundRemoval>,
) -> Result<CommandOutcome, CommandError> {
    if let Some(ref cfg) = config {
        if !cfg.threshold.is_finite() || !(0.0..=1.0).contains(&cfg.threshold) {
            return Err(CommandError::InvalidMask(
                "background removal threshold must be in 0..=1".into(),
            ));
        }
        if !cfg.feather.is_finite() || !(0.0..=1.0).contains(&cfg.feather) {
            return Err(CommandError::InvalidMask(
                "background removal feather must be in 0..=1".into(),
            ));
        }
    }
    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.background_removal = config;
    Ok(CommandOutcome::Applied)
}

fn default_background_removal() -> BackgroundRemoval {
    BackgroundRemoval {
        enabled: true,
        model_id: "heuristic".into(),
        threshold: 0.15,
        feather: 0.0,
        temporal: false,
        matte_cache_dir: None,
    }
}

/// Commands that analyse decoded source frames (or apply to a clip's own audio/pixels)
/// have no meaning on a nested sequence in v7; refuse instead of guessing.
fn reject_sequence_media(
    project: &Project,
    media_id: Id,
    command: &'static str,
) -> Result<(), CommandError> {
    match project.find_media(media_id) {
        Some(m) if m.kind == crate::project::MediaKind::Sequence => {
            Err(CommandError::UnsupportedForSequence(command))
        }
        _ => Ok(()),
    }
}

fn decode_clip_frame(
    project: &Project,
    clip: &MediaClip,
    source_time: f64,
) -> Result<crate::media::RgbaFrame, CommandError> {
    let media = project
        .find_media(clip.media_id)
        .ok_or(CommandError::MediaNotFound(clip.media_id))?;
    match media.kind {
        crate::project::MediaKind::Image => {
            let img = image::open(&media.path)
                .map_err(|e| CommandError::Analysis(e.to_string()))?
                .to_rgba8();
            Ok(crate::media::RgbaFrame {
                width: img.width(),
                height: img.height(),
                pixels: img.into_raw(),
            })
        }
        crate::project::MediaKind::Video => {
            let mut reader = crate::media::VideoReader::open(&media.path, source_time.max(0.0))
                .map_err(|e| CommandError::Analysis(e.to_string()))?;
            reader
                .read_frame()
                .map_err(|e| CommandError::Analysis(e.to_string()))?
                .ok_or_else(|| CommandError::Analysis("no frame decoded".into()))
        }
        crate::project::MediaKind::Audio => Err(CommandError::Analysis(
            "clip media has no video frames".into(),
        )),
        crate::project::MediaKind::Sequence => Err(CommandError::UnsupportedForSequence(
            "GenerateBackgroundMatte",
        )),
    }
}

fn generate_background_matte(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Video {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Video,
        ));
    }
    let clip = track
        .clips
        .iter()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?
        .as_media()
        .ok_or(CommandError::NotMediaClip(clip_id))?
        .clone();

    reject_sequence_media(project, clip.media_id, "GenerateBackgroundMatte")?;
    let mid_source = (clip.source_in_secs + clip.source_out_secs) * 0.5;
    let frame = decode_clip_frame(project, &clip, mid_source)?;

    let mut cfg = clip
        .background_removal
        .clone()
        .unwrap_or_else(default_background_removal);
    cfg.enabled = true;

    let cache_dir = std::env::temp_dir().join(format!(
        "renderly-matte-{}",
        crate::segmentation::cache_key(clip.media_id, &cfg)
    ));
    crate::segmentation::bake_frame_matte(&frame, &cfg, &cache_dir)
        .map_err(|e| CommandError::Analysis(e.to_string()))?;

    let feather = cfg.feather;
    cfg.matte_cache_dir = Some(cache_dir.clone());

    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.background_removal = Some(cfg);
    clip.mask = Some(ClipMask {
        enabled: true,
        invert: false,
        feather,
        kind: ClipMaskKind::Generated { cache_dir },
    });
    Ok(CommandOutcome::Applied)
}

fn set_clip_audio_denoise(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    config: Option<AudioDenoise>,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Audio {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Audio,
        ));
    }
    if let Some(media_id) = track
        .find_clip(clip_id)
        .and_then(Clip::as_media)
        .map(|m| m.media_id)
    {
        reject_sequence_media(project, media_id, "SetClipAudioDenoise")?;
    }
    let config = match config {
        None => None,
        Some(mut cfg) => {
            if cfg.backend != "afftdn" && cfg.backend != "rnnoise" {
                return Err(CommandError::InvalidDenoise(format!(
                    "backend must be \"afftdn\" or \"rnnoise\", got \"{}\"",
                    cfg.backend
                )));
            }
            if !cfg.strength.is_finite() {
                return Err(CommandError::InvalidDenoise(
                    "strength must be finite".into(),
                ));
            }
            cfg.strength = cfg.strength.clamp(0.0, 1.0);
            Some(cfg)
        }
    };
    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.audio_denoise = config;
    Ok(CommandOutcome::Applied)
}

fn apply_template(
    project: &mut Project,
    pack_id: &str,
    template_id: &str,
    position_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    if !position_secs.is_finite() {
        return Err(CommandError::Template(
            "position_secs must be finite".into(),
        ));
    }
    let packs = crate::packs::load_project_packs(project);
    let pack = crate::packs::find_pack(&packs, pack_id).ok_or_else(|| {
        CommandError::Template(format!("pack '{pack_id}' not found or not loaded"))
    })?;
    let template = pack
        .manifest
        .templates
        .iter()
        .find(|t| t.id == template_id)
        .ok_or_else(|| {
            CommandError::Template(format!("template '{pack_id}/{template_id}' not found"))
        })?;
    let commands: Vec<serde_json::Value> = template.commands.clone();
    drop(packs);

    let backup = project.clone();
    for raw in commands {
        let mut cmd: Command = match serde_json::from_value(raw) {
            Ok(c) => c,
            Err(e) => {
                *project = backup;
                return Err(CommandError::Template(format!(
                    "invalid template command: {e}"
                )));
            }
        };
        if let Command::AddClip {
            position_secs: ref mut pos,
            ..
        } = cmd
        {
            *pos += position_secs;
        }
        if let Err(e) = apply_command(project, cmd) {
            *project = backup;
            return Err(e);
        }
    }
    Ok(CommandOutcome::Applied)
}

fn write_generated_sticker_png(path: &std::path::Path, prompt: &str) -> Result<(), CommandError> {
    use ab_glyph::{point, Font, PxScale, ScaleFont};
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(MediaError::Io)?;
        }
    }
    let mut hash: u32 = 2166136261;
    for b in prompt.as_bytes() {
        hash ^= u32::from(*b);
        hash = hash.wrapping_mul(16777619);
    }
    let r = ((hash >> 16) & 0xff) as u8;
    let g = ((hash >> 8) & 0xff) as u8;
    let b = (hash & 0xff) as u8;
    let w = 512u32;
    let h = 512u32;
    let mut pixels = vec![0u8; (w * h * 4) as usize];
    for px in pixels.as_chunks_mut::<4>().0 {
        px[0] = r.max(40);
        px[1] = g.max(40);
        px[2] = b.max(40);
        px[3] = 255;
    }

    // Draw a thick rounded border
    let radius = 32.0;
    for y in 0..h {
        for x in 0..w {
            let dx = if x < radius as u32 {
                radius - x as f32
            } else if x > w - radius as u32 {
                x as f32 - (w as f32 - radius)
            } else {
                0.0
            };
            let dy = if y < radius as u32 {
                radius - y as f32
            } else if y > h - radius as u32 {
                y as f32 - (h as f32 - radius)
            } else {
                0.0
            };
            let dist = (dx * dx + dy * dy).sqrt();
            if dist > radius {
                let i = ((y * w + x) * 4) as usize;
                pixels[i + 3] = 0;
            } else if dist > radius - 8.0 {
                let i = ((y * w + x) * 4) as usize;
                pixels[i] = 255;
                pixels[i + 1] = 255;
                pixels[i + 2] = 255;
            }
        }
    }

    // Render the text
    if let Ok(font) = crate::captions::load_font() {
        let scale = PxScale::from(64.0);
        let font_scaled = font.as_scaled(scale);
        let text = prompt.trim();
        let mut lines = Vec::new();
        let mut current_line = String::new();
        for word in text.split_whitespace() {
            let test_line = if current_line.is_empty() {
                word.to_string()
            } else {
                format!("{} {}", current_line, word)
            };
            let width: f32 = test_line
                .chars()
                .map(|c| font_scaled.h_advance(font.glyph_id(c)))
                .sum();
            if width > w as f32 - 100.0 && !current_line.is_empty() {
                lines.push(current_line);
                current_line = word.to_string();
            } else {
                current_line = test_line;
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line);
        }

        let line_height = 80.0;
        let total_h = lines.len() as f32 * line_height;
        let start_y = (h as f32 - total_h) / 2.0 + 50.0;

        for (li, line) in lines.iter().enumerate() {
            let glyph_width: f32 = line
                .chars()
                .map(|c| font_scaled.h_advance(font.glyph_id(c)))
                .sum();
            let start_x = (w as f32 - glyph_width) / 2.0;
            let baseline_y = start_y + li as f32 * line_height;

            let mut cursor_x = start_x;
            for ch in line.chars() {
                let glyph_id = font.glyph_id(ch);
                let glyph = glyph_id.with_scale_and_position(scale, point(cursor_x, baseline_y));
                if let Some(outlined) = font.outline_glyph(glyph) {
                    let bounds = outlined.px_bounds();
                    outlined.draw(|x, y, coverage| {
                        let px = bounds.min.x as i32 + x as i32;
                        let py = bounds.min.y as i32 + y as i32;
                        if px >= 0 && py >= 0 && px < w as i32 && py < h as i32 {
                            let i = ((py as u32 * w + px as u32) * 4) as usize;
                            let c = 255; // White text
                            let alpha = (coverage * 255.0) as u8;
                            // Simple blend
                            let old_a = pixels[i + 3] as f32 / 255.0;
                            let new_a = alpha as f32 / 255.0;
                            for j in 0..3 {
                                pixels[i + j] =
                                    (c as f32 * new_a + pixels[i + j] as f32 * (1.0 - new_a)) as u8;
                            }
                            pixels[i + 3] = ((new_a + old_a * (1.0 - new_a)) * 255.0) as u8;
                        }
                    });
                }
                cursor_x += font_scaled.h_advance(glyph_id);
            }
        }
    }

    image::save_buffer(path, &pixels, w, h, image::ColorType::Rgba8)
        .map_err(|e| CommandError::Analysis(e.to_string()))?;
    Ok(())
}

fn generate_sticker(
    project: &mut Project,
    prompt: &str,
    track_id: Id,
    position_secs: f64,
    output_path: &str,
) -> Result<CommandOutcome, CommandError> {
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Video {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Video,
        ));
    }
    let path = std::path::PathBuf::from(output_path);
    write_generated_sticker_png(&path, prompt)?;
    let media_id = find_or_import_media(project, &path)?;
    add_clip(project, track_id, media_id, position_secs, 0.0, 3.0)
}

fn find_media_clip_mut(project: &mut Project, clip_id: Id) -> Option<&mut MediaClip> {
    for track in project.all_tracks_mut() {
        for clip in &mut track.clips {
            if clip.id() == clip_id {
                return clip.as_media_mut();
            }
        }
    }
    None
}

fn create_multicam_group(
    project: &mut Project,
    name: String,
    clip_ids: Vec<Id>,
) -> Result<CommandOutcome, CommandError> {
    if clip_ids.is_empty() {
        return Err(CommandError::Multicam("clip_ids must not be empty".into()));
    }
    for &id in &clip_ids {
        if find_media_clip_mut(project, id).is_none() {
            return Err(CommandError::Multicam(format!("clip not found: {id}")));
        }
    }
    let group_id = Id::new_v4();
    for &id in &clip_ids {
        if let Some(clip) = find_media_clip_mut(project, id) {
            clip.multicam_group_id = Some(group_id);
        }
    }
    project.multicam_groups.push(MulticamGroup {
        id: group_id,
        name,
        angle_clip_ids: clip_ids,
        active_angle: 0,
        sync_offsets_secs: Vec::new(),
    });
    Ok(CommandOutcome::Applied)
}

fn set_multicam_angle(
    project: &mut Project,
    group_id: Id,
    active_angle: usize,
) -> Result<CommandOutcome, CommandError> {
    let group = project
        .multicam_groups
        .iter_mut()
        .find(|g| g.id == group_id)
        .ok_or_else(|| CommandError::Multicam(format!("group not found: {group_id}")))?;
    if active_angle >= group.angle_clip_ids.len() {
        return Err(CommandError::Multicam(format!(
            "active_angle {active_angle} out of range ({} angles)",
            group.angle_clip_ids.len()
        )));
    }
    group.active_angle = active_angle;
    Ok(CommandOutcome::Applied)
}

/// Centred reframe (no subject detection): the fallback used by `reframe::auto_reframe_clip`.
fn centred_reframe(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    target_aspect: f64,
) -> Result<CommandOutcome, CommandError> {
    if !target_aspect.is_finite() || target_aspect <= 0.0 {
        return Err(CommandError::Analysis(
            "target_aspect must be a finite number > 0".into(),
        ));
    }
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Video {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Video,
        ));
    }
    let clip = track
        .clips
        .iter()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?
        .as_media()
        .ok_or(CommandError::NotMediaClip(clip_id))?;
    reject_sequence_media(project, clip.media_id, "AutoReframeClip")?;
    let media = project
        .find_media(clip.media_id)
        .ok_or(CommandError::MediaNotFound(clip.media_id))?;
    let src_w = media.width.unwrap_or(project.settings.width).max(1) as f64;
    let src_h = media.height.unwrap_or(project.settings.height).max(1) as f64;
    let src_aspect = src_w / src_h;
    let scale = (src_aspect / target_aspect)
        .max(target_aspect / src_aspect)
        .max(1.0);
    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.transform.scale_x = scale;
    clip.transform.scale_y = scale;
    clip.transform.x = 0.0;
    clip.transform.y = 0.0;
    Ok(CommandOutcome::Applied)
}

fn generate_voiceover(
    project: &mut Project,
    text: &str,
    track_id: Id,
    position_secs: f64,
    output_path: &str,
    provider: VoiceoverProvider,
) -> Result<CommandOutcome, CommandError> {
    use crate::project::MediaItem;
    use std::path::PathBuf;

    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Audio {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Audio,
        ));
    }

    let path_buf = PathBuf::from(output_path);
    if let Some(parent) = path_buf.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(MediaError::Io)?;
        }
    }

    synthesize_to_wav(text, &provider, &path_buf)?;

    let probed = media::probe(&path_buf)?;
    let duration = probed.duration_secs.unwrap_or(0.1).max(0.1);
    let media_id = Id::new_v4();
    project.media.push(MediaItem {
        id: media_id,
        path: path_buf.clone(),
        kind: probed.kind.unwrap_or(crate::project::MediaKind::Audio),
        duration_secs: Some(duration),
        width: probed.width,
        height: probed.height,
        fps: probed.fps,
        sequence_id: None,
    });

    // The only way this can still fail here is `add_clip`'s overlap check (track/media
    // validity and the `[0, duration)` range are already guaranteed) — but real audio has
    // already been synthesized to `path_buf` by this point (its duration isn't knowable
    // before synthesis, so the overlap couldn't have been checked any earlier). Clean up
    // the file on failure rather than leaving an orphaned WAV with no project reference.
    let outcome = match add_clip(project, track_id, media_id, position_secs, 0.0, duration) {
        Ok(outcome) => outcome,
        Err(e) => {
            let _ = std::fs::remove_file(&path_buf);
            return Err(e);
        }
    };
    let clip_id = match outcome {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        _ => unreachable!(),
    };

    Ok(CommandOutcome::VoiceoverGenerated { media_id, clip_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Easing, Keyframe, Project, Settings};
    use std::io::Write;

    fn test_project() -> Project {
        Project::new("test", Settings::default())
    }

    fn write_temp_wav(dir: &std::path::Path, name: &str, duration_secs: f64) -> std::path::PathBuf {
        let sample_rate = 48000u32;
        let byte_rate = sample_rate * 2; // 16-bit mono
        let data_size = (byte_rate as f64 * duration_secs) as u32;
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&(36 + data_size).to_le_bytes()).unwrap();
        f.write_all(b"WAVE").unwrap();
        f.write_all(b"fmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
        f.write_all(&1u16.to_le_bytes()).unwrap(); // mono
        f.write_all(&sample_rate.to_le_bytes()).unwrap();
        f.write_all(&byte_rate.to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap(); // block align
        f.write_all(&16u16.to_le_bytes()).unwrap(); // bits per sample
        f.write_all(b"data").unwrap();
        f.write_all(&data_size.to_le_bytes()).unwrap();
        f.write_all(&vec![0u8; data_size as usize]).unwrap();
        path
    }

    #[test]
    fn import_media_wav_probes_duration() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 2.0);

        let mut project = test_project();
        let outcome = apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap();

        let CommandOutcome::MediaImported { media_id } = outcome else {
            panic!("expected MediaImported");
        };
        let media = project.find_media(media_id).unwrap();
        assert_eq!(media.kind, crate::project::MediaKind::Audio);
        assert!((media.duration_secs.unwrap() - 2.0).abs() < 0.01);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_track_honors_caller_supplied_id() {
        let mut project = test_project();
        let wanted_id = Id::new_v4();
        let outcome = apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: Some(wanted_id),
                sequence_id: None,
            },
        )
        .unwrap();
        let CommandOutcome::TrackAdded { track_id } = outcome else {
            panic!("expected TrackAdded");
        };
        assert_eq!(track_id, wanted_id);
        assert!(project.find_track(wanted_id).is_some());
    }

    #[test]
    fn add_clip_rejects_overlap() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 10.0);

        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 5.0,
            },
        )
        .unwrap();

        let err = apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 2.0,
                source_in_secs: 0.0,
                source_out_secs: 3.0,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::Overlap(_, _, _)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn split_clip_produces_two_contiguous_clips() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 10.0);

        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 6.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        let outcome = apply_command(
            &mut project,
            Command::SplitClip {
                track_id,
                clip_id,
                at_secs: 2.0,
            },
        )
        .unwrap();
        let CommandOutcome::ClipSplit { left_id, right_id } = outcome else {
            panic!()
        };

        let track = project.find_track(track_id).unwrap();
        let left = track.find_clip(left_id).unwrap();
        let right = track.find_clip(right_id).unwrap();
        assert!((left.duration_secs() - 2.0).abs() < 1e-9);
        assert!((right.duration_secs() - 4.0).abs() < 1e-9);
        assert!((left.end_secs() - right.position_secs()).abs() < 1e-9);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_clip_with_ripple_shifts_later_clips() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 10.0);

        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let first = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        let second = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 2.0,
                source_in_secs: 0.0,
                source_out_secs: 3.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::DeleteClip {
                track_id,
                clip_id: first,
                ripple: true,
            },
        )
        .unwrap();

        let track = project.find_track(track_id).unwrap();
        let remaining = track.find_clip(second).unwrap();
        assert!((remaining.position_secs() - 0.0).abs() < 1e-9);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_caption_requires_caption_track() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };

        let err = apply_command(
            &mut project,
            Command::AddCaption {
                track_id,
                text: "hi".into(),
                position_secs: 0.0,
                duration_secs: 1.0,
                style_id: "default".into(),
            },
        )
        .unwrap_err();

        assert!(matches!(err, CommandError::TrackKindMismatch(_, _, _)));
    }

    #[test]
    fn set_caption_updates_text_and_timing() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Caption,
                name: "C1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };

        let clip_id = match apply_command(
            &mut project,
            Command::AddCaption {
                track_id,
                text: "hello".into(),
                position_secs: 1.0,
                duration_secs: 2.0,
                style_id: "tiktok-bold-yellow".into(),
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::SetCaption {
                track_id,
                clip_id,
                text: Some("updated".into()),
                position_secs: Some(1.5),
                duration_secs: Some(2.5),
                style_id: None,
            },
        )
        .unwrap();

        let clip = project
            .find_track(track_id)
            .unwrap()
            .find_clip(clip_id)
            .unwrap();
        match clip {
            Clip::Caption(c) => {
                assert_eq!(c.text, "updated");
                assert!((c.position_secs - 1.5).abs() < 1e-9);
                assert!((c.duration_secs - 2.5).abs() < 1e-9);
                assert_eq!(c.style_id, "tiktok-bold-yellow");
            }
            _ => panic!("expected caption clip"),
        }

        let err = apply_command(
            &mut project,
            Command::SetCaption {
                track_id,
                clip_id,
                text: None,
                position_secs: None,
                duration_secs: None,
                style_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::SetCaptionRequiresChange));
    }

    #[test]
    fn set_caption_reports_clip_not_found_not_overlap() {
        // A bogus clip_id used to fall through to default position/duration (0.0/0.1)
        // before failing, which could spuriously report Overlap instead of ClipNotFound
        // if those defaults happened to collide with an existing caption.
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Caption,
                name: "C1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        apply_command(
            &mut project,
            Command::AddCaption {
                track_id,
                text: "hello".into(),
                position_secs: 0.0,
                duration_secs: 0.1,
                style_id: "tiktok-bold-yellow".into(),
            },
        )
        .unwrap();

        let err = apply_command(
            &mut project,
            Command::SetCaption {
                track_id,
                clip_id: Id::new_v4(),
                text: Some("nope".into()),
                position_secs: None,
                duration_secs: None,
                style_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::ClipNotFound(_, _)));
    }

    #[test]
    fn move_clip_kind_mismatch_reports_clip_kind_not_dest_kind_twice() {
        let mut project = test_project();
        let video_track = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let caption_track = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Caption,
                name: "C1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddCaption {
                track_id: caption_track,
                text: "hi".into(),
                position_secs: 0.0,
                duration_secs: 1.0,
                style_id: "tiktok-bold-yellow".into(),
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        let err = apply_command(
            &mut project,
            Command::MoveClip {
                track_id: caption_track,
                clip_id,
                new_position_secs: 0.0,
                new_track_id: Some(video_track),
            },
        )
        .unwrap_err();
        match err {
            CommandError::TrackKindMismatch(_, dest_kind, expected_kind) => {
                assert_eq!(dest_kind, TrackKind::Video);
                assert_eq!(expected_kind, TrackKind::Caption);
            }
            other => panic!("expected TrackKindMismatch, got {other:?}"),
        }
    }

    #[test]
    fn move_clip_repositions_within_same_track_and_across_tracks() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 10.0);

        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_a = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let track_b = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A2".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id: track_a,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        // Reposition within the same track.
        apply_command(
            &mut project,
            Command::MoveClip {
                track_id: track_a,
                clip_id,
                new_position_secs: 5.0,
                new_track_id: None,
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_a)
            .unwrap()
            .find_clip(clip_id)
            .unwrap();
        assert!((clip.position_secs() - 5.0).abs() < 1e-9);

        // Move to a different (kind-compatible) track.
        apply_command(
            &mut project,
            Command::MoveClip {
                track_id: track_a,
                clip_id,
                new_position_secs: 1.0,
                new_track_id: Some(track_b),
            },
        )
        .unwrap();
        assert!(project
            .find_track(track_a)
            .unwrap()
            .find_clip(clip_id)
            .is_none());
        let moved = project
            .find_track(track_b)
            .unwrap()
            .find_clip(clip_id)
            .unwrap();
        assert!((moved.position_secs() - 1.0).abs() < 1e-9);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generate_captions_requires_caption_track() {
        let mut project = test_project();
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: "x.wav".into(),
            kind: crate::project::MediaKind::Audio,
            duration_secs: Some(1.0),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };

        let err = apply_command(
            &mut project,
            Command::GenerateCaptions {
                media_id,
                track_id,
                style_id: "tiktok-bold-yellow".into(),
                timeline_offset_secs: 0.0,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::TrackKindMismatch(_, _, _)));
    }

    #[test]
    fn set_audio_gain_on_audio_clip_and_rejects_non_audio() {
        let mut project = test_project();
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: "voice.wav".into(),
            kind: crate::project::MediaKind::Audio,
            duration_secs: Some(5.0),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 5.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::SetAudioGain {
                track_id,
                clip_id,
                gain_db: -6.0,
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .find_clip(clip_id)
            .unwrap();
        match clip {
            Clip::Audio(c) => assert!((c.gain_db - -6.0).abs() < 1e-9),
            _ => panic!("expected audio clip"),
        }

        // A caption clip has no audio.
        let caption_track = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Caption,
                name: "C1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let caption_clip_id = match apply_command(
            &mut project,
            Command::AddCaption {
                track_id: caption_track,
                text: "hi".into(),
                position_secs: 0.0,
                duration_secs: 1.0,
                style_id: "tiktok-bold-yellow".into(),
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        let err = apply_command(
            &mut project,
            Command::SetAudioGain {
                track_id: caption_track,
                clip_id: caption_clip_id,
                gain_db: -6.0,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::NoAudio(_)));
    }

    #[test]
    fn set_audio_fade_on_audio_clip() {
        let mut project = test_project();
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: "voice.wav".into(),
            kind: crate::project::MediaKind::Audio,
            duration_secs: Some(5.0),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 5.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::SetAudioFade {
                track_id,
                clip_id,
                fade_in_secs: 0.5,
                fade_out_secs: 1.0,
            },
        )
        .unwrap();

        let clip = project
            .find_track(track_id)
            .unwrap()
            .find_clip(clip_id)
            .unwrap();
        match clip {
            Clip::Audio(c) => {
                assert!((c.fade_in_secs - 0.5).abs() < 1e-9);
                assert!((c.fade_out_secs - 1.0).abs() < 1e-9);
            }
            _ => panic!("expected audio clip"),
        }
    }

    #[test]
    fn set_track_audio_role_requires_audio_track() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };

        let err = apply_command(
            &mut project,
            Command::SetTrackAudioRole {
                track_id,
                role: Some(crate::project::TrackAudioRole::Music),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::TrackKindMismatch(_, _, _)));
    }

    #[test]
    fn set_project_settings_updates_dimensions_and_fps() {
        let mut project = test_project();
        apply_command(
            &mut project,
            Command::SetProjectSettings {
                width: Some(1920),
                height: Some(1080),
                fps: Some(30.0),
            },
        )
        .unwrap();
        assert_eq!(project.settings.width, 1920);
        assert_eq!(project.settings.height, 1080);
        assert!((project.settings.fps - 30.0).abs() < 1e-9);

        let err = apply_command(
            &mut project,
            Command::SetProjectSettings {
                width: None,
                height: None,
                fps: None,
            },
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CommandError::SetProjectSettingsRequiresChange
        ));

        let err = apply_command(
            &mut project,
            Command::SetProjectSettings {
                width: Some(0),
                height: None,
                fps: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidDimensions));
    }

    #[test]
    fn set_track_flags_updates_only_given_fields() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::SetTrackFlags {
                track_id,
                muted: None,
                locked: None,
                hidden: Some(true),
            },
        )
        .unwrap();
        let track = project.find_track(track_id).unwrap();
        assert!(track.hidden);
        assert!(!track.muted);
        assert!(!track.locked);

        let err = apply_command(
            &mut project,
            Command::SetTrackFlags {
                track_id,
                muted: None,
                locked: None,
                hidden: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::SetTrackFlagsRequiresChange));
    }

    #[test]
    fn rename_track_changes_name() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        apply_command(
            &mut project,
            Command::RenameTrack {
                track_id,
                name: "Gameplay A".into(),
            },
        )
        .unwrap();
        assert_eq!(project.find_track(track_id).unwrap().name, "Gameplay A");
    }

    #[test]
    fn delete_track_removes_it() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        apply_command(&mut project, Command::DeleteTrack { track_id }).unwrap();
        assert!(project.find_track(track_id).is_none());

        let err = apply_command(&mut project, Command::DeleteTrack { track_id }).unwrap_err();
        assert!(matches!(err, CommandError::TrackNotFound(_)));
    }

    fn add_video_track(project: &mut Project, name: &str) -> Id {
        match apply_command(
            project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: name.into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        }
    }

    #[test]
    fn move_track_moves_up() {
        let mut project = test_project();
        let a = add_video_track(&mut project, "A");
        let b = add_video_track(&mut project, "B");
        let c = add_video_track(&mut project, "C");
        // [A, B, C] -> move A to index 2 -> [B, C, A]
        apply_command(
            &mut project,
            Command::MoveTrack {
                track_id: a,
                new_index: 2,
            },
        )
        .unwrap();
        let ids: Vec<Id> = project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![b, c, a]);
    }

    #[test]
    fn move_track_moves_down() {
        let mut project = test_project();
        let a = add_video_track(&mut project, "A");
        let b = add_video_track(&mut project, "B");
        let c = add_video_track(&mut project, "C");
        // [A, B, C] -> move C to index 0 -> [C, A, B]
        apply_command(
            &mut project,
            Command::MoveTrack {
                track_id: c,
                new_index: 0,
            },
        )
        .unwrap();
        let ids: Vec<Id> = project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![c, a, b]);
    }

    #[test]
    fn move_track_clamps_out_of_range_index() {
        let mut project = test_project();
        let a = add_video_track(&mut project, "A");
        let b = add_video_track(&mut project, "B");
        apply_command(
            &mut project,
            Command::MoveTrack {
                track_id: a,
                new_index: 999,
            },
        )
        .unwrap();
        let ids: Vec<Id> = project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![b, a]);
    }

    #[test]
    fn move_track_unknown_id_errors() {
        let mut project = test_project();
        add_video_track(&mut project, "A");
        let err = apply_command(
            &mut project,
            Command::MoveTrack {
                track_id: Id::new_v4(),
                new_index: 0,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::TrackNotFound(_)));
    }

    #[test]
    fn move_track_same_position_is_noop() {
        let mut project = test_project();
        let a = add_video_track(&mut project, "A");
        let b = add_video_track(&mut project, "B");
        apply_command(
            &mut project,
            Command::MoveTrack {
                track_id: a,
                new_index: 0,
            },
        )
        .unwrap();
        let ids: Vec<Id> = project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn set_clip_enabled_toggles_media_clip_and_rejects_captions() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 5.0);

        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 5.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::SetClipEnabled {
                track_id,
                clip_id,
                enabled: false,
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .find_clip(clip_id)
            .unwrap();
        match clip {
            Clip::Audio(m) => assert!(!m.enabled),
            _ => panic!("expected audio clip"),
        }

        let caption_track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Caption,
                name: "C1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let caption_clip_id = match apply_command(
            &mut project,
            Command::AddCaption {
                track_id: caption_track_id,
                text: "hi".into(),
                position_secs: 0.0,
                duration_secs: 1.0,
                style_id: "tiktok-bold-yellow".into(),
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        let err = apply_command(
            &mut project,
            Command::SetClipEnabled {
                track_id: caption_track_id,
                clip_id: caption_clip_id,
                enabled: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::NotMediaClip(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trim_clip_rejects_overlap_with_neighbor() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 10.0);

        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let first = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 2.0,
                source_in_secs: 0.0,
                source_out_secs: 3.0,
            },
        )
        .unwrap();

        // Extending clip 1's source_out lengthens its on-timeline span past 2.0s, colliding
        // with clip 2 which starts exactly there.
        let err = apply_command(
            &mut project,
            Command::TrimClip {
                track_id,
                clip_id: first,
                new_source_in_secs: None,
                new_source_out_secs: Some(4.0),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::Overlap(_, _, _)));

        // Original clip must be untouched after the rejected trim.
        let clip = project
            .find_track(track_id)
            .unwrap()
            .find_clip(first)
            .unwrap();
        assert!((clip.duration_secs() - 2.0).abs() < 1e-9);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trim_clip_rejects_exceeding_media_duration() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 5.0);

        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 3.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        let err = apply_command(
            &mut project,
            Command::TrimClip {
                track_id,
                clip_id,
                new_source_in_secs: None,
                new_source_out_secs: Some(9.0),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::ExceedsMediaDuration(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn trim_clip_rejects_caption_clip() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Caption,
                name: "C1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddCaption {
                track_id,
                text: "hi".into(),
                position_secs: 0.0,
                duration_secs: 1.0,
                style_id: "tiktok-bold-yellow".into(),
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        let err = apply_command(
            &mut project,
            Command::TrimClip {
                track_id,
                clip_id,
                new_source_in_secs: Some(0.5),
                new_source_out_secs: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::NotMediaClip(_)));
    }

    #[test]
    fn add_track_rejects_duplicate_id() {
        let mut project = test_project();
        let wanted_id = Id::new_v4();
        apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: Some(wanted_id),
                sequence_id: None,
            },
        )
        .unwrap();

        let err = apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: Some(wanted_id),
                sequence_id: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::DuplicateTrackId(id) if id == wanted_id));
    }

    #[test]
    fn add_caption_rejects_zero_duration() {
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Caption,
                name: "C1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };

        let err = apply_command(
            &mut project,
            Command::AddCaption {
                track_id,
                text: "hi".into(),
                position_secs: 0.0,
                duration_secs: 0.0,
                style_id: "tiktok-bold-yellow".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidDuration(_)));
    }

    #[test]
    fn set_project_settings_rejects_invalid_fps() {
        let mut project = test_project();
        let err = apply_command(
            &mut project,
            Command::SetProjectSettings {
                width: None,
                height: None,
                fps: Some(0.0),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidFps(_)));
    }

    #[test]
    fn export_requires_ffmpeg_or_empty_timeline() {
        let mut project = test_project();
        let err = apply_command(
            &mut project,
            Command::Export {
                output_path: "out.mp4".into(),
                preset: ExportPreset::TikTok9x16,
            },
        )
        .unwrap_err();
        assert!(
            matches!(err, CommandError::Export(_))
                || matches!(err, CommandError::NotImplemented(_)),
            "unexpected: {err:?}"
        );
    }

    fn setup_audio_clip() -> (Project, Id, Id, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 10.0);
        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 6.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        (project, track_id, clip_id, dir)
    }

    #[test]
    fn set_clip_transform_updates_media_clip() {
        let (mut project, track_id, clip_id, dir) = setup_audio_clip();
        let transform = ClipTransform {
            x: 0.25,
            y: -0.1,
            scale_x: 1.2,
            scale_y: 0.8,
            rotation_deg: 15.0,
            opacity: 0.5,
        };
        apply_command(
            &mut project,
            Command::SetClipTransform {
                track_id,
                clip_id,
                transform,
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_id)
            .unwrap()
            .as_media()
            .unwrap();
        assert_eq!(clip.transform, transform);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_clip_keyframes_sorts_and_rejects_duplicates() {
        let (mut project, track_id, clip_id, dir) = setup_audio_clip();
        apply_command(
            &mut project,
            Command::SetClipKeyframes {
                track_id,
                clip_id,
                keyframes: vec![KeyframeTrack {
                    property: AnimProperty::Opacity,
                    keys: vec![
                        Keyframe {
                            time_secs: 2.0,
                            value: 1.0,
                            easing: Easing::Linear,
                            handle_left: None,
                            handle_right: None,
                        },
                        Keyframe {
                            time_secs: 0.0,
                            value: 0.0,
                            easing: Easing::Linear,
                            handle_left: None,
                            handle_right: None,
                        },
                    ],
                }],
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_id)
            .unwrap()
            .as_media()
            .unwrap();
        assert!((clip.keyframes[0].keys[0].time_secs - 0.0).abs() < 1e-9);
        assert!((clip.keyframes[0].keys[1].time_secs - 2.0).abs() < 1e-9);

        let err = apply_command(
            &mut project,
            Command::SetClipKeyframes {
                track_id,
                clip_id,
                keyframes: vec![
                    KeyframeTrack {
                        property: AnimProperty::Opacity,
                        keys: vec![],
                    },
                    KeyframeTrack {
                        property: AnimProperty::Opacity,
                        keys: vec![],
                    },
                ],
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidKeyframes(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_clip_effects_stores_and_rejects_duplicate_ids() {
        let (mut project, track_id, clip_id, dir) = setup_audio_clip();
        let effect_id = Id::new_v4();
        apply_command(
            &mut project,
            Command::SetClipEffects {
                track_id,
                clip_id,
                effects: vec![EffectInstance {
                    id: effect_id,
                    effect_id: "builtin:blur".into(),
                    enabled: true,
                    params: [("radius".into(), 4.0)].into_iter().collect(),
                }],
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_id)
            .unwrap()
            .as_media()
            .unwrap();
        assert_eq!(clip.effects.len(), 1);
        assert_eq!(clip.effects[0].effect_id, "builtin:blur");

        let err = apply_command(
            &mut project,
            Command::SetClipEffects {
                track_id,
                clip_id,
                effects: vec![
                    EffectInstance {
                        id: effect_id,
                        effect_id: "builtin:blur".into(),
                        enabled: true,
                        params: Default::default(),
                    },
                    EffectInstance {
                        id: effect_id,
                        effect_id: "builtin:lut_warm".into(),
                        enabled: true,
                        params: Default::default(),
                    },
                ],
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidEffects(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_clip_effects_rejects_unknown_effect_id() {
        let (mut project, track_id, clip_id, dir) = setup_audio_clip();
        let err = apply_command(
            &mut project,
            Command::SetClipEffects {
                track_id,
                clip_id,
                effects: vec![EffectInstance {
                    id: Id::new_v4(),
                    effect_id: "builtin:not_a_real_effect".into(),
                    enabled: true,
                    params: Default::default(),
                }],
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidEffects(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn split_clip_remaps_keyframes_and_remints_effect_ids() {
        let (mut project, track_id, clip_id, dir) = setup_audio_clip();
        let effect_id = Id::new_v4();
        apply_command(
            &mut project,
            Command::SetClipTransform {
                track_id,
                clip_id,
                transform: ClipTransform {
                    opacity: 0.75,
                    ..ClipTransform::default()
                },
            },
        )
        .unwrap();
        apply_command(
            &mut project,
            Command::SetClipKeyframes {
                track_id,
                clip_id,
                keyframes: vec![KeyframeTrack {
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
                }],
            },
        )
        .unwrap();
        apply_command(
            &mut project,
            Command::SetClipEffects {
                track_id,
                clip_id,
                effects: vec![EffectInstance {
                    id: effect_id,
                    effect_id: "builtin:blur".into(),
                    enabled: true,
                    params: Default::default(),
                }],
            },
        )
        .unwrap();

        let outcome = apply_command(
            &mut project,
            Command::SplitClip {
                track_id,
                clip_id,
                at_secs: 2.5,
            },
        )
        .unwrap();
        let CommandOutcome::ClipSplit { left_id, right_id } = outcome else {
            panic!("expected ClipSplit");
        };

        let track = project.find_track(track_id).unwrap();
        let left = track
            .clips
            .iter()
            .find(|c| c.id() == left_id)
            .unwrap()
            .as_media()
            .unwrap();
        let right = track
            .clips
            .iter()
            .find(|c| c.id() == right_id)
            .unwrap()
            .as_media()
            .unwrap();

        assert!((left.transform.opacity - 0.75).abs() < 1e-9);
        assert!((right.transform.opacity - 0.75).abs() < 1e-9);
        assert_eq!(left.keyframes[0].keys.len(), 2);
        assert_eq!(right.keyframes[0].keys.len(), 1);
        assert!((right.keyframes[0].keys[0].time_secs - 1.5).abs() < 1e-9);
        assert_eq!(left.effects[0].id, effect_id);
        assert_ne!(right.effects[0].id, effect_id);
        assert_eq!(right.effects[0].effect_id, "builtin:blur");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_clip_transition_requires_following_video_clip() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // Use wav on video track? Need video media — use two audio on video track won't work.
        // Build minimal project with video clips via direct struct insert.
        let mut project = test_project();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let media_a = Id::new_v4();
        let media_b = Id::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_a,
            path: dir.join("a.mp4"),
            kind: crate::project::MediaKind::Video,
            duration_secs: Some(10.0),
            width: Some(1280),
            height: Some(720),
            fps: Some(30.0),
            sequence_id: None,
        });
        project.media.push(crate::project::MediaItem {
            id: media_b,
            path: dir.join("b.mp4"),
            kind: crate::project::MediaKind::Video,
            duration_secs: Some(10.0),
            width: Some(1280),
            height: Some(720),
            fps: Some(30.0),
            sequence_id: None,
        });
        let clip_a = Id::new_v4();
        let clip_b = Id::new_v4();
        let track = project.find_track_mut(track_id).unwrap();
        track.clips.push(Clip::Video(MediaClip {
            id: clip_a,
            media_id: media_a,
            position_secs: 0.0,
            source_in_secs: 0.0,
            source_out_secs: 4.0,
            ..MediaClip::default()
        }));
        track.clips.push(Clip::Video(MediaClip {
            id: clip_b,
            media_id: media_b,
            position_secs: 4.0,
            source_in_secs: 0.0,
            source_out_secs: 4.0,
            ..MediaClip::default()
        }));

        apply_command(
            &mut project,
            Command::SetClipTransition {
                track_id,
                clip_id: clip_a,
                transition: Some(ClipTransition {
                    kind: TransitionKind::Crossfade,
                    duration_secs: 0.5,
                }),
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_a)
            .unwrap()
            .as_media()
            .unwrap();
        assert_eq!(
            clip.outgoing_transition.as_ref().unwrap().duration_secs,
            0.5
        );

        apply_command(
            &mut project,
            Command::SetClipTransition {
                track_id,
                clip_id: clip_a,
                transition: None,
            },
        )
        .unwrap();
        assert!(project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_a)
            .unwrap()
            .as_media()
            .unwrap()
            .outgoing_transition
            .is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn add_sticker_from_pack_imports_and_places_clip() {
        let pack_root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/packs/starter");
        assert!(
            pack_root.join("pack.json").is_file(),
            "starter pack missing at {}",
            pack_root.display()
        );

        let mut project = Project::new("sticker-test", Settings::default());
        apply_command(
            &mut project,
            Command::LoadAssetPack {
                path: pack_root.display().to_string(),
            },
        )
        .unwrap();
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            other => panic!("unexpected {other:?}"),
        };
        let outcome = apply_command(
            &mut project,
            Command::AddStickerFromPack {
                pack_id: "starter".into(),
                sticker_id: "star".into(),
                track_id,
                position_secs: 1.0,
            },
        )
        .unwrap();
        assert!(matches!(outcome, CommandOutcome::ClipAdded { .. }));
        assert!(!project.media.is_empty());
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find_map(|c| c.as_media())
            .unwrap();
        assert!((clip.position_secs - 1.0).abs() < 1e-9);
        assert!((clip.source_out_secs - 3.0).abs() < 1e-9);
    }

    #[test]
    fn set_clip_mask_sets_and_clears() {
        let (mut project, track_id, clip_id, dir) = setup_audio_clip();
        let mask = ClipMask {
            enabled: true,
            invert: false,
            feather: 0.2,
            kind: ClipMaskKind::Rect {
                x: 0.1,
                y: 0.1,
                width: 0.5,
                height: 0.5,
            },
        };
        apply_command(
            &mut project,
            Command::SetClipMask {
                track_id,
                clip_id,
                mask: Some(mask.clone()),
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_id)
            .unwrap()
            .as_media()
            .unwrap();
        assert_eq!(clip.mask.as_ref(), Some(&mask));

        apply_command(
            &mut project,
            Command::SetClipMask {
                track_id,
                clip_id,
                mask: None,
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_id)
            .unwrap()
            .as_media()
            .unwrap();
        assert!(clip.mask.is_none());

        let err = apply_command(
            &mut project,
            Command::SetClipMask {
                track_id,
                clip_id,
                mask: Some(ClipMask {
                    enabled: true,
                    invert: false,
                    feather: 2.0,
                    kind: ClipMaskKind::None,
                }),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidMask(_)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_clip_audio_denoise_clamps_and_validates() {
        let (mut project, track_id, clip_id, dir) = setup_audio_clip();
        apply_command(
            &mut project,
            Command::SetClipAudioDenoise {
                track_id,
                clip_id,
                config: Some(AudioDenoise {
                    enabled: true,
                    backend: "afftdn".into(),
                    strength: 1.5,
                }),
            },
        )
        .unwrap();
        let clip = project
            .find_track(track_id)
            .unwrap()
            .clips
            .iter()
            .find(|c| c.id() == clip_id)
            .unwrap()
            .as_media()
            .unwrap();
        assert!((clip.audio_denoise.as_ref().unwrap().strength - 1.0).abs() < 1e-9);

        let err = apply_command(
            &mut project,
            Command::SetClipAudioDenoise {
                track_id,
                clip_id,
                config: Some(AudioDenoise {
                    enabled: true,
                    backend: "deepfilter".into(),
                    strength: 0.5,
                }),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidDenoise(_)));

        // `rnnoise` is an accepted backend.
        apply_command(
            &mut project,
            Command::SetClipAudioDenoise {
                track_id,
                clip_id,
                config: Some(AudioDenoise {
                    enabled: true,
                    backend: "rnnoise".into(),
                    strength: 0.7,
                }),
            },
        )
        .unwrap();

        // Reject on video tracks.
        let v_track = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let err = apply_command(
            &mut project,
            Command::SetClipAudioDenoise {
                track_id: v_track,
                clip_id,
                config: None,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::TrackKindMismatch(_, _, _)));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_multicam_group_and_set_angle() {
        let dir = std::env::temp_dir().join(format!("renderly-test-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = write_temp_wav(&dir, "clip.wav", 10.0);
        let mut project = test_project();
        let media_id = match apply_command(
            &mut project,
            Command::ImportMedia {
                path: wav_path.to_string_lossy().to_string(),
            },
        )
        .unwrap()
        {
            CommandOutcome::MediaImported { media_id } => media_id,
            _ => unreachable!(),
        };
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Audio,
                name: "A1".into(),
                id: None,
                sequence_id: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_a = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        let clip_b = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 3.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };

        apply_command(
            &mut project,
            Command::CreateMulticamGroup {
                name: "A/B".into(),
                clip_ids: vec![clip_a, clip_b],
            },
        )
        .unwrap();
        assert_eq!(project.multicam_groups.len(), 1);
        let group_id = project.multicam_groups[0].id;
        assert_eq!(project.multicam_groups[0].active_angle, 0);
        for id in [clip_a, clip_b] {
            let clip = project
                .find_track(track_id)
                .unwrap()
                .clips
                .iter()
                .find(|c| c.id() == id)
                .unwrap()
                .as_media()
                .unwrap();
            assert_eq!(clip.multicam_group_id, Some(group_id));
        }

        apply_command(
            &mut project,
            Command::SetMulticamAngle {
                group_id,
                active_angle: 1,
            },
        )
        .unwrap();
        assert_eq!(project.multicam_groups[0].active_angle, 1);

        let err = apply_command(
            &mut project,
            Command::SetMulticamAngle {
                group_id,
                active_angle: 99,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::Multicam(_)));

        std::fs::remove_dir_all(&dir).ok();
    }
}
