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

## Effect runtime (pack format 2, step 1)

Status: the runtime exists and is tested; the live pipeline does not use it yet. Design:
[effect-pack-format.md](effect-pack-format.md) (the normative parts move here and into
asset-pack.md when the format ships).

`renderly-core/src/compose/runtime/` runs an effect, filter or transition described by an
`effect.json` manifest plus WGSL passes. It is cross-target and touches no filesystem: the
manifest, shader text, decoded textures and LUTs arrive as data.

- `manifest.rs`: `EffectManifest::parse` reads format 1. Unknown item kinds, `requires`
  features, pass types, pass inputs, param types and derived types yield
  `ManifestError::NeedsNewer` (the item is installed but disabled). Everything else is
  validated: identifiers, WGSL reserved words, ranges and defaults, expressions, pass wiring
  (a target must be written before it is read, the last pass writes `out`) and the limits
  (32 params, 16 passes, 8 targets, 8 textures, 2 LUTs). Unknown optional fields are ignored.
- `expr.rs`: the `expr.v1` shaping language: `+ - * / %`, comparisons, `&& || !`, `?:`, and
  the built-ins in the design. Types are `num` and `bool`; a bool coerces to 0/1 in numeric
  positions, a number is never a condition. Division by zero and non-finite results are
  evaluation errors. At most 256 nodes.
- `prelude.rs`: generates each pass's WGSL prelude (`Host`, `Params`, the group 0 uniforms and
  clamp samplers, one binding per input at group 1, one sampler per file texture at group 2,
  and a fullscreen `vs_main`). `UniformLayout` lays out `Params` with WGSL alignment and
  padding fields `_padN`; the host block is fixed at 40 bytes. An input named after a WGSL
  reserved word (`from`, `filter`, `target`, ...) is bound as `in_<name>`.
- `executor.rs`: `CompiledEffect::load(json, shaders, textures)` validates the manifest and
  checks each pass's prelude + author code with naga (`wgsl-in`, the validator, and a fragment
  entry point), so a broken item fails at load. `EffectExecutor::run` encodes the passes on a
  caller's device and encoder, clears each target to transparent, binds inputs by name
  (`src`, `from`, `to`, LUT keys, targets, `prev`, file textures) and returns an
  `EffectOutput`. Targets and outputs come from a pool keyed by size and format;
  `recycle` returns an output. Pipelines and file textures are cached per item content hash.
  No state is kept between runs, so frame N is the same cold or after frames 0..N-1.
- `lut.rs`: `.cube` LUTs as `texture_3d` in `Rgba8Unorm`, using the same bytes as the pack LUT
  path. PNG LUT layouts (grid, strip, Hald) are not read yet.
- `compose/builtins/`: format-2 sources for `blur` (two passes, a `blurred` target, `radius`)
  and `crossfade` (transition), embedded with `include_str!`. `tests/effect_runtime.rs` checks
  both against the committed golden frames (`fx_blur_t0500ms`,
  `transition_crossfade_t{1250,1500,1750}ms`) bit for bit, and checks seek purity.

Not yet: the builtin dispatch in `compose/effects` and `compose/transition.rs` still runs the
hard-coded passes, and the other four effects and eight transitions are not ported. Items
are not yet loaded from packs (no `pack.json` format 2, library or store), there is no
`describe_*` output, no hash-based ref resolution, and no importer. Compute passes, meshes,
feedback and AI inputs are reserved features and are rejected as `NeedsNewer` today. The
naga dependency is direct (`renderly-core/Cargo.toml`) at the version wgpu already uses.

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

## In-app agent tools (2026-10-16)

The desktop app hosts the agent's tools itself, for the Claude Code session. Full design:
[agent-chat-design.md](agent-chat-design.md) §0.

```
renderly-app (Tauri)                               Claude Code (user's login)
  AppState ──apply_commands_inner──┐                 --mcp-config: http, bearer token
  History (undo groups)            │                         │
  agent_chat/mcp_http.rs ◄─────────┼── Streamable HTTP ──────┘  127.0.0.1:<random>
    rmcp server: RenderlyTools     │
      edit · timeline · look       │   same command dispatch as the GUI
      find_media · assets          │   (AGENTS.md §0.1); project:changed emitted
      permission_prompt ───────────┘
```

- `renderly-core` has a non-default `schema` feature. It derives `schemars::JsonSchema` on
  `Command` and the types it reaches, so the `edit` tool's schema is the real enum. The default and
  wasm builds do not include it.
- Each session has its own endpoint and token, which stop with the session.
- `renderly-mcp` is unchanged. It still serves external agents. The in-app Codex driver no longer
  uses it: Codex reaches the same in-process endpoint as Claude Code (agent-chat-design.md §0.8).

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

Undo/redo history lives in `renderly-core` (`renderly_core::history`, UI-free), so the app,
the live bridge, the MCP server and the CLI share one model. The full design (records,
transactions, coalescing, preview overrides and the memory bound) is in
"History, transactions and preview overrides" below. In short: `renderly_core::history::History<S>`
is a bounded stack of state snapshots, and the app instantiates it with `Arc<Project>`.
Each record holds the state *before* its change, so undo and redo are the same swap between
two stacks.

`renderly-app/src-tauri/src/lib.rs` keeps one `History` in `AppState`. `apply_command`
records the *pre-mutation* project only after the command has actually succeeded, so a
rejected command leaves no record. `undo`/`redo` swap the session's project with the
recorded snapshot.

This does **not** violate AGENTS.md's "every edit through `apply_command`" rule. Every
snapshot in the stack is a `Project` value that only ever came from a successful
`apply_command` call (or a previous undo/redo). Restoring one does not construct or mutate
project state through any other path, so this is history storage, not a second edit path.
The command-log undo that `renderly-core` may adopt later (see command-api.md's `Export`
note) can replace the storage without changing the contract that GUI, CLI and MCP see.

`quick_start_project`/`new_project`/`open_project` all clear history. Undoing past a project
switch would restore a snapshot from a different project.

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

### Agent chat (`src-tauri/src/agent_chat/`, Phase 1)

Status: implemented; no full turn has been run against the real Claude Code or Codex CLI. Codex's
handshake and model list, and Claude Code's model list, were checked live (see
[agent-chat-design.md](agent-chat-design.md) §0 and §0.10). Nothing here is in `renderly-core`; the
module is a UI-layer dispatcher, like the bridge.

Three drivers share one tool set (`tools.rs`, a `ToolHost` trait): Claude Code and Codex reach it
through the in-process MCP endpoint (`mcp_http.rs`), and Ollama calls it directly in its own loop.

| File | Role |
|---|---|
| `driver.rs` | `AgentDriver` / `AgentSession` traits, `AgentId` (Claude Code, Codex, Ollama), `LaunchOptions` (model and effort are the agent's own values, not fixed lists), `SessionConfig`, launch-file helpers |
| `claude.rs` | Claude Code driver: `claude -p --input-format stream-json --output-format stream-json`, `renderly` as the only MCP server, `--permission-prompt-tool` routed to the app. Also the model-list probe (`control_request` `initialize`). Documents the flags and message shapes it relies on |
| `codex.rs` | Codex driver: `codex app-server` (JSON-RPC over JSONL), with the MCP endpoint as `-c` overrides and the token in the child's environment. Also the `model/list` probe. Protocol shapes are cited against the 0.163.0 schema in the file header |
| `ollama.rs` | Ollama driver: Renderly runs the loop over `/api/chat` (streamed) with a slim tool set, a 16-round cap, and cancel by aborting the stream. Also the model list from `/api/tags` and `/api/show` |
| `models.rs` | `AgentModels` / `ModelOption` / `ModelStatus` and the pure parsers for each agent's list |
| `tools.rs` | The Renderly tool functions over a `ToolHost` (one implementation, used by MCP and Ollama) |
| `events.rs` | `AgentEvent` (one enum for every driver) and `AgentEnvelope` (`session_id` + event), sent as the `agent:event` Tauri event |
| `permission.rs` | `PermissionBroker`: classifies each tool (perception / edit / file write / model download / native / unknown), applies the §4.3 defaults, holds pending prompts and session-scoped "always" answers |
| `detect.rs` | `agent_list`: `PATH` lookup, `--version`, and `claude auth status` (exit code only; no credential files read) |
| `process.rs` | PATH resolution (`.exe`/`.cmd`/`.bat` on Windows), no-console spawn (mirrors `renderly_core::proc::command`), tree kill, stderr tail |
| `mod.rs` | `AgentChatHub` (one active session), Tauri commands, the context preamble, `mcp_binary_path`, `stop_active` (project close and app exit) |

Flow: the webview calls `agent_start`; the backend spawns the CLI in the project's folder
with `renderly-mcp --project <open project>` registered. Each prompt goes through
`agent_send`; the CLI's stdout is normalised into `AgentEvent`s. When the agent asks to use a
tool outside the pre-allowed read-only set, Claude Code calls the `permission_prompt` tool of
`renderly-mcp`, which calls the bridge method `agent_permission`. `bridge.rs` hands it to
`agent_chat::bridge_permission`, which waits for the user's answer in the Agent tab. Edits
then reach the open project through the normal bridge `apply_command` path and share the
undo stack.

Tests: `cargo test -p renderly-app` runs unit tests and `tests/agent_chat.rs`, which drives the
real drivers against `tests/fake_agent` (a test-only binary that speaks the two stdio
protocols). No vendor CLI or network is involved.

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

## History, transactions and preview overrides

Design status: specified here and in [command-api.md](command-api.md), which holds the
command-facing half. Implemented in `renderly-core::history` and
`renderly-core::compose::preview_overrides`.

### History in core

`renderly_core::history::History<S>` is generic over the state type `S: Clone`. It holds no
UI, time source or I/O. The app instantiates it with `Arc<Project>`, so each snapshot is a
cheap reference-count bump. A **record** holds:

- `label`: the display name (the first command's name, plus `+N` when there are more).
- `commands`: the command names it applied, bounded (the first 32 are kept, with the true
  count alongside). Used for agent and MCP display.
- `merge_key`: optional, see Coalescing.
- `before`: the state to restore on undo. This is the snapshot model. An inverse is not
  needed, because a project's state is small enough to hold whole, and snapshotting is the
  simplest proven mechanism the app already used.

Undo and redo are the same swap. `undo(current)` pops a record from `undo`, pushes a record
holding `current` onto `redo`, and returns the popped `before` for the caller to install.
`redo` does the mirror image. Any new record clears `redo`.

**Memory bound.** Each stack holds at most `DEFAULT_CAP = 100` records, and the oldest is
evicted first. Each record holds one whole-project snapshot, so the worst case is about 100
project copies. For typical projects that is a few MB each. For very large projects the
bound is still count-based, and a byte budget is future work.

### Transactions

`begin(label)` ... `commit()` / `rollback()`:

- Everything recorded between `begin` and `commit` is **one** record, labelled with the
  transaction's label and listing the commands it applied.
- Transactions nest by depth counting. An inner `begin` folds into the outer transaction, and
  only the outermost `commit` pushes a record. `commit` returns whether it pushed one.
- `rollback()` aborts the **outermost** transaction, however deeply nested. It returns the
  state recorded when the first change in the transaction arrived, so the caller can restore
  it. Nothing is pushed. A rollback of a transaction that changed nothing returns `None`.
- A transaction that changed nothing commits nothing.
- `undo`/`redo` while a transaction is open first commit it, so undo never runs against a
  half-open group.
- `clear()` (project open, close or create) also drops any open transaction.

Users of transactions:

- **Agent turns** (app): `begin("Agent turn")` when the turn is sent, and `commit` at
  `TurnFinished`. This replaces the earlier `begin_group`/`end_group` pair, which kept its own
  base-index bookkeeping.
- **Batches** (`apply_commands`): one record per batch. The batch is already atomic, so it is
  a record rather than a transaction.
- **Batch "apply to all"** (later, see docs/schema-v8-design.md): one command applying to many
  targets is one record.
- **CLI `apply-script`**: one transaction. Any failing command rolls the whole script back.

### Coalescing

A command may carry an optional **merge key**, a string such as `transform:<clip id>`,
`effects:<clip id>:<effect instance id>` or `opacity:<clip id>`.

- The key is supplied in the request **envelope**, next to the command, as a `merge_key`
  field (Tauri `apply_command`, bridge `apply_command`). It is never part of the `Command`
  enum, so the command schema stays clean.
- If the top record has the same `merge_key`, was last touched within `COALESCE_WINDOW`
  (1 s by default), and its gesture is still open, the new change **amends** that record. The
  record keeps its original `before`, so one undo restores the value from before the whole
  gesture.
- A keyless command, a different key, an expired window or an ended gesture each start a new
  record.
- **Gesture end.** `end_gesture()` (Tauri `end_gesture`, bridge `end_gesture`) closes the top
  record's gesture by clearing its key, so the next change with that key starts a new record.
  The UI calls it on mouse-up or key-up. Without an explicit end, the window ends a gesture
  after one second of inactivity.
- Transaction records never coalesce. A merge key inside an open transaction is ignored,
  because the whole transaction is already one record.

Time is passed in by the caller (`record(..., now: Instant)`), so the core never reads a
clock. Core stays wasm-buildable, and tests use synthetic instants.

### Preview overrides

A **preview override** is a temporary change to what a clip *renders*. It is not a project
change: it is not saved, not in undo, and not a `Command`.

- Target: a clip id (media or adjustment). An override may set any of these fields:
  - `transform`: a full `ClipTransform` replacing the clip's static transform. Keyframes on a
    property still win over it, exactly as they win over the saved static transform.
  - `opacity`: a value replacing the clip's opacity (adjustment layers use their own `opacity`
    field; media clips use `transform.opacity`).
  - `effects`: a full effect stack replacing the clip's `effects`, in `EffectInstance` shape
    (`{id, effect_id, enabled, params}`). Hover preview sends the current stack plus one
    temporary instance.
- API (core): `PreviewOverrides` with `set(target, override)`, `clear(target)`, `clear_all()`,
  `is_empty()` and `apply(&Project) -> Cow<Project>`. `apply` borrows when there are no
  overrides, so the common path costs nothing. Unknown targets are ignored.
- **Where it applies.** `apply` runs at the top of the shared render path, before
  `eval::active_steps` and `build_scene`. Every consumer of that path honours an override the
  same way:
  - the native `FrameRenderer` (used by the app's live `render_frame`, and by the MCP `look`
    and `render_frame` tools through the bridge);
  - the wasm `WasmCompositor` (the webview preview), which applies overrides when it rebuilds
    its project view;
  - the golden suite's `PreviewPipeline` stand-in, which has a parity case for an override.
- **Export does not apply overrides.** `export_project` renders the saved project only, so a
  temporary hover effect or slider position can never reach an exported file. This is a
  deliberate choice: the override channel is for previews and agent perception, not a second
  edit path.
- Overrides never appear in the saved project. `apply` leaves keyframes and static fields
  unchanged, and the clone it makes is discarded after the frame is rendered.

### How each surface uses it

| Surface | History | Coalescing | Overrides |
|---|---|---|---|
| App (Tauri) | `begin_transaction(label)`, `commit_transaction()`, `rollback_transaction()`, `undo`, `redo`. `apply_command` and `apply_commands` take an optional `merge_key` | `apply_command` `merge_key`, `end_gesture()` | `set_preview_override(target, override)`, `clear_preview_override(target)`. These also drive `render_frame` |
| Bridge | Same methods, mirrored in [bridge-protocol.md](bridge-protocol.md) | Same `merge_key`, `end_gesture` | Same two methods |
| MCP | Agent turns run in a transaction automatically. `undo`/`redo` tools call the bridge | Passes `merge_key` through `apply_command` | `render_frame` and `look` honour overrides set in the live app |
| CLI | `apply-script` is one transaction, rolled back on any failure | n/a (one process) | n/a (headless; no live app) |
| Webview (wasm) | None (reads the project from the store) | The store passes `merge_key` on drags | `set_preview_override(json)`, `clear_preview_override(target)`, `clear_all_preview_overrides()` |

Live drags use this pattern. The UI shows the override on every change, then commits **one**
command with a merge key on release. Undo after a drag restores the pre-drag value in one
step. Hover preview uses the same override channel, with no command and no record.
