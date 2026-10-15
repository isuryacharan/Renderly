# Feature parity board

What Renderly does today, what's open for contribution, and what's deferred on purpose.
This is the board PLAN.md §4 calls for — pick work from here.

**Legend:** ✅ shipped · 🟡 works, wants a better implementation · ⬜ open · ⏸️ deferred

Open an issue saying what you're picking up before starting large work, and read
[CONTRIBUTING.md](../CONTRIBUTING.md) plus [AGENTS.md](../AGENTS.md) §0 first. The
highest-leverage contributions are the 🟡 rows: the command, the GUI surface, and the tests
already exist, so the work is swapping the algorithm underneath a stable API.

## Editing core

| Feature | State | Entry points |
|---|---|---|
| Multi-track timeline (video/audio/caption) | ✅ | Array order = compositing z-order |
| Trim / split / move / ripple, snapping, groups, razor | ✅ | `timeline/interactions.ts`, command API |
| Track reordering | ✅ | `MoveTrack`; drag the track header grip |
| Keyframe animation + easing curves | ✅ | `project/anim.rs`; transform, opacity, volume |
| Speed ramp with pitch-corrected audio | ✅ | `SetClipSpeed` |
| Undo/redo | ✅ | Shared with agent edits over the bridge |
| Nested sequences / compound clips | ✅ | schema v7; `CreateSequence`, `NestClips`, `UnnestClip`. Double-click a nested clip to open it; Ctrl+G nests a selection. A sequence with its own size previews stretched to the project canvas while open. [schema-v7-design.md](schema-v7-design.md) |
| Adjustment layers | ✅ | schema v7; `AddAdjustmentLayer`, `SetAdjustmentLayer`. Track-header menu; inspector with effects, opacity keyframes and mask. No WASM-plugin effects; not drawn in the Canvas2D fallback |

## Rendering & effects

| ID | Feature | State | Entry points / notes |
|---|---|---|---|
| — | WGSL effect system (color, blur, glitch, LUT…) | ✅ | Same code in preview and export |
| — | Transitions (10 built-in) | ✅ | `SetClipTransition` |
| P4-1a | Shape masks (rect/ellipse) + compositor alpha path | ✅ | schema v6; `SetClipMask`; `renderly-core/src/mask.rs` |
| P4-3b | Shape mask authoring UI | ✅ | Mask tool (M); `PreviewMaskOverlay`; inspector feather/invert |
| — | Raster / generated mattes in preview | ✅ | Host reads the file, wasm core composites it ([preview-webview.md](preview-webview.md) P4b). Not in preview: background removal without a baked matte (export builds a live heuristic matte) |
| — | Pack `.cube` LUTs in preview | ✅ | Same host-supplied path; parsed by the core `.cube` parser on wasm32 |
| P4-3 | Chroma key | ✅ | `builtin:chroma_key`; WGSL pass in the shared compositor (export and preview), optional `spill` suppression. Keys on RGB distance |
| P4-1b | Background removal | ✅ | `SetClipBackgroundRemoval`, `GenerateBackgroundMatte`; "Fast (heuristic)" or "AI" (below). `RENDERLY_SEG_CLI` still available |
| P4-ML | AI video matting (Robust Video Matting) | ✅ | In-process with `tract`; per-frame matte baked to a PNG sequence (~107 ms/frame, about 3× slower than realtime at 1080p30). Model (15 MB, GPL-3.0) is downloaded on consent, never bundled. Preview can lag the matte by a frame while scrubbing; mattes live in the temp dir and need regenerating if it is cleared. Stills-quality matting (BiRefNet) not done |
| — | Captions burned in | ✅ | Preview and export share the `ab_glyph` rasterizer; translucent boxes and shadows keep their alpha |
| — | Captions in the Canvas2D fallback | ⏸️ | WebGPU covers evergreen browsers; low value |
| — | Hardware decode (`-hwaccel`) / encode (NVENC/QSV) | ✅ | Probed, software fallback |
| — | Linked FFmpeg (`ffmpeg-the-third`) | 🟡 | Foundation only: probe/decode backend seam, off-by-default `linked-ffmpeg` feature with a linked probe (uncompiled locally), informational Linux CI job. Linked decode, Windows/macOS libs and packaging remain — [linked-ffmpeg.md](linked-ffmpeg.md) |

## AI & automation

| ID | Feature | State | Entry points / notes |
|---|---|---|---|
| — | MCP server (full command API) | ✅ | `renderly-mcp` |
| — | Perception: scenes, silence, peaks, transcript, frame render | ✅ | |
| — | Live bridge to the running app (shared undo) | ✅ | [bridge-protocol.md](bridge-protocol.md) |
| — | Whisper auto-captions | ✅ | Local; `RENDERLY_WHISPER_MODEL` |
| — | TTS voiceover (local Piper / BYO cloud key) | ✅ | `GenerateVoiceover` |
| — | Ducking, fades, mixing | ✅ | |
| P4-2 | Audio denoise | ✅ | `SetClipAudioDenoise`; `rnnoise` backend (`nnnoiseless`, in-process, cached per source) alongside `afftdn`. Weak on steady white hiss; strong on hum, rumble and noise between words |
| P4-4 | Motion tracking | ✅ | `TrackMotion`; Shi–Tomasi corners + pyramidal Lucas–Kanade in `renderly-core/src/tracking.rs`, optional region and rotation/scale. Frame-to-frame, no template re-anchoring. Can pin another clip (sticker, overlay) to the tracked point |
| P4-5 | Stabilization | ✅ | `StabilizeClip`; RANSAC similarity per frame, smoothed camera path, auto-zoom capped at 1.4×. No rolling-shutter or parallax correction |
| P4-4b | Auto-reframe | ✅ | `AutoReframeClip`; follows the main face (bundled YuNet, run with `tract`) across the whole frame by keyframing the cover-fit pan (`fit_x`/`fit_y`). Faces only — no body or object subjects |
| P4-6b | Text-to-sticker | 🟡 | `GenerateSticker`; locally generated PNG, not an image model |
| P4-6 | Templates | ✅ | Pack `templates[]` + `ApplyTemplate` |
| P4-7 | Multicam groups + angle switch | ✅ | `CreateMulticamGroup`, `SetMulticamAngle` |
| — | In-app agent chat | 🟡 | Phase 1: "Agent" tab beside the inspector (`Ctrl+L`) driving the user's installed, signed-in Claude Code or Codex CLI — no login or API key handled by Renderly. Edits go through `renderly-mcp --project` with shared undo. Confirmed working in the desktop app with Claude Code (2026-10-16); the Codex driver is not yet run against a real `codex` binary. [agent-chat-design.md](agent-chat-design.md) |

## Platform & distribution

| Feature | State | Notes |
|---|---|---|
| Windows | ✅ | Primary target, dogfooded daily |
| macOS / Linux — builds | ✅ | Green in CI on every push |
| macOS / Linux — runtime QA | ⬜ | **Wanted:** run it, report what breaks |
| Installer / packaged release | 🟡 | `tauri build` produces one; release flow being set up |
| Linux preview on WebKitGTK | ⬜ | WebGPU lags there; may need a Chromium-based webview — see [preview-webview.md](preview-webview.md) risks. (The native per-OS preview, including the Wayland subsurface path, was removed in that migration's P4.) |
| OTIO interop | ✅ | Pure-Rust `.otio` import and export ([otio.md](otio.md)): `renderly export-otio` / `import-otio`, MCP `export_otio` / `import_otio`, app top bar. Renderly-only state rides in `metadata["renderly"]`, so a round trip is lossless apart from frame-snapping |

## Testing & infrastructure

| Area | State | Notes |
|---|---|---|
| `renderly-core` tests | ✅ | 107 |
| `renderly-app` (Rust) tests | ✅ | 16 |
| `renderly-mcp` tests | ✅ | 43 — headless tool handlers, a fake loopback bridge, and a stdio JSON-RPC end-to-end run of the built binary |
| `renderly-cli` tests | ✅ | 19 — 16 run the built binary through every subcommand, including a real export |
| Frontend tests | ✅ | Vitest, 260 — snapping, layout, preview/mask math, command builders checked against [command-api.md](command-api.md), the editor store. `npm test`; not yet covered: `timeline/interactions.ts`, components |
| Export golden frames | ✅ | `renderly-core/tests/golden_frames.rs` — 40 committed frames: every built-in effect and transition, masks, keyframes, captions. `RENDERLY_UPDATE_GOLDEN=1` regenerates |
| Preview vs export parity | 🟡 | Both paths build layers through `ComposeLayer::from_active`; the suite runs a native stand-in for the wasm compositor against export. Not exercised: browser decode, external-image upload, the surface blit — that needs a headless WebGPU browser run |

## Good first issues

- macOS or Linux runtime QA: install, run, file what breaks.
- An asset pack — caption styles, LUTs, SFX, transitions
  ([`examples/packs/starter`](../examples/packs/starter) is the template).
- A WASM plugin effect ([`examples/plugins/invert`](../examples/plugins/invert), ~30 lines).

## Recorded decision — inference runtime

Models run in core through **`tract`** (pure Rust, no native library), native builds only.
Licences were reviewed in [ml-licence-review.md](ml-licence-review.md): YuNet (MIT) is
bundled; matting weights are never bundled — they are downloaded from the upstream host on
first use, with consent and a pinned SHA-256, into the per-user model cache. `ort` / ONNX
Runtime is a fallback only for a model `tract` cannot run, with the library path fixed to
our own bundled copy, and never as a user-loadable native DLL plugin. The
`RENDERLY_SEG_CLI` escape hatch remains.
