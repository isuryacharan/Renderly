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
