# Project schema — v8

Status: **current**. This is the source of truth for `renderly-core`'s project
model. Implementation types in `renderly-core/src/project/` must match this document; if
they diverge, fix whichever one is wrong and note it in the same PR. Schema changes bump
`schema_version` and are documented in the "Version history" section at the bottom.

The project file is a single JSON document, human-readable, git-diffable, on disk with
extension `.renderly.json`.

## Top-level shape

```jsonc
{
  "schema_version": 8,
  "id": "b3f1c2a0-...-uuid",
  "name": "ultra-bruno-ep12",
  "settings": { "fps": { "num": 60, "den": 1 }, "width": 1080, "height": 1920, "sample_rate": 48000, "duck_db": -12.0 },
  "media": [ /* MediaItem[] */ ],
  "tracks": [ /* Track[] — the root timeline */ ],
  "sequences": [ /* Sequence[] — nested timelines, see "Sequences" */ ],
  "markers": [ /* Marker[] — see "Markers" */ ],
  "transcripts": [ /* Transcript[] — see "Transcripts" */ ],
  "legacy_pack_paths": [],
  "multicam_groups": [],
  "segmentation_model_path": null
}
```

| Field | Type | Notes |
|---|---|---|
| `schema_version` | u32 | `8` for this spec. Loaders accept `1`..=`8` (older files are migrated on load); saves write `8`. |
| `tracks` | Track[] | The root timeline (z-order: first = bottom). |
| `sequences` | Sequence[] | Nested timelines (schema v7). Defaults to `[]`; always written. |
| `markers` | Marker[] | Named points on the root timeline (schema v8). Omitted when empty. See "Markers". |
| `transcripts` | Transcript[] | Word-level transcripts of media items, in source media time (schema v8). Omitted when empty. See "Transcripts". |
| `legacy_pack_paths` | path[] | Transitional (v8): pack and plugin folders a v7 project listed; hosts install the pack folders into the user library and clear them. Omitted when empty. See "Packs". |
| `multicam_groups` | MulticamGroup[] | Optional sync groups (Phase 4). |
| `segmentation_model_path` | path? | Optional local model marker for BG removal. |

### Loading and validation

Every loader (`renderly-cli`, `renderly-mcp`, the desktop app) goes through
`Project::from_json`: it parses the file, rejects a `schema_version` outside `1..=8`, migrates older files
(see "Migration and backups" below), runs `Project::validate`, and stamps the current version
so the next save writes `8`. `validate` checks what serde cannot:

- track ids are unique across the root timeline **and** every sequence;
- sequence ids are unique, and **exactly one** `kind: "sequence"` media item exists per
  sequence (`sequence_id` set iff the kind is `"sequence"`, and it resolves);
- no sequence contains itself, directly or transitively, and nesting is at most
  `MAX_SEQUENCE_DEPTH = 8` deep;
- transcript ids, the word ids inside one transcript, and marker ids are unique.

A file that fails loads with an error naming the offending id instead of rendering wrongly.

## Time

Schema v8 stores every time as an **integer count of microseconds** (`i64`), in fields named
`*_us`. There are no float seconds anywhere in a project file, so a time always means the same
value however many times the file is loaded, edited and saved.

| Field | Where |
|---|---|
| `start_us`, `source_in_us`, `source_out_us`, `fade_in_us`, `fade_out_us` | `MediaClip` |
| `start_us`, `duration_us` | `CaptionClip`, `TextClip`, `AdjustmentClip` |
| `start_us`, `end_us` | `CaptionWord` (relative to the clip start), `TranscriptWord` (source media time) |
| `time_us` | `Marker` |
| `duration_us` | `TextAnimationRef` (optional override) |
| `duration_us` | `ClipTransition`, `MediaItem` (probed duration; `null` = unknown) |
| `time_us`, `handle_left` / `handle_right` first element (µs) | `Keyframe` |
| `start_us` | `background_removal.matte_frames` |
| `sync_offsets_us` | `MulticamGroup` |

Not times: `MediaItem.fps` (probed rate, display only, float) and `ProjectSummary.fps` in tool
results stay `f64`; clip timing always uses the project rate.

**Frame rate** is a rational, `settings.fps = { "num": 30000, "den": 1001 }` (also
`matte_frames.fps`). A plain number is still accepted when reading (hand-written files, old
tools) and snapped to the nearest standard rational within 0.01 (`23.976` becomes `24000/1001`);
saves always write the rational.

**Frames are computed, not stored.** Frame `n` of rate `num/den` starts at
`floor(n * 1_000_000 * den / num)` µs (`Fps::frame_start`, exact integer maths). At 30 fps frame 1
is 33 333 µs, at 29.97 fps frame 100 is 3 336 666 µs. Commands that position by time snap to this
function (see [command-api.md](command-api.md) "Time"), so the same frame is always the same
integer.

**Command input** accepts the `Time` grammar (integer µs, `"1.5s"`, `"1500ms"`, `"90f"`,
`"00:00:01:15"`); it is resolved to integer µs before it touches the project. The same functions
are exported to the web UI through `renderly-wasm`, so the GUI and the engine agree.

## Migration and backups

`Project::from_json` runs a chain of pure steps on the parsed JSON (`project/migrate.rs`), one
per schema version that changed stored data. Files at 1..=6 need no data change of their own
(serde defaults cover the fields they lack), so v1..v7 all go through the single v7 -> v8 step:

- **time** (`v7->v8:time`): `position_secs` -> `start_us`, `source_in_secs`/`source_out_secs` ->
  `source_in_us`/`source_out_us`, `duration_secs` -> `duration_us`, `fade_*_secs` -> `fade_*_us`,
  keyframe `time_secs` -> `time_us` (handle time offsets too), `start_secs` -> `start_us` on matte
  frames, `sync_offsets_secs` -> `sync_offsets_us`, `settings.fps` (and matte `fps`) -> rational.
  Values are rounded to the nearest µs. Frame-positioned values (clip start, source in/out, caption
  and adjustment spans, transition durations) are additionally **snapped to the nearest frame
  boundary when within 1 ms**, which removes accumulated float drift; keyframe times, handles,
  fades and probed media durations are only rounded.
- **effects** (`v7->v8:effects`): `effect_id` becomes a versioned `ref` (`builtin:blur` ->
  `builtin:blur@1`, `pack:<pack>:lut:<lut>` -> `pack:local.<pack>/<lut>@1`, or
  `pack:<publisher.pack>/<lut>@1` when the pack id already has a publisher; `wasm:<plugin>` is
  unchanged), and the list is split into the slots **without changing what runs when**. v7 ran
  the list in order and then the mask; v8 runs filter, key, adjust, `effects[]`, then the mask.
  So every effect goes to `effects[]` in its original order, except that a pack LUT in **first**
  position becomes the `filter`, and a chroma key that is first (or second, right after that
  LUT) becomes the `key` (on adjustment layers, which have no key slot, it stays in `effects[]`).
  Both moves keep the order, so no project changes. Instance ids, `enabled` and params are kept.
- **transitions** (`v7->v8:transitions`): `kind: "wipe_left"` -> `ref: "builtin:wipe_left@1"`;
  `params: {}` and `easing: null` are added.
- **easing** (`v7->v8:easing`): `ease_in` and `ease_out` keys become **exact** `bezier` keys: ease-in
  (t²) is the cubic Bezier with control points (1/3, 0) and (2/3, 1/3), ease-out (1 − (1 − t)²) is
  (1/3, 2/3) and (2/3, 1), written as handles scaled to each span in µs. `ease_in_out` is not a
  single cubic, so it stays as the legacy `ease_in_out` variant; a last key has no span after it,
  so its easing becomes `linear`. Migrated tracks evaluate to the same values as v7.
- **packs** (`v7->v8:packs`): `asset_pack_paths` and `wasm_plugin_paths` move, in order and
  without duplicates, to the transitional `legacy_pack_paths`.
- **captions** (`v7->v8:captions`): `text` becomes `body: { text, runs: [], words: [] }` and
  `style_id` becomes a style ref `builtin:<id>@1` (`params` and `overrides` empty). When **every**
  clip on a caption track had the same `style_id` it moves to the track's `caption_style` and the
  clips get `style: null` (inherit); otherwise each clip keeps its own `style`. A `style_id` that
  is not a valid item id (`[a-z0-9_-]+`) is lower-cased with other characters turned into `-`; a
  clip without one gets `tiktok-bold-yellow`, the style v7 drew. Every clip gets `animations: {}`;
  every track gets `magnetic: false` (no surprise ripples in old projects); the project gets empty
  `markers` and `transcripts`. The pictures are unchanged: captions still render with the four
  builtin styles (see "Text, captions and transcripts").

The migration happens in memory. Hosts that save (`renderly-cli`, `renderly-mcp`, the desktop
app) go through `project::backup::save_project_file`:

1. when the file on disk has a `schema_version` older than the current one, its original bytes are
   first copied to `<name>.v<version>.renderly.json` next to it (once; never overwritten);
2. every save also copies the file it replaces into a rolling history,
   `<project dir>/.renderly-backups/<name>/<yyyymmdd-HHMMSS>.json` (UTC), keeping the newest 20
   plus the newest one per UTC day for the last 7 days;
3. the new contents are written atomically (temp file, then rename).

A file with a `schema_version` newer than the build is refused, not guessed at. Fixtures for every
version live in `renderly-core/tests/fixtures/projects/` (`v7_effects.renderly.json` covers the
slot-split cases, LUTs, chroma key, a WASM effect, all ten transitions and eased keyframes;
`v7_captions.renderly.json` covers shared, mixed and empty caption tracks) and
`renderly-core/tests/migration.rs` pins the expected output. The existing golden frames are the
acceptance test for "order preserved exactly": they pass unchanged on the migrated shapes.

Installing a v7 project's pack folders is a **host** step, not part of the pure migration:
`library::adopt_legacy_paths` (used by the app, the CLI and the MCP server on open) installs each
folder with `Library::install` and clears the list.

## Clips

`Clip` is an internally tagged enum (`"type"`):

| `type` | Struct | Track kind |
|---|---|---|
| `video` | `MediaClip` | video |
| `audio` | `MediaClip` | audio |
| `caption` | `CaptionClip` | caption |
| `text` | `TextClip` (schema v8) | video |
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
  "duration_us": null, "width": null, "height": null, "fps": null }
```

`MediaKind` gains `sequence`; `MediaItem` gains `sequence_id: Id?` (present iff
`kind == "sequence"`, omitted otherwise). `path` is empty and `duration_us`, `width`,
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

`source_in_us`/`source_out_us` are in the sequence's own time, so trim, split, speed (and
speed keyframes), transform, keyframes, effects, masks and transitions all work on a nested
clip exactly as on a file-backed one. Past the end of the inner timeline the sequence renders
transparent.

### Evaluation and rendering

Nested sequences are evaluated by `compose::eval` (cross-target, shared by native export and
the wasm preview): `active_steps(project, t)` yields the root's `ComposeStep`s; a layer whose
source is `ActiveSource::Sequence { sequence_id, time_us, instance }` is expanded with
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
  "start_us": 4000000,
  "duration_us": 6000000,
  "enabled": true,
  "opacity": 1.0,        // 0..1 — mix between unadjusted and adjusted picture
  "filter": null,        // EffectInstance? — slots as on MediaClip, but no `key`
  "adjust": null,
  "effects": [ /* EffectInstance[] — same shape as MediaClip.effects */ ],
  "blend_mode": "normal", // omitted when normal
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
- Effects: builtins and pack items, in the `filter` / `adjust` slots and `effects[]` (see
  "Effects, slots and blend modes"). **WASM frame-plugin effects are not supported**
  on adjustment layers (they run on CPU pixels of a decoded source frame; an adjustment
  layer's input exists only on the GPU) and `SetClipEffects` rejects them.
- `blend_mode` blends the adjusted picture over the unadjusted one before the opacity/mask mix.
- Keyframes: `opacity`, and effect parameters of its own instances (`effect:<id>/<param>`).

## Effects, slots and blend modes (schema v8)

Video `MediaClip`s and `AdjustmentClip`s carry effects in **fixed slots**; audio clips keep a
plain `effects` list (WASM audio plugins only). The slots always run in this order, whatever
the order of the fields in the file:

```
decode → WASM frame plugins (CPU) → filter → key → adjust → effects[] (list order)
       → background-removal matte or clip mask → transform + composite (blend_mode, opacity)
```

```jsonc
// on MediaClip (video) and AdjustmentClip
"filter":  { "id": "uuid", "ref": "pack:local.starter/punch@1", "hash": "sha256:…", "enabled": true,
             "params": { "intensity": 0.8 } },
"key":     { "id": "uuid", "ref": "builtin:chroma_key@1", "enabled": true, "params": { "tolerance": 0.4 } },
"adjust":  { "id": "uuid", "ref": "builtin:color_adjust@1", "enabled": true, "params": { "exposure": 0.2 } },
"effects": [ { "id": "uuid", "ref": "builtin:blur@1", "enabled": true, "params": { "radius": 8 } } ],
"blend_mode": "screen"
```

- `filter`, `key`, `adjust` are optional and omitted when empty; `effects` is always written.
  `key` does not exist on `AdjustmentClip` (nor does the matte). `blend_mode` is omitted when
  `normal`.
- `filter` takes filters and LUTs; `key` the chroma key (or an item whose category is `key`);
  `adjust` the colour adjustment. `effects[]` takes any single-input effect, filter or LUT (a
  second LUT stacked on the first goes there) and `wasm:<plugin>` frame/audio plugins, which run
  on the CPU before the GPU chain wherever they sit in the list and are not supported on
  adjustment layers.
- The matte or clip mask is applied **after** all the slots (so a blur stays inside the mask
  instead of smearing its edge); a background-removal matte replaces the clip mask.

### `EffectInstance`

| Field | Type | Notes |
|---|---|---|
| `id` | Id | Stable. Keyframe tracks (`effect:<id>/<param>`) and hover preview address it. |
| `ref` | string | `builtin:<id>@<v>`, `pack:<pack-id>/<item>@<v>` (effects, filters and LUTs alike), or `wasm:<plugin>`. |
| `hash` | string? | Content hash of the pack item when it was applied (pack effects only). |
| `enabled` | bool | Default `true`. A disabled instance is skipped. |
| `params` | map | **Control values**, typed per the effect's declaration: a number (also `int` and `choice` option indexes), a bool, `[x, y]`, `[r, g, b, a]` (0..1, straight alpha), or a LUT ref string. A key that is absent takes the effect's `default` (the identity). |

Refs name the pack id and the item version, not the pack release: any installed release of the
pack that holds that item version satisfies it. Pack ids are `<publisher>.<name>`; packs that came
from v7 projects are installed as `local.<id>`.

**Status** (`ok`, `changed`, `missing`, `disabled`) is computed on load and never stored:

| Status | Meaning | Rendering |
|---|---|---|
| `ok` | Resolved; the hash matches (or none was stored). | Normal. |
| `changed` | Resolved, but the installed item's hash differs from the stored one. | Renders with the installed files. |
| `missing` | No installed pack holds the ref. | **Pass-through**; the instance keeps its params. |
| `disabled` | Installed, but this build cannot run it (a newer engine feature, a failed load). | Pass-through. |

`get_project` (MCP) lists the instances and transitions that are not `ok` under `effect_status`
(`clip_id`, `instance_id`, `ref`, `status`); the in-app outline marks them. Nothing is dropped when
an item is missing, so installing it later restores the look.

### Blend modes

`blend_mode` on video clips and adjustment layers: `normal` (default, omitted), `multiply`,
`screen`, `overlay`, `darken`, `lighten`, `color_dodge`, `color_burn`, `soft_light`,
`hard_light`, `difference`, `exclusion`, `add`. They are the W3C compositing formulas, applied
in the composite shader on the premultiplied accumulation (so a half-transparent or masked layer
blends only where it has coverage); `add` is the plus operator. On an adjustment layer the
adjusted picture is blended over the unadjusted one before the opacity/mask mix. The two sides of
a transition are always drawn `normal`.

## Transitions (schema v8)

```jsonc
"outgoing_transition": { "ref": "builtin:crossfade@1", "hash": null, "duration_us": 500000,
                         "params": {}, "easing": null }
```

`ref` is a transition item (`builtin:<kind>@1` for the ten builtins: `crossfade`, `fade_black`,
`wipe_left`, `wipe_right`, `wipe_up`, `wipe_down`, `slide_left`, `slide_right`, `iris`,
`blur_dissolve`; or a pack transition; a format-1 pack's transition alias runs the builtin it
names). `params` are the item's control values. `easing` is `null` (the item's declared default;
all builtins are linear), `"linear"`, or `{"bezier": [x1, y1, x2, y2]}` (CSS control points over
normalised progress; the presets `ease_in`, `ease_out`, `ease_in_out` are accepted when reading
and stored as Bezier points). The window, overlap and the half-the-clip duration rule are unchanged
from v7. A missing or disabled transition shows the outgoing clip until the cut.

## Keyframes and easing (schema v8)

```jsonc
"keyframes": [
  { "property": "opacity", "keys": [ /* … */ ] },
  { "property": "effect:<instance-id>/intensity", "keys": [ { "time_us": 0, "value": 0.2, "easing": "linear" } ] },
  { "property": "effect:<instance-id>/tint", "keys": [ { "time_us": 0, "value": [1, 0, 0, 1], "easing": "linear" } ] }
]
```

- `property` is a string: an `AnimProperty` name (`pos_x`, `pos_y`, `scale_x`, `scale_y`,
  `rotation`, `opacity`, `volume`, `speed`, `fit_x`, `fit_y`) or `effect:<instance-id>/<param>`
  addressing a parameter of an instance in the same clip's slots. An unknown property is a load
  error.
- `value` is a number, or an array for `color` / `vec2` parameters (interpolated per component).
  `bool`, `choice` and LUT parameters, and parameters declared `animatable: false`, cannot be
  keyframed (`SetClipKeyframes` rejects a track on one). `int` parameters interpolate and are
  rounded where they are used.
- Deleting an effect instance deletes its tracks. Splitting a clip gives the right half fresh
  instance ids and its tracks follow them.
- Times are clip-relative `time_us`. Before the first key and after the last key the end values
  hold. At a key's own time the track has that key's value.

`easing` describes the span **after** the key:

| Value | Meaning |
|---|---|
| `linear` | Straight line (the default). |
| `hold` | Keep this key's value until the next key. |
| `bezier` | Cubic Bezier through this key's `handle_right` and the next key's `handle_left` (`[time_offset_us, value_offset]` relative to the key). x is solved for t like CSS `cubic-bezier`; control x values are clamped to the span, so time never runs backwards. For array values the value offset is a fraction of the span's change. Absent handles default to a flat third of the span. |
| `auto` | Smooth monotone cubic (Fritsch-Carlson) with tangents derived from the neighbouring keys; it never overshoots the keys and a flat run stays flat. No handles are stored. |
| `ease_in_out` | Legacy only: the v7 piecewise quadratic, kept so migrated projects render identically. New edits write `bezier` handles instead. |

The "Ease in / out / in-out" presets in the UI and in `SetKeyframeEasing` write `bezier` handles
(CSS values: ease-in `(0.42, 0, 1, 1)`, ease-out `(0, 0, 0.58, 1)`, ease-in-out
`(0.42, 0, 0.58, 1)`, scaled to the span), so a preset is a starting point that can then be
dragged. Inserting a key inside a Bezier span splits the curve (de Casteljau) so the motion does
not change.

## Tracks (schema v8 fields)

```jsonc
{ "id": "uuid", "kind": "video", "name": "V1", "clips": [ … ],
  "audio_role": null, "muted": false, "locked": false, "hidden": false,
  "magnetic": true,                 // omitted when false
  "caption_style": null }           // caption tracks only; omitted when null
```

- **`magnetic`** (video and audio tracks; never caption tracks). On a magnetic track the clips are
  contiguous from time 0 and their starts are frame-snapped. The commands that change a track's
  clips keep that true themselves (ripple), so the GUI, the CLI and an agent get the same result,
  and the whole edit is one command and one undo step: `AddClip`, `MoveClip`, `DeleteClip` (always
  closes the gap), `TrimClip`, `SetClipSpeed`, `SplitClip`, `AddText`, `SetTextTiming`,
  `NestClips` / `UnnestClip`, `ApplyTranscriptCuts`, and `SetTrackFlags { magnetic: true }`
  (packs the track once). A clip given a `start` on a magnetic track lands **where its centre falls
  among the clips already there** (after every clip whose centre is at or before its own), so a
  start past the end appends and a start in the middle inserts and pushes the rest. Defaults:
  `true` on the first video track of a new project (the first video track `AddTrack` creates on
  the root timeline), `false` otherwise, and `false` on every track migrated from v7. Overlap
  checks do not apply to a magnetic track (nothing can overlap).
- **`caption_style`**: the `TextStyleRef` that captions without their own `style` inherit, so
  restyling a whole track is one edit. Without either, captions draw with `tiktok-bold-yellow`.

## Text, captions and transcripts (schema v8)

Design: [text-format.md](text-format.md), [schema-v8-design.md](schema-v8-design.md) section 4. The
project carries the **full** v8 shape for text now; the shaping and SDF renderer, the animation
runtime and the `text_style` / `text_animation` / `font` pack kinds come later and need no further
schema change.

> **Rendering status.** Captions and text clips are drawn by the existing caption renderer
> (`captions::render_caption_styled`, the four builtin styles `tiktok-bold-yellow`,
> `tiktok-minimal`, `tiktok-box`, `youtube-lower-thirds`, or an installed format-1 pack caption
> style of that id), so their pictures are the same as in v7. Of a style's `overrides` it reads
> `fill`, `size` and `anchor`; `params`, `runs`, `words` timing and `animations` are **stored,
> validated and round-tripped; rendered from the captions phase**.

### `TextBody`

```jsonc
{ "text": "Let's build something great",
  "runs":  [ { "start": 6, "end": 11, "style": { "fill": "#FFD400FF" } } ],
  "words": [ { "text": "build", "start_us": 340000, "end_us": 690000, "emphasis": true, "word_id": "w-102" } ] }
```

- `runs` override style properties for a range of **grapheme clusters** (end-exclusive); a run
  needs `start < end` and `end` not past the text. (Today the bound is the code point count, the
  upper bound of the grapheme count; the exact check arrives with the shaper.) A run's `style`
  uses the same property names as a style's `overrides`.
- `words` are speech-recognition evidence: times are **relative to the clip start**, in order,
  not overlapping, inside the clip. Editing `text` never touches them. `emphasis` marks key words
  (default `false`, omitted when false). `word_id` (optional, an extension of the design) is the id
  of the transcript word the timing came from.
- `words` may be empty (typed captions).

### `TextStyleRef` and animations

```jsonc
{ "ref": "builtin:tiktok-box@1", "hash": null, "params": {}, "overrides": { "fill": "#FFFFFF" } }
```

- `ref` is `builtin:<id>@<v>` or `pack:<pack>/<item>@<v>`. An unknown ref is **stored**; it shows
  as `missing` in `effect_status` and the caption falls back to the track's style, then the
  default. The four v7 styles are `builtin:<id>@1`.
- `params` are the style's knobs. A builtin caption style declares none, so they must be empty;
  other styles' params are checked when their kind lands. `overrides` keys are one of `font`,
  `size`, `fill`, `strokes`, `shadows`, `background`, `letter_spacing`, `line_height`, `align`,
  `case`, `max_width`, `emphasis`, `anchor`, `margin`; `fill` is `#RRGGBB[AA]`, `size` and the
  spacing/width/margin values are numbers >= 0, `anchor` is `top`, `center` or `bottom`.
- `animations` maps a phase (`in`, `out`, `loop`, `active`) to
  `{ "ref", "hash"?, "params", "duration_us"? }`; at most one per phase, `{}` when none. Stored
  and validated (ref syntax, phase name, positive duration); rendered from the captions phase.

### `CaptionClip`

```jsonc
{ "type": "caption", "id": "uuid", "start_us": 12000000, "duration_us": 2400000,
  "body": { … }, "transcript_ref": { "transcript_id": "uuid", "first_word": "w-102", "last_word": "w-106" },
  "style": null,            // null = inherit the track's caption_style
  "animations": {} }
```

`transcript_ref` (omitted when absent) records which transcript words the caption was built from.

### `TextClip` (video tracks)

```jsonc
{ "type": "text", "id": "uuid", "start_us": 0, "duration_us": 3000000, "enabled": true,
  "body": { "text": "EPISODE 12", "runs": [], "words": [] },
  "style": { "ref": "builtin:tiktok-minimal@1", "params": {}, "overrides": { "anchor": "center" } },
  "animations": {},
  "transform": { … }, "keyframes": [],
  "filter": null, "adjust": null, "effects": [],        // slots: no `key`
  "blend_mode": "normal", "mask": null, "outgoing_transition": null }
```

A title is a **layer** like a video clip: static transform and keyframes (position, scale,
rotation, opacity and effect parameters; not volume, speed or fit), the `filter` / `adjust` /
`effects[]` slots (no `key`, no WASM plugins), `blend_mode`, `mask`, and a transition into the next
clip. It is evaluated by `compose::eval` as an `ActiveSource::Text`; its picture is the caption
renderer's output at the output size, composited through the same path as every other layer. A new
text clip's style is `builtin:tiktok-minimal@1` with `anchor: center`; move it with the transform.
Text clips live on video tracks, take part in magnetic ripple, and can be nested.

### Transcripts

```jsonc
"transcripts": [
  { "id": "uuid", "media_id": "uuid", "language": "en", "model": "ggml-base.en",
    "words": [ { "id": "w-1", "text": "So", "start_us": 1200000, "end_us": 1380000,
                 "deleted": false, "tags": ["filler"] } ] } ]
```

Times are in **source media time**, so one transcript serves every clip cut from that file. There
is one transcript per media item; `GenerateCaptions` creates or replaces it. `tags` are detection
tags (`filler`, `pause`, `repeat`). Setting `deleted` and cutting is `ApplyTranscriptCuts`:
the deleted ranges become **real cuts** with ripple on every clip of that media; the transcript
stays the record, and `RestoreTranscriptWords` clears the flag and puts the media back.

### Markers

```jsonc
"markers": [ { "id": "uuid", "time_us": 4200000, "label": "drop", "color": "#E53935", "kind": "user" } ]
```

`kind` is `user` or `beat` (written by beat detection). `color` is `#RRGGBB`, empty for the
default. Markers belong to the root timeline and are kept sorted by time. Snapping includes them:
`Project::snap_points()` returns the root timeline's clip edges and the markers, sorted, which is
what the UI snaps to.

## Packs (schema v8)

A v8 project stores **refs only**: items are found by ref in the user library
(`library::Library`, see [asset-pack.md](asset-pack.md)); the project does not store pack paths. The file may
carry the transitional `legacy_pack_paths: []` (omitted when empty), which holds what a v7
project listed in `asset_pack_paths` and `wasm_plugin_paths`. Hosts (the desktop app, the CLI,
the MCP server) install each pack folder in it into the library as a `local.<id>` pack when they
open the project and clear the list; folders of WASM plugins (which are not library items yet),
missing folders and packs that fail to install stay listed and are reported. `LoadWasmPlugin`
records a plugin folder here too. `Project::from_json` itself never touches the library.

## MediaClip (Phase 4 fields)

| Field | Notes |
|---|---|
| `mask` | Optional `ClipMask` — shape / raster / generated matte; invert + feather.
  Shape UVs are source-frame (0..1). Rect/ellipse authoring is via the app Mask tool
  overlay + `SetClipMask` (live drag patches the Zustand store directly for the preview
  canvas's own redraw — no backend round trip; see docs/preview-webview.md P4). |
| `background_removal` | Optional config: `model_id` (`heuristic` built-in; `rvm` = Robust Video Matting in-process, needs the downloaded weights; `cli` / `birefnet` = external binary via `RENDERLY_SEG_CLI`), `threshold` (heuristic only), `feather`, `temporal` (superseded for `rvm`, which is inherently temporal), `matte_cache_dir`, and optional `matte_frames`. Without a baked matte it is not applied to a nested sequence's output (it stays per inner clip). See "Background removal mattes" below. |
| `audio_denoise` | Optional `{ enabled, backend, strength }` — **audio tracks only** in v1. `backend` is `"afftdn"` (FFmpeg filter; the default when the field is absent) or `"rnnoise"` (RNNoise via `nnnoiseless`; `strength` = wet/dry mix). Added without a schema bump. Not settable on a clip that places a sequence (the inner clips carry their own). |
| `multicam_group_id` | Optional link into `project.multicam_groups`. |

### Background removal mattes

`background_removal.matte_cache_dir` names the baked matte. Two layouts live there:

- **single image** (`heuristic`, `cli`, `birefnet`): `matte.png`, applied to every frame of the
  clip (as before);
- **per-frame sequence** (`rvm`): `f000000.png`, `f000001.png`, ... (8-bit grayscale PNG,
  255 = subject), a representative `matte.png` (the middle frame, so single-image consumers keep
  working), and `matte.json` (provenance: model id + weights SHA-256, source path / mtime /
  size, covered source range, fps, frame count, size, stamp).

`background_removal.matte_frames` (optional, omitted when absent) is the sequence index the
engine uses without touching the filesystem: `{ start_us, fps, count, width, height, stamp }` (`fps` rational, like the project rate).
Matte frame `i` belongs to **source** time `start_us + i / fps`; a layer showing source time
`s` uses frame `round((s - start_us) * fps)` clamped to `0..count-1` (nearest). `stamp` (ms
since epoch) changes on every regeneration so hosts can invalidate cached frames. The matte then
goes through the usual `Generated` mask path (`feather`, invert) before the clip mask stage.
If two layers visible in the same output frame share one `matte_cache_dir` at different source
times (e.g. both sides of a transition between two pieces of one split clip), they share the
last-registered frame; this is an accepted edge case.

### Builtin effects (additions)

| `ref` | Params |
|---|---|
| `builtin:chroma_key@1` | `key_r`, `key_g`, `key_b` (key colour, 0..1, default green), `tolerance` (0..1, default 0.3: RGB distance at or below which a pixel is fully transparent), `softness` (0..1, default 0.1: width of the linear alpha ramp above `tolerance`), `spill` (optional, 0..1, default 0 = off: strips the key colour's chroma from RGB; absent == 0 so older projects render unchanged) |

### Effect / matte order

source decode → WASM frame plugins (CPU) → **filter** → **key** (chroma key) → **adjust** →
**`effects[]`** (list order, GPU WGSL passes through the one effect runtime, builtins and pack items
alike) → **background-removal matte** or **clip mask** → composite (blend mode, opacity).

For a nested sequence the same chain runs on the sequence's composited picture (minus the
CPU-side steps, which have no decoded frame): slots → clip mask → composite blend.

## Cover-fit position (`ClipTransform.fit_x` / `fit_y`)

The compositor cover-fits every media layer to the canvas: it crops the source to the canvas
aspect and only then applies the clip transform. `fit_x` and `fit_y` choose **which part of the
source** that cover window shows, on the axis the source overflows:

- `fit_x`, `fit_y` in `-1..=1` (`f64`, both `#[serde(default)]` = `0`, clamped on write and on
  evaluation). `0` is the centred window (the old behaviour), `-1` puts the window flush with
  the left (top) edge of the source, `+1` flush with the right (bottom) edge.
- The window start is `offset = (1 - scale) * (1 + fit) / 2` in source UV, where `scale` is the
  cover scale of that axis. An axis with no slack (`scale == 1`) ignores its fit.
- Keyframable as `AnimProperty::FitX` / `FitY` (`"fit_x"` / `"fit_y"`), evaluated by
  `evaluate_transform` like the other transform properties.
- Masks are unaffected: clip masks and mattes are in **source-frame** UVs and are applied to the
  source before the cover draw, so they stay attached to the picture as the window moves.

## Limits accepted in v7/v8

- Per-sequence frame rate / sample rate.
- A blend mode on a layer that is one side of a transition is ignored for the transition window.
- `bool`, `choice` and LUT effect parameters are not animatable.
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

- **v8**: integer-microsecond time (`*_us` fields replace every `*_secs`), rational `fps`,
  a stepwise migration chain and automatic backups (see "Time" and "Migration and backups");
  effects as versioned refs in fixed slots (`filter`, `key`, `adjust`, `effects[]`) with typed
  params, `blend_mode`, keyframes on effect parameters, `hold` / `bezier` / `auto` easing,
  transitions as refs with params and easing, and pack folders replaced by refs resolved from the
  user library (`legacy_pack_paths` while migrating); captions as `body` / `style` / `animations`,
  `TextClip`, `Track.magnetic` and `caption_style`, project `markers` and `transcripts`. Breaking
  for readers of the raw JSON; files v1..v7 migrate on load. Design: the schema v8 design notes.

- **v7, additive (cover-fit pan)**: `ClipTransform.fit_x` / `fit_y` (serde-defaulted `0`) and
  `AnimProperty::FitX` / `FitY`, choosing where the cover-fit window sits inside the source (see
  "Cover-fit position" above). **No version bump**: v7 has not been released, the fields are
  optional, and a file without them loads and renders exactly as before.

- **v7, additive (AI background removal)**: optional `MediaClip.background_removal.matte_frames`
  (`{ start_secs, fps, count, width, height, stamp }` at the time, `start_us` in v8, serde-defaulted, omitted when absent) and
  `model_id: "rvm"` now meaning in-process Robust Video Matting. **No version bump**: v7 has
  not been released, and the field is purely additive - every existing v1-v7 file loads
  unchanged and re-saves without the field.

- **v5**: Speed keyframes; stickers/SFX; audio WASM.
- **v6** (Phase 4): `ClipMask`, background removal, audio denoise, multicam groups; chroma key builtin.
- **v7**: nested sequences (`Project.sequences`, `MediaKind::Sequence`,
  `MediaItem.sequence_id`) and adjustment layers (`Clip::Adjustment`). Purely additive:
  v1–v6 files load unchanged. Design and rationale: [schema-v7-design.md](schema-v7-design.md).
