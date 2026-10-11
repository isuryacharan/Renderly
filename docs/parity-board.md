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
| Nested sequences / compound clips | ⬜ | Schema change — discuss in an issue first |
| Adjustment layers | ⬜ | Effects applied to everything below a track |

## Rendering & effects

| ID | Feature | State | Entry points / notes |
|---|---|---|---|
| — | WGSL effect system (color, blur, glitch, LUT…) | ✅ | Same code in preview and export |
| — | Transitions (10 built-in) | ✅ | `SetClipTransition` |
| P4-1a | Shape masks (rect/ellipse) + compositor alpha path | ✅ | schema v6; `SetClipMask`; `renderly-core/src/mask.rs` |
| P4-3b | Shape mask authoring UI | ✅ | Mask tool (M); `PreviewMaskOverlay`; inspector feather/invert |
| — | Raster / generated mattes in preview | ✅ | Host reads the file, wasm core composites it ([preview-webview.md](preview-webview.md) P4b). Not in preview: background removal without a baked matte (export builds a live heuristic matte) |
| — | Pack `.cube` LUTs in preview | ✅ | Same host-supplied path; parsed by the core `.cube` parser on wasm32 |
| P4-3 | Chroma key | 🟡 | `builtin:chroma_key`; CPU per-pixel, wants a WGSL pass |
| P4-1b | Background removal | 🟡 | `SetClipBackgroundRemoval`, `GenerateBackgroundMatte`; heuristic + optional `RENDERLY_SEG_CLI` |
| P4-ML | Core-hosted ONNX RVM/BiRefNet | ⬜ | **The headline open item.** See the recorded decision below |
| — | Captions burned in | ✅ | Preview and export share the `ab_glyph` rasterizer |
| — | Captions in the Canvas2D fallback | ⏸️ | WebGPU covers evergreen browsers; low value |
| — | Hardware decode (`-hwaccel`) / encode (NVENC/QSV) | ✅ | Probed, software fallback |
| — | Linked FFmpeg (`ffmpeg-the-third`) | ⏸️ | Waiting on vcpkg/`FFMPEG_DIR` across all environments |

## AI & automation

| ID | Feature | State | Entry points / notes |
|---|---|---|---|
| — | MCP server (full command API) | ✅ | `renderly-mcp` |
| — | Perception: scenes, silence, peaks, transcript, frame render | ✅ | |
| — | Live bridge to the running app (shared undo) | ✅ | [bridge-protocol.md](bridge-protocol.md) |
| — | Whisper auto-captions | ✅ | Local; `RENDERLY_WHISPER_MODEL` |
| — | TTS voiceover (local Piper / BYO cloud key) | ✅ | `GenerateVoiceover` |
| — | Ducking, fades, mixing | ✅ | |
| P4-2 | Audio denoise | 🟡 | `SetClipAudioDenoise` (`afftdn`); wants an RNNoise-class model |
| P4-4 | Motion tracking | ✅ | `TrackMotion`; Shi–Tomasi corners + pyramidal Lucas–Kanade in `renderly-core/src/tracking.rs`, optional region and rotation/scale. Frame-to-frame, no template re-anchoring |
| P4-5 | Stabilization | ✅ | `StabilizeClip`; RANSAC similarity per frame, smoothed camera path, auto-zoom capped at 1.4×. No rolling-shutter or parallax correction |
| P4-4b | Auto-reframe | 🟡 | `AutoReframeClip`; wants subject detection |
| P4-6b | Text-to-sticker | 🟡 | `GenerateSticker`; locally generated PNG, not an image model |
| P4-6 | Templates | ✅ | Pack `templates[]` + `ApplyTemplate` |
| P4-7 | Multicam groups + angle switch | ✅ | `CreateMulticamGroup`, `SetMulticamAngle` |

## Platform & distribution

| Feature | State | Notes |
|---|---|---|
| Windows | ✅ | Primary target, dogfooded daily |
| macOS / Linux — builds | ✅ | Green in CI on every push |
| macOS / Linux — runtime QA | ⬜ | **Wanted:** run it, report what breaks |
| Installer / packaged release | 🟡 | `tauri build` produces one; release flow being set up |
| Linux preview on WebKitGTK | ⬜ | WebGPU lags there; may need a Chromium-based webview — see [preview-webview.md](preview-webview.md) risks. (The native per-OS preview, including the Wayland subsurface path, was removed in that migration's P4.) |
| OTIO interop | ⬜ | PLAN.md §2; nobody has needed it yet |

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
- Chroma key as a WGSL pass — self-contained port of existing CPU code, sitting next to
  effects that already work that way.

## Recorded decision — inference runtime

WASM frame plugins instantiate per call and copy full RGBA, which is unsuitable for
RVM/BiRefNet. Phase 4.1 therefore ships **heuristic mattes** plus an optional
**`RENDERLY_SEG_CLI`** escape hatch (same shape as the Whisper integration). Linking `ort`
/ ONNX into core remains a follow-up pending AGPL-compatible license review — and never as
a user-loadable native DLL plugin.
