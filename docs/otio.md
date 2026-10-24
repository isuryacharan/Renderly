# OpenTimelineIO interop

Status: **implemented** (`renderly-core/src/otio/`). Renderly reads and writes the native
[OpenTimelineIO](https://opentimelineio.readthedocs.io/) JSON format (`.otio`) in pure Rust — no
OTIO C++/Python library, no new dependencies (`serde_json` only).

- **Export** — a Renderly project becomes an OTIO `Timeline`. Everything OTIO cannot express
  is stored under `metadata["renderly"]`, so Renderly → OTIO → Renderly is **lossless**.
- **Import** — any OTIO file (ours, or from Premiere/Resolve/Hiero/… exports) becomes a **new**
  project. Things without a Renderly equivalent become [warnings](#warnings), never errors.

Importing is a *load-path* operation ("create a project from a file"), exactly like opening a
`.renderly.json`: it is not a `Command` and never mutates an open project. Exporting is a pure
read of the project, like `Export` (see [command-api.md](command-api.md)).

| Surface | Export | Import |
|---|---|---|
| Rust | `renderly_core::otio::export_otio(&project)` / `export_otio_with_options` | `renderly_core::otio::import_otio(json, &ImportOptions)` → `(Project, Vec<OtioWarning>)`; optional `probe_media(&mut project, base_dir)` |
| CLI | `renderly export-otio <project> <out.otio>` | `renderly import-otio <in.otio> <project-out> [--width W --height H --probe]` |
| MCP | `export_otio { output_path }` | `import_otio { input_path, output_path, width?, height?, probe? }` (the new project becomes the open one) |
| App | top bar **Export timeline (OTIO)** (`project.export-otio`) | top bar **Import timeline (OTIO)** (`project.import-otio`); creates a project in the projects folder and opens it |

## Schemas written and read

Written (current versions, verified against the
[serialized-schema reference](https://opentimelineio.readthedocs.io/en/latest/tutorials/otio-serialized-schema.html)
and by loading the output with the real library — see [Validation](#validation)):
`Timeline.1`, `Stack.1`, `Track.1`, `Clip.2` (`media_references` + `active_media_reference_key`),
`Gap.1`, `Transition.1`, `Effect.1`, `LinearTimeWarp.1`, `ExternalReference.1`,
`GeneratorReference.1`, `Marker.2`, `RationalTime.1`, `TimeRange.1`.

Read: the above plus `Clip.1` (single `media_reference`), `Sequence.*` (old name of `Track`),
`Filler.*` (old name of `Gap`), `SerializableCollection.1` (the first `Timeline` inside), a bare
`Stack`/`Track`/`Clip` as the document root, `MissingReference`, `ImageSequenceReference`,
`FreezeFrame`, `Marker.*` (root-stack markers become project markers), bare `NaN`/`Infinity`
doubles. Schema *versions*
are not checked: `Clip.9` is read as a `Clip` and unknown fields are ignored. An unknown schema
in a track is a gap plus a warning. Project markers are written as `Marker.2` on the root stack
(see [markers](#markers-and-transcripts)).

## Mapping: Renderly → OTIO

| Renderly | OTIO |
|---|---|
| project | `Timeline` — `name`, `global_start_time` = 0 at the project fps |
| root timeline (`tracks`) | the timeline's `Stack` (`name: "tracks"`). Track order is bottom-first in both (verified against *Timeline Structure*: "iterated from the bottom (the first entry in the stack) towards the top") |
| video / audio track | `Track` with `kind` `Video` / `Audio`; `enabled` = not hidden (video) / not muted (audio) |
| caption track | `Track` of kind `Video` tagged `metadata.renderly.track.kind = "caption"` — see [captions](#captions) |
| text clip (title) | `Clip.2` with a `GeneratorReference` of kind `renderly.text`, effects as `Effect.1` — see [captions](#captions) |
| project markers | `Marker.2` on the root stack — see [markers](#markers-and-transcripts) |
| clip | `Clip.2` with `source_range` (frames at project fps) and one `ExternalReference` (`target_url`, plus `available_range` when the media duration is known) |
| gap between clips | `Gap` (whole frames, never zero) |
| `enabled: false` | `enabled: false` on the clip |
| constant `speed` ≠ 1 | `LinearTimeWarp` effect, `time_scalar` = speed |
| speed keyframes | `LinearTimeWarp` with the *average* scalar (source span ÷ timeline span); keyframes in metadata |
| `outgoing_transition` | `Transition` between the two touching clips — see [transitions](#transitions) |
| effects (`builtin:*`, `pack:*`) | `Effect.1` in render order (filter, key, adjust, `effects[]`); `effect_name` = the ref (`builtin:blur@1`); `id`, `ref`, `hash`, `slot` and `params` in `metadata.renderly` |
| nested sequence clip | a `Stack` **in place** (name = sequence name) holding the sequence's tracks; `source_range` is the used part of the sequence |
| adjustment layer | `Gap` carrying its effects as `Effect.1`; the whole layer in `metadata.renderly.adjustment` |
| everything else | `metadata["renderly"]`, see [the metadata layout](#metadata-layout) |

### Time and rounding

OTIO times are `value / rate`; Renderly times are `f64` seconds.

- **Out:** every time is snapped to the nearest **frame at the project fps**: `floor(secs × fps + 0.5 + 1e-9)`
  (round half up; the epsilon makes `x.5` ties and float noise round the same way every time).
  A track is laid out in whole frames from the snapped clip *ends*, so clips that touch in the
  project still touch in OTIO; gaps are the differences. A clip is at least one frame long. A
  clip's `source_range.duration` is its **timeline** duration (OTIO: effects do not change an
  item's duration; the media consumed is `duration × time_scalar`).
  `available_range.duration` is rounded **up**, so a snapped source range can never end past
  the media it points into. Rates are written as given (`29.97002997…` stays fractional).
- **In:** each value is converted with **its own** `rate` (`value / rate`) — a 15 fps source range
  in a 30 fps timeline is read as 15 fps, not assumed to be the timeline rate. Positions come from
  summing item durations (a gap and a clip authored at different rates add correctly). Results are
  rounded to 1e-12 s only to remove float noise; they are **not** snapped to the project grid.
  `source_range.start_time` includes the reference's `available_range.start_time` (timecode
  offset), which is subtracted to get a file-relative source time.
- **Stable:** OTIO is the authority and `metadata.renderly` keeps the exact `f64` times. On import
  a stored time is used only while it still rounds to the OTIO value (within the snapping error),
  so un-edited round trips are bit-exact — even for off-grid times like `12.3456` — and a time
  changed in another tool always wins. Export → import → export is a fixed point.

### Transitions

Renderly's transition window is `[cut − d, cut)` — it ends at the cut (`compose/eval.rs`). OTIO
transitions straddle a cut: `in_offset` = how much of the *previous* item it overlaps,
`out_offset` = how much of the *next*. Renderly writes `in_offset = d`, `out_offset = 0`: the
overlap is entirely in the outgoing clip, matching the window, and (as OTIO requires) the
timeline length is unchanged.

| `ref` | `transition_type` |
|---|---|
| `builtin:crossfade@1` | `SMPTE_Dissolve` |
| every other ref (`builtin:fade_black@1`, `wipe_*`, `slide_*`, `iris`, `blur_dissolve`, pack transitions) | `Custom_Transition`, the transition's `name` is the ref and `metadata.renderly.transition` holds the full `ClipTransition` (params, easing) |

A transition is written only between two *touching* clips on a video track; one that Renderly
keeps on a clip followed by a gap survives in the clip's metadata only.

On import the duration is `in_offset + out_offset` (a centred 10+10-frame dissolve becomes a
20-frame crossfade ending at the cut — the centring is lost), shortened to half the shorter
adjacent clip (Renderly's rule) with a warning. `SMPTE_Dissolve` and any type containing
"dissolve" → `builtin:crossfade@1`; a `Custom_Transition` whose name is a transition ref keeps it even without the metadata; other foreign types → `builtin:crossfade@1` with a warning. A transition with no
clip on one side, or next to a gap, is dropped with a warning.

### Captions

OTIO has no caption primitive. Options considered: `Marker`s on the stack (loses duration/overlap
structure and cannot be positioned per track), or clips with a `MissingReference` (other tools
show them as offline media). Renderly writes each caption as a **`Clip.2` whose media reference is
a `GeneratorReference`** — OTIO's own notion of synthesised media such as titles — with
`generator_kind: "renderly.caption"` and `parameters: { text, style_ref }` (`style_ref` is the
clip's own style ref or `null`; schema 7 wrote `style_id`, still read), `name` = the text and
the exact clip — `body` with its runs and word timings, style, animations, `transcript_ref` — in
`metadata.renderly.caption`. Other tools therefore see a correctly timed, named
title clip on a video track, and Renderly restores a caption track (the track is tagged in its
metadata, which also carries `magnetic` and the track's `caption_style`; without the tag, a track
holding only such generators is still read as captions).
Foreign generators (`SMPTEBars`, `Slug`, …) cannot be rendered and become gaps with a warning.

A **text clip** (a title on a video track) is the same shape with `generator_kind:
"renderly.text"`, `parameters: { text, style_ref }`, the slot effects as `Effect.1`, `enabled`,
a native `Transition` to the next clip when they touch, and the exact clip (transform, keyframes,
blend mode, mask, style, animations) in `metadata.renderly.text`. Without that metadata a
`renderly.text` generator is read as a text clip with the default style.

### Markers and transcripts

Each project marker is written as a `Marker.2` on the root `Stack`: `name` = the label,
`marked_range` = a zero-length range at the marker's frame, `color` = the nearest OTIO colour
(user markers without a colour are `RED`, beat markers `CYAN`), and the exact marker (id, time in
µs, colour, kind) in `metadata.renderly.marker`. On import a marker with that metadata comes back
exactly while it still sits on the frame it was written at; a foreign marker becomes a `user`
marker at the start of its range with its label and its OTIO colour as a hex colour. Markers on
tracks and clips are still dropped with a warning (Renderly's markers belong to the timeline).
Project transcripts travel in the timeline metadata (`transcripts`).

### Nested sequences and adjustment layers

A clip placing a sequence is written as a nested `Stack` in the track (OTIO's native nesting): the
sequence's tracks are its children, `source_range` is the part of the sequence used (in the
sequence's own time), and the clip's speed/effects/enabled flag are on the `Stack`. OTIO has no
instancing, so a sequence placed twice is written twice; on import the copies are recognised by
the sequence id in metadata and become **one** sequence again (the first copy's content wins).
A sequence placed nowhere (a media-bin item) travels whole in the timeline metadata
(`unplaced_sequences`). Without metadata, every nested `Stack` or `Track` becomes its own sequence;
tracks inside it follow the same rules (depth is capped at `MAX_SEQUENCE_DEPTH`).

An adjustment layer is a `Gap` (so other tools treat it as empty space of the right length) whose
`effects` list the layer's effects; it is restored from `metadata.renderly.adjustment`. Without that
metadata it is just a gap.

## Mapping: OTIO → Renderly

| OTIO | Renderly |
|---|---|
| `Timeline` / top-level `Stack` | project; the stack's children are the root tracks (in order) |
| `Track` (`kind` Video/Audio) | video / audio track (unknown kinds → video + warning); empty names become `V1`, `A1`, … |
| `Clip` with `ExternalReference` | video clip (video track) or audio clip (audio track) on a media item; same file on both kinds → two media items (Renderly media is video *or* audio) |
| `source_range` | source in/out (file-relative); position from the running sum of item durations |
| no `source_range` | the reference's whole `available_range` |
| `Gap` / `Filler` | space (positions only) |
| `LinearTimeWarp` (product of all enabled ones) | `speed`, clamped to `0.25..4` (warning if clamped) |
| `FreezeFrame`, `time_scalar ≤ 0` | speed 1 + warning |
| `Stack` / `Track` nested in a track | sequence (+ its media item) placed by a clip; `source_range` → the clip's source range |
| `Transition` | see [above](#transitions) |
| `enabled: false` | clip `enabled: false`; a disabled track → hidden (video) / muted (audio) |
| `Track.source_range` | the track's content is trimmed to / shifted by that window. A window that misses all the content (some exporters write a record timecode there) is ignored with a warning rather than emptying the track |
| `MissingReference`, `ImageSequenceReference`, non-`file:` URLs, unsupported generators, unknown references | **gap** (same length, so everything after keeps its position) + warning |
| `Effect` (not ours) | dropped + warning |
| `Marker` on the root stack | project marker (see [markers](#markers-and-transcripts)) |
| `Marker` on a track or clip | dropped + one warning per object |
| `GeneratorReference` of kind `renderly.caption` / `renderly.text` | caption / text clip |
| `global_start_time` ≠ 0 | ignored + warning (the timeline starts at 0) |
| unknown fields, unknown `metadata` | ignored |
| bare `NaN` / `Infinity` doubles | read as missing values |

### Project frame rate

`Settings.fps` is the most frequently used rate among all `source_range`s and transition offsets
in the tracks (ties: the `global_start_time` rate if tied, else the higher rate). More than one
rate in the file produces a `rate` warning listing the counts; every time is still converted with
its own rate. A file written by Renderly keeps its own fps while any range still uses it. The
`fps` option overrides everything.

### Project dimensions, audio and the rest of the settings

OTIO carries no frame size. Precedence: explicit `width`/`height`/`sample_rate` option >
`metadata.renderly.settings` > 1920 × 1080 @ 48000 Hz. `duck_db` comes from metadata (default −12).

### Media

`target_url` → path ([URLs](#file-urls)). Relative URLs resolve against `base_dir` (the `.otio`
file's directory). Missing files are fine: the media item is recorded with the duration from the
reference's `available_range` (or `null`) and no size. The pure import never probes; pass
`--probe` / `probe: true` (or call `probe_media`) to fill duration, size and fps from files that
exist. Media is de-duplicated by (URL, kind). A clip that points at a still image
(`png`, `jpg`, …) on a video track becomes image media; an audio file on a video track, or an
image on an audio track, cannot be placed and becomes a gap + warning. Ids restored from
metadata are bound to a media item only while the clip still points at the same file — a clip
relinked in another tool gets a new media item and keeps the rest of its data.

### File URLs

Pure string functions (`otio::url`), identical on every OS:

| path | URL |
|---|---|
| `D:\media\my clip.mp4` | `file:///D:/media/my%20clip.mp4` |
| `/home/u/clip.mp4` | `file:///home/u/clip.mp4` |
| `\\server\share\clip.mp4` | `file://server/share/clip.mp4` |
| `media/clip.mp4` (relative) | `media/clip.mp4` |

Everything except unreserved characters and `/` is percent-encoded (UTF-8 bytes, upper-case hex).
Reading also accepts `file://localhost/D%3a/x.mov` (Premiere), `file:///D|/x`, `file://D:/x`,
lower-case `%3a`, `file:/x`, and raw unencoded paths. Relative paths are written absolute when
`base_dir` is given (CLI/MCP/app pass the project file's directory).

## What is lossless, approximated, dropped

**Lossless (Renderly → OTIO → Renderly, compared as JSON):** all of the project, including: ids of
everything, names, settings (fps, size, sample rate, `duck_db`), media items (incl. relative
paths, unused ones, and their order), tracks (order, flags, audio roles), clips (exact positions
and source ranges even off the frame grid, speed incl. keyframes, transforms, keyframes, effects
and their instance ids, masks, background removal, audio gain/fades/denoise, multicam ids,
transitions of every kind), captions (body with runs and word timings, style, animations,
transcript links), text clips, markers, transcripts, magnetic flags, adjustment layers (opacity, effects, mask,
keyframes), sequences (placed once, many times, or never, with sizes), multicam groups, asset
pack / plugin paths. Pinned by `renderly-core/tests/otio_roundtrip.rs`.

**Approximated** (for other OTIO tools, or when metadata is gone): times are frame-snapped;
a keyframed speed is its average; transition geometry (end-aligned vs centred); captions are
generator clips; adjustment layers are gaps; effects are named `Effect`s whose meaning only
Renderly knows.

**Dropped with a warning on import:** media-less clips (→ gaps), markers, foreign effects,
`FreezeFrame`, unsupported time warps, non-file media, image sequences, foreign generators,
`global_start_time`, unknown schemas. Not representable at all: OTIO metadata other than
`metadata.renderly` is not round-tripped (it is ignored on import).

## Warnings

`OtioWarning { kind, path, message }`; `kind` is one of `unknown_schema`, `missing_media`,
`unsupported_effect`, `time_warp`, `speed_clamped`, `transition`, `markers_ignored`,
`track_trimmed`, `rate`, `other`. `path` locates the object (`tracks.children[1].children[4]`).
The CLI prints them to stderr as `warning: …`; MCP returns them in the `import_otio` JSON; the
app counts them in a toast and logs them to the console.

## Metadata layout

`metadata.renderly` is namespaced as OTIO recommends. It is an implementation detail of Renderly
(`RENDERLY_METADATA_FORMAT = 1`) — other tools should preserve and otherwise ignore it.

| Object | Keys |
|---|---|
| `Timeline` | `format`, `schema_version`, `project_id`, `settings`, `media` (all items, in order), `legacy_pack_paths` (older exports: `asset_pack_paths`, `wasm_plugin_paths`, merged on import), `multicam_groups`, `segmentation_model_path`, `unplaced_sequences`, `transcripts` |
| `Track` | `track`: `{ id, kind, audio_role, muted, locked, hidden, magnetic, caption_style }` |
| `Clip` / nested `Stack` | `clip`: the full `MediaClip`; nested `Stack` also `sequence`: `{ id, name, width, height }` |
| caption `Clip` | `caption`: the full `CaptionClip` |
| text `Clip` | `text`: the full `TextClip` |
| `Marker` | `marker`: the full `Marker` |
| adjustment `Gap` | `adjustment`: the full `AdjustmentClip` |
| `Transition` | `transition`: the `ClipTransition` (`ref`, `hash`, `duration_us`, `params`, `easing`) |
| `Effect` | `id`, `ref`, `hash`, `slot` (`filter` / `key` / `adjust` / `effects`), `params` |

## Validation

- Structure: `renderly-core/tests/otio_roundtrip.rs` checks every exported object has a known
  `OTIO_SCHEMA`, ranges are non-negative whole frames at the project rate, no two transitions are
  adjacent, gaps are never empty, and clip ranges stay inside known available ranges.
- Real samples: `renderly-core/tests/otio_import.rs` imports six unmodified files from the
  OpenTimelineIO repository (`tests/fixtures/otio/`, Apache-2.0, see its README) and asserts
  concrete positions, source ranges, transitions and nesting.
- The real library: `renderly-core/tests/otio_python.rs` (`#[ignore]`d; needs a Python with
  `opentimelineio`; `RENDERLY_OTIO_PYTHON` picks the interpreter) loads Renderly's export with
  `opentimelineio.adapters.read_from_file`, compares track kinds, child counts and durations,
  rewrites it through the library and checks Renderly → OTIO → library → Renderly is lossless, and
  imports a file authored by the library. Run with
  `cargo test -p renderly-core --test otio_python -- --ignored`.

## Limits

- OTIO metadata of other tools is not preserved through an import (Renderly has nowhere to put it).
- Transitions are end-aligned; a centred foreign transition is longer-before-the-cut than authored.
- `Track.source_range` and nested-stack `source_range` are honoured; `Stack.source_range` on the
  timeline's top-level stack is ignored.
- Audio tracks inside a nested stack placed on a *video* track are not played by that clip (OTIO
  semantics: the audio of a nested stack belongs to an audio-track copy of it).
