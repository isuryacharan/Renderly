# MCP agent guide

Status: **Phase 1 complete**. `renderly-mcp` exposes the command API and read-only perception tools
over stdio. Every edit tool wraps `renderly_core::apply_command` — see [command-api.md](command-api.md).

## Running the server

```sh
cargo run -p renderly-mcp
```

Configure in Cursor / Claude Desktop MCP settings:

```json
{
  "mcpServers": {
    "renderly": {
      "command": "cargo",
      "args": ["run", "-p", "renderly-mcp", "--quiet"],
      "cwd": "/path/to/video-editor"
    }
  }
}
```

Logging goes to **stderr**; stdout is reserved for MCP JSON-RPC.

### `--project <path>` (startup project)

```sh
renderly-mcp --project "C:/Users/me/Videos/my-project/edit.renderly.json"
```

The server opens that project at startup exactly as the `open_project` tool would, so the
session starts on it without a bootstrap call. With the desktop app live on the same path,
edits then go through the bridge and share the app's undo history. Without the flag the
behaviour is unchanged (no project until `open_project` or `new_project`).

- A missing or invalid file, or a missing value after `--project`, makes the process exit
  with an error before serving MCP. It never falls back to a headless session with no project.
- Any other argument is rejected with a usage message.
- The Renderly desktop agent chat passes this flag for each session (see
  [agent-chat-design.md](agent-chat-design.md)).

## Live desktop bridge (E1/E2)

When `renderly-app` is running, it publishes a discovery file and a loopback WebSocket
JSON-RPC server — see [bridge-protocol.md](bridge-protocol.md). `renderly-mcp` prefers that
bridge for `get_project`, `apply_command`, `export`, and `render_frame` when the project opened
with `open_project` / `new_project` is the one the app has open; otherwise it works headlessly on
the file. With no project open these tools fail with "no project open" — they never act on
whatever the app happens to have on screen. Call `open_project` with the app's project path
(from `get_editor_status`) to edit it live.

| Tool | Description |
|---|---|
| `get_editor_status` | `{ live, project_path, playhead, selection, … }` — whether the desktop app is connected |
| `permission_prompt` | Used by the desktop agent chat only (`--permission-prompt-tool` for Claude Code). Forwards the request to the app, which asks the user in its Agent tab. Returns `{behavior: allow\|deny}`. Denies when the app is not live on this project. Not for other clients |

Agent edits over the bridge share the app's undo stack (Ctrl+Z undoes agent changes) and
emit `project:changed` so the GUI updates live.

## Environment

| Variable | Purpose |
|---|---|
| `RENDERLY_WHISPER_MODEL` | Path to whisper.cpp `ggml-*.bin` model for `get_transcript` / `GenerateCaptions` |
| `RENDERLY_FONT_PATH` | Optional `.ttf` for caption burn-in (defaults to system font) |
| `RENDERLY_PIPER_MODEL` | Piper ONNX model for `GenerateVoiceover` with `piper_local` provider |
| `RENDERLY_PIPER_CONFIG` | Optional Piper voice config JSON |
| `OPENAI_API_KEY` | BYO OpenAI TTS for `GenerateVoiceover` with `open_ai` provider |
| `RENDERLY_MODEL_DIR` | Directory holding already-downloaded ML model files (e.g. `rvm_mobilenetv3_fp32.onnx`); checked before the per-user model cache (`%LOCALAPPDATA%\renderly\models` / `$XDG_DATA_HOME/renderly/models`). Used as-is, unverified. Without the weights in either place, `GenerateBackgroundMatte` with `model_id: "rvm"` fails with `ModelNotAvailable` |
| `RENDERLY_SEG_CLI` | External segmentation binary for background-removal `model_id` `cli` / `birefnet` (argv1 input PNG, argv2 alpha PNG) |
| `RENDERLY_BRIDGE_DISCOVERY` | Overrides the live-bridge discovery file path. Point it at a nonexistent file to force headless mode (tests, sandboxes); unset in normal use |
| PATH must include | `ffmpeg`, `ffprobe`; optional `whisper-cli` (or `whisper`) for STT; optional `piper` for local TTS |

## Tools

### Project lifecycle

| Tool | Description |
|---|---|
| `new_project` | Create and open a `.renderly.json` file |
| `open_project` | Load an existing project |
| `get_project` | Return the project JSON with `project_revision` at its top level, plus `effect_status` when an effect or transition is `changed`, `missing` or `disabled` (see "Effects, slots and keyframes") |

### Editing (command API)

| Tool | Description |
|---|---|
| `apply_command` | Apply one `Command` JSON object and save (same shape as CLI `apply`). Accepts the edit-contract envelope: `expected_revision`, `dry_run`, `request_id`, `allow_deletion` (see "Edit contract" below) |
| `undo` / `redo` | Step the open app's shared undo history. One step is one agent turn, one batch, or one coalesced drag. Needs the desktop app open on the project (the history lives in the app session) |
| `export` | Render open project to MP4 (`preset`: `tiktok` or `youtube`) |
| `export_otio` | Write the open project as an OpenTimelineIO `.otio` file (`output_path`); lossless for Renderly, see [otio.md](otio.md) |
| `import_otio` | Create a **new** project file from a `.otio` file (`input_path`, `output_path`, optional `width`/`height`/`probe`) and open it; returns `{ project_path, fps, tracks, media, sequences, warnings[] }` — read `warnings` for what could not be carried over |

All edit commands from [command-api.md](command-api.md) are available via `apply_command`, including
Phase 1's `GenerateCaptions`, `GenerateVoiceover`, `SetAudioFade`, and `SetTrackAudioRole`.

### Time

Command parameters and perception tools take a `Time`: integer microseconds, or a string -
`"1.5s"`, `"1500ms"`, `"90f"` (frame 90 at the project rate), `"00:00:01:15"` (non-drop
timecode). Everything you read back (`get_project`, `detect_silence`, `get_transcript`,
`render_frame`, ...) is integer microseconds in `*_us` fields, and `settings.fps` is a rational
`{ "num": 30000, "den": 1001 }`. Frame-based positions are snapped to the project's frame grid, so
asking for the same frame always yields the same value. The v7 float-second names
(`position_secs`, `at_secs`, `time_secs`, `min_duration_secs`, ...) still work for one release;
each use adds a deprecation warning to the result (`warnings`). Details:
[command-api.md](command-api.md) "Time", [project-schema.md](project-schema.md) "Time".

### Perception (read-only)

| Tool | Description |
|---|---|
| `probe_media` | Probe a file path (kind, duration, dimensions) |
| `get_transcript` | Transcript of a `media_id`: the project's stored word-level transcript (`{source: "project", transcript}`: words with ids, source-time µs, `deleted` flags and `tags`) when `GenerateCaptions` made one, else a fresh local Whisper run (`{source: "whisper", ...}`; segments carry `words` when whisper.cpp reports token timings). `refresh: true` always runs Whisper |
| `render_frame` | PNG preview at `time` (a Time such as `"12.5s"`, `"90f"` or integer µs; returns JSON with `time_us` and `png_base64`; the old `time_secs` float is a deprecated alias) |
| `detect_silence` | Silent spans in a `media_id` (FFmpeg `silencedetect`); `noise_db`, `min_duration` (a Time, default `"0.5s"`; the old `min_duration_secs` float is a deprecated alias); spans carry `start_us`, `end_us`, `duration_us` |
| `detect_scenes` | Scene cuts in a `media_id` (FFmpeg scene filter) |
| `get_audio_peaks` | Downsampled peak envelope for waveform display |
| `get_background_matte_status` | `{track_id, clip_id}` -> `not_generated` / `ready` / `stale {reason}` for a clip's background-removal matte |
| `list_library` | Packs installed in the user library: each release with its source, enabled state and item counts, the reason for every item that is not ok, and the builtin pack. Read-only, needs no project. Install and uninstall are not exposed: the app asks the user to confirm them (see [asset-pack.md](asset-pack.md)) |

### Local ML models

| Tool | Description |
|---|---|
| `list_models` | Known models with licence, size, download host and whether each is installed (never touches the network) |
| `download_model` | `{id, user_consented}`: download a model from its upstream host into the model cache (SHA-256 verified, atomic). **Refuses unless `user_consented` is `true`** - show the user the licence, size and host from `list_models` and get their agreement first. The CLI equivalent is `renderly models list` / `renderly models download <id> --yes` |

AI background removal: set `SetClipBackgroundRemoval` with `"model_id": "rvm"`, make sure
`list_models` shows `rvm-mobilenetv3-fp32` installed (GPL-3.0 weights, ~15 MB, downloaded only
with the user's consent; never bundled), then run `GenerateBackgroundMatte`. The bake takes about
0.1 s per frame and the tool call blocks until it is done (when the desktop app is live on the
project it runs there with a progress bar and the user can cancel it). A missing model is the
typed error `model rvm-mobilenetv3-fp32 is not installed`. Check `get_background_matte_status`
after trimming the clip or replacing the source: `stale` means regenerate.

## Edit contract: writing safely

The full rules are in [command-api.md](command-api.md) "Edit contract". What an agent needs:

1. **Read first.** `get_project` and `get_editor_status` return `project_revision`. Keep the
   value from your latest read.
2. **Pass it back.** Send `expected_revision` with each `apply_command`. If the project changed
   since (the user edited, or another write landed), the write fails with `revision_conflict`
   and changes nothing. Re-read, then retry.
3. **Preview big or destructive edits.** Send `dry_run: true`. The result lists `changed.created`,
   `changed.modified`, `changed.deleted` and a `patch`. Nothing is written. Send the same command
   again without `dry_run` to apply it.
4. **Make retries safe.** Give each logical edit a `request_id`. Retrying the same id and command
   returns the first result (`replayed: true`) instead of applying it twice.
5. **Deleting needs permission.** Deleting clips, tracks, media or sequences is refused
   (`deletion_not_allowed`) unless `allow_deletion` has a `reason` and the exact full ids that a
   dry run lists under `changed.deleted`. Deleting a track also deletes its clips, so those ids
   are listed too. Ask the user before you delete anything they did not clearly ask you to remove.

Errors are tool errors (`isError: true`) whose text is JSON: `{ "code", "message", "hint" }`. Do
what the `hint` says. Codes: `invalid_command`, `not_found`, `revision_conflict`,
`request_id_reused`, `deletion_not_allowed`, `busy`, `rolled_back`, `internal`. A batch that fails
part-way reports `rolled_back`, and nothing from it was applied.

`describe_commands` returns the capabilities (`renderly.capabilities.v1`), every command's
shape, and the effects, filters, transitions and LUTs it can apply (the builtins plus the packs
installed in the user library). `describe_effects { packs?: [folder] }` lists the same catalogue
(or the pack folders you name, without a project); each line gives the ref, kind, label, state,
the usual slot and params.
Pack item states and refs are described in [asset-pack.md](asset-pack.md#pack-format-2-effects-filters-transitions-luts). Renderly's ready-made workflows for Claude Code are in `skills/` (see `skills/README.md`).

## Worked example: script + footage → export

Assume gameplay MP4s in `C:/footage/` and a project at `edit.renderly.json`.

1. **Create project** — `new_project { path, name, width, height, fps }`
2. **Import each clip** — `apply_command { command: { "command":"ImportMedia", "path":"..." } }`
3. **Add tracks** — video track `V1`, audio `A1` (voiceover), `A2` (music), caption track `C1`
4. **Set mix roles** — `SetTrackAudioRole` with `voiceover` on narration track, `music` on BGM track
5. **Place clips** — `AddClip` per script beat (use `get_project` to read back `media_id` / `track_id`)
6. **Voiceover** — `apply_command { command: { "command":"GenerateVoiceover", "text":"...", "track_id":"...", "start": 0, "output_path": "voice.wav", "provider": { "provider": "piper_local" } } }`
7. **Auto-caption** — `apply_command { command: { "command":"GenerateCaptions", "media_id":"...", "track_id":"...", "timeline_offset": 0 } }`  
   It stores a word-level transcript and gives each caption its `body.words`. Re-page them with
   `SplitCaptions` (`max_chars`, `max_lines`, `max_duration`), restyle the whole track with
   `SetTrackCaptionStyle`, or call `get_transcript` first and batch `AddCaption` commands.
8. **Trim dead air** — `detect_silence` on raw footage, then `TrimClip` / `DeleteClip` as needed
9. **Verify** — `render_frame { time: "12.5s", preset: "tiktok" }` (inspect PNG)
10. **Export** — `export { output_path: "out.mp4", preset: "tiktok" }`

Built-in caption styles (refs `builtin:<id>@1`): `tiktok-bold-yellow`, `tiktok-minimal`, `tiktok-box`,
`youtube-lower-thirds`.

## Captions, titles, transcripts, markers and magnetic tracks (schema v8)

- **Captions** are `body { text, runs, words }` plus `style` (a `{ref, params, overrides}` object, or
  `null` to inherit the caption track's `caption_style`) and `animations`. Word timings are
  relative to the clip start and are never rewritten by text edits. `AddCaption` / `SetCaption`
  edit them (the old `text` / `style_id` parameters still work, with a warning), `SetTextBody`,
  `SetTextStyle`, `SetTextAnimation` change many clips at once with `targets`,
  `SetCaptionWords` / `SetWordEmphasis` set timing and key words, `SplitCaptions` re-pages.
- **Titles**: `AddText` puts a text layer on a **video** track; it takes `SetClipTransform`,
  `SetClipKeyframes`, `SetClipEffects`, `SetClipTransition`, `SetClipMask`, blend modes like any
  clip. Move it with the transform; resize its time with `SetTextTiming`.
- Today the pictures come from the four builtin styles; of a style's `overrides` only `fill`
  (`#RRGGBB`), `size` (fraction of the frame height) and `anchor` change the picture.
  `animations` and `params` are stored and validated, and render later. A style that is not
  installed shows as `missing` in `get_project`'s `effect_status` and the caption falls back to
  the track's or the default style.
- **Transcripts**: `get_transcript` returns the stored word-level transcript. Delete words with
  `ApplyTranscriptCuts { transcript_id, word_ids | tags }`: it marks them `deleted` and cuts them
  out of every clip of that media, closing the gaps (a real edit; agents need `allow_deletion`
  for the whole clips it removes). `RestoreTranscriptWords` puts them back.
- **Markers**: `AddMarkers` / `RemoveMarkers`; timeline edits in the app snap to them.
- **Magnetic tracks** (`magnetic: true`, the first video track of a new project): the clips stay
  contiguous from 0 and `AddClip`, `MoveClip`, `DeleteClip`, `TrimClip` ripple for you; a clip's
  `start` only picks its place in the order (it lands where its centre falls). Check the flag in
  `get_project` before planning positions; `SetTrackFlags { magnetic: false }` turns it off.

Music ducking is automatic when voice/dialog and music tracks are both present (`settings.duck_db`, default −12 dB).

## Nested timelines and adjustment layers (schema v7)

Both are plain commands through `apply_command`; there is no GUI-only path. `get_project`
shows the result: `sequences[]` (each with its own `tracks`), a `kind: "sequence"` media item
per sequence, and `type: "adjustment"` clips. **Track ids are unique across the root timeline
and all sequences**, so any command that takes a `track_id` addresses a track inside a
sequence the same way as a root track — no extra parameter. `render_frame` and `export`
always render the root timeline (nested sequences are rendered wherever a clip places them),
so verify a sequence by placing it on the root.

### Make a compound clip from clips you already placed

1. Read the project (`get_project`) for the `track_id` / `clip_id` of each clip.
2. `NestClips { "clips": [{"track_id": "...", "clip_id": "..."}, ...], "name": "Intro" }` — the
   clips (which must all be on one timeline) move into a new sequence with their relative
   times and track layering preserved, and one clip (plus one audio clip if any were audio)
   takes their place. The outcome is `ClipsNested { sequence_id, media_id, video_clip_id?,
   audio_clip_id? }`; the new clips behave like any other clip (transform, effects, speed,
   transitions, `SplitClip`, ...).
3. To edit the contents later, address the inner tracks/clips by their own ids with the usual
   commands (`MoveClip`, `SetClipEffects`, `AddClip` onto an inner track, ...).
4. `UnnestClip { track_id, clip_id }` undoes it for an untouched clip (it names the reason if
   it cannot); the sequence stays until `DeleteSequence`.

### Build a sequence from scratch

```jsonc
[
  { "command": "CreateSequence", "name": "Lower third", "width": 1920, "height": 300, "id": "<seq-uuid>" },
  { "command": "AddTrack", "kind": "video", "name": "V1", "id": "<inner-track-uuid>", "sequence_id": "<seq-uuid>" },
  { "command": "AddClip", "track_id": "<inner-track-uuid>", "media_id": "<file-media-id>",
    "start": 0, "source_in": 0, "source_out": "3s" }
]
```

`CreateSequence` returns `SequenceCreated { sequence_id, media_id }`; place the sequence with
`AddClip { media_id: <that media_id>, ... }` on a root (or other sequence's) video track — or
an audio track for its sound. The source range is in the sequence's own time (its duration is
derived from its contents, `0` while empty). A sequence renders over transparency at its own
size and is cover-fitted into the parent frame like any media, so a short lower-third keeps
its alpha. `SequenceCycle` rejects putting a sequence inside itself; `SequenceInUse` blocks
`DeleteSequence` while clips place it.

### Adjustment layers (effects over everything beneath)

```jsonc
[
  { "command": "AddAdjustmentLayer", "track_id": "<video-track>", "start": "4s", "duration": "6s" },
  // then, with the clip_id returned in ClipAdded:
  { "command": "SetClipEffects", "track_id": "<video-track>", "clip_id": "<adj>",
    "adjust": { "ref": "builtin:color_adjust@1", "params": { "saturation": 0.0, "contrast": 1.3 } } },
  { "command": "SetAdjustmentLayer", "track_id": "<video-track>", "clip_id": "<adj>", "opacity": 0.6 }
]
```

An adjustment layer applies its effect chain to the picture composited from **every lower
video track** (z-order = track order, first = bottom) for its duration, then mixes the result
with the unadjusted picture by `opacity` (and `mask`, whose UVs are canvas coordinates
here). Layers on higher tracks are untouched. It shares the track's no-overlap rule with media
clips. Builtin and pack effects work (WASM plugin effects are rejected), it has no `key` slot,
only `opacity` and its own effects' parameters can be keyframed, and `TrimClip` does not apply
(use `SetAdjustmentLayer`). A `blend_mode` blends the adjusted picture over the original.

`TrackMotion`, `StabilizeClip`, `AutoReframeClip`, `GenerateBackgroundMatte`,
`GenerateCaptions` and `SetClipAudioDenoise` reject sequence media with
`UnsupportedForSequence`; run them on the inner clips instead.

## Effects, slots and keyframes (schema v8)

Effects are applied **by ref**, never by file path. Call `describe_effects` (or
`describe_commands`) for what is installed: builtins are `builtin:<id>@1`; items from installed
packs are `pack:<pack-id>/<item>@<version>`; a LUT is a pack item too. A clip has four places to
put them, which always run in this order: `filter` (a LUT or look), `key` (chroma key), `adjust`
(colour adjustment), then the `effects` list in list order; the clip mask and then the blend mode
come after.

```jsonc
{ "command": "SetClipEffects", "track_id": "<track>", "clip_id": "<clip>",
  "filter": { "ref": "pack:mira.film-looks/print@1", "params": { "intensity": 0.8 } },
  "effects": [ { "ref": "builtin:blur@1", "params": { "radius": 8 } } ],
  "blend_mode": "screen" }
```

- Omit a field to leave it alone; send `null` to clear it. `effects` replaces the whole list.
  Give an instance's `id` (from `get_project`) to edit it in place and keep its keyframes.
- Params are typed: numbers, bools, `[x, y]`, `[r, g, b, a]` (0..1), an option index for a choice,
  a LUT ref string. Leave `params` out on a new instance to get the effect's initial values; a
  value out of range is clamped and an unknown key is an error that lists the valid ones.
- `targets: [{track_id, clip_id}, ...]` applies one change to many clips as one undo step (also on
  `SetClipTransition` and `SetClipTransform`); a failure on any target changes none.
- `SetClipTransition { ref: "builtin:crossfade@1", duration: "0.5s" }`; `ref: null` clears it.
  `easing` is optional (`"ease_in_out"`, `{"bezier": [x1, y1, x2, y2]}`, `null` = the transition's
  default).
- **Status.** `get_project` adds `effect_status` when an instance or transition is `changed`
  (the installed item differs from the one applied), `missing` (not installed: it renders as
  nothing but keeps its params) or `disabled` (installed, this build cannot run it). Tell the
  user which pack is missing instead of removing the effect; installing it restores the look.
- **Animating.** `SetClipKeyframes` tracks take `property: "effect:<instance-id>/<param>"` with
  numeric values, or arrays for color and vec2 params. Bool, choice and LUT params cannot be
  animated. `SetKeyframeEasing { property, time, easing }` sets a span to `linear`, `hold`
  (step), `auto` (smooth, no overshoot), a preset (`ease_in`, `ease_out`, `ease_in_out`) or CSS
  Bezier points. Deleting an effect deletes its tracks.
- The v7 shapes (`effect_id`, `transition: {kind, duration_us}`, `ease_in` keys) still work for
  one release and add a deprecation warning to the result.

