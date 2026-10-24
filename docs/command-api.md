# Command API

Status: **current** (matches project schema v8). This is the source of truth for the `Command` enum and
`apply_command` in `renderly-core`. GUI, CLI, and MCP must all dispatch through this exact
set (see AGENTS.md §0.1) — none of them may mutate `Project` state any other way.

## Time

Every time in a command is a `Time`: JSON accepts

| Form | Example | Meaning |
|---|---|---|
| integer | `1500000` | microseconds |
| `"<n>s"` | `"1.5s"` | seconds (decimal allowed) |
| `"<n>ms"` | `"1500ms"` | milliseconds |
| `"<n>f"` | `"90f"` | frame index at the project's frame rate |
| `"HH:MM:SS:FF"` | `"00:00:01:15"` | non-drop timecode at the project's frame rate |

Negative times are rejected. Floats are not accepted as bare numbers: write `"1.5s"`.
Stored times and every result are integer microseconds in `*_us` fields (see
[project-schema.md](project-schema.md) "Time").

**Frame snapping.** Commands that position, trim or split by time (`AddClip`, `SplitClip`,
`TrimClip`, `MoveClip`, `AddCaption`/`SetCaption` start and duration, `AddAdjustmentLayer`/
`SetAdjustmentLayer`, `AddStickerFromPack`, `AddSfxFromPack`, `GenerateVoiceover`,
`GenerateSticker`, `ApplyTemplate`) snap the resolved time to the nearest frame boundary of the
project rate (`Fps::frame_start`), so one frame is always one microsecond value whichever surface
sent it; values that are already frame starts are unchanged, and a positive duration never snaps
below one frame. Fades, `timeline_offset`, keyframe times and transition durations are not
snapped.

**Deprecated aliases.** The v7 parameter names that took float seconds are still accepted for one
release and are mapped to `Time` (`1.5` becomes `"1.5s"`): `position_secs`/`new_position_secs`
-> `start`, `at_secs` -> `at`, `source_in_secs`/`new_source_in_secs` -> `source_in`,
`source_out_secs`/`new_source_out_secs` -> `source_out`, `duration_secs` -> `duration`,
`fade_in_secs` -> `fade_in`, `fade_out_secs` -> `fade_out`, `timeline_offset_secs` ->
`timeline_offset`. Each use adds a deprecation line to the write result's `warnings`. If both the
old and the new name are sent, the new one wins.

**Deprecated v7 effect and transition shapes** (accepted for one release, with a `warnings` line
each; see the commands below):

- `SetClipEffects { effects: [{ effect_id, params }] }`, where `effect_id` is `builtin:<id>` or
  `pack:<pack>:lut:<lut>`, replaces the clip's **whole** stack, as it always did: it is converted
  to `ref`s (`builtin:blur@1`, `pack:local.<pack>/<lut>@1`), a LUT in first position goes to
  `filter`, `key` and `adjust` are cleared, and the rest is `effects[]` in the same order.
- `SetClipTransition { transition: { kind, duration_us } | null }` is `{ ref: "builtin:<kind>@1",
  duration }` / `{ ref: null }`.
- Keyframes with `easing: "ease_in" | "ease_out"` become the exact `bezier` handles of the same
  curve.
- `LoadAssetPack` / `UnloadAssetPack` install into / remove from the user library instead of
  recording a path on the project.

## Shape

```rust
pub enum Command {
    ImportMedia { path: String },
    AddTrack { kind: TrackKind, name: String, id: Option<Id>, sequence_id: Option<Id>, magnetic: Option<bool> },
    AddClip { track_id: Id, media_id: Id, start: Time, source_in: Time, source_out: Time },
    SplitClip { track_id: Id, clip_id: Id, at: Time },
    TrimClip { track_id: Id, clip_id: Id, source_in: Option<Time>, source_out: Option<Time> },
    MoveClip { track_id: Id, clip_id: Id, start: Time, new_track_id: Option<Id> },
    DeleteClip { track_id: Id, clip_id: Id, ripple: bool },
    AddCaption { track_id: Id, body: BodyInput, start: Time, duration: Time, style: Option<TextStyleRef>, animations: Option<TextAnimations> },
    SetCaption { track_id: Id, clip_id: Id, body: Option<BodyInput>, start: Option<Time>, duration: Option<Time>, style: Option<Option<TextStyleRef>> /* null = inherit the track */, animations: Option<TextAnimations> },
    AddText { track_id: Id, body: BodyInput, start: Time, duration: Time, style: Option<TextStyleRef>, animations: Option<TextAnimations> },
    SetTextTiming { track_id: Id, clip_id: Id, start: Option<Time>, duration: Option<Time> },
    SetTextBody { track_id: Option<Id>, clip_id: Option<Id>, targets: Vec<ClipRef>, text: Option<String>, runs: Option<Vec<TextRun>> },
    SetTextStyle { track_id: Option<Id>, clip_id: Option<Id>, targets: Vec<ClipRef>, ref: Option<Option<String>>, hash: Option<Option<String>>, params: Option<Map<String, Value>>, overrides: Option<Map<String, Value>> },
    SetTextAnimation { track_id: Option<Id>, clip_id: Option<Id>, targets: Vec<ClipRef>, phase: String /* in | out | loop | active */, ref: Option<String> /* required; null clears */, hash: Option<String>, params: Option<Map<String, Value>>, duration: Option<Time> },
    SetTrackCaptionStyle { track_id: Id, style: Option<TextStyleRef> /* required; null clears */, reset_clips: bool },
    SetCaptionWords { track_id: Id, clip_id: Id, words: Vec<WordInput> },
    SetWordEmphasis { track_id: Id, clip_id: Id, indices: Vec<usize>, emphasis: bool /* default true */ },
    SplitCaptions { track_id: Id, max_chars: usize /* 32 */, max_lines: usize /* 2 */, max_duration: Time /* 5s */ },
    AddMarkers { markers: Vec<MarkerInput> },
    RemoveMarkers { ids: Vec<Id>, kind: Option<MarkerKind> },
    ApplyTranscriptCuts { transcript_id: Id, word_ids: Vec<String>, tags: Vec<String> },
    RestoreTranscriptWords { transcript_id: Id, word_ids: Vec<String> },
    SetAudioGain { track_id: Id, clip_id: Id, gain_db: f64 },
    GenerateCaptions { media_id: Id, track_id: Id, style: Option<TextStyleRef>, timeline_offset: Time },
    GenerateVoiceover { text: String, track_id: Id, start: Time, output_path: String, provider: VoiceoverProvider },
    SetAudioFade { track_id: Id, clip_id: Id, fade_in: Time, fade_out: Time },
    SetTrackAudioRole { track_id: Id, role: Option<TrackAudioRole> },
    SetProjectSettings { width: Option<u32>, height: Option<u32>, fps: Option<Fps> },
    SetTrackFlags { track_id: Id, muted: Option<bool>, locked: Option<bool>, hidden: Option<bool>, magnetic: Option<bool> },
    RenameTrack { track_id: Id, name: String },
    DeleteTrack { track_id: Id },
    MoveTrack { track_id: Id, new_index: usize },
    SetClipEnabled { track_id: Id, clip_id: Id, enabled: bool },
    SetClipTransform { track_id: Option<Id>, clip_id: Option<Id>, targets: Vec<ClipRef>, transform: ClipTransform },
    SetClipKeyframes { track_id: Id, clip_id: Id, keyframes: Vec<KeyframeTrack> },
    SetKeyframeEasing { track_id: Id, clip_id: Id, property: TrackProperty, time: Time, easing: EasingChoice },
    SetClipEffects { track_id: Option<Id>, clip_id: Option<Id>, targets: Vec<ClipRef>, filter: Option<Option<EffectSpec>>, key: Option<Option<EffectSpec>>, adjust: Option<Option<EffectSpec>>, effects: Option<Vec<EffectSpec>>, blend_mode: Option<BlendMode> },
    SetClipTransition { track_id: Option<Id>, clip_id: Option<Id>, targets: Vec<ClipRef>, ref: Option<String> /* required; null clears */, duration: Option<Time>, params: Option<EffectParams>, easing: Option<Option<ProgressEasing>> },
    SetClipSpeed { track_id: Id, clip_id: Id, speed: f64 },
    LoadAssetPack { path: String },
    UnloadAssetPack { pack_id: String },
    LoadWasmPlugin { path: String },
    UnloadWasmPlugin { plugin_id: String },
    AddStickerFromPack { pack_id: String, sticker_id: String, track_id: Id, start: Time },
    AddSfxFromPack { pack_id: String, sfx_id: String, track_id: Id, start: Time },
    SetClipMask { track_id: Id, clip_id: Id, mask: Option<ClipMask> },
    SetClipBackgroundRemoval { track_id: Id, clip_id: Id, config: Option<BackgroundRemoval> },
    GenerateBackgroundMatte { track_id: Id, clip_id: Id },
    AttachBackgroundMatte { track_id: Id, clip_id: Id, cache_dir: PathBuf },
    SetClipAudioDenoise { track_id: Id, clip_id: Id, config: Option<AudioDenoise> },
    ApplyTemplate { pack_id: String, template_id: String, start: Time },
    GenerateSticker { prompt: String, track_id: Id, start: Time, output_path: String },
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
    AddAdjustmentLayer { track_id: Id, start: Time, duration: Time },
    SetAdjustmentLayer { track_id: Id, clip_id: Id, start: Option<Time>, duration: Option<Time>, opacity: Option<f64> },
    Export { output_path: String, preset: ExportPreset },
}

pub struct ClipRef { pub track_id: Id, pub clip_id: Id }

/// The text of a caption or title. `runs` / `words` omitted = empty on a new clip, unchanged on an existing one.
pub struct BodyInput { pub text: String, pub runs: Option<Vec<TextRun>>, pub words: Option<Vec<WordInput>> }

/// A recognised word; `start` / `end` are `Time`s relative to the clip start.
pub struct WordInput { pub text: String, pub start: Time, pub end: Time, pub emphasis: bool, pub word_id: Option<String> }

/// `{ "ref": "builtin:tiktok-box@1", "params": {}, "overrides": {} }`
pub struct TextStyleRef { pub ref: String /* JSON field name */, pub hash: Option<String>, pub params: Map<String, Value>, pub overrides: Map<String, Value> }

pub struct MarkerInput { pub id: Option<Id>, pub time: Time, pub label: String, pub color: String /* #RRGGBB */, pub kind: MarkerKind /* user | beat */ }

/// An effect to put in a slot. Only `ref` is required.
pub struct EffectSpec { pub id: Option<Id>, pub ref: String /* JSON field name */, pub hash: Option<String>, pub enabled: bool /* default true */, pub params: Option<EffectParams> }

/// "linear" | "hold" | "auto" | "ease_in" | "ease_out" | "ease_in_out" | { "bezier": [x1, y1, x2, y2] }
pub enum EasingChoice { /* … */ }

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

`magnetic` sets the track's magnetic flag (see "Magnetic tracks" below). Omitted, it is `true` for
the **first video track of the root timeline** (a new project's main track) and `false` for every
other track.

- Errors: caller-supplied `id` collides with an existing track id (in the root timeline or
  any sequence), `sequence_id` not found (`SequenceNotFound`), a `caption` track requested
  inside a sequence (`UnsupportedForSequence` — caption tracks are root-only in v7), or a
  magnetic caption track.

### `AddClip`
Adds a `VideoClip`/`AudioClip` (matching the target track's kind) referencing `media_id` to
`track_id`, placed at `start`, sourced from `[source_in, source_out)` of
the media. **Sequence media** (schema v7, `kind: "sequence"`) may go on a video track (its
picture) or an audio track (its mixed sound); the source range is in the sequence's own time
and is checked against its derived duration.

- Errors: `track_id`/`media_id` not found, track kind doesn't match media kind, resulting
  clip overlaps an existing clip on that track, `source_out <= source_in`, or the
  source range exceeds the media's probed duration (skipped if duration is unknown — see
  project-schema.md), or placing the sequence would make it contain itself, directly or
  through other sequences (`SequenceCycle`).

### `SplitClip`
Splits the clip at timeline position `at` into two clips with adjusted
`source_in`/`source_out`, both keeping the same `media_id`. New clip on the right
gets a fresh id; the left retains the original id. Media clips copy `transform`; keyframes
are split at the cut (left keeps `t < split`, right remaps `t' = t - split`); effects are
copied with new instance ids on the right clip. Adjustment layers split the same way
(`duration` divided at the cut, opacity keyframes split, effects re-id'd).

- Errors: `at` not strictly inside the clip's timeline span.

### `TrimClip`
Adjusts `source_in` and/or `source_out` in place (ripple is not implied — this
only changes which part of the source plays, not other clips' positions). At least one of
the two optional fields must be `Some`.

- Errors: `clip_id` is a caption clip (trim only applies to media clips) or an adjustment
  layer (`TrimAdjustment` — it has no source range; use `SetAdjustmentLayer`), resulting
  `source_out <= source_in`, new range exceeds media bounds, or the resulting
  on-timeline span (position unchanged, duration recomputed from the new range) overlaps a
  neighboring clip on the same track.

### `MoveClip`
Changes a clip's `start`, optionally moving it to a different track
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
Adds a `CaptionClip` with `body` at `start` for `duration`. `body` is `{ "text": ..., "runs"?, "words"? }`
(`words` use the `Time` grammar, relative to the clip start; they are stored as integer µs).
`style` is a `TextStyleRef`; omitted, the caption has no style of its own and **inherits the
caption track's `caption_style`** (or the default, `tiktok-bold-yellow`). `animations` maps
phases to animations; they are stored, not rendered yet. Target track must be `kind: "caption"`.

Deprecated aliases (accepted for one release, each use adds a warning): `text` for `body.text`,
`style_id: "tiktok-box"` for `style: { "ref": "builtin:tiktok-box@1" }`.

- Errors: track kind mismatch, `duration <= 0`, overlap with an existing caption clip on that
  track, an invalid body (a word outside the clip, words out of order, a run past the text), an
  invalid style (malformed `ref`, `params` on a builtin style, an unknown or ill-typed
  `overrides` key), an invalid animation (malformed `ref`, `duration_us <= 0`).

### `SetCaption` (Phase 2, body and style in v8)
Updates an existing `CaptionClip`. At least one of `body`, `start`, `duration`, `style`,
`animations` must be given; omitted fields are unchanged. `body.text` replaces the displayed
text; `body.runs` / `body.words`, when given, replace those lists, and **otherwise the words are
kept** (word timing is evidence from the audio, not a copy of the text). `style: null` makes the
caption inherit the track's style again. `text` and `style_id` are deprecated aliases as in
`AddCaption`. Used by the GUI caption inspector.

- Errors: track/clip not found, track kind mismatch, no fields to change, resulting
  `duration <= 0` or a word that no longer fits, resulting overlap with another caption on the
  same track, invalid style/animation.

### `AddText` (schema v8)
Adds a `TextClip` (a title) to a **video** track at `start` for `duration`. It is a layer like a
media clip: move it with `SetClipTransform`, animate it with `SetClipKeyframes`, put effects on it
with `SetClipEffects` (`filter`, `adjust`, `effects`; no `key`, no WASM plugins), set `blend_mode`,
`SetClipMask`, `SetClipTransition`, `SetClipEnabled`, `SplitClip`, `MoveClip`, `DeleteClip`,
`NestClips`. `style` omitted: `builtin:tiktok-minimal@1` with `anchor: "center"`. Body, style and
animations are validated as for captions. On a magnetic track the title is inserted like any clip.

- Errors: not a video track, `duration <= 0`, overlap on a non-magnetic track, invalid body, style
  or animation.

### `SetTextTiming` (schema v8)
Moves and/or resizes a text clip (`start`, `duration`; at least one).

- Errors: clip is not a text clip, `duration <= 0`, overlap on a non-magnetic track, a word of the
  body that no longer fits.

### `SetTextBody`, `SetTextStyle`, `SetTextAnimation` (schema v8)
Edit the text of **caption and text clips** alike. Each takes `track_id` + `clip_id`, or `targets`
to apply one change to many clips of either kind in one undo step; all targets are checked first,
so a batch applies to every clip or to none.

- `SetTextBody { text?, runs? }`: change the displayed text and/or its style runs (at least one).
  Words are never touched.
- `SetTextStyle { ref?, hash?, params?, overrides? }`: omitted = unchanged. `ref` switches the
  style (its `params` / `overrides` are kept unless given); `ref: null` clears it (a caption
  inherits its track again, a text clip returns to the default style). `params` and `overrides`
  **replace** the stored maps. At least one field is required.
- `SetTextAnimation { phase, ref, hash?, params?, duration? }`: `phase` is `in`, `out`, `loop` or
  `active`; `ref` is required and `null` clears the phase; re-sending the same `ref` keeps the
  stored `hash`, `params` and `duration` that are not given. Animations are stored and validated
  now and rendered from the captions phase.

- Errors: a target is not a caption or text clip (`InvalidText`), invalid body/style/animation.

### `SetTrackCaptionStyle` (schema v8)
Sets (or, with `style: null`, clears) the `caption_style` of a caption track: the style every
caption without its own `style` inherits. `reset_clips: true` also clears the clips' own styles, so
the whole track follows the new style.

- Errors: not a caption track, invalid style.

### `SetCaptionWords`, `SetWordEmphasis` (schema v8)
- `SetCaptionWords { words }` replaces a caption's word timings (imports, manual timing). Words
  must be in order, not overlap and lie inside the clip; the text is not changed.
- `SetWordEmphasis { indices, emphasis }` marks (default) or unmarks the words at those positions
  of `body.words` as key words.

### `SplitCaptions` (schema v8)
Rebuilds the word-timed captions of a caption track as **pages**. The words of every caption that
has them are laid on the timeline in order and cut into pages of at most `max_lines` lines of
`max_chars` characters (greedy line fill; a word longer than a line sits alone), no page longer
than `max_duration` from its first word's start to its last word's end; a page always holds at
least one word. A page's clip runs from its first word's start to its last word's end (snapped to
frames, never overlapping the previous page), its `text` is the lines joined with `\n`, its words
are stored relative to the page start, and it takes the `style` and `animations` of the caption
its first word came from. A page whose words all carry `word_id`s from one transcript gets a
`transcript_ref`. Captions without words (typed text) are left as they are. Deterministic: the
same words and limits give the same pages, so running it twice changes nothing.

- Errors: not a caption track, `max_chars` or `max_lines` is 0, `max_duration <= 0`, a typed
  caption would overlap a page (`Overlap`).

### `AddMarkers`, `RemoveMarkers` (schema v8)
`AddMarkers { markers: [{ id?, time, label?, color?, kind? }] }` adds markers to the root timeline
(kept sorted by time; times are stored as given, not frame-snapped). `RemoveMarkers { ids?, kind? }`
removes the listed ids and/or every marker of a kind (`user` | `beat`). Markers are part of
snapping: `Project::snap_points()` is the sorted set of root-timeline clip edges and markers.

- Errors (`InvalidMarker`): no markers, negative time, a colour that is not `#RRGGBB`, an id that
  already exists, an id to remove that does not exist, neither `ids` nor `kind`.

### `ApplyTranscriptCuts`, `RestoreTranscriptWords` (schema v8)
The transcript of a media item (`transcripts[]`, source media time) is the record of what the
speaker said; deleting words **cuts the timeline** (one model of the timeline).

- `ApplyTranscriptCuts { transcript_id, word_ids?, tags? }` marks the given words (and every word
  carrying one of `tags`, such as `filler`) `deleted`, then removes every deleted range from every
  video/audio clip of that media (root timeline and sequences). A range is a run of consecutive
  deleted words, from the first word's start to the last word's end, snapped to frames; the
  silence around it stays. The clip is split at the range ends (with the usual split rules for
  effects and keyframes) and the middle piece deleted with ripple, so the gap closes on each
  track the clip is on (clips of other media on those tracks shift too; other tracks are not
  touched). Already-cut ranges are skipped, so repeating the command is a no-op.
- `RestoreTranscriptWords { transcript_id, word_ids? }` clears `deleted` (all deleted words when
  `word_ids` is empty) and puts their ranges back: after the clip whose source ends nearest before
  it (extending that clip when they touch, else as a new clip with its settings but no keyframes
  or transition), or before the first clip when none does, shifting everything after it later.

- Errors (`InvalidTranscript`): unknown transcript or word id, no words selected / no deleted
  words, or a clip of that media is retimed (speed other than 1, or speed keyframes), because the
  cuts are in source time. Agents need `allow_deletion` for a write that removes whole clips.

### Magnetic tracks (schema v8)
On a track with `magnetic: true` the clips stay contiguous from time 0, frame-snapped. The
engine enforces it, so every caller gets the same result in one undo step:

| Command | On a magnetic track |
|---|---|
| `AddClip`, `AddText`, `AddStickerFromPack` | the clip is inserted where its **centre** falls among the clips already there (after every clip whose centre is at or before its own); a `start` past the end appends; later clips shift right |
| `MoveClip` (same track) | reorders by the same rule; the others move to close and open space |
| `MoveClip` (to another track) | the source track closes the gap; a magnetic target inserts by centre |
| `DeleteClip` | always ripples, whatever `ripple` says |
| `TrimClip`, `SetClipSpeed`, `SetClipKeyframes` (speed), `SetTextTiming`, `SetAdjustmentLayer` | the clip changes length, later clips follow |
| `SplitClip` | the pieces stay contiguous |
| `NestClips`, `UnnestClip`, `ApplyTemplate`, `ApplyTranscriptCuts`, `RestoreTranscriptWords` | the track is packed afterwards |
| `SetTrackFlags { magnetic: true }` | packs the track once; `false` leaves the clips where they are |

Overlap checks do not apply (nothing can overlap). Caption tracks cannot be magnetic.

### `SetAudioGain`
Sets `gain_db` on an audio-bearing clip (`AudioClip`, or a `VideoClip` with embedded audio
once that's modeled — v0 audio gain applies to `AudioClip` only).

- Errors: clip not found, or clip type has no audio.

### `GenerateCaptions` (Phase 1)
Runs local Whisper STT on `media_id` (video or audio) and adds one `CaptionClip` per
transcript segment to `track_id` (must be a caption track). Segments are placed at
`timeline_offset + segment.start_us` with duration `segment.end - segment.start`. `style`
(optional) gives the new captions their own style; omitted, they inherit the track's
`caption_style`. Built-in styles: `builtin:tiktok-bold-yellow@1`, `builtin:tiktok-minimal@1`,
`builtin:tiktok-box@1`, `builtin:youtube-lower-thirds@1`. (`style_id` is a deprecated alias.)

**Word timing.** whisper.cpp is run with `-ojf -ml 60 -sow` (full JSON with per-token offsets;
segments split on word boundaries into lines of at most 60 characters). The tokens are grouped
into words (a token starting with a space starts a word; word pieces and punctuation extend it;
special tokens are skipped). The command then fills **both** `transcripts[]` (one transcript per
media item, replaced on a new run: words `w-1`, `w-2`, … in source media time, `deleted: false`)
and each new caption's `body.words` (relative to the caption start, each with its `word_id`) and
`transcript_ref`. A whisper.cpp build that rejects those flags is run again with plain `-oj`:
sentence-level segments and **no words** (`words` empty, no transcript stored).

Requires `whisper-cli` (or `whisper`) on PATH and `RENDERLY_WHISPER_MODEL` pointing at a
ggml model file.

- Errors: track/media not found, track kind mismatch, caption overlap, Whisper unavailable.

### `GenerateVoiceover` (Phase 1)
Synthesizes narration from `text` using `provider` and writes WAV to `output_path`, then
imports it and places a clip on `track_id` at `start`. Returns `VoiceoverGenerated`
with `media_id` and `clip_id`.

`VoiceoverProvider` variants (JSON tag `provider`):

- `piper_local` — local Piper ONNX via `piper` CLI; requires `RENDERLY_PIPER_MODEL`.
- `open_ai` — OpenAI TTS (`tts-1`); requires `OPENAI_API_KEY` (BYO, opt-in).

- Errors: track not found or not audio, TTS unavailable, overlap, probe/import failure.

### `SetAudioFade` (Phase 1)
Sets `fade_in` and `fade_out` on an audio clip. Applied during export via FFmpeg
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

- Errors: no fields given, resulting `width`/`height` is `0`, or `fps` with a zero numerator or denominator. `fps` is the rational `{ "num": 30000, "den": 1001 }`; a plain number (`29.97`) is still accepted as a deprecated alias and snapped to the nearest standard rational (within 0.01), exactly as the v7 -> v8 migration does.

### `SetTrackFlags` (GUI rebuild M1)
Sets `muted`, `locked`, `hidden` and/or `magnetic` on a track. At least one field must be `Some`;
omitted fields are left unchanged. `muted`/`hidden` are enforced by the engine (export and
playback both skip muted/hidden tracks); `locked` is GUI-honored only — this command does
not itself reject edits to a locked track (see project-schema.md's `Track` note).

- Errors: track not found, no fields given, `magnetic: true` on a caption track.

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

Give `track_id` + `clip_id` for one clip, or `targets: [{track_id, clip_id}, ...]` to set the same
transform on several clips in **one command and one undo step** ("apply to all"). Every target
is checked first, so a batch applies to all of them or to none.

- Errors: track/clip not found, not a media clip, or non-finite transform fields.

### `SetClipKeyframes` (Phase 3.1, extended in v8)
Replaces all keyframe tracks on a media clip or adjustment layer. A track's `property` is a
string: a transform/audio property (`pos_x`, `pos_y`, `scale_x`, `scale_y`, `rotation`, `opacity`,
`volume`, `speed`, `fit_x`, `fit_y`) or **`effect:<instance-id>/<param>`**, a parameter of one of
the clip's effect instances. `value` is a number, or an array for `color` (`[r, g, b, a]`) and
`vec2` (`[x, y]`) parameters. Validates unique properties, non-negative finite times (no two keys
at one time), opacity keys in `0..=1`, speed keys in `0.25..=4`; sorts keys by time on write.

For an effect-parameter track the instance must exist on the clip, the parameter must be declared
by the effect, and it must be animatable: `bool`, `choice` and LUT parameters (and parameters the
effect declares `animatable: false`) are a validation error, as is an array of the wrong length.
On an adjustment layer `opacity` and effect-parameter tracks are accepted.

A key added strictly inside an existing `bezier` span, with a value on the current curve and no
handles, splits the curve (de Casteljau) so the motion does not change: the new key and its
neighbours get the handles of the two halves. (Follow with `SetKeyframeEasing` to change a span.)

- Errors: track/clip not found, not a media clip or adjustment layer, invalid keyframe data
  (`InvalidKeyframes`: unknown instance or parameter, not animatable, wrong value shape, a
  transform property on an adjustment layer).

### `SetKeyframeEasing` (schema v8)
`{ track_id, clip_id, property, time, easing }` sets how the span **after** the key at `time` is
interpolated. `easing` is `"linear"`, `"hold"`, `"auto"` (smooth monotone cubic, no handles), a
preset `"ease_in"` / `"ease_out"` / `"ease_in_out"`, or `{"bezier": [x1, y1, x2, y2]}` (CSS
control points). Curves are **written as `bezier` handles**: ease-in `(0.42, 0, 1, 1)`, ease-out
`(0, 0, 0.58, 1)`, ease-in-out `(0.42, 0, 0.58, 1)`, scaled to the span, so a preset is a starting
point the user can then drag. `linear`, `hold` and `auto` clear that span's handles. `time` picks
the nearest key within half a frame.

- Errors: no such track `property` on the clip, no key at `time` (the message lists the key
  times), or the last key (it has no span after it).

### `SetClipEffects` (Phase 3.1 / 3.4 / 3, slots in v8)
Sets the effect slots of a media clip or adjustment layer. Effects are applied **by ref**:
`builtin:<id>@<version>`, `pack:<pack-id>/<item>@<version>` (effects, filters and LUTs), or
`wasm:<plugin>`; `describe_commands` / `describe_effects` list what is available, with each
item's params and usual slot.

```jsonc
{ "command": "SetClipEffects", "track_id": "…", "clip_id": "…",
  "filter":  { "ref": "pack:mira.film-looks/print@1", "params": { "intensity": 0.8 } },
  "key":     { "ref": "builtin:chroma_key@1" },
  "adjust":  { "ref": "builtin:color_adjust@1", "params": { "exposure": 0.2 } },
  "effects": [ { "ref": "builtin:blur@1", "params": { "radius": 8 } } ],
  "blend_mode": "screen" }
```

- Every field except the target is optional: **omitted = unchanged, `null` = cleared**
  (`blend_mode` has no `null`; send `"normal"`). `effects` replaces the whole `effects[]` list.
  At least one field must be present.
- Slots run in the order filter, key, adjust, `effects[]`, whatever order they are sent in.
  `filter` accepts filters and LUTs, `key` the chroma key, `adjust` the colour adjustment (an item
  that does not fit is an error naming the slot); `effects[]` takes any single-input effect,
  filter or LUT. Adjustment layers have no `key`. Audio clips take only `effects[]`.
- An instance is `{ ref, id?, hash?, enabled?, params? }`. Give an existing instance's `id` (with
  the same `ref`) to edit it in place: its keyframe tracks stay, and omitted `params` keep their
  values. A new instance gets a fresh id; without `params` it gets the effect's `initial` values
  (so a new blur is visible). Params are checked against the effect's declaration (unknown key,
  wrong shape: error) and clamped into range; a LUT param must name an installed LUT.
- The `hash` of a pack item is recorded when it is applied. A ref that is not installed is an
  error, except that an instance already on the clip (same `id` and `ref`) whose item has gone
  missing stays editable: it keeps its params and renders pass-through.
- Removing an instance (it is absent from the new slots) deletes its keyframe tracks.
- `targets: [{track_id, clip_id}, ...]` (instead of `track_id` + `clip_id`) applies the same
  change to every listed clip in **one command, one undo step**; each clip gets fresh instance ids
  (given `id`s are ignored) and a failure on any target changes none of them.
- GPU effects run in the compositor; `wasm:*` plugins run on the CPU frame before upload and are
  rejected on adjustment layers (`InvalidEffects`: they need CPU pixels of a decoded frame).

Deprecated: `effects: [{ effect_id, params }]` (see "Deprecated v7 effect and transition shapes").

- Errors: track/clip not found, not a media clip or adjustment layer, or an invalid effect
  (`InvalidEffects`: malformed or uninstalled ref, wrong slot, bad params, duplicate instance id).

### `SetClipTransition` (Phase 3.5 / 3, refs in v8)
`{ track_id, clip_id, ref, duration?, params?, easing? }` sets or clears the outgoing transition
of a video media clip. `ref` is **required**: a transition ref (`builtin:crossfade@1`,
`builtin:fade_black@1`, `builtin:wipe_left@1` / `_right` / `_up` / `_down`,
`builtin:slide_left@1` / `_right`, `builtin:iris@1`, `builtin:blur_dissolve@1`, or an installed
pack transition), or `null` to clear. `duration` is a `Time`, required when setting a new
transition (an existing one keeps its duration), `> 0` and at most half of each adjacent clip's
*timeline* duration (honors `speed`); it is snapped to the project's frames. `params` are the
transition's control values (omitted: the existing ones if the ref is unchanged, else the
item's initial values). `easing` is `"linear"`, `"ease_in"` / `"ease_out"` / `"ease_in_out"`
(written as Bezier control points), `{"bezier": [x1, y1, x2, y2]}`, `null` for the transition's
declared default, or omitted to keep the current one. Requires a following clip. `targets`
works as for `SetClipEffects` ("apply to all", one undo step).

- Errors: not a video track, no following clip, invalid duration, a ref that is malformed, not
  installed or not a transition (`InvalidTransition`).

### `SetClipSpeed` (Phase 3)
Sets constant base playback `speed` on a media clip (`0.25`..`4.0`) used when no Speed
keyframes are present. Timeline duration and source-time mapping update; audio uses
pitch-preserving `atempo`. Rejects overlaps caused by a longer timeline span when slowing
down. Speed keyframes (`SetClipKeyframes` with `AnimProperty::Speed`) override via integral
mapping (schema v5).

### `LoadAssetPack` / `UnloadAssetPack` (Phase 3, deprecated in v8)
Packs live in the **user library**, not in the project (schema v8), so these now install a pack
directory (`pack.json`) into the library, or uninstall a pack from it (the same as
`renderly pack install` / `uninstall`; `UnloadAssetPack` accepts the id with or without the
`local.` publisher). They do not change the project, so they are not part of undo. Apply the
installed items by ref.

### `LoadWasmPlugin` / `UnloadWasmPlugin` (Phase 3)
Load or unload a WASM plugin directory (`plugin.json` + `.wasm`); the folder is recorded in the
project's `legacy_pack_paths` (plugins are not library items yet). Plugins may export frame
`process` and/or audio `process_audio`, and are applied as `wasm:<plugin-id>` refs.

### `AddStickerFromPack` / `AddSfxFromPack` (Phase 3 deferred)
Import a pack sticker (image → video track) or SFX (audio → audio track) and place a clip
at `start`. Requires the pack to be loaded.

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
sequentially at <= 360 px tall via one `VideoReader`; clip `speed`, `source_in` and Speed
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
  - Times: keyframe times are target-local (`timeline - target.start_us`). The
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
`start` for `duration`. Give it effects with `SetClipEffects`, a mask with
`SetClipMask`, opacity with `SetAdjustmentLayer` / opacity keyframes. Returns `ClipAdded`.

- Errors: `TrackNotFound`, `TrackKindMismatch` (not a video track), `Overlap` with another
  clip on the track, `InvalidDuration` (`duration <= 0`), `InvalidAdjustment`
  (`start` negative).

### `SetAdjustmentLayer` (schema v7)
Updates an adjustment layer's `start`, `duration` and/or `opacity` (`0..=1`).
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
`TikTok9x16`, `Youtube16x9`, or a `Custom { width, height, fps }` variant, with `fps` rational like the project rate). Muxes mixed
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

- **Schema v8, stage 3** (breaking for caption commands; the old shapes are accepted for one
  release with a deprecation warning): `AddCaption` / `SetCaption` take `body` and `style`
  (`text` and `style_id` are aliases), `GenerateCaptions` takes `style` and stores word-level
  transcripts; new `AddText`, `SetTextTiming`, `SetTextBody`, `SetTextStyle`, `SetTextAnimation`,
  `SetTrackCaptionStyle`, `SetCaptionWords`, `SetWordEmphasis`, `SplitCaptions`, `AddMarkers`,
  `RemoveMarkers`, `ApplyTranscriptCuts`, `RestoreTranscriptWords`; `AddTrack` and `SetTrackFlags`
  gained `magnetic` (enforced ripple on magnetic tracks, on by default for a new project's first
  video track); text clips take `SetClipTransform` / `SetClipKeyframes` / `SetClipEffects` /
  `SetClipTransition` / `SetClipMask` / `SetClipEnabled` / `SplitClip` / `MoveClip` like media
  clips; new errors `InvalidText`, `InvalidMarker`, `InvalidTranscript`. See
  [project-schema.md](project-schema.md) "Text, captions and transcripts".

- **Phase 3.4** (non-breaking): `SetClipEffects` validates known builtin ids + param
  clamps; compositor executes `builtin:color_adjust` / `blur` / `lut_contrast` / `lut_warm`.

- **Schema v8, stage 2** (breaking for effect and transition commands; the old shapes are accepted
  for one release with a deprecation warning): `SetClipEffects` takes the slots `filter` / `key` /
  `adjust` / `effects` / `blend_mode` (omitted = unchanged, `null` = cleared) with effects applied
  by versioned `ref` and typed params, and a `targets` batch; `SetClipTransition` takes a `ref`,
  `duration`, `params` and `easing`; `SetClipKeyframes` accepts `effect:<instance-id>/<param>`
  properties and array values; new `SetKeyframeEasing`; `SetClipTransform` gained `targets`;
  `LoadAssetPack` / `UnloadAssetPack` install into / remove from the user library. See
  [project-schema.md](project-schema.md) for the stored shapes.

