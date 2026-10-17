# Live agent bridge protocol

Status: **Sprint 4 (E3 live perception)**. Contract for the loopback WebSocket JSON-RPC
bridge between `renderly-app` and `renderly-mcp`. See
[improvement-plan.md](improvement-plan.md) workstream E and
[mcp-agent-guide.md](mcp-agent-guide.md).

## Discovery

On bridge start the app writes:

`%LOCALAPPDATA%/renderly/bridge.json` (Windows) or
`$XDG_DATA_HOME/renderly/bridge.json` / `~/.local/share/renderly/bridge.json` (Unix)

```json
{
  "pid": 12345,
  "port": 54321,
  "token": "<64 hex chars>",
  "project_path": "C:/Users/.../Documents/Renderly/Untitled.renderly.json"
}
```

`project_path` is updated on open/close/create and may be `null` when no project is open.
The file is deleted when the main window is destroyed.

MCP reads this file, connects to `ws://127.0.0.1:<port>/`, and authenticates with `token`.
If the file is missing, the TCP connect fails, or (for edit tools) the open MCP project path
does not match `project_path`, MCP stays in headless file mode.

## Transport

- Bind: `127.0.0.1:0` only (loopback).
- Framing: one JSON-RPC 2.0 request/response per WebSocket text message.
- Auth: every request's `params` object must include `"token": "<discovery token>"`.

### Request

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "apply_command",
  "params": {
    "token": "...",
    "command": { "command": "SetClipOpacity", "...": "..." }
  }
}
```

### Response

```json
{ "jsonrpc": "2.0", "id": 1, "result": { "revision": 3, "patch": [...], "outcome": "..." } }
```

or

```json
{ "jsonrpc": "2.0", "id": 1, "error": { "code": -32000, "message": "..." } }
```

## Methods

| Method | Params | Result |
|---|---|---|
| `get_project` | _(token only)_ | `{ project, revision }` |
| `apply_command` | `command`, `merge_key?` | `{ revision, patch, outcome }` — RFC-6902 patch; shared undo; `merge_key` coalesces consecutive changes into one undo record (command-api.md "History, transactions, coalescing and preview overrides"); emits `project:changed` **without** `mutation_id` so the GUI refetches |
| `apply_commands` | `commands[]` | `{ revision, patch, outcomes }` |
| `end_gesture` | _(token only)_ | `{ can_undo, can_redo }`. Closes the top record's merge gesture |
| `begin_transaction` | `label` | `{ can_undo, can_redo }`. Opens an undo transaction (nests by depth) |
| `commit_transaction` | _(token only)_ | `{ can_undo, can_redo }`. Commits the outermost transaction as one record |
| `rollback_transaction` | _(token only)_ | `{ can_undo, can_redo, revision, patch }`. Restores the pre-transaction state |
| `set_preview_override` | `target`, `override` (`{ transform?, opacity?, effects? }`) | `{ ok }`. Temporary render-only override; never saved or in undo |
| `clear_preview_override` | `target` | `{ ok }` |
| `undo` / `redo` | _(token only)_ | `{ can_undo, can_redo, revision, patch }` |
| `play` | optional `time_secs` | `{ ok }` |
| `pause` | | `{ time_secs }` |
| `seek` | `time_secs` | `{ ok }` |
| `set_playhead` | `time_secs` | `{ ok, time_secs }` — seeks/previews like `seek`, then emits Tauri event `bridge:playhead` `{ time_secs }` so the GUI store syncs |
| `export` | `output_path`, `preset` | `{ ok, output_path }` |
| `render_frame` | `time_secs`, `preset` | `{ time_secs, png_base64, byte_len }` |
| `get_editor_status` | | `{ live, project_path, project_name, playhead, playing, selection, revision }` |
| `agent_permission` | `tool_name`, `input`, `tool_use_id?` | `{ behavior: "allow", updatedInput }` or `{ behavior: "deny", message }`. Sent by `renderly-mcp`'s `permission_prompt` tool for the desktop agent chat. Waits for the user's answer in the app; denies when no agent session is running. Not for other clients |

### `render_frame` (E3 — live FrameRenderer)

Uses the **session's live project** and a `FrameRenderer` at **preview resolution**:
`ExportSettings` from `preset` (fps / encode defaults), with width/height overridden to the
project aspect scaled to the playback engine's `target_height` (preview panel size). If the
panel has not reported a size yet (`target_height == 0`), falls back to
`ExportSettings::from_preset` dimensions. Decode uses
`DecodeOptions { target_height: Some(h), output_fps: None }` when preview size is known.
This is the same readback path the preview stack retains — not a fresh headless
export-size `perceive::render_frame_png`.

### Selection (C3)

Selection is mirrored from the webview via the Tauri `set_editor_selection` command as:

```json
{ "primary": { "trackId": "...", "clipId": "..." }, "all": [ /* primary first */ ] }
```

or `null` when nothing is selected. `get_editor_status.selection` echoes that shape.
`primary` is the focused clip (inspector / trim handles); `all` is the full multi-selection.
