# Command API

Status: **current** (matches project schema v6). This is the source of truth for the `Command` enum and
`apply_command` in `renderly-core`. GUI, CLI, and MCP must all dispatch through this exact
set (see AGENTS.md §0.1) — none of them may mutate `Project` state any other way.

## Shape

```rust
pub enum Command {
    ImportMedia { path: String },
    AddTrack { kind: TrackKind, name: String, id: Option<Id> },
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
    SetClipAudioDenoise { track_id: Id, clip_id: Id, config: Option<AudioDenoise> },
    ApplyTemplate { pack_id: String, template_id: String, position_secs: f64 },
    GenerateSticker { prompt: String, track_id: Id, position_secs: f64, output_path: String },
    CreateMulticamGroup { name: String, clip_ids: Vec<Id> },
    SetMulticamAngle { group_id: Id, active_angle: usize },
    TrackMotion { track_id: Id, clip_id: Id, sample_count: u32, region: Option<TrackRegion>, track_rotation_scale: bool },
    StabilizeClip { track_id: Id, clip_id: Id, strength: Option<f64>, sample_count: Option<u32> },
    AutoReframeClip { track_id: Id, clip_id: Id, target_aspect: f64 },
    Export { output_path: String, preset: ExportPreset },
}

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
`media_id` and `clip_id`; `Export` drives the render pipeline).

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
`project.tracks`. Returns the new `track_id`. `id` is normally omitted (server generates
one); the GUI supplies it for CapCut-style auto-track-on-drop, where an `AddTrack` and a
following `AddClip{track_id}` in the same `apply_commands` batch need to agree on the new
track's id before the server has actually created it (see "Batch application" below).

- Errors: caller-supplied `id` collides with an existing track id.

### `AddClip`
Adds a `VideoClip`/`AudioClip` (matching the target track's kind) referencing `media_id` to
`track_id`, placed at `position_secs`, sourced from `[source_in_secs, source_out_secs)` of
the media.

- Errors: `track_id`/`media_id` not found, track kind doesn't match media kind, resulting
  clip overlaps an existing clip on that track, `source_out_secs <= source_in_secs`, or the
  source range exceeds the media's probed duration (skipped if duration is unknown — see
  project-schema.md).

### `SplitClip`
Splits the clip at timeline position `at_secs` into two clips with adjusted
`source_in_secs`/`source_out_secs`, both keeping the same `media_id`. New clip on the right
gets a fresh id; the left retains the original id. Media clips copy `transform`; keyframes
are split at the cut (left keeps `t < split`, right remaps `t' = t - split`); effects are
copied with new instance ids on the right clip.

- Errors: `at_secs` not strictly inside the clip's timeline span.

### `TrimClip`
Adjusts `source_in_secs` and/or `source_out_secs` in place (ripple is not implied — this
only changes which part of the source plays, not other clips' positions). At least one of
the two optional fields must be `Some`.

- Errors: `clip_id` is a caption clip (trim only applies to media clips), resulting
  `source_out_secs <= source_in_secs`, new range exceeds media bounds, or the resulting
  on-timeline span (position unchanged, duration recomputed from the new range) overlaps a
  neighboring clip on the same track.

### `MoveClip`
Changes a clip's `position_secs`, optionally moving it to a different track
(`new_track_id`). Used for reordering clips to match the script.

- Errors: destination track kind mismatch, resulting overlap with another clip on the
  destination track.

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
Removes a track and all its clips.

- Errors: track not found.

### `MoveTrack` (drag-to-reorder)
Reorders a track within `Project::tracks` — this array order *is* the compositing
z-order (index 0 = bottom of the stack). Removes the track from its current position and
reinserts it at `new_index`, which is clamped to `0..tracks.len() - 1`. A no-op if the
clamped index equals the track's current position.

- Errors: track not found.

### `SetClipEnabled` (GUI rebuild M1)
Soft-enables/disables a media clip (`VideoClip`/`AudioClip`) without deleting it — the
existing `enabled` field on those clip types, now settable directly instead of only via
`AddClip`'s default.

- Errors: track/clip not found, or the clip is a `CaptionClip` (no `enabled` field).

### `SetClipTransform` (Phase 3.1)
Replaces the static `transform` on a media clip (video or audio). Finite numbers only;
`opacity` is clamped to `0..=1`.

- Errors: track/clip not found, not a media clip, or non-finite transform fields.

### `SetClipKeyframes` (Phase 3.1)
Replaces all keyframe tracks on a media clip. Validates unique properties, non-negative
finite times, finite values, opacity keys in `0..=1`; sorts keys by time on write.

- Errors: track/clip not found, not a media clip, or invalid keyframe data.

### `SetClipEffects` (Phase 3.1 / 3.4 / 3)
Replaces the effect-instance list on a media clip. Validates unique instance ids, known
`effect_id`s (builtins, loaded `pack:…:lut:…`, loaded `wasm:…`), finite params, and clamps
known params. GPU builtins and pack `.cube` LUTs run in the compositor; WASM frame effects run on the CPU
frame before upload.

- Errors: track/clip not found, not a media clip, or invalid effect list.

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
project canvas, so a layer with these keyframes sits on the tracked point. Previously written
`PosX`/`PosY` (and, when `track_rotation_scale`, `Rotation`/`ScaleX`/`ScaleY`) tracks are
replaced; other keyframe tracks are untouched. `Rotation` is degrees, counter-clockwise on
screen. Still images are a no-op (`Applied`, nothing written).

Errors: `TrackNotFound`, `ClipNotFound`, `NotMediaClip`, `TrackKindMismatch` (not a video
track), `MediaNotFound`, `Analysis` (audio-only media, zero-length clip, invalid `region`, no
trackable features in the region, decode failure). Old JSON without `region` /
`track_rotation_scale` still deserialises.

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

### `Export`
Renders the current `Project` timeline to `output_path` using `preset` (e.g.
`TikTok9x16`, `Youtube16x9`, or a `Custom { width, height, fps }` variant). Muxes mixed
audio (with fades and optional music ducking) and burns caption clips. Video layers honor
evaluated transform+opacity, speed, transitions, and effects (builtin / pack / wasm).

`Export` does not mutate `Project`. Progress / cancel for interactive clients go through
`renderly_core::export::export_project_with_progress` (GUI Tauri command + CLI status
line); `apply_command(Command::Export)` and MCP still call the fire-and-forget
`export_project` wrapper. Returning `false` from the progress callback yields
`ExportError::Cancelled` and removes the export temp directory.

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

- **RNNoise denoise** (non-breaking, schema stays v6): `SetClipAudioDenoise` also accepts
  `backend: "rnnoise"`; `afftdn` unchanged.

- **Phase 4 improvement push** (non-breaking): `MoveTrack` — drag-to-reorder tracks/layers;
  reorders `Project::tracks` (the compositing z-order) in place.

- **Phase 3.4** (non-breaking): `SetClipEffects` validates known builtin ids + param
  clamps; compositor executes `builtin:color_adjust` / `blur` / `lut_contrast` / `lut_warm`.
