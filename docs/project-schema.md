# Project schema — v7

Status: **current**. This is the source of truth for `renderly-core`'s project
model. Implementation types in `renderly-core/src/project/` must match this document; if
they diverge, fix whichever one is wrong and note it in the same PR. Schema changes bump
`schema_version` and are documented in the "Version history" section at the bottom.

The project file is a single JSON document, human-readable, git-diffable, on disk with
extension `.renderly.json`.

## Top-level shape

```jsonc
{
  "schema_version": 7,
  "id": "b3f1c2a0-...-uuid",
  "name": "ultra-bruno-ep12",
  "settings": { "fps": 60.0, "width": 1080, "height": 1920, "sample_rate": 48000, "duck_db": -12.0 },
  "media": [ /* MediaItem[] */ ],
  "tracks": [ /* Track[] — the root timeline */ ],
  "sequences": [ /* Sequence[] — nested timelines, see "Sequences" */ ],
  "asset_pack_paths": [],
  "wasm_plugin_paths": [],
  "multicam_groups": [],
  "segmentation_model_path": null
}
```

| Field | Type | Notes |
|---|---|---|
| `schema_version` | u32 | `7` for this spec. Loaders accept `1`..=`7`; saves write `7`. |
| `tracks` | Track[] | The root timeline (z-order: first = bottom). |
| `sequences` | Sequence[] | Nested timelines (schema v7). Defaults to `[]`; always written. |
| `multicam_groups` | MulticamGroup[] | Optional sync groups (Phase 4). |
| `segmentation_model_path` | path? | Optional local model marker for BG removal. |

### Loading and validation

Every loader (`renderly-cli`, `renderly-mcp`, the desktop app) goes through
`Project::from_json`: it parses the file, rejects a `schema_version` outside `1..=7`, runs
`Project::validate`, and stamps the current version so the next save writes `7`. v1–v6 files
need no migration: every v7 field has a serde default. `validate` checks what serde cannot:

- track ids are unique across the root timeline **and** every sequence;
- sequence ids are unique, and **exactly one** `kind: "sequence"` media item exists per
  sequence (`sequence_id` set iff the kind is `"sequence"`, and it resolves);
- no sequence contains itself, directly or transitively, and nesting is at most
  `MAX_SEQUENCE_DEPTH = 8` deep.

A file that fails loads with an error naming the offending id instead of rendering wrongly.

## Clips

`Clip` is an internally tagged enum (`"type"`):

| `type` | Struct | Track kind |
|---|---|---|
| `video` | `MediaClip` | video |
| `audio` | `MediaClip` | audio |
| `caption` | `CaptionClip` | caption |
| `adjustment` | `AdjustmentClip` (schema v7) | video |

## Sequences (schema v7)

A **sequence** is a nested timeline that can be placed on another timeline as one clip
("compound clip"). The root timeline stays in `project.tracks`; each sequence has its own
`tracks`.

```jsonc
{
  "id": "uuid",
  "name": "Intro",
  "width": null,     // u32? — null inherits settings.width
  "height": null,    // u32? — null inherits settings.height
  "tracks": [ /* Track[] — same shape as the root timeline */ ]
}
```

- A sequence renders at the project's `fps` and `sample_rate` (no per-sequence values), at
  its own resolution (`width`/`height`, each inheriting when `null`), over a **transparent**
  background — nesting a picture-in-picture or lower-third keeps its alpha.
- **Track ids are globally unique** across the root timeline and all sequences, so every
  command that takes a `track_id` addresses a track in a sequence with no extra parameter
  (`Project::find_track` searches all of them; `Project::container_of_track` says which
  timeline owns one).
- Caption tracks are root-only in v7 (burned-in captions are not rendered inside
  sequences).
- The tracks of a sequence follow all the ordinary rules (video tracks composite bottom to
  top, one clip at a time per track, adjustment layers, transitions, masks, effects).

### A sequence is placed through a `MediaItem`

```jsonc
{ "id": "uuid", "kind": "sequence", "sequence_id": "uuid", "path": "",
  "duration_secs": null, "width": null, "height": null, "fps": null }
```

`MediaKind` gains `sequence`; `MediaItem` gains `sequence_id: Id?` (present iff
`kind == "sequence"`, omitted otherwise). `path` is empty and `duration_secs`, `width`,
`height`, `fps` are stored as `null` and **derived on demand** (`Project::media_duration`,
`Project::media_size`, `Project::sequence_duration`) so they can never go stale when the
inner timeline is edited:

- duration = end of the last **enabled** video/audio/adjustment clip in the sequence;
- size = the sequence's own `width`/`height` (inheriting the project's).

Exactly one media item exists per sequence (`CreateSequence` makes both, `DeleteSequence`
removes both).

A nested sequence on a timeline is an ordinary `MediaClip` whose `media_id` is that item:

- on a **video track** a `Clip::Video` shows the sequence's composited picture;
- on an **audio track** a `Clip::Audio` plays the sequence's mixed audio.

`source_in_secs`/`source_out_secs` are in the sequence's own time, so trim, split, speed (and
speed keyframes), transform, keyframes, effects, masks and transitions all work on a nested
clip exactly as on a file-backed one. Past the end of the inner timeline the sequence renders
transparent.

### Evaluation and rendering

Nested sequences are evaluated by `compose::eval` (cross-target, shared by native export and
the wasm preview): `active_steps(project, t)` yields the root's `ComposeStep`s; a layer whose
source is `ActiveSource::Sequence { sequence_id, time_secs, instance }` is expanded with
`sequence_steps` and composited into an offscreen target at the sequence's resolution (over
transparent), which then goes through the ordinary layer path (cover-fit, transform,
effects, mask, transition, opacity). Nesting is capped at 8 levels (an `EvalError`, not a
stack overflow). Each live layer has an `InstanceKey` — the chain of decoder keys from the
root to the layer — so the same media needed at two different times in one frame (a sequence
placed twice) gets independent decoders / `<video>` elements.

`timeline_duration(project)` and `Export` stay root-only.

### Audio

A nested `Clip::Audio` expands into the sequence's own audio clips, mapped through the outer
clip's position, trim, speed (including speed keyframes), gain and fades, and clipped to its
span; the mix is one ffmpeg filter graph over leaf media. Per-clip settings of the inner clips
(effects, RNNoise/`afftdn` denoise, gain, fades) are preserved. Ducking roles are resolved
per container: if a sequence holds both voice and music tracks its audio is first mixed with
ducking into one intermediate bus (the only case that renders an intermediate), and the
expanded clips inherit the **outer** track's role on the parent. Known limits: an outer fade
longer than the first/last expanded piece is applied to that piece only; combined speed beyond
`0.25..4` (outer x inner) is clamped by the tempo filter.

## AdjustmentClip (schema v7)

A video-track clip that carries effects and applies them to everything composited beneath it
for its duration. It is a separate `Clip` variant (not a `MediaClip` with no media) because
almost nothing on `MediaClip` applies: no source range, speed, gain, fades, transform,
transition, background removal, or multicam.

```jsonc
{
  "type": "adjustment",
  "id": "uuid",
  "position_secs": 4.0,
  "duration_secs": 6.0,
  "enabled": true,
  "opacity": 1.0,        // 0..1 — mix between unadjusted and adjusted picture
  "effects": [ /* EffectInstance[] — same shape as MediaClip.effects */ ],
  "mask": null,          // ClipMask? — limits where the adjustment applies (omitted when unset)
  "keyframes": []        // KeyframeTrack[] — `opacity` only
}
```

- It follows the track's normal no-overlap rule (it shares tracks with media clips; at most
  one clip is active per track at a time). Its position in the track order is its position in
  the stack: it adjusts every lower video track and nothing above it.
- Compositing: finish the steps beneath into the accumulation target, run the effect chain on
  it, then write `mix(unadjusted, adjusted, opacity × mask)` back and continue. The
  accumulation includes the background, so an adjustment layer over empty timeline adjusts
  black. An adjustment layer at the bottom of the stack is legal.
- `mask` UVs are **output-frame** coordinates (0..1 across the canvas); `raster`/`generated`
  mattes are stretched to the canvas. A mask of kind `none` means "no restriction" here.
- `opacity` keyframes (`AnimProperty::Opacity`, times relative to the layer's start) override
  the static `opacity`.
- Effects: builtins and pack `.cube` LUTs only. **WASM frame-plugin effects are not supported**
  on adjustment layers (they run on CPU pixels of a decoded source frame; an adjustment
  layer's input exists only on the GPU) and `SetClipEffects` rejects them.
- Blend modes on adjustment layers are not supported (opacity only).

## MediaClip (Phase 4 fields)

| Field | Notes |
|---|---|
| `mask` | Optional `ClipMask` — shape / raster / generated matte; invert + feather.
  Shape UVs are source-frame (0..1). Rect/ellipse authoring is via the app Mask tool
  overlay + `SetClipMask` (live drag patches the Zustand store directly for the preview
  canvas's own redraw — no backend round trip; see docs/preview-webview.md P4). |
| `background_removal` | Optional config (`model_id` `heuristic` or CLI-backed `rvm`/`birefnet`, threshold, feather, `matte_cache_dir`). Without a baked matte it is not applied to a nested sequence's output (it stays per inner clip). |
| `audio_denoise` | Optional `{ enabled, backend, strength }` — **audio tracks only** in v1. `backend` is `"afftdn"` (FFmpeg filter; the default when the field is absent) or `"rnnoise"` (RNNoise via `nnnoiseless`; `strength` = wet/dry mix). Added without a schema bump. Not settable on a clip that places a sequence (the inner clips carry their own). |
| `multicam_group_id` | Optional link into `project.multicam_groups`. |

### Builtin effects (additions)

| `effect_id` | Params |
|---|---|
| `builtin:chroma_key` | `key_r`, `key_g`, `key_b` (key colour, 0..1, default green), `tolerance` (0..1, default 0.3: RGB distance at or below which a pixel is fully transparent), `softness` (0..1, default 0.1: width of the linear alpha ramp above `tolerance`), `spill` (optional, 0..1, default 0 = off: strips the key colour's chroma from RGB; absent == 0 so older projects render unchanged) |

### Effect / matte order

source decode → pack LUT → WASM frame → **chroma key (GPU WGSL effect pass, in effect-chain order)** → **background-removal matte** → **clip mask** → GPU builtins (color/blur/lut/glitch) → composite blend.

For a nested sequence the same chain runs on the sequence's composited picture (minus the
CPU-side steps, which have no decoded frame): GPU builtins → clip mask → composite blend.

## Limits accepted in v7

- Per-sequence frame rate / sample rate; blend modes on adjustment layers.
- WASM frame-plugin effects on adjustment layers (rejected by `SetClipEffects`).
- Background removal without a baked matte on a nested sequence's output.
- Transitions inside a sequence blend over opaque black (as on the root timeline, a
  transition covers the layers beneath it), so the transition window is not transparent.
- Commands that analyse decoded frames (`TrackMotion`, `StabilizeClip`, `AutoReframeClip`,
  `GenerateBackgroundMatte`), `GenerateCaptions` and `SetClipAudioDenoise` reject sequence
  media (`UnsupportedForSequence`).
- The Canvas2D preview fallback ignores sequences and adjustment layers (the WebGPU preview
  renders them with the same Rust code as export).

## Version history

- **v5**: Speed keyframes; stickers/SFX; audio WASM.
- **v6** (Phase 4): `ClipMask`, background removal, audio denoise, multicam groups; chroma key builtin.
- **v7**: nested sequences (`Project.sequences`, `MediaKind::Sequence`,
  `MediaItem.sequence_id`) and adjustment layers (`Clip::Adjustment`). Purely additive:
  v1–v6 files load unchanged. Design and rationale: [schema-v7-design.md](schema-v7-design.md).
