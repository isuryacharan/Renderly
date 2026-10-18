# Command API

Status: **current** (matches project schema v7). This is the source of truth for the `Command` enum and
`apply_command` in `renderly-core`. GUI, CLI, and MCP must all dispatch through this exact
set (see AGENTS.md §0.1) — none of them may mutate `Project` state any other way.

## Shape

```rust
pub enum Command {
    ImportMedia { path: String },
    AddTrack { kind: TrackKind, name: String, id: Option<Id>, sequence_id: Option<Id> },
    AddClip { track_id: Id, media_id: Id, position_secs: f64, source_in_secs: f64, source_out_secs: f64 },
    SplitClip { track_id: Id, clip_id: Id, at_secs: f64 },
    TrimClip { track_id: Id, clip_id: Id, new_source_in_secs: Option<f64>, new_source_out_secs: Option<f64> },
    MoveClip { track_id: Id, clip_id: Id, new_position_secs: f64, new_track_id: Option<Id> },
    DeleteClip { track_id: Id, clip_id: Id, ripple: bool },
    AddCaption { track_id: Id, text: String, position_secs: f64, duration_secs: f64, style_id: String },
    SetCaption { track_id: Id, clip_id: Id, text: Option<String>, position_secs: Option<f64>, duration_secs: Option<f64>, style_id: Option<String> },
    SetAudioGain { track_id: Id, clip_id: Id, gain_db: f64 },
    GenerateCaptions { media_id: Id, track_id: Id, style_id: String, timeline_offset_secs: f64 },
    GenerateVoiceover { text: String, track_id: Id, position_secs: f64, output_path: String, provider: VoiceoverProvider },
    SetAudioFade { track_id: Id, clip_id: Id, fade_in_secs: f64, fade_out_secs: f64 },
    SetTrackAudioRole { track_id: Id, role: Option<TrackAudioRole> },
    SetProjectSettings { width: Option<u32>, height: Option<u32>, fps: Option<f64> },
    SetTrackFlags { track_id: Id, muted: Option<bool>, locked: Option<bool>, hidden: Option<bool> },
    RenameTrack { track_id: Id, name: String },
    DeleteTrack { track_id: Id },
    MoveTrack { track_id: Id, new_index: usize },
    SetClipEnabled { track_id: Id, clip_id: Id, enabled: bool },
    SetClipTransform { track_id: Id, clip_id: Id, transform: ClipTransform },
    SetClipKeyframes { track_id: Id, clip_id: Id, keyframes: Vec<KeyframeTrack> },
    SetClipEffects { track_id: Id, clip_id: Id, effects: Vec<EffectInstance> },
    SetClipTransition { track_id: Id, clip_id: Id, transition: Option<ClipTransition> },
    SetClipSpeed { track_id: Id, clip_id: Id, speed: f64 },
    LoadAssetPack { path: String },
    UnloadAssetPack { pack_id: String },
    LoadWasmPlugin { path: String },
    UnloadWasmPlugin { plugin_id: String },
    AddStickerFromPack { pack_id: String, sticker_id: String, track_id: Id, position_secs: f64 },
    AddSfxFromPack { pack_id: String, sfx_id: String, track_id: Id, position_secs: f64 },
    SetClipMask { track_id: Id, clip_id: Id, mask: Option<ClipMask> },
    SetClipBackgroundRemoval { track_id: Id, clip_id: Id, config: Option<BackgroundRemoval> },
    GenerateBackgroundMatte { track_id: Id, clip_id: Id },
    AttachBackgroundMatte { track_id: Id, clip_id: Id, cache_dir: PathBuf },
    SetClipAudioDenoise { track_id: Id, clip_id: Id, config: Option<AudioDenoise> },
    ApplyTemplate { pack_id: String, template_id: String, position_secs: f64 },
    GenerateSticker { prompt: String, track_id: Id, position_secs: f64, output_path: String },
    CreateMulticamGroup { name: String, clip_ids: Vec<Id> },
    SetMulticamAngle { group_id: Id, active_angle: usize },
    TrackMotion { track_id: Id, clip_id: Id, sample_count: u32, region: Option<TrackRegion>, track_rotation_scale: bool, target: Option<ClipRef> },
    StabilizeClip { track_id: Id, clip_id: Id, strength: Option<f64>, sample_count: Option<u32> },
    AutoReframeClip { track_id: Id, clip_id: Id, target_aspect: f64 },
    CreateSequence { name: String, width: Option<u32>, height: Option<u32>, id: Option<Id> },
    RenameSequence { sequence_id: Id, name: String },
    SetSequenceSettings { sequence_id: Id, width: Option<u32>, height: Option<u32> },
    DeleteSequence { sequence_id: Id },
    NestClips { clips: Vec<ClipRef>, name: String },
    UnnestClip { track_id: Id, clip_id: Id },
    AddAdjustmentLayer { track_id: Id, position_secs: f64, duration_secs: f64 },
    SetAdjustmentLayer { track_id: Id, clip_id: Id, position_secs: Option<f64>, duration_secs: Option<f64>, opacity: Option<f64> },
    Export { output_path: String, preset: ExportPreset },
}

pub struct ClipRef { pub track_id: Id, pub clip_id: Id }

pub fn apply_command(project: &mut Project, cmd: Command) -> Result<CommandOutcome, CommandError>;
```

The GUI's Tauri layer also exposes `apply_commands(Vec<Command>)`, applying a batch
atomically (all-or-nothing against one project clone, one undo snapshot) — see "Batch
application" below. This is a GUI/Tauri-layer convenience over the same `apply_command`
per-command logic, not a second core API; CLI/MCP callers just call `apply_command`
per command as before.

**App IPC shape (not core).** In `renderly-app`, the Tauri (and bridge) `apply_command` /
`apply_commands` / `undo` / `redo` handlers return `{ revision, patch, … }` where `patch`
is an RFC-6902 JSON Patch (`json-patch` crate `diff` of pre/post `Project` JSON). This is
session-layer transport so the webview can apply deltas without a full `get_project` each
edit — it does **not** change the core `Command` enum or `apply_command` signature above.
See [architecture.md](architecture.md) "Tauri command surface".

`CommandOutcome` carries whatever the caller needs back (e.g. `ImportMedia` returns the new
`media_id`; edit commands return `Ok(CommandOutcome::Applied)`; `GenerateVoiceover` returns
`media_id` and `clip_id`; `CreateSequence` returns `SequenceCreated { sequence_id,
media_id }`; `NestClips` returns `ClipsNested { sequence_id, media_id, video_clip_id?,
audio_clip_id? }`; `Export` drives the render pipeline).

**Timelines and addressing (schema v7).** A project has one root timeline (`Project::tracks`)
and any number of nested sequences, each with its own tracks. Track ids are unique across
all of them, so every command that takes a `track_id` addresses a track inside a sequence
the same way as one on the root — no extra parameter. `MoveTrack`/`DeleteTrack` act within
the track's own timeline. See [project-schema.md](project-schema.md) "Sequences".

Every command except `Export` is a pure mutation of `Project` and must be representable in
the project's command log for undo/redo (Phase 2+ feature; v0 just needs the enum to be
`Serialize`/`Deserialize`-clean so logging is possible later — don't build the log itself
yet).

## Commands

### `ImportMedia`
Probes the file at `path` (duration, dimensions, fps as applicable) and adds a `MediaItem`
to `project.media`. Returns the new `media_id`. Does not place anything on a track.

- Errors: file not found, unsupported/unprobeable format.

### `AddTrack`
Appends a new empty `Track` of the given `kind` (`video` | `audio` | `caption`) to
`project.tracks` — or, when `sequence_id` is given (schema v7), to that sequence's tracks.
Returns the new `track_id`. `id` is normally omitted (server generates
one); the GUI supplies it for CapCut-style auto-track-on-drop, where an `AddTrack` and a
following `AddClip{track_id}` in the same `apply_commands` batch need to agree on the new
track's id before the server has actually created it (see "Batch application" below).

- Errors: caller-supplied `id` collides with an existing track id (in the root timeline or
  any sequence), `sequence_id` not found (`SequenceNotFound`), or a `caption` track requested
  inside a sequence (`UnsupportedForSequence` — caption tracks are root-only in v7).

### `AddClip`
Adds a `VideoClip`/`AudioClip` (matching the target track's kind) referencing `media_id` to
`track_id`, placed at `position_secs`, sourced from `[source_in_secs, source_out_secs)` of
the media. **Sequence media** (schema v7, `kind: "sequence"`) may go on a video track (its
picture) or an audio track (its mixed sound); the source range is in the sequence's own time
and is checked against its derived duration.

- Errors: `track_id`/`media_id` not found, track kind doesn't match media kind, resulting
  clip overlaps an existing clip on that track, `source_out_secs <= source_in_secs`, or the
  source range exceeds the media's probed duration (skipped if duration is unknown — see
  project-schema.md), or placing the sequence would make it contain itself, directly or
  through other sequences (`SequenceCycle`).

### `SplitClip`
Splits the clip at timeline position `at_secs` into two clips with adjusted
`source_in_secs`/`source_out_secs`, both keeping the same `media_id`. New clip on the right
gets a fresh id; the left retains the original id. Media clips copy `transform`; keyframes
are split at the cut (left keeps `t < split`, right remaps `t' = t - split`); effects are
copied with new instance ids on the right clip. Adjustment layers split the same way
(`duration_secs` divided at the cut, opacity keyframes split, effects re-id'd).

- Errors: `at_secs` not strictly inside the clip's timeline span.

### `TrimClip`
Adjusts `source_in_secs` and/or `source_out_secs` in place (ripple is not implied — this
only changes which part of the source plays, not other clips' positions). At least one of
the two optional fields must be `Some`.

- Errors: `clip_id` is a caption clip (trim only applies to media clips) or an adjustment
  layer (`TrimAdjustment` — it has no source range; use `SetAdjustmentLayer`), resulting
  `source_out_secs <= source_in_secs`, new range exceeds media bounds, or the resulting
  on-timeline span (position unchanged, duration recomputed from the new range) overlaps a
  neighboring clip on the same track.

### `MoveClip`
Changes a clip's `position_secs`, optionally moving it to a different track
(`new_track_id`, which may be in another timeline — root or a sequence). Used for reordering
clips to match the script. Works on adjustment layers (video tracks only).

- Errors: destination track kind mismatch, resulting overlap with another clip on the
  destination track, or moving a sequence clip into a timeline that would make the sequence
  contain itself (`SequenceCycle`).

### `DeleteClip`
Removes a clip. If `ripple: true`, all subsequent clips on the same track shift left to
close the gap; if `false`, a gap is left in place.

- Errors: clip not found on track.

### `AddCaption`
Adds a `CaptionClip` with `text` at `position_secs` for `duration_secs`, tagged with
`style_id` (a built-in style name in v0; asset-pack-provided styles come in Phase 3). Target
track must be `kind: "caption"`.

- Errors: track kind mismatch, `duration_secs <= 0`, overlap with existing caption clip on
  that track.

### `SetCaption` (Phase 2)
Updates an existing `CaptionClip` on a caption track. At least one of `text`,
`position_secs`, `duration_secs`, or `style_id` must be `Some`. Omitted fields are left
unchanged. Used by the GUI caption inspector.

- Errors: track/clip not found, track kind mismatch, no fields to change, resulting
  `duration_secs <= 0`, or resulting overlap with another caption on the same track.

### `SetAudioGain`
Sets `gain_db` on an audio-bearing clip (`AudioClip`, or a `VideoClip` with embedded audio
once that's modeled — v0 audio gain applies to `AudioClip` only).

- Errors: clip not found, or clip type has no audio.

### `GenerateCaptions` (Phase 1)
Runs local Whisper STT on `media_id` (video or audio) and adds one `CaptionClip` per
transcript segment to `track_id` (must be a caption track). Segments are placed at
`timeline_offset_secs + segment.start_secs` with duration `segment.end - segment.start`.
Uses `style_id` for export burn-in (built-in: `tiktok-bold-yellow`, `tiktok-minimal`,
`tiktok-box`, `youtube-lower-thirds`).

Requires `whisper-cli` (or `whisper`) on PATH and `RENDERLY_WHISPER_MODEL` pointing at a
ggml model file.

- Errors: track/media not found, track kind mismatch, caption overlap, Whisper unavailable.

### `GenerateVoiceover` (Phase 1)
Synthesizes narration from `text` using `provider` and writes WAV to `output_path`, then
imports it and places a clip on `track_id` at `position_secs`. Returns `VoiceoverGenerated`
with `media_id` and `clip_id`.

`VoiceoverProvider` variants (JSON tag `provider`):

- `piper_local` — local Piper ONNX via `piper` CLI; requires `RENDERLY_PIPER_MODEL`.
- `open_ai` — OpenAI TTS (`tts-1`); requires `OPENAI_API_KEY` (BYO, opt-in).

- Errors: track not found or not audio, TTS unavailable, overlap, probe/import failure.

### `SetAudioFade` (Phase 1)
Sets `fade_in_secs` and `fade_out_secs` on an audio clip. Applied during export via FFmpeg
`afade`.

- Errors: clip not found, not audio, negative fade durations.

### `SetTrackAudioRole` (Phase 1)
Sets `audio_role` on an audio track: `voiceover`, `dialog`, `music`, or `ambience`. Pass
`null` for `role` to clear. When a voice/dialog track and a music track are both present,
export applies sidechain ducking using `settings.duck_db` (default −12 dB).

- Errors: track not found, track is not audio.

### `SetProjectSettings` (GUI rebuild M1)
Changes project-level output settings (`width`, `height`, `fps` — e.g. an aspect-ratio
switch in the GUI). At least one field must be `Some`.

- Errors: no fields given, resulting `width`/`height` is `0`, or resulting `fps <= 0`.

### `SetTrackFlags` (GUI rebuild M1)
Sets `muted`, `locked`, and/or `hidden` on a track. At least one field must be `Some`;
omitted fields are left unchanged. `muted`/`hidden` are enforced by the engine (export and
playback both skip muted/hidden tracks); `locked` is GUI-honored only — this command does
not itself reject edits to a locked track (see project-schema.md's `Track` note).

- Errors: track not found, no fields given.

### `RenameTrack` (GUI rebuild M1)
Sets a track's `name`.

- Errors: track not found.

### `DeleteTrack` (GUI rebuild M1)
Removes a track and all its clips, from whichever timeline (root or sequence) owns it.

- Errors: track not found.

### `MoveTrack` (drag-to-reorder)
Reorders a track within its own timeline's track list (`Project::tracks` for a root track,
the sequence's `tracks` for a sequence track) — this array order *is* the compositing
z-order (index 0 = bottom of the stack). Removes the track from its current position and
reinserts it at `new_index`, which is clamped to that list's `0..len - 1`. A no-op if the
clamped index equals the track's current position.

- Errors: track not found.

### `SetClipEnabled` (GUI rebuild M1)
Soft-enables/disables a media clip (`VideoClip`/`AudioClip`) or an adjustment layer without
deleting it — the existing `enabled` field on those clip types, now settable directly
instead of only via `AddClip`'s default.

- Errors: track/clip not found, or the clip is a `CaptionClip` (no `enabled` field).

### `SetClipTransform` (Phase 3.1)
Replaces the static `transform` on a media clip (video or audio). Finite numbers only;
`opacity` is clamped to `0..=1` and the cover-fit position `fit_x` / `fit_y` to `-1..=1`
(see [project-schema.md](project-schema.md) "Cover-fit position"). The GUI's "Pan within frame"
sliders dispatch this command; keyframes of `fit_x` / `fit_y` go through `SetClipKeyframes`
(values in `-1..=1`).

- Errors: track/clip not found, not a media clip, or non-finite transform fields.

### `SetClipKeyframes` (Phase 3.1)
Replaces all keyframe tracks on a media clip. Validates unique properties, non-negative
finite times, finite values, opacity keys in `0..=1`; sorts keys by time on write. On an
adjustment layer only `opacity` keyframes are accepted.

- Errors: track/clip not found, not a media clip or adjustment layer, invalid keyframe data,
  or a non-opacity property on an adjustment layer (`InvalidKeyframes`).

### `SetClipEffects` (Phase 3.1 / 3.4 / 3)
Replaces the effect-instance list on a media clip. Validates unique instance ids, known
`effect_id`s (builtins, loaded `pack:…:lut:…`, loaded `wasm:…`), finite params, and clamps
known params. GPU builtins and pack `.cube` LUTs run in the compositor; WASM frame effects run on the CPU
frame before upload. Also accepted on adjustment layers (schema v7), where the chain runs on
the composited picture beneath the layer; **WASM plugin effects are rejected there**
(`InvalidEffects`) because they need CPU pixels of a decoded frame and an adjustment layer's
input exists only on the GPU.

- Errors: track/clip not found, not a media clip or adjustment layer, or invalid effect list.

### `SetClipTransition` (Phase 3.5 / 3)
Sets or clears (`None`) the outgoing `ClipTransition` on a video media clip. Supports all
ten builtin kinds (`crossfade`, `fade_black`, `wipe_*`, `slide_*`, `iris`, `blur_dissolve`).
Requires a following clip; duration must be `> 0` and ≤ half of each adjacent clip's
*timeline* duration (honors `speed`).

- Errors: not a video track, no following clip, invalid duration.

### `SetClipSpeed` (Phase 3)
Sets constant base playback `speed` on a media clip (`0.25`..`4.0`) used when no Speed
keyframes are present. Timeline duration and source-time mapping update; audio uses
pitch-preserving `atempo`. Rejects overlaps caused by a longer timeline span when slowing
down. Speed keyframes (`SetClipKeyframes` with `AnimProperty::Speed`) override via integral
mapping (schema v5).

### `LoadAssetPack` / `UnloadAssetPack` (Phase 3)
Load or unload a declarative pack directory (`pack.json`). Paths are stored on the project.

### `LoadWasmPlugin` / `UnloadWasmPlugin` (Phase 3)
Load or unload a WASM plugin directory (`plugin.json` + `.wasm`). Plugins may export frame
`process` and/or audio `process_audio`.

### `AddStickerFromPack` / `AddSfxFromPack` (Phase 3 deferred)
Import a pack sticker (image → video track) or SFX (audio → audio track) and place a clip
at `position_secs`. Requires the pack to be loaded.

### `TrackMotion` (Phase 4)
Tracks a region of a video clip and writes keyframes describing where it goes.

- `sample_count`: number of keyframes written (clamped to `2..=analysed frames`). Tracking
  itself always uses every source frame (<= 30 fps, <= 900 frames) so large inter-frame motion
  never breaks the tracker; the keyframes are samples of that dense path.
- `region` (optional `TrackRegion { x: f64, y: f64, width: f64, height: f64 }`, default
  `{ x: 0.25, y: 0.25, width: 0.5, height: 0.5 }`): normalised
  rectangle (0..1, origin top-left, y down) in the clip's video frame, *before* the compositor's
  cover-fit crop, defined on the first frame of the clip. It must lie inside the frame with
  positive size.
- `track_rotation_scale` (default `false`): fit a 4-DOF similarity instead of a translation and
  also write `Rotation`, `ScaleX` and `ScaleY` keyframes (cumulative rotation/scale of the
  region relative to the first frame).

Algorithm: Shi-Tomasi corners inside the region (grid-distributed), pyramidal Lucas-Kanade
flow with a forward-backward consistency check, RANSAC fit of translation (or similarity) per
frame pair so features on a different-moving foreground are rejected, region carried along with
the estimate, features re-detected when fewer than half survive. Frames are decoded
sequentially at <= 360 px tall via one `VideoReader`; clip `speed`, `source_in_secs` and Speed
keyframes are honoured when mapping timeline time to source frames.

Output convention (matches the compositor): `PosX`/`PosY` are the tracked point's position as
an NDC offset from the canvas centre (+x right, **+y up**), after the cover-fit mapping to the
project canvas, so a layer with these keyframes sits on the tracked point. The cover window is
the one the clip's static `transform.fit_x` / `fit_y` select (both `0` = centred, the usual case);
keyframed fit is not read here. Previously written
`PosX`/`PosY` (and, when `track_rotation_scale`, `Rotation`/`ScaleX`/`ScaleY`) tracks are
replaced; other keyframe tracks are untouched. `Rotation` is degrees, counter-clockwise on
screen. Still images are a no-op (`Applied`, nothing written).

- `target` (optional `ClipRef`, default `None` = this clip): write the keyframes onto another
  video clip so that clip follows the tracked point (e.g. a sticker or text overlay pinned to
  a moving object). The tracked clip is then not modified at all. Target semantics:
  - The target must be a `Clip::Video` on a **video** track in the **same timeline** (root or
    the same sequence) as the tracked clip, must not be the tracked clip, and must overlap it
    in timeline time. Otherwise `InvalidTrackTarget` (with a reason), or `TrackKindMismatch`
    for a non-video track.
  - Coordinates: the tracked point is placed on the canvas with the tracked clip's transform
    at each frame's time (`evaluate_transform`, keyframes included): with the tracked clip's
    position `p` (cover-fit NDC, +y up), scale `(sx, sy)`, rotation `θ` (degrees, CCW) and
    offset `(tx, ty)`, the canvas point is `P = R(θ) · (sx·p.x, sy·p.y) + (tx, ty)`.
  - Offset (one behaviour, by design): the target's `PosX` = `P.x + target.transform.x` and
    `PosY` = `P.y + target.transform.y`, i.e. the target's **static** `x`/`y` is kept as a
    user-placed offset from the tracked point. Re-tracking does not accumulate it.
  - Times: keyframe times are target-local (`timeline - target.position_secs`). The
    `sample_count` keys are spread evenly over the **overlap** of the two clips, so every key
    lies inside the target's span and the first/last keys sit on the overlap's ends.
  - `track_rotation_scale`: `Rotation` = `target.transform.rotation_deg + Δθ`, `ScaleX` =
    `target.transform.scale_x × Δsx`, `ScaleY` = `target.transform.scale_y × Δsy`, where `Δ` is
    the region's rotation/scale relative to the first tracked frame, multiplied by the change
    of the tracked clip's own rotation/scale since its first frame (so a scaled or rotated
    tracked layer is accounted for).
  - Replaces the target's existing `PosX`/`PosY` keyframe tracks (and `Rotation`/`ScaleX`/
    `ScaleY` in rotation/scale mode). Its other keyframe tracks are left alone; its static
    transform is never changed.
  - Accuracy: tracking is unchanged; the mapping is exact per analysed frame, and the target
    is linearly interpolated between its keys. The core tests measure < 0.001 NDC on a
    synthetic pan with the tracked clip at identity and < 0.001 NDC with a scaled, rotated
    and offset tracked clip.

Errors: `TrackNotFound`, `ClipNotFound`, `NotMediaClip`, `TrackKindMismatch` (not a video
track), `MediaNotFound`, `InvalidTrackTarget(String)` (bad `target`: self, not a video media
clip, other timeline, no overlap), `Analysis` (audio-only media, zero-length clip, invalid
`region`, no trackable features in the region, decode failure). Old JSON without `region` /
`track_rotation_scale` / `target` still deserialises.

### `StabilizeClip` (Phase 4)
Estimates the camera's global motion and writes compensating transform keyframes.

- `strength` (optional, default `0.6`, clamped to `0..=1`): smoothing amount. The camera path
  is low-passed with a Gaussian of `0.05 + 1.5 * strength` seconds; `0` only removes
  frame-to-frame noise, `1` gives a very steady, heavily smoothed path.
- `sample_count` (optional): number of frames to analyse. Default is one per native source
  frame (<= 30 fps), capped at 900; smaller values skip frames and track larger motions.

Algorithm: whole-frame corner features, pyramidal LK with forward-backward check, RANSAC
similarity (translation + rotation + uniform scale) per frame pair, accumulation into a camera
path, Gaussian smoothing of its translation/angle/log-scale (linear-trend padding at the ends
so a steady pan is preserved), and per-frame correction `smoothed * path^-1`. A constant zoom
(`<= 1.4`) is added to the scale keyframes so borders are not exposed; if the corrections
need more, edges can still show. The corrections are composed with the clip's static
`transform` (keyframes override static values), and keys are simplified (Ramer-Douglas-Peucker,
sub-pixel tolerance).

Writes `PosX`, `PosY`, `Rotation`, `ScaleX`, `ScaleY` tracks (existing tracks for those
properties are replaced). Values use the compositor's conventions (NDC y-up positions, degrees
counter-clockwise on screen). The compositor rotates in NDC rather than pixel space, so on
non-square canvases large rotation corrections are slightly anisotropic. Still images are a
no-op. Errors as for `TrackMotion`, plus `Analysis` for a non-finite `strength`.

### `AutoReframeClip` (Phase 4)
Subject-aware reframing for `target_aspect` (width / height, finite and > 0; the GUI passes the
project canvas aspect). Signature unchanged; no new fields.

Detection: frames are decoded sequentially (<= 360 px tall, about 5 per second of *timeline*
time, so speed changes are respected, capped at 900 samples) and run through the bundled
YuNet face detector (OpenCV Zoo, MIT; `renderly-core/src/ml`, pure-Rust `tract` inference, no
network, no download). The subject is the face with the largest `confidence x area`, with a
continuity bonus for faces near the previous subject so it does not flip between two people;
it is held through gaps up to 2 s and interpolated across longer ones (leading/trailing gaps
take the nearest detection). Detections need >= 0.7 confidence in at least 10% of the samples.

Framing: the subject is followed by **moving the cover-fit window across the source**, not by
moving or zooming the layer. The subject path (smoothed, ~0.12 s) drives a dead-zone camera on
the window centre (the camera ignores movement within 15% of the visible half-extent, then
~0.3 s Gaussian smoothing), kept inside the source. The result is written as `FitX` keyframes
(clip-local times, simplified), so the window pans across the whole frame; `FitY` keyframes
are written only when the source overflows the canvas vertically (taller than the canvas
aspect). The static `transform.fit_x/fit_y` is set to the first sample. The mapping is the
inverse of the compositor's window placement: a subject at source fraction `u` is centred by
`fit = (2u - 1) / (1 - cs)`, `cs` being the visible fraction of that axis, clamped to `-1..=1`.
A subject outside the reachable range (the window cannot leave the source) sits at the nearest
flush position.

Scale: `transform.scale_x/scale_y` = the minimum zoom for the target aspect (`1` when the canvas
already has that aspect, which is the usual case). The older face-filling zoom is gone.
`PosX` / `PosY` / `ScaleX` / `ScaleY` / `FitX` / `FitY` keyframes are replaced, and
`transform.x/y` are reset to 0. Still images: one detection, a static `transform` (`fit_x/fit_y`
set, no keyframes).

Fallback: if no face is found (or the media cannot be decoded) the clip is centred: `x = y = 0`,
`fit_x = fit_y = 0`, `scale = ` the minimum zoom for the target aspect (`1` when the canvas
matches it), and any `FitX` / `FitY` keyframes are removed. The earlier fallback zoomed in to
`max(src_aspect/target, target/src_aspect)` (about 3.16x for 16:9 footage on a 9:16 target); that
cut away most of the picture even though the cover fit already fills the canvas, so it was
replaced.

Limitation: the framing is a window position, so a subject can only be centred while the window
can reach it (the window stays inside the source). Errors as before (`TrackNotFound`,
`ClipNotFound`, `TrackKindMismatch`, `NotMediaClip`, `MediaNotFound`, `Analysis` for a bad
`target_aspect`).

### `SetClipAudioDenoise` (Phase 4)
Sets (`Some`) or clears (`None`) `audio_denoise` on a clip of an **audio** track. `config` is
`{ enabled, backend, strength }`; `strength` is clamped to `0..=1` (non-finite is an error).
Backends:

- `"afftdn"` — FFmpeg spectral denoise filter in the mix graph; `strength` maps to noise
  reduction (0..20 dB). The serde default when `backend` is omitted, so older projects behave
  exactly as before.
- `"rnnoise"` — RNNoise network (`nnnoiseless`, BSD-3-Clause). `strength` is a wet/dry mix of
  the denoised and original signal (1.0 = fully denoised, 0.0 = original). Mix/export
  decode the clip's **whole source** to 48 kHz, run RNNoise in-process per channel, and cache
  the result as a WAV in `<temp>/renderly-denoise-cache/`, keyed by source path + mtime +
  size + backend + strength (edits invalidate; repeat runs reuse it). That file replaces the
  source as the clip's mix input, so export, playback and scrub all share it. The GUI
  creates new configs with `"rnnoise"`.

Errors: `InvalidDenoise` (unknown backend, non-finite strength), track not an audio track.

### `SetClipMask` (Phase 4, extended in v7)
Sets (`Some`) or clears (`None`) the mask on a media clip **or an adjustment layer**. On an
adjustment layer the mask limits where the adjustment applies, and its UVs are output-frame
(canvas) coordinates because the layer has no source frame; `raster`/`generated` mattes are
stretched to the canvas.

- Errors: track/clip not found, caption clip, invalid mask (`InvalidMask`).

### `CreateSequence` (schema v7)
Adds an empty nested sequence and its media item (`kind: "sequence"`) in one step, so the
sequence can be placed with `AddClip` like any media. `width`/`height` of `None` inherit the
project size. `id` (optional) lets a script reference the new sequence in later commands of
the same batch; it is generated when omitted. Returns
`SequenceCreated { sequence_id, media_id }`. Add tracks to it with
`AddTrack { sequence_id }`.

- Errors: `DuplicateSequenceId`, `InvalidDimensions` (a given size is `0`).

### `RenameSequence` (schema v7)
Sets the sequence's `name`.

- Errors: `SequenceNotFound`.

### `SetSequenceSettings` (schema v7)
Sets the sequence's output size. Both fields are written exactly as given: `None` resets
that axis to the project size (so to change one axis and keep the other, pass the current
value of the other).

- Errors: `SequenceNotFound`, `InvalidDimensions`.

### `DeleteSequence` (schema v7)
Removes the sequence and its media item.

- Errors: `SequenceNotFound`; `SequenceInUse` while any clip, on any timeline, still places
  it (delete or `UnnestClip` those clips first).

### `NestClips` (schema v7)
Moves the listed clips into a new sequence named `name` and replaces them with one clip
placing it ("compound clip"). All clips must be on the same timeline (the root or one
sequence). Relative times are preserved (the earliest clip starts at `0` inside the
sequence) and so is layering: each source track becomes one track of the sequence, bottom to
top, keeping its kind, name, audio role and flags; the moved clips keep their ids. A video
clip (`source_in 0`, `source_out` = the nested span) replaces the video/adjustment clips —
placed on the highest source video track with room, else on a new track at the top — and an
audio clip replaces any audio clips (same rule). Returns
`ClipsNested { sequence_id, media_id, video_clip_id?, audio_clip_id? }`. The command is
atomic: on any error the project is unchanged.

- Errors: empty `clips` or clips on different timelines (`InvalidNest`), `TrackNotFound` /
  `ClipNotFound`, a caption clip (`UnsupportedForSequence`).

### `UnnestClip` (schema v7)
Inverse of `NestClips` for an untouched nested clip: copies the sequence's clips of the
placing clip's kind (video/adjustment clips for a video clip, audio clips for an audio clip)
back into the parent timeline on new tracks inserted just above the clip's track (same
kinds, names, roles and flags; fresh clip and effect ids; times shifted by the clip's
position) and removes the placing clip. The sequence itself is kept (it may be placed
elsewhere; `DeleteSequence` removes it). Atomic.

- Errors: `CannotUnnest(reason)` — the clip does not place a sequence, is trimmed, has a
  speed change, effects, a mask, a transform, keyframes, a transition or background
  removal (audio: gain, fades or denoise); `TrackNotFound` / `ClipNotFound`.

### `AddAdjustmentLayer` (schema v7)
Adds an adjustment layer (`Clip::Adjustment`, `opacity 1`, no effects) to a video track at
`position_secs` for `duration_secs`. Give it effects with `SetClipEffects`, a mask with
`SetClipMask`, opacity with `SetAdjustmentLayer` / opacity keyframes. Returns `ClipAdded`.

- Errors: `TrackNotFound`, `TrackKindMismatch` (not a video track), `Overlap` with another
  clip on the track, `InvalidDuration` (`duration_secs <= 0`), `InvalidAdjustment`
  (`position_secs` negative or not finite).

### `SetAdjustmentLayer` (schema v7)
Updates an adjustment layer's `position_secs`, `duration_secs` and/or `opacity` (`0..=1`).
At least one field must be `Some`; omitted fields are unchanged (like `SetCaption`).

- Errors: `SetAdjustmentLayerRequiresChange`, `TrackNotFound`, `ClipNotFound`,
  `NotAdjustmentClip`, `InvalidDuration`, `InvalidAdjustment` (bad position/opacity),
  `Overlap`.

### Background removal and mattes

`SetClipBackgroundRemoval` stores the config (`model_id`, `threshold`, `feather`, `temporal`,
`matte_cache_dir`, `matte_frames`; see project-schema.md) and validates `threshold` / `feather`
in 0..=1. `GenerateBackgroundMatte` bakes the matte the clip then renders with, according to the
configured `model_id` (default `heuristic` when the clip has no config):

| `model_id` | Backend | Result |
|---|---|---|
| `heuristic` | built-in luma / green-screen threshold | one `matte.png` for the clip (middle source frame) |
| `cli`, `birefnet` | the binary named by `RENDERLY_SEG_CLI` (argv1 = input PNG, argv2 = alpha PNG) | one `matte.png` for the clip |
| `rvm` | **Robust Video Matting, in-process** ([`renderly_core::ml::rvm`]) | a matte **per source frame** over the clip's used source range |

`rvm` details:

- Frames are decoded sequentially (one ffmpeg reader, <= 720 px tall, <= 30 fps), shrunk to a
  working size (long edge <= 512, multiples of 32, aspect kept) with `downsample_ratio` chosen
  from it (0.5 at the full 512 px size, else 1.0), matted with the recurrent state carried
  frame to frame and **reset at the start of the range**, and the alpha is bilinearly resampled
  to the decode size and stored as 8-bit grayscale PNGs `f000000.png`, `f000001.png`, ... in
  `matte_cache_dir` (`$TMP/renderly-matte-rvm-<media>-<clip>`), plus a representative
  `matte.png` (middle frame) and a `matte.json` sidecar (model id + SHA-256 of the weights
  used, source path / mtime / size, covered source range, fps, frame count, size, stamp).
  The sequence is built next to the cache dir and swapped in only on success, so a failed or
  cancelled run leaves the previous matte intact. Speed: ~0.1 s per frame in release (CPU).
- Mattes are indexed by **source** time (frame `i` is `range_start + i / fps`), not timeline
  time: speed changes, moves and splits keep working; a clip trimmed to a sub-range, or a split
  piece, is still covered. Export and the preview pick the nearest matte frame to each layer's
  source time.
- The command **never downloads** the weights: when they are not installed it fails with
  `ModelNotAvailable { model_id: "rvm-mobilenetv3-fp32" }` and changes nothing. Install them
  explicitly (`renderly models download rvm-mobilenetv3-fp32 --yes`, MCP `download_model`, or the
  app's consent dialog); `RENDERLY_MODEL_DIR` points at an existing copy.
- `threshold` and `temporal` do not apply to `rvm` (the matte is already soft and temporally
  coherent); `feather` and the clip's `mask.invert` apply exactly as for single-image mattes.
- Via `apply_command` the bake **blocks** until finished (CLI, headless MCP). The desktop app (and
  the live bridge) intercept it: the bake runs against a project snapshot on a worker thread with
  `matte:progress` events and a cancel, off the edit lock, and the app then applies the small
  `AttachBackgroundMatte` command (`CommandError::Cancelled` when cancelled).

`AttachBackgroundMatte` points a clip at an already-baked `rvm` sequence: it reads `matte.json`
from `cache_dir`, refuses a sequence that is stale for the clip (different source file, or the
clip uses source time outside the baked range), and sets `background_removal.matte_cache_dir`,
`matte_frames` (the frame index) and the `Generated` clip mask. `GenerateBackgroundMatte(rvm)`
runs it itself; it exists for hosts that bake off the project lock.

Matte state for UIs/agents: `renderly_core::segmentation::matte_status` (MCP
`get_background_matte_status`, app `background_matte_status`) reports `not_generated`, `ready`, or
`stale { reason }` (source file changed on disk, clip trimmed outside the baked range, cache files
gone, model changed).

### Commands that reject sequence media (schema v7)
`TrackMotion`, `StabilizeClip`, `AutoReframeClip`, `GenerateBackgroundMatte`,
`GenerateCaptions` and `SetClipAudioDenoise` reject a clip/media that is a nested sequence
with `UnsupportedForSequence` instead of guessing (they analyse or process the clip's own
decoded frames/audio; a sequence has none). Everything else — transform, keyframes,
transitions, speed, masks, effects, gain, fades, split, trim, move — works on sequence
clips unchanged.

### `Export`
Renders the current `Project` timeline to `output_path` using `preset` (e.g.
`TikTok9x16`, `Youtube16x9`, or a `Custom { width, height, fps }` variant). Muxes mixed
audio (with fades and optional music ducking) and burns caption clips. Video layers honor
evaluated transform+opacity, speed, transitions, and effects (builtin / pack / wasm).

`Export` renders the **root** timeline (nested sequences are rendered recursively where
clips place them; `timeline_duration` is the root's own length). `render_frame` likewise.

`Export` does not mutate `Project`. Progress / cancel for interactive clients go through
`renderly_core::export::export_project_with_progress` (GUI Tauri command + CLI status
line); `apply_command(Command::Export)` and MCP still call the fire-and-forget
`export_project` wrapper. Returning `false` from the progress callback yields
`ExportError::Cancelled` and removes the export temp directory.

### OpenTimelineIO export / import (not commands)
Like `Export`, OpenTimelineIO **export** is a read of the project, not a mutation, and so is not
a `Command`: `renderly_core::otio::export_otio` (CLI `export-otio`, MCP `export_otio`, app
"Export timeline (OTIO)"). **Import** *creates a new project from a file* — it is a load-path
function beside `Project::from_json`, not a `Command` (a `Command` mutates the open project, and
importing replaces it wholesale): `renderly_core::otio::import_otio` (CLI `import-otio`, MCP
`import_otio`, app "Import timeline (OTIO)"). All surfaces share the same two core functions.
Mapping, rounding rules and warnings: [otio.md](otio.md).

## Batch application (GUI rebuild M3)

The Tauri `apply_commands` command takes `Vec<Command>` and applies them in order against
a single `Project` clone: all-or-nothing (any command failing discards the whole batch,
leaving the session's project untouched) and exactly one undo snapshot/save/`project:changed`
emit for the whole batch, not one per command. This exists for gestures that are logically
a single edit but need more than one `Command` — the motivating case is CapCut-style
auto-track-on-drop: dropping a media card below the last timeline track dispatches
`[AddTrack{id: Some(new_id)}, AddClip{track_id: new_id, ...}]` as one batch, so undo
removes both the track and the clip together instead of leaving an empty track behind.
`apply_commands` is Tauri/GUI-layer only (`renderly-app/src-tauri/src/lib.rs`), not part
of `renderly-core`'s public API — CLI and MCP keep calling `apply_command` once per
command, which is sufficient for scripted/agent use.

## History, transactions, coalescing and preview overrides

The `Command` enum and `apply_command` are unchanged by this section. Everything here is
envelope and session-layer behaviour around them. The full design is in
[architecture.md](architecture.md) "History, transactions and preview overrides"; this
section fixes the command-facing rules.

- **History lives in core.** `renderly_core::history::History<S>` owns the undo/redo
  records (label, applied command names, merge key, pre-change state). The app, bridge and
  CLI all record into it. The GUI remains a thin dispatcher.
- **Transactions.** `begin_transaction(label)`, `commit_transaction()` and
  `rollback_transaction()` (Tauri and bridge) group every edit between begin and commit into
  one undo record. Transactions nest by depth, and rollback aborts the outermost one. Agent
  turns open a transaction when they are sent and commit it at `TurnFinished`. The CLI's
  `apply-script` is one transaction.
- **Merge key (coalescing).** `apply_command` and `apply_commands` accept an optional
  `merge_key` **next to** the command, never inside the `Command` enum. A change with the
  same key as the top record, within `COALESCE_WINDOW` (1 s) and before `end_gesture()`,
  amends that record instead of adding one, so a slider drag is one undo step. The record
  keeps its original pre-gesture state. Suggested keys: `transform:<clip id>`,
  `effects:<clip id>:<effect instance id>`, `opacity:<clip id>`.
- **Gesture end.** `end_gesture()` (Tauri and bridge) closes the top record's gesture. The UI
  calls it on mouse-up or key-up. Without it, the window ends the gesture after 1 s idle.
- **Preview overrides are not commands.** `set_preview_override(target, override)` and
  `clear_preview_override(target)` (Tauri and bridge) set or clear a temporary clip transform,
  opacity or effect stack. They never touch the project, never save and never enter undo. The
  shared render path applies them, so the native `FrameRenderer` (`render_frame`, `look`) and
  the wasm preview honour them identically. Export never applies them.
- **Memory.** Each undo or redo stack holds at most 100 records (oldest evicted). Each record
  holds one whole-project snapshot.

## Edit contract

Normative for every write surface: `renderly-core` (`renderly_core::edit_contract`), the Tauri
commands, the live bridge ([bridge-protocol.md](bridge-protocol.md)), `renderly-mcp`
([mcp-agent-guide.md](mcp-agent-guide.md)), the in-app agent tools, and `renderly-cli`. There is
one implementation: `plan_write` in core. No surface re-implements a rule. Capabilities are
reported as `renderly.capabilities.v1` (`describe_commands`; core `edit_contract::capabilities()`).
`describe_commands` also lists the effects, filters, transitions and LUTs the engine can apply
(builtins and the open project's packs, `renderly_core::packs::describe`); `describe_effects`
lists them for pack folders named in the call.

### Envelope

Every write may carry these optional fields next to its command(s):

| Field | Meaning |
|---|---|
| `expected_revision` (u64) | The `project_revision` the client last read. If it is not the current revision the write is rejected with `revision_conflict`. |
| `dry_run` (bool, default false) | Apply to a scratch copy and report. Changes nothing, records no history, commits nothing. |
| `request_id` (string) | Idempotency key. See "Idempotency". |
| `allow_deletion` (`{reason, ids}`) | Required for an agent write that deletes anything. See "Deletion guard". |

A batch (`apply_commands`, `edit` with several commands, CLI `apply-script`) is one write: all or
nothing, one undo step.

### Revisions

- `project_revision` is a monotonic integer per open project. It is returned by every read that
  describes project state: `get_project`, `get_editor_status`, the agent `timeline` outline, and
  `renderly show`.
- The desktop app counts every committed edit, undo and redo (its existing revision counter).
  Headless MCP and CLI sessions count their own committed writes. A new project or an opened
  project starts at 0.
- Checking happens under the project's edit lock, before anything is applied. A write that
  changes nothing does not bump the revision.

### Results

A write returns:

```json
{
  "committed": true,
  "revision": 7,
  "changed": { "created": ["…"], "modified": ["…"], "deleted": ["…"] },
  "summary": ["Split clip", "Modified 1 clip"],
  "warnings": [],
  "patch": [ { "op": "replace", "path": "…", "value": "…" } ],
  "dry_run": false,
  "replayed": false
}
```

- `committed`: this call changed the project. False for dry runs, no-op writes and rejected
  writes. True for a replayed `request_id` (the original commit; `replayed: true`).
- `changed` ids cover media items, tracks, clips and sequences. They are computed by diffing the
  project before and after, keyed by id, not per command. A nested sequence's own entry excludes
  its children, so changing a clip marks only that clip. A moved clip keeps its id and shows as
  `modified`.
- `patch` is an RFC 6902 patch from the pre-write project to the post-write project.
- App IPC keeps its existing top-level fields (`revision`, `patch`, `outcome`), with the result
  fields flattened beside them.

### Errors

Codes are stable strings. Each error is `{ "code", "message", "hint", "data"? }`.

| Code | When | `data` |
|---|---|---|
| `invalid_command` | Malformed JSON, invalid fields, overlaps, dry run of a file-writing command, `Export` sent as a write. | |
| `not_found` | A track, clip, media item or sequence id does not exist. | |
| `revision_conflict` | `expected_revision` is not current. Hint: "re-read with timeline, then retry". | `current_revision` |
| `request_id_reused` | The id was used for a different payload. | |
| `deletion_not_allowed` | Deletion without a matching `allow_deletion`. | `would_delete` |
| `busy` | Reserved: another write is in progress. Surfaces wait on the edit lock rather than return this today. | |
| `rolled_back` | A batch failed part-way; nothing was applied. | `failed_index`, `failed_command`, `cause` |
| `internal` | Media, export, model or file failures, or a bug. | |

A single-command failure reports its own code. A failing batch of two or more commands reports
`rolled_back`. Surfaces map errors as follows: MCP and the in-app tools return them as tool errors
(`isError: true`) with the JSON text; the bridge returns a JSON-RPC error with `data` set to the
error object; the CLI prints the JSON on stderr and exits non-zero; the webview receives the
message.

### Idempotency

- A committed write with a `request_id` is remembered together with a fingerprint of its
  commands and `allow_deletion`. The session keeps the last **256** committed request ids.
- The same id with the same payload returns the original result with `replayed: true` and applies
  nothing. This check runs before the `expected_revision` check, so a retry is never a conflict.
- The same id with a different payload fails with `request_id_reused`.
- Dry runs neither consult nor record the ledger.
- Opening or creating a project clears the ledger. The CLI keeps its ledger in a sidecar file
  next to the project (`<project>.state.json`) with its revision.

### Dry runs

A dry run applies the commands to a copy and returns the same result as a real write, with
`committed: false` and `dry_run: true`. It runs the deletion guard too, so it reports the ids a
deletion would need. It refuses commands that write files outside the project (`GenerateVoiceover`,
`GenerateSticker`, `GenerateBackgroundMatte`, `AttachBackgroundMatte`, `GenerateCaptions`), because
it cannot undo those writes.

### Deletion guard

Applies to **agent callers**: MCP, the live bridge, the in-app agent tools and the CLI. It does
**not** apply to the webview's own actions (the user acting).

A write that would delete any clip, track, media item or sequence is rejected with
`deletion_not_allowed` unless `allow_deletion` is present with a non-empty `reason` and an `ids`
list equal to exactly the ids the write would delete. "Would delete" is computed by the same diff
as `changed.deleted`, so the list includes children: deleting a track lists its clips too; deleting
a sequence lists its tracks and its media item. The error message lists the ids and the hint shows
the field to add; a dry run returns the same list.

Moves are not deletions. `NestClips` keeps the moved clips' ids, so it is not guarded.
`UnnestClip` removes its container sequence and that sequence's media item, so it is guarded like
any other deletion.

### Concurrency

- One write at a time per project: check, apply and commit run under one lock, so
  `expected_revision` is compared against the state it applies to.
- Reads may happen at any time. Read `project_revision` before planning a write and pass it back.

## Non-goals (later)

In-app remote marketplace with payments, custom pack WGSL shaders — see
[project-schema.md](project-schema.md). Chroma-key polish and full temporal matte baking
remain follow-ups on top of the Phase 4 command surface above.

## Version history

- **v0** (Phase 0): the initial 10 commands, matching project schema v0.
- **v0 + Phase 1** (non-breaking): added `GenerateCaptions`, `GenerateVoiceover`,
  `SetAudioFade`, `SetTrackAudioRole`; export muxes audio with fades/ducking and burns captions.
- **GUI rebuild M1** (non-breaking, project schema v1): added `SetProjectSettings`,
  `SetTrackFlags`, `RenameTrack`, `DeleteTrack`, `SetClipEnabled`.
- **GUI rebuild M3** (non-breaking): `AddTrack` gained an optional `id` field (server
  still generates one when omitted); added the Tauri-layer `apply_commands` batch API.
- **GUI rebuild M6** (non-breaking): `export_project_with_progress` + `ExportError::Cancelled`
  for cooperative cancel; GUI emits `export:progress` (~10 Hz) and exposes `cancel_export`.
- **Phase 3.1** (project schema v2): added `SetClipTransform`, `SetClipKeyframes`,
  `SetClipEffects`; `SplitClip` inherits transform / splits keyframes / remints effect ids.
- **Phase 3.4**: `SetClipEffects` executes builtin WGSL effects.
- **AI background removal** (non-breaking, schema stays v7): `GenerateBackgroundMatte` with
  `model_id: "rvm"` runs Robust Video Matting in-process and bakes a per-frame matte;
  new `AttachBackgroundMatte`; new `CommandError::ModelNotAvailable` / `Cancelled`; optional
  `background_removal.matte_frames` (see project-schema.md). `model_id: "rvm"` no longer routes
  through `RENDERLY_SEG_CLI` (use `cli` or `birefnet` for the external binary).
- **Phase 4** (non-breaking): `builtin:chroma_key` is a WGSL pass in the shared compositor (native export and wasm preview); optional `spill` param (0..1, default 0) added.
- **Phase 3.5** (project schema v3): added `SetClipTransition` (crossfade).
- **Phase 3 close-out** (project schema v4): `SetClipSpeed`; ten transition kinds; glitch;
  `LoadAssetPack` / `UnloadAssetPack`; `LoadWasmPlugin` / `UnloadWasmPlugin`.
- **Phase 3 deferred** (project schema v5): Speed keyframes; `AddStickerFromPack` /
  `AddSfxFromPack`; audio WASM `process_audio`.
- **Phase 4** (project schema v6): `SetClipMask`, `SetClipBackgroundRemoval`,
  `GenerateBackgroundMatte`, `SetClipAudioDenoise`, `ApplyTemplate`, `GenerateSticker`,
  `CreateMulticamGroup`, `SetMulticamAngle`, `TrackMotion`, `StabilizeClip`,
  `AutoReframeClip`; pack `templates`; export multicam angle filter; afftdn denoise mix.

- **Phase 4 feature tracking** (non-breaking): `TrackMotion` and `StabilizeClip` now use
  Lucas-Kanade feature tracking + RANSAC instead of a bright-pixel centroid; added optional
  `region` / `track_rotation_scale` (`TrackMotion`) and `strength` / `sample_count`
  (`StabilizeClip`); `PosX`/`PosY` are now NDC (previously an ad-hoc percent-like unit).

- **Auto-reframe subject detection** (non-breaking, schema stays v6): `AutoReframeClip` detects
  faces (YuNet via tract) and keyframes `PosX`/`PosY`; centred scale kept as the no-face fallback.

- **Cover-fit pan** (additive, schema stays v7): `SetClipTransform` takes `fit_x`/`fit_y`
  (`-1..=1`, default `0`); `SetClipKeyframes` accepts `fit_x`/`fit_y` keys in `-1..=1`.
  `AutoReframeClip` now follows the subject with `FitX`/`FitY` keyframes (the window pans across
  the whole source) at scale 1 instead of `PosX`/`PosY` plus a zoom; its no-face fallback is
  centred at scale 1 (it previously zoomed to the source/target aspect ratio).

- **RNNoise denoise** (non-breaking, schema stays v6): `SetClipAudioDenoise` also accepts
  `backend: "rnnoise"`; `afftdn` unchanged.

- **Phase 4 improvement push** (non-breaking): `MoveTrack` — drag-to-reorder tracks/layers;
  reorders `Project::tracks` (the compositing z-order) in place.

- **Schema v7** (additive; v1–v6 files load unchanged): nested sequences and adjustment
  layers. Added `CreateSequence`, `RenameSequence`, `SetSequenceSettings`, `DeleteSequence`,
  `NestClips`, `UnnestClip`, `AddAdjustmentLayer`, `SetAdjustmentLayer`; `AddTrack` gained
  `sequence_id`; `MoveTrack`/`DeleteTrack` are container-scoped; `AddClip` accepts sequence
  media (cycle-checked); `SetClipEffects`/`SetClipMask`/`SetClipKeyframes` (opacity only)/
  `SetClipEnabled`/`MoveClip`/`SplitClip`/`DeleteClip` accept adjustment layers; new errors
  `SequenceNotFound`, `DuplicateSequenceId`, `SequenceInUse`, `SequenceCycle`,
  `UnsupportedForSequence`, `CannotUnnest`, `InvalidNest`, `NotAdjustmentClip`,
  `InvalidAdjustment`, `SetAdjustmentLayerRequiresChange`, `TrimAdjustment`; new outcomes
  `SequenceCreated`, `ClipsNested`. See [schema-v7-design.md](schema-v7-design.md).

- **Phase 3.4** (non-breaking): `SetClipEffects` validates known builtin ids + param
  clamps; compositor executes `builtin:color_adjust` / `blur` / `lut_contrast` / `lut_warm`.
