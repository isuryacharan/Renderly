# Schema v7 design — adjustment layers and nested sequences

Status: **implemented** (engine in phase A, app UI in phase B). The normative parts live in
[project-schema.md](project-schema.md) and [command-api.md](command-api.md); this file stays
as the rationale. "Implementation notes (engine, phase A)" and "Implementation notes (app,
phase B)" at the end list where the shipped code deliberately differs from or extends this
design.

v7 adds two editing features that v6 cannot express:

- **Adjustment layers** — a clip that carries effects and applies them to everything
  composited beneath it for its duration.
- **Nested sequences** (compound clips) — a timeline that can be placed on another timeline
  as a single clip.

Both are additive. A v6 file is a valid v7 file once `schema_version` is bumped; no field
is renamed, moved, or reinterpreted.

## Goals and non-goals

Goals:

1. Every existing command keeps working unchanged on the new clip kinds wherever it makes
   sense (move, split, trim, delete, transform, effects, transitions, speed).
2. Preview and export stay pixel-identical: both features are evaluated in
   `renderly-core::compose`, which the native renderer and the wasm preview share.
3. Agents can build and edit nested timelines through the command API alone (AGENTS.md
   §0.1, §2 "no GUI-only feature").

Non-goals for v7:

- Per-sequence frame rate or sample rate. A sequence renders at the project's `fps` and
  `sample_rate`.
- Blend modes on adjustment layers (only opacity).
- WASM frame plugins on adjustment layers (see "Limits").

## Data model

### Sequences

```jsonc
{
  "schema_version": 7,
  // ...existing fields; `tracks` is still the root timeline...
  "sequences": [
    {
      "id": "uuid",
      "name": "Intro",
      "width": null,          // u32? — null inherits settings.width
      "height": null,         // u32? — null inherits settings.height
      "tracks": [ /* Track[] — same shape as the root timeline */ ]
    }
  ]
}
```

The root timeline stays in `project.tracks`. Making the root "just another sequence" would
be more uniform but would move every existing track in every existing file, and every
consumer (GUI store, JSON patches, MCP agents reading `tracks`) would break for no user
benefit.

**Track ids stay globally unique** across the root timeline and all sequences. That is the
property that lets existing commands address a track inside a sequence with no new
parameter: `find_track(track_id)` searches the root and every sequence.

### A sequence is placed through a `MediaItem`

```jsonc
{ "id": "uuid", "kind": "sequence", "sequence_id": "uuid", "path": "",
  "duration_secs": null, "width": null, "height": null, "fps": null }
```

- `MediaKind` gains `sequence`.
- `MediaItem` gains `sequence_id: Id?` (present iff `kind == "sequence"`).
- `path` is empty for sequence media. `duration_secs`, `width`, `height`, `fps` are stored
  as `null` and **derived on demand** (`Project::media_duration`, `Project::media_size`) so
  they can never go stale when the inner timeline is edited.

A nested sequence on a timeline is then an ordinary `MediaClip` whose `media_id` points at
that item:

- on a **video track**, a `Clip::Video` shows the sequence's composited picture;
- on an **audio track**, a `Clip::Audio` plays the sequence's mixed audio.

This matches how the model already separates picture and sound (video clips are silent;
audio lives in audio clips), and it is how other editors present compound clips — as an
item in the media bin.

Why not a new `Clip::Sequence` variant: roughly forty commands and the whole inspector
operate on `MediaClip`. A new variant would need each of them taught about it, and any one
missed would be a feature that silently does not work on nested clips. Reusing `MediaClip`
gives transform, keyframes, effects, masks, transitions, speed, trim and split for free.

Exactly one `MediaItem` exists per sequence. `CreateSequence` creates both; `DeleteSequence`
removes both.

### Adjustment layers

`Clip` gains a fourth variant, valid only on video tracks:

```jsonc
{
  "type": "adjustment",
  "id": "uuid",
  "position_secs": 4.0,
  "duration_secs": 6.0,
  "enabled": true,
  "opacity": 1.0,                 // 0..1 — mix between unadjusted and adjusted picture
  "effects": [ /* EffectInstance[] — same shape as MediaClip.effects */ ],
  "mask": null,                   // ClipMask? — limits where the adjustment applies
  "keyframes": []                 // KeyframeTrack[] — `opacity` only in v7
}
```

It is a separate variant rather than a `MediaClip` with no media because almost nothing on
`MediaClip` applies to it (no source range, speed, gain, fades, transform, transition,
background removal, multicam), and an adjustment layer has no pixels of its own.

Mask UVs on an adjustment layer are **output-frame** coordinates (0..1 across the canvas),
since there is no source frame. `Raster`/`Generated` masks are allowed and are stretched to
the canvas.

## Evaluation

`compose::eval` today returns a flat `Vec<ActiveLayer>` in z-order. v7 changes it to a list
of steps:

```rust
pub enum ComposeStep {
    Layer(ActiveLayer),
    /// Apply `effects` to everything composited so far, then mix with the unadjusted
    /// picture by `opacity` (and `mask`).
    Adjustment { effects: Vec<EffectInstance>, opacity: f32, mask: Option<ClipMask> },
}
```

Tracks are walked bottom to top as today. A video track contributes at most one step at
time `t`: its active media clip (or the two halves of a transition), or its active
adjustment clip. Adjustment and media clips share the track's no-overlap rule, so there is
never both.

### Adjustment layers in the compositor

On an `Adjustment` step the compositor:

1. finishes compositing the steps so far into the accumulation target `A`;
2. runs the effect chain with `A` as the source, into a scratch target `B`;
3. writes `mix(A, B, opacity * mask)` back as the new accumulation target;
4. continues with the steps above.

The accumulation target includes the background, so an adjustment layer over empty
timeline adjusts black. An adjustment layer at the bottom of the stack is legal and does
exactly that.

The effect chain is the existing one (`builtin:*` and pack LUTs), reused as-is — the same
shader passes a clip's effects go through, fed a canvas-sized source.

### Nested sequences in the compositor

`ActiveLayer` gains a source discriminator:

```rust
pub enum ActiveSource {
    Media { media_id: Id, path: PathBuf, source_time: f64 },
    Sequence { sequence_id: Id, time_secs: f64, instance: InstanceKey },
}
```

To render a `Sequence` layer the renderer evaluates the inner sequence's steps at
`time_secs`, composites them into an offscreen target at the sequence's resolution, and
uses that texture as the layer's source. From there the layer is ordinary: cover-fit,
transform, effects, mask, transition, opacity.

- **Recursion.** Sequences may nest. Depth is capped at 8 (`MAX_SEQUENCE_DEPTH`); exceeding
  it is an `EvalError`, not a stack overflow.
- **Cycles.** A sequence may not contain itself, directly or transitively. Commands that
  could create a cycle (`AddClip`, `MoveClip` across tracks, `NestClips`) reject it with
  `CommandError::SequenceCycle`. The depth cap is the backstop for hand-edited files.
- **Time mapping.** Inside the clip, sequence time is `source_time_at(t)` exactly as for
  media, so trim, speed and speed keyframes work unchanged. Past the end of the inner
  timeline the sequence renders transparent.
- **Instance keys.** The same media can be needed at two different times in one frame (a
  sequence used twice, or a clip both nested and on the root). Decoders — native ffmpeg
  readers and the preview's `<video>` pool — are keyed by `InstanceKey`, the chain of
  decoder keys from the root down to the layer (one per enclosing sequence layer, then the
  layer's own — its track id, or the derived id of a transition's incoming side), not by
  track id alone. (Decoder keys rather than clip ids, so a decoder keeps running across
  consecutive clips of one track exactly as before; see "Implementation notes".)
- **Transparency.** A sequence renders over transparent, not black, so nesting a
  lower-third or picture-in-picture keeps its alpha.

`timeline_duration(project)` remains the root timeline's duration.
`sequence_duration(project, id)` is the end of the last enabled clip in that sequence.

### Audio

`collect_audio_clips` becomes recursive. An `Audio` clip whose media is a sequence expands
into the sequence's own audio clips, each mapped through the outer clip's position, trim,
speed, gain and fades, and clipped to the outer clip's span. The mix is still a single
ffmpeg filter graph over leaf media — no intermediate renders. Ducking roles
(`TrackAudioRole`) are resolved per container: voice tracks inside a sequence duck music
inside that sequence, and the expanded clips inherit the outer track's role on the parent.

Speed keyframes on the outer clip are handled per constant-speed segment (the same 50 ms
windows a keyframed leaf clip already uses), so there is no constant-speed fallback; see
"Implementation notes" for the remaining limits.

## Commands

New:

| Command | Effect |
|---|---|
| `CreateSequence { name, width?, height?, id? }` | Adds an empty sequence and its `MediaItem`. Returns `SequenceCreated { sequence_id, media_id }`. |
| `RenameSequence { sequence_id, name }` | |
| `SetSequenceSettings { sequence_id, width?, height? }` | `null` resets to inherit. |
| `DeleteSequence { sequence_id }` | Errors with `SequenceInUse` if any clip references it. |
| `NestClips { clips: [{ track_id, clip_id }], name }` | Moves the listed clips (which must share one container) into a new sequence, preserving their relative times and track layering, and replaces them with one video clip — plus one audio clip if any were audio. Returns `ClipsNested { sequence_id, media_id, video_clip_id?, audio_clip_id? }`. |
| `UnnestClip { track_id, clip_id }` | Inverse of `NestClips` for an untrimmed, speed-1, effect-free nested clip: copies the sequence's clips back into the parent (new tracks as needed). Errors with `CannotUnnest` otherwise, naming the reason. Does not delete the sequence. |
| `AddAdjustmentLayer { track_id, position_secs, duration_secs }` | Adds an adjustment clip. Returns `ClipAdded`. Errors on overlap or a non-video track. |
| `SetAdjustmentLayer { track_id, clip_id, position_secs?, duration_secs?, opacity? }` | Like `SetCaption`: at least one field. |

Changed:

- `AddTrack` gains `sequence_id: Id?` (default: root timeline).
- `MoveTrack`, `DeleteTrack` act within the track's own container.
- `AddClip` accepts sequence media on video and audio tracks, and checks for cycles.
- `SetClipEffects`, `SetClipMask`, `SetClipKeyframes` (opacity only), `SetClipEnabled`,
  `MoveClip`, `SplitClip`, `DeleteClip` accept adjustment clips. `TrimClip` does not — it is
  defined in source time; use `SetAdjustmentLayer`.
- `Export` and `render_frame` are unchanged: they render the root timeline.

Unchanged and working on nested clips with no code change beyond evaluation: transform,
keyframes, transitions, speed, masks, effects.

Commands that analyse decoded frames (`TrackMotion`, `StabilizeClip`, `AutoReframeClip`,
`GenerateBackgroundMatte`) and `GenerateCaptions` reject sequence media with
`UnsupportedForSequence` in v7 rather than guess.

## Loading and compatibility

- `SCHEMA_VERSION = 7`. Loaders accept `1..=7`; saves write `7`.
- Every new field has a serde default (`sequences: []`, `sequence_id: null`), so v1–v6 files
  load without a migration step.
- A v6 build opening a v7 file already refuses by version. No downgrade path is provided.
- Validation on load (new `Project::validate`): unique track ids across containers, every
  `sequence_id` resolves, exactly one media item per sequence, no cycles. A file that fails
  loads with an error naming the offending id, rather than rendering wrongly.

## App

What the app does (implementation detail in "Implementation notes (app, phase B)"):

- The timeline shows one container at a time. Double-clicking a nested clip opens its
  sequence; a breadcrumb above the ruler (`Project > Intro > Titles`, any nesting depth)
  returns to an ancestor. The store holds `activeSequenceId` (null = root) plus the opened
  path; the timeline, track headers, playhead, snapping, selection, clipboard, drop targets
  and every container-scoped dispatch read tracks through one selector (`activeTracks`).
  History is global, so undo/redo keep working across containers; if the open sequence
  disappears the editor falls back to the nearest surviving ancestor (the root).
- Sequences are listed in the media panel alongside imported files (own icon, a "Sequences"
  filter chip, hover actions: open, rename, settings, delete) and can be dragged to the
  timeline like any media - onto a video lane for a picture clip, an audio lane for its mix.
  "New sequence" creates an empty one.
- "Nest clips" (multi-selection context menu, `Ctrl+G`) and "Unnest" (nested-clip context
  menu, `Ctrl+Shift+G`); engine refusals (`CannotUnnest`, `SequenceInUse`, overlap) surface
  as toasts. "Add adjustment layer" is on the video track header menu.
- Adjustment clips render as a distinct amber block with trim handles (a trim commits one
  `SetAdjustmentLayer`) and get an inspector: timing, opacity (+ opacity keyframes), the same
  effect stack and mask sections the media-clip inspector uses, enable toggle. WASM plugin
  effects are not offered for them.
- The preview follows the open sequence: while one is open it renders that sequence at the
  local playhead.

## Tests

- Round-trip and v6-load tests for the new fields.
- A unit test per new command, plus cycle, depth, overlap and in-use errors.
- Golden frames ([`golden_frames.rs`](../renderly-core/tests/golden_frames.rs)): adjustment
  layer over two layers; adjustment with opacity and mask; a layer above the adjustment is
  untouched; nested sequence with a transform and an effect; a sequence nested twice at
  different times; transparency preserved through nesting. The preview stand-in runs the
  same scenes, so parity is checked for both features.
- Audio: a nested sequence's mix equals the flat equivalent (sample comparison within
  tolerance).

## Limits accepted in v7

- WASM frame-plugin effects are ignored on adjustment layers. They run on CPU pixels of a
  decoded source frame, and an adjustment layer's input exists only on the GPU. The command
  rejects them so this is never silent.
- Background removal without a baked matte is not applied to a nested sequence's output
  (it stays per inner clip).
- Per-sequence fps and blend modes: out of scope, above.
- Nested audio: an outer fade is applied to the first/last expanded piece only, and a
  combined speed (outer x inner) outside `0.25..4` is clamped by the tempo filter.

## OTIO mapping

Implemented — see [otio.md](otio.md) for the full mapping table, rounding rules and warnings.
Summary of the schema-v7 parts:

- Root timeline → `Timeline` with a `Stack` of `Track`s.
- A nested sequence clip → a nested `Stack` in place (OTIO's native representation), with
  the sequence's name; on import, a nested `Stack` or `Timeline` becomes a sequence.
- An adjustment layer has no OTIO equivalent. Export writes a `Gap` carrying the effect
  list under `metadata["renderly"]`; import restores it from that metadata and otherwise
  treats it as a gap.

## Implementation notes (engine, phase A)

Where the shipped engine extends or differs from the design above (everything else matches):

- **`InstanceKey`** is the chain of *decoder keys* (track id, or the derived id of a
  transition's incoming side) rather than clip ids. A root layer's key is `[track_id]`,
  identical to the pre-v7 decoder key, so decoders keep running across consecutive clips of a
  track (cuts do not reopen ffmpeg) and nothing changes for projects without sequences. Keys
  are unique per simultaneously active layer, which is all the design needs.
- **Evaluation API.** `active_layers` is replaced by `active_steps` (root) and
  `sequence_steps` (inside a sequence layer); `leaf_media_layers` flattens every media layer
  across nesting (prefetch, the preview's element manager). `compose::build_scene` +
  `SceneHost` is the shared host layer (adjustment steps, sequence recursion, sequence masks);
  export, `renderly-wasm` and the golden-test preview stand-in are thin `SceneHost`s.
- **Compositor.** Accumulation targets are treated as premultiplied alpha (the root is opaque,
  where premultiplied == straight); a nested sequence is converted to straight alpha by one
  extra pass before it is drawn as a layer, and adjustment layers copy the accumulation to
  straight alpha for their effect chain (an exact copy where alpha is 1). Intermediate
  targets are pooled per size; the effect ping-pong targets are now cached per size so a
  sequence at a different resolution does not reallocate them every layer. A transition
  inside a sequence still blends over opaque black (a transition covers the layers beneath it
  on the root timeline too), so its window is not transparent.
- **Adjustment mask** of kind `none` means "no restriction" (on a layer it hides everything).
- **`SetSequenceSettings`** writes both fields as given (`null` = inherit); there is no
  "leave unchanged" because both fields are `Option`.
- **`CreateSequence`** also accepts an optional caller-supplied `id` (like `AddTrack`) so a
  batch/script can reference the new sequence; `AddTrack` into a sequence rejects `caption`
  tracks (`UnsupportedForSequence`) — caption rendering is root-only in v7.
- **`NestClips`** places the replacement clip on the highest source track of the right kind
  that has room, else on a new track at the top of the timeline. Moved clips keep their ids;
  the inner tracks are new tracks mirroring the source tracks (kind, name, role, flags). The
  command is atomic. **`UnnestClip`** copies back the clips of the placing clip's own class
  (video/adjustment or audio) onto new tracks just above the clip's track with fresh ids and
  removes the placing clip.
- **Extra rejections.** `SetClipAudioDenoise` on a clip placing a sequence is rejected like
  the other per-clip analysis commands (the inner clips carry their own denoise);
  `TrimClip` on an adjustment layer has its own error (`TrimAdjustment`).
- **Audio.** Per-container ducking is implemented with one intermediate ducked bus, rendered
  only for a sequence that itself holds both voice and music roles (the single-filter-graph
  property holds otherwise); the bus lives in a temp dir cleaned up on drop. Outer fades
  apply only at the outer clip's edges (to the piece that touches them).
- **Loading.** `Project::from_json` (parse, version-range check, `validate`, stamp the current
  version) is what the CLI, MCP and the desktop app use. The app also skips thumbnail /
  waveform generation for sequence media.
- **Preview.** `WasmCompositor::active_media_layers(t)` tells the JS engine which elements
  (by instance key) the frame needs, with source time and playback rate measured from the
  same Rust evaluation; `webviewPreviewEngine.ts` only manages `<video>` element state for
  them. The Canvas2D fallback does not render sequences or adjustment layers.

## Implementation notes (app, phase B)

- **One container selector.** `lib/container.ts` owns the rule "the timeline is the open
  container": `activeTracks(project, activeSequenceId)` (root fallback for an unknown id),
  `containerView(project, id)` (the project with `tracks` swapped, cached per project/id so it
  is referentially stable - used by the geometry/render/snap helpers that are written against
  `project.tracks`), `replaceActiveTracks` / `updateClip` (write an optimistic drag preview back
  into the right container), plus breadcrumb/path and adjustment-trim helpers. Audit decision per
  `project.tracks` use: timeline, track headers, scrollbars, snapping, drag/drop, clipboard,
  selection pruning, inspector and left-panel lookups, preview overlays, transport/duration and
  the preview engine's element management read the **active container**;
  `ensureStarterTracks` (a fresh project) and the project inspector's counters stay on the
  **root**. Export, `Project::timeline_duration` and the bridge are unchanged (root).
- **State.** `activeSequenceId` + `sequencePath` (the opened chain; a sequence can be placed in
  many parents, so the path cannot be derived from the project). `openSequence(id)` appends,
  `openSequence(id, { fromRoot: true })` (media panel) starts a fresh path,
  `closeSequence()` / `navigateToSequence(id | null)` walk back. Switching container stops
  playback, clears selection and rewinds the playhead/scroll. A store subscription keeps
  `sequencePath` valid whenever the project changes (undo, delete, external edit), so no code
  path can leave the editor in a container that no longer exists.
- **Preview.** `Project::with_sequence_as_root(id)` (core) returns the project with the
  sequence's tracks as its root timeline. `WasmCompositor::set_active_sequence(id?)` keeps the
  host's project and renders that view, so `render`, `active_media_layers` (instance keys,
  rates) and the matte/LUT sync all follow the open sequence with no change to the shared
  evaluation code; `webviewPreviewEngine.ts` reads the same container through `containerView`
  to decide which `<video>` elements to seek. `with_sequence_as_root` sets the view's
  `settings.width`/`height` to the sequence's effective size, so the compositor output is the
  sequence's own size while it is open and the preview canvas letterboxes to its aspect
  (`activeContainerSize` in `lib/container.ts` drives `PreviewPanel` and the handles/mask
  overlays). Nested sequences that inherit the size are pinned to the project size in the view,
  so they still resolve against the project. Closing the sequence restores the project
  size. A sequence placed inside a parent is still cover-fitted into that parent like any
  media. Captions are root-only, so none render inside a sequence.
- **Native playback / scrub audio.** The `set_active_sequence` Tauri command records the open
  sequence; `AppState::playback_project()` hands playback, `refresh_frame`, scrub audio and the
  bridge `play` the same `with_sequence_as_root` view, so audio for an open sequence plays that
  sequence's mix (including its nested sequences and per-container ducking). Playback produces
  no frames (the webview renders video), so the view's size has no effect here.
- **Nest / unnest / adjustment from the UI** go through `NestClips`, `UnnestClip`,
  `AddAdjustmentLayer` (default 5 s at the playhead on the selected, else topmost, video track;
  a container with no video track gets one created in the same batch), `SetAdjustmentLayer`
  (edge drags) - nothing mutates project state outside the existing optimistic drag patterns.
- **Harness.** `dev/tauriMock.ts` implements the v7 commands (including `AddTrack.sequence_id`,
  errors for overlap / `SequenceInUse` / `CannotUnnest` / cycles, `SplitClip`, and a
  snapshot-based undo/redo), and the sample project contains the "Intro" sequence (placed on the
  "Nested" track) and an adjustment layer on the "Adjustments" track.
