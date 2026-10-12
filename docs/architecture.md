# Architecture

Status: **Phase 3 complete; Phase 4 in progress.** Export drives FFmpeg subprocess
decode/encode with an offscreen wgpu compositor. Schema **v7** adds nested sequences and
adjustment layers (see "Evaluation and compositing" below); schema v5 covered clip transform /
keyframes (incl. Speed) / builtins (incl. glitch) / ten WGSL transitions / clip speed,
asset packs (LUTs, stickers, SFX), and a wasmtime frame+audio plugin host. See
[asset-pack.md](asset-pack.md) and [plugin-api.md](plugin-api.md). The preview renders
entirely inside the webview (`renderly-core`'s compositor compiled to wasm32/WebGPU) —
the earlier native per-OS child-window preview was removed in P4; see
[preview-webview.md](preview-webview.md).
CI runs on Windows/macOS/Linux. Manual QA: [qa-checklist.md](qa-checklist.md).

## Crate graph

```
                     ┌───────────────────┐
                     │   renderly-core     │   headless engine, no UI deps
                     │  (lib crate)        │
                     └─────────┬─────────┘
              ┌────────────────┼────────────────┐
              │                │                 │
   ┌──────────▼──────┐ ┌───────▼───────┐ ┌───────▼────────┐
   │  renderly-cli    │ │ renderly-mcp   │ │ renderly-app    │
   │  (bin)           │ │ (bin, MCP       │ │ (bin, Tauri 2   │
   │                  │ │  server)        │ │  desktop app)   │
   └──────────────────┘ └────────────────┘ └────────────────┘
```

Dependency direction is strictly downward: `renderly-core` never imports from the other
three. All three frontends depend on `renderly-core` and nothing else depends on them.

## `renderly-core`

Owns:

- **Project model** — serde types matching [project-schema.md](project-schema.md).
- **Command API** — the `Command` enum and `apply_command(&mut Project, Command) -> Result<...>`
  function matching [command-api.md](command-api.md). This is the *only* sanctioned way to
  mutate a project. Undo/redo is (eventually) a command log, not ad hoc state snapshots.
- **Media I/O** — FFmpeg-backed decode/encode. Phase 0 invokes `ffmpeg`/`ffprobe` as
  subprocesses (no link-time libav dependency); migrate to `ffmpeg-the-third` when dev/CI
  ships FFmpeg development libraries consistently.
- **Compositing** — wgpu offscreen render graph (cover-fit scale/blit; per-layer
  translate/scale/rotate + opacity; builtin effect chain — color_adjust / blur /
  lut_contrast+lut_warm / glitch; dual-layer transitions). Pack LUTs and WASM frame
  effects run on CPU before GPU upload. Phase 4 adds clip masks / mattes and
  chroma / background-removal on the shared alpha path.
- **Perception** — frame rendering to image, transcript (whisper-rs), scene/silence
  detection. These back the MCP perception tools and are engine functions, not MCP-specific
  code, so the CLI can expose them too.

Internal module boundaries (indicative, refine as code lands):

```
renderly-core/src/
  project/       schema types, versioning, (de)serialization
  commands/      Command enum, apply_command, per-command logic + unit tests
  media/         decode, encode, probe (FFmpeg)
  compose/       wgpu render graph, text/caption rendering
  audio/         mixing, TTS, STT
  perceive/      frame render-to-image, transcript, scene/silence detection
```

## Evaluation and compositing (schema v7)

`renderly-core/src/compose/eval.rs` is the single, decode-free answer to "what is on screen at
time `t`": `active_steps(project, t)` returns a bottom-to-top list of `ComposeStep`s — a
`Layer(ActiveLayer)` for each active video-track clip (both halves of a transition window), or
an `Adjustment { effects, opacity, mask }` for an active adjustment layer. An `ActiveLayer`
samples an `ActiveSource`: `Media` (a file's frame/still at a source time) or `Sequence` (a
nested timeline at a sequence time), and carries an `InstanceKey` (the chain of decoder keys
from the root) so the same media needed twice in one frame gets two decoders/elements.
`sequence_steps` evaluates the inside of a sequence layer; recursion is capped at
`MAX_SEQUENCE_DEPTH = 8`. The module is cross-target (no fs/native deps).

`compose::build_scene` turns steps into compositor input (`SceneStep`s) with a host-supplied
`SceneHost`: the host only supplies pixels for `Media` layers (ffmpeg decode in
`export::FrameRenderer`, `<video>`/`<img>` textures in `renderly-wasm`) and its mask policy;
adjustment steps and the recursion into sequences are shared, so preview and export cannot
drift. The `Compositor` runs an adjustment step as: accumulation -> straight-alpha copy ->
effect chain -> `mix(unadjusted, adjusted, opacity x mask)` written back; and a nested sequence
as: composite its steps into a pooled offscreen target at the sequence's resolution (cleared
to transparent, premultiplied accumulation), convert to straight alpha, then draw it as a
layer. Targets are pooled per size and reused across frames; a project with neither feature
runs exactly the same passes as before. Audio expands nested audio clips recursively into leaf
mix pieces (`export::collect_audio_clips`).

## `renderly-cli`

Thin binary. Subcommands operate on a project JSON file on disk:

- `new-project` — create an empty project file at schema v0.
- `apply` — apply one or more commands (from args or a JSON script file) and save.
- `show` — print project state (or a summary) for inspection.
- `export` — render the project to a video file.

This is the simplest agent-drivable surface and the first thing Phase 0's milestone
("CLI renders a cuts-only timeline JSON to MP4") runs through.

## `renderly-mcp`

MCP server (stdio, HTTP later) wrapping the same `renderly-core` API as the CLI, plus
perception tools exposed as MCP tools. Built in Phase 1 — do not build ahead of the CLI
proving the command API is sufficient.

## `renderly-app`

Tauri 2 desktop app. Rust backend commands call into `renderly-core` exactly like the CLI
does — the Tauri command layer is another thin dispatcher, not a reimplementation.

One rendering surface: the webview. There is no separate native preview window (removed
in P4 — see [preview-webview.md](preview-webview.md) for the full migration history).

- **Webview** — UI chrome: media bin, timeline widget (canvas-rendered inside the webview),
  inspectors, dialogs, *and* the video preview canvas. Built with **React 19 + TypeScript +
  Zustand** (GUI rebuild M2; PLAN.md §3 left the framework choice open — React was picked
  for its ecosystem and because the canvas-heavy timeline benefits from keeping React out
  of the hot render path entirely, see below). State lives in one Zustand store
  (`renderly-app/src/store/editorStore.ts`); components read it via selectors and never
  call `invoke`/`listen` directly — `renderly-app/src/lib/ipc.ts` is the *only* file that
  imports `@tauri-apps/api`, so the backend surface is typed and grep-auditable in one
  place (`docs/command-api.md`'s command builders live in `lib/commands.ts` next to it).
  The window is **frameless** (`decorations: false`) with a custom titlebar in `TopBar`
  (drag region + in-app minimize/maximize/close via `WindowControls`). Chrome styling
  lives in `styles/tokens.css` (charcoal surfaces, ember accent `#ff5a3d`, Source Sans 3)
  and `styles/globals.css`; UI glyphs are **lucide-react** (no emoji icons). Timeline
  canvas colors read the same CSS vars through `timeline/theme.ts`.
- **Preview canvas** (`renderly-app/src/preview/WebviewPreview.tsx` +
  `webviewPreviewEngine.ts`) — a plain `<canvas>` DOM sibling positioned over the
  letterboxed content rect of `#preview-host`, driven by `renderly-wasm`'s
  `WasmCompositor` (the same `renderly-core` compositor compiled to wasm32/WebGPU that
  export uses natively). Browser-decoded `<video>`/`<img>` elements feed it frames with
  zero CPU readback (`copyExternalImageToTexture`); a Canvas2D fallback (no
  effects/masks/captions) covers browsers without WebGPU. Being a plain DOM element (not
  an OS-composited child window) means the handle/mask overlays
  (`PreviewHandlesOverlay` / `PreviewMaskOverlay`) are simple z-ordered siblings painted
  above it — no click-through/occlusion tricks needed. Both overlays patch the Zustand
  store directly during a drag (optimistic, no backend round trip); the canvas
  subscribes to the same store and redraws on every patch. See
  [preview-webview.md](preview-webview.md) for the full architecture and migration
  history.

**Timeline architecture:** the canvas timeline is deliberately *not* a React render
target. `renderly-app/src/timeline/renderer.ts` is a pure `(canvas, state) => void` draw
function (colors sourced from `timeline/theme.ts`, which reads `styles/tokens.css` custom
properties — no hex literals in `renderer.ts`, enforced by grep gate) and
`timeline/interactions.ts` is a mouse-event state machine (hit-test → drag → commit-on-
mouseup) that mutates the Zustand store directly. View state includes `scrollX`/`scrollY`
(wheel pan; Ctrl+wheel zoom anchored under cursor; middle-mouse pan) and a scrub mode on
the ruler/empty lane (drag playhead + snap guides). `components/timeline/TimelineCanvas.tsx`
is a thin host: one `<canvas>`, a `useEffect` that calls `renderTimeline` on relevant store
changes, and the `useTimelineInteractions` hook. This keeps 60fps drag feedback off React's
reconciler — the same drag-commit pattern the old vanilla-TS timeline used (optimistic
local mutation during drag, one command dispatched on mouseup), just running inside a
Zustand-store-backed React app instead of a hand-rolled `render()` loop.

**Containers (schema v7):** the canvas timeline shows one container at a time - the root
timeline or an open nested sequence. `src/lib/container.ts` is the single place that decides
which tracks "the timeline" is (`activeTracks(project, activeSequenceId)`; `containerView`
gives the project with those tracks swapped so `layout.ts` / `renderer.ts` / `snap.ts` need no
container awareness), and `TimelineBreadcrumb` + the store's `openSequence` / `closeSequence` /
`navigateToSequence` move between containers. Optimistic drag previews are computed on the
view and written back with `replaceActiveTracks`; commands are always the same global
command API (track ids are globally unique, `AddTrack` takes `sequence_id`). The preview and
native audio follow the open container (`WasmCompositor::set_active_sequence`,
`set_active_sequence` Tauri command -> `Project::with_sequence_as_root`). See
[schema-v7-design.md](schema-v7-design.md) "Implementation notes (app, phase B)".

Phase 2 MVP → GUI rebuild M2/M3 (complete): media bin (draggable cards),
canvas timeline with drag/trim/split/razor/snap/zoom/fit, DOM `TrackHeaders` column
(dbl-click rename, mute/lock/hide toggles, delete), cross-track drag, locked-track mouse
guard (hatched overlay; CLI/MCP can still edit a locked track by design), bin→timeline
drag with a ghost preview and CapCut-style auto-track-on-drop, right-click `ContextMenu`
(split/duplicate/copy/paste/delete/ripple-delete/enable-disable), full keymap (see
`App.tsx`'s `onKeyDown`), transport with editable timecode + ±1 frame buttons + fullscreen
preview (Esc exits; bounds recompute), aspect-ratio quick-switch (`RatioMenu` →
`SetProjectSettings`, including Original from first video media dims), tab rail
(Media/Audio/Text/Stickers/Effects/Transitions/Filters/Adjust/Extensions live),
  inspectors (video/audio clip with gain/fades/enable/trim, caption style gallery,
project canvas settings), Text panel auto-captions + Audio panel TTS voiceover, export
dialog with progress bar / ETA / cancel (M6), M7 polish (Coming Soon panels, empty states,
media skeletons, focus rings / scrollbars, `playback:error` toasts, pause-on-edit).
Thumbnails/waveforms are M4.

Tauri commands: `quick_start_project`, `new_project`, `open_project`, `save_project`,
`get_project`, `apply_command`, `apply_commands`, `undo`, `redo`, `export_project`,
`cancel_export`, `play`, `pause`, `seek`, `refresh_frame`, `scrub_audio` are all
`async fn`, dispatched off the main thread (see "Playback engine" below for why) — there
is no longer a sync-command exception now that the native preview's Win32-thread-affinity
requirement is gone (see P4 in [preview-webview.md](preview-webview.md)). `apply_commands`
(GUI rebuild M3) batches several `Command`s atomically — one project clone, one undo
snapshot, one save, one `project:changed` emit — for gestures that are logically a single
edit but need more than one core command (see command-api.md "Batch application"); it's a
thin Tauri-layer convenience over `renderly_core::apply_command`, not a second core API.
`export_project` clones the project and never holds `edit_lock` during encode; it accepts
preset shorthand (`"tiktok"` / `"youtube"`) or full `ExportPreset` JSON (including
`Custom`), throttles `export:progress` to ~10 Hz, and cooperates with `cancel_export`.

### Playback engine

Phase 2's original playback path froze the window on play: every Tauri command was a sync
`fn`, which Tauri 2 runs on the main thread, and the JS driver was a `setInterval` at
`1000/fps` calling `update_preview` — which called `render_frame_at`, creating a **new
wgpu Instance+Adapter+Device** and a fresh decoder map on *every tick*, and spawning
**ffprobe + ffmpeg per video layer per frame**. At 60 fps that's ~60 GPU device creations
and ~120 subprocess spawns a second, on the thread that also has to pump window messages —
hence "Not Responding".

**`renderly_core::export::FrameRenderer`** — a persistent renderer that holds one wgpu
`Compositor` and one open decoder per source media across repeated `render()` calls,
instead of rebuilding both per call — is what fixed that for the paths that still decode
video on the backend: `export_project`'s loop, project thumbnails, and the MCP live-agent
bridge's `render_frame` (each constructs its own short-lived `FrameRenderer`; see
`AppState::render_live_frame_png`).

**`renderly-app/src-tauri/src/playback.rs::PlaybackEngine`** no longer touches
`FrameRenderer` or video decode at all — since P4 (docs/preview-webview.md) the webview
owns 100% of preview rendering (browser decode → `renderly-wasm`'s compositor → the
preview `<canvas>`, see "Two rendering surfaces" above, now just one). What's left on the
backend is audio + tick-clock duties, owning two long-lived workers managed from
`AppState` so the playback commands (`play`, `pause`, `seek`, `refresh_frame`,
`scrub_audio`) never block the async runtime's worker pool for more than a clone + a
channel send:

- A **play-session worker thread**, spawned by `play(time_secs)` and torn down by
  `pause()`/a new `play()`/session switch. It pre-mixes timeline audio **once**, for
  `[time_secs, duration)`, into a temp WAV via `mix_timeline_audio_range_to_file` (a
  single ffmpeg filtergraph, not one spawn per playback chunk), starts a rodio `Sink`
  on it, then loops: `t = start + clock`, where `clock` is `Sink::get_pos()` when audio
  is present (rodio quantizes this to buffer boundaries, so it's read directly rather
  than blended with a wall-clock delta) or `Instant::elapsed()` when the timeline has
  no audio; emit `playback:tick` at ~30 Hz so the webview's own clock can slew to it.
  A `seek(time_secs)` call while playing writes into a one-slot `Mutex<Option<f64>>`
  that the loop checks every iteration and coalesces (a newer seek arriving before the
  loop observes an older one simply overwrites it) — on observing one it stops the sink,
  discards the temp WAV, and restarts the pre-mix/sink from the new position.
  `refresh_frame` live-swaps the session's `Project` (`PlaybackEngine::update_live_project`)
  so the *next* audio chunk reflects an edit without restarting the sink.
- An always-on **scrub-audio worker thread** serves `scrub_audio` calls (short audio-blip
  feedback while paused and dragging the playhead) — requests are coalesced (one-slot,
  latest wins) and each carries the `PlaybackEngine`'s `play_epoch` (bumped once per
  `play()` call) captured at submission time, so a blip queued just before the user hits
  Play is skipped rather than playing after playback audio has already started. The
  paused-state *visual* scrub redraw is entirely the webview canvas's own doing
  (`WebviewPreviewEngine.seek`) — this worker has no rendering concern at all.
- `pause()`/`stop()` join the play-session worker thread, which can block for as long as
  an in-flight audio pre-mix takes (multi-second, on a long timeline) if called right
  after `play()` starts — that join runs inside `spawn_blocking`, not inline in the
  async command handler, so it can't stall the tokio worker pool behind it (it does not
  make the *join itself* faster — that would need the pre-mix to be cancellable, a
  bigger change not yet done).

The frontend never advances the playhead itself for audio-clock purposes; it listens for
`playback:tick`/`playback:state` and slews its local rAF clock to them (see
docs/preview-webview.md's Architecture section for the webview side of this).

### Undo/redo

`renderly-app/src-tauri/src/lib.rs::History` is a bounded (100-entry) stack of full
`Project` snapshots — `undo: Vec<Project>`, `redo: Vec<Project>` — held in `AppState`
alongside the session, not in `renderly-core`. `apply_command` pushes the *pre-mutation*
project onto `undo` (and clears `redo`, as any new edit invalidates the old redo branch)
only after the command has actually succeeded; `undo`/`redo` pop a snapshot, push the
current project onto the other stack, and install the popped one as the session's project.

This does **not** violate AGENTS.md's "every edit through `apply_command`" rule: every
entry in the stack is a `Project` value that only ever came from a successful
`apply_command` call (or a previous undo/redo) — restoring one doesn't construct or mutate
project state through any path other than that. It's app-session-layer state management
(what to show right now), not a second way to edit a project. The command-log-based
undo/redo `renderly-core` eventually wants (see command-api.md's `Export` note on
serializable commands) is a different, more powerful mechanism (replay/audit/collab) that
can replace this later without changing the contract GUI/CLI/MCP see.

`quick_start_project`/`new_project`/`open_project` all clear history — undoing past a
project switch would try to restore a snapshot from a different project entirely.

**Serialization.** `AppState::edit_lock` (a `tauri::async_runtime::Mutex<()>`, i.e. a
`tokio` async mutex — needed because it's held across `.await` points, which a
`parking_lot` guard can't safely do without the `send_guard` feature) is acquired for the
full duration of `apply_command`, `apply_commands`, `undo`, `redo`, and project
open/create/quick-start, end to end: snapshot, compute, history push, session write-back,
and save. Earlier, these steps happened as several independent lock acquisitions
(`session`, then `history`, then `session` again), so two overlapping calls — e.g. a
double-tapped Ctrl+Z firing two `undo` invokes before the first resolved — could each read
the same pre-mutation project and race on which write-back landed last, silently
corrupting the undo/redo stacks. `edit_lock` makes that structurally impossible: every
whole-project mutation is now fully serialized, not just "usually fine because it's rare."

**Session-lock discipline.** `AppState::commit_project` is the only way any of the above
write a project back into the session: it holds `session`'s lock just long enough to swap
in the new `Arc<Project>` and read the save path, then does the actual serialize+`fs::write`
in `spawn_blocking`, outside any lock. Play/seek/refresh take `Arc::clone` of that snapshot
(no deep clone). This matters because `session`'s lock is also what
`play`/`seek`/`scrub_audio`/`get_project` need — a blocking disk write held under that lock
(the previous behavior) stalls all of those behind every single edit's save.

### Event contract

| Event | Payload | Emitted by |
|---|---|---|
| `playback:tick` | `{ time_secs: f64, playing: bool }` | ~30 Hz while playing; once on seek/EOF |
| `playback:state` | `{ playing: bool, time_secs: f64 }` | play start, pause, end-of-timeline |
| `playback:error` | `{ message: string }` | audio pre-mix failure mid-play (toast, no crash) |
| `project:changed` | `{ revision: u64, can_undo: bool, can_redo: bool }` | every successful `apply_command`, `undo`, `redo`, and on project open/new/quick-start |
| `media:thumbnails-ready` | `{ media_id, strip_path, cols, rows, tile_width, tile_height, interval_secs }` | media asset worker, on import and on project open (cache hit or fresh generation) |
| `media:waveform-ready` | `{ media_id, peaks: f32[], bucket_secs }` | media asset worker, same triggers as above |
| `export:progress` | `{ phase: "video"\|"audio"\|"mux", frame, total_frames, fraction }` | ~10 Hz during `export_project`; `cancel_export` stops at the next checkpoint |

### Tauri command surface (M7)

| Command | Notes |
|---|---|
| `quick_start_project` / `new_project` / `open_project` / `save_project` / `get_project` | Session I/O; clears history on open/new/quick-start; `get_project` returns `{ project, revision }` |
| `apply_command` / `apply_commands` | Core edits; `apply_commands` is one undo snapshot; both return `{ revision, patch, outcome(s) }` (RFC-6902 JSON Patch — app IPC only, not part of the core `Command` enum) |
| `undo` / `redo` | Session snapshot stacks (see Undo/redo); return `{ can_undo, can_redo, revision, patch }` |
| `export_project` / `cancel_export` | Progress via `export:progress`; clones project, no `edit_lock` |
| `play` / `pause` / `seek` / `refresh_frame` / `scrub_audio` | PlaybackEngine worker (audio + tick clock only — see Playback engine); play/seek/scrub take `Arc<Project>` snapshots |
| `request_media_assets` / `get_media_assets` | Thumbnail/waveform cache |

Mutating GUI edits pause playback first (CapCut-style) so decoders and audio don't race the
new project state.

`project:changed` carries `{ revision, can_undo, can_redo, mutation_id? }` but not the project
body or a patch. The frontend applies the patch returned by its own `apply_command` /
`apply_commands` / `undo` / `redo` invoke (B3), and only full-fetches via `get_project` for
external writers (MCP bridge with no matching `mutation_id`) or on patch-apply failure /
revision gap.

### Media assets (thumbnails + waveforms, M4)

`renderly-app/src-tauri/src/media_assets.rs` generates and caches a tiled thumbnail-strip
PNG (`renderly_core::generate_thumbnail_strip` — one ffmpeg call, not one spawn per
thumbnail) and waveform peaks (`renderly_core::audio_peaks`, reused as-is from the
perception module) for each imported media item, entirely off the edit path: it's a plain
`tauri::async_runtime::spawn` task, not gated by `AppState::edit_lock`, so importing media
or opening a project with many items never slows down `apply_command`/`undo`/`redo`.

- **Cache key:** `(path, mtime)` hashed with `std::hash::Hash`/`DefaultHasher` — not
  cryptographic, deliberately (see the doc comment on `cache_key`), since this only needs
  to be a stable local-disk cache key, not a security boundary. Cached in
  `{app_cache_dir}/media-cache/{key}.png` + `{key}.json` (the JSON sidecar holds
  cols/rows/tile dimensions/interval and the peaks array, so `get_media_assets` can answer
  synchronously from a cache hit with zero ffmpeg work).
- **Triggers:** automatically after a successful `ImportMedia` command, and for every
  media item already in the project on `open_project` (a cache hit there is a cheap file
  read, so this is safe to call unconditionally rather than tracking "did this project
  already get its assets"). `request_media_assets(media_id)` exists for the frontend to
  explicitly retry after a failure.
- **Delivery:** `media:thumbnails-ready`/`media:waveform-ready` events (see the table
  above) for the automatic paths, or synchronously via `get_media_assets(media_id)` for a
  cache-hit check with no generation triggered (used on mount/selection).
- **Serving the strip PNG to the webview:** the Tauri asset protocol, scoped to
  `$APPCACHE/media-cache/*` (`tauri.conf.json`'s `security.assetProtocol`) — the frontend
  converts the real filesystem path to a loadable URL via `convertFileSrc`
  (`lib/ipc.ts::assetUrl`), not base64-encoded over IPC.
- **Frontend rendering:** `timeline/renderer.ts` tiles the strip image across a video
  clip's rectangle, sampling the tile nearest each on-screen column's source time (mapped
  through `source_in_secs`/`source_out_secs`, so a trimmed clip shows only its trimmed
  sub-range); audio clips draw a `Path2D`-free (plain `fillRect` bars) waveform sliced the
  same way from the peaks array. `MediaPanel.tsx`'s bin cards show a hover-scrub
  filmstrip (mouse X position selects which strip tile to display via CSS
  `background-position`), CapCut-style.

Note: `perceive`'s MCP-facing thumbnail tool (letting an agent request a strip
independent of the GUI cache) is a follow-up, not yet wired up.

## Plugins (Phase 3)

Two tiers are implemented:

- **Asset packs** — data-only (`pack.json` + assets). See [asset-pack.md](asset-pack.md).
  Caption styles and `.cube` LUTs; transition entries alias builtins only.
- **WASM plugins** — sandboxed via `wasmtime`, frame-effect ABI (`memory` + `process`).
  See [plugin-api.md](plugin-api.md). Template: `examples/plugins/invert`.

Registry seed: `examples/registry/README.md` (no in-app browser yet).

## Why this shape

See PLAN.md §2 for the four architectural principles this graph exists to serve — in
short: one command API keeps AI and human editing paths identical by construction, and the
core/frontend split keeps the engine testable and the UI swappable.
