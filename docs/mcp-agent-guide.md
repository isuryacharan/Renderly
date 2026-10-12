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

## Live desktop bridge (E1/E2)

When `renderly-app` is running, it publishes a discovery file and a loopback WebSocket
JSON-RPC server — see [bridge-protocol.md](bridge-protocol.md). `renderly-mcp` prefers that
bridge for `get_project`, `apply_command`, `export`, and `render_frame` when the open project
path matches; otherwise it keeps today's headless file mode.

| Tool | Description |
|---|---|
| `get_editor_status` | `{ live, project_path, playhead, selection, … }` — whether the desktop app is connected |

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
| `RENDERLY_BRIDGE_DISCOVERY` | Overrides the live-bridge discovery file path. Point it at a nonexistent file to force headless mode (tests, sandboxes); unset in normal use |
| PATH must include | `ffmpeg`, `ffprobe`; optional `whisper-cli` (or `whisper`) for STT; optional `piper` for local TTS |

## Tools

### Project lifecycle

| Tool | Description |
|---|---|
| `new_project` | Create and open a `.renderly.json` file |
| `open_project` | Load an existing project |
| `get_project` | Return current project JSON |

### Editing (command API)

| Tool | Description |
|---|---|
| `apply_command` | Apply one `Command` JSON object and save (same shape as CLI `apply`) |
| `export` | Render open project to MP4 (`preset`: `tiktok` or `youtube`) |

All edit commands from [command-api.md](command-api.md) are available via `apply_command`, including
Phase 1's `GenerateCaptions`, `GenerateVoiceover`, `SetAudioFade`, and `SetTrackAudioRole`.

### Perception (read-only)

| Tool | Description |
|---|---|
| `probe_media` | Probe a file path (kind, duration, dimensions) |
| `get_transcript` | Whisper transcript for a `media_id` in the open project |
| `render_frame` | PNG preview at `time_secs` (returns JSON with `png_base64`) |
| `detect_silence` | Silent spans in a `media_id` (FFmpeg `silencedetect`) |
| `detect_scenes` | Scene cuts in a `media_id` (FFmpeg scene filter) |
| `get_audio_peaks` | Downsampled peak envelope for waveform display |

## Worked example: script + footage → export

Assume gameplay MP4s in `C:/footage/` and a project at `edit.renderly.json`.

1. **Create project** — `new_project { path, name, width, height, fps }`
2. **Import each clip** — `apply_command { command: { "command":"ImportMedia", "path":"..." } }`
3. **Add tracks** — video track `V1`, audio `A1` (voiceover), `A2` (music), caption track `C1`
4. **Set mix roles** — `SetTrackAudioRole` with `voiceover` on narration track, `music` on BGM track
5. **Place clips** — `AddClip` per script beat (use `get_project` to read back `media_id` / `track_id`)
6. **Voiceover** — `apply_command { command: { "command":"GenerateVoiceover", "text":"...", "track_id":"...", "position_secs": 0.0, "output_path": "voice.wav", "provider": { "provider": "piper_local" } } }`
7. **Auto-caption** — `apply_command { command: { "command":"GenerateCaptions", ... } }`  
   Or call `get_transcript` first to inspect timing, then batch `AddCaption` commands.
8. **Trim dead air** — `detect_silence` on raw footage, then `TrimClip` / `DeleteClip` as needed
9. **Verify** — `render_frame { time_secs: 12.5, preset: "tiktok" }` (inspect PNG)
10. **Export** — `export { output_path: "out.mp4", preset: "tiktok" }`

Built-in caption styles: `tiktok-bold-yellow`, `tiktok-minimal`, `tiktok-box`, `youtube-lower-thirds`.

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
    "position_secs": 0.0, "source_in_secs": 0.0, "source_out_secs": 3.0 }
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
  { "command": "AddAdjustmentLayer", "track_id": "<video-track>", "position_secs": 4.0, "duration_secs": 6.0 },
  // then, with the clip_id returned in ClipAdded:
  { "command": "SetClipEffects", "track_id": "<video-track>", "clip_id": "<adj>",
    "effects": [{ "id": "<new-uuid>", "effect_id": "builtin:color_adjust", "enabled": true,
                  "params": { "saturation": 0.0, "contrast": 1.3 } }] },
  { "command": "SetAdjustmentLayer", "track_id": "<video-track>", "clip_id": "<adj>", "opacity": 0.6 }
]
```

An adjustment layer applies its effect chain to the picture composited from **every lower
video track** (z-order = track order, first = bottom) for its duration, then mixes the result
with the unadjusted picture by `opacity` (and `mask`, whose UVs are canvas coordinates
here). Layers on higher tracks are untouched. It shares the track's no-overlap rule with media
clips. Only builtin and pack-LUT effects work (WASM plugin effects are rejected), only
`opacity` can be keyframed, and `TrimClip` does not apply (use `SetAdjustmentLayer`).

`TrackMotion`, `StabilizeClip`, `AutoReframeClip`, `GenerateBackgroundMatte`,
`GenerateCaptions` and `SetClipAudioDenoise` reject sequence media with
`UnsupportedForSequence`; run them on the inner clips instead.
