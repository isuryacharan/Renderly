# Renderly — Open-Source, AI-Native Video Editor

> Working name: **Renderly** (renamed from Uppercut in July 2026).
> Alternates if it conflicts later: **Smashcut**, **Reelforge**, **Crosscut**.
> Positioning: "The video editor built for AI agents — and the humans who finish the job."
> Never market as a CapCut clone. Say: *an open-source alternative to paywalled editors*.

## 1. Vision

A blazingly fast, native, cross-platform video editor where:

- **AI agents are first-class users.** Claude Code (or any model) can drive the entire edit — import, clip, order to a script, caption, voiceover, export — via a built-in MCP server. The human gives final touches in the GUI.
- **The community owns the feature set.** Effects, transitions, stickers, filters, caption styles, and integrations are plugins and asset packs, not core code.
- **Nothing is paywalled. Ever.** AI features are local-first (Whisper, local TTS) with bring-your-own-API-key for cloud services.

License: **AGPL-3.0** (forks and hosted services must stay open).

## 2. Core architectural principles

These four decisions make or break the project — everything else is negotiable.

1. **One command API for humans and AI.** Every edit operation (split clip, move clip, add caption, apply effect…) is a serializable command. The GUI dispatches commands; the MCP server dispatches the *same* commands; the CLI dispatches the same commands. Benefits: AI has 100% feature parity with the GUI by construction, undo/redo falls out of the command log, and collaborative/scripted editing becomes possible later.

2. **Text-first project format.** The project file is stable, documented, human-readable JSON. An AI can read and reason about the whole edit; git can diff it; the community can build tooling around it. Support OpenTimelineIO import/export for interop with pro tools.

3. **The engine never touches the UI.** `renderly-core` is a headless Rust library. The Tauri app, the MCP server, and the CLI are all thin frontends. The engine must be able to render an entire video with no window open.

4. **AI needs eyes.** The MCP server exposes not just edit commands but *perception*: render frame N as an image, get audio waveform peaks, get the transcript, run scene/silence detection. A model that can see its output can iterate; one that can't is editing blind. This is the killer feature no existing editor has.

## 3. Tech stack

### Engine — Rust (`renderly-core`)

| Concern | Choice | Notes |
|---|---|---|
| Media decode/encode | FFmpeg (via `ffmpeg-the-third` or FFI) | *As built: FFmpeg CLI subprocess; hardware accel via probed `-hwaccel`/NVENC/QSV flags. Linked FFmpeg deferred — see §3.1.* |
| GPU compositing | `wgpu` (DX12 on Windows, Vulkan/Metal elsewhere) | Render graph; every effect/transition is a WGSL shader node. *Also compiled to wasm32/WebGPU for the preview.* |
| Audio engine | `cpal` (output) + `symphonia` (decode) + `rubato` (resample) | *As built: FFmpeg premix + `rodio` sink — see §3.1.* |
| Text/captions | `cosmic-text` + `glyphon` (GPU text) | *As built: `ab_glyph` CPU rasterization composited as a normal layer — works identically in export and in the wasm preview.* |
| Speech-to-text | `whisper-rs` (whisper.cpp) | Local auto-captions with word-level timestamps. |
| TTS voiceover | Local: Piper/Kokoro. Cloud: BYO keys (ElevenLabs, OpenAI TTS) | Voice-over track generated from script text inside the editor. |
| Scene/silence detection | FFmpeg scene filter + custom audio analysis | Feeds the AI workflow: "find the kill at ~2:14". |
| Project model | `serde` JSON, versioned schema | Command-sourced edits; OTIO interop. |
| Plugins | `wasmtime` (WASM) | See §5. |

### App — Tauri 2 (`renderly-app`)

- **Frontend:** ~~Svelte (or React — pick by contributor comfort)~~ → **React 19 + Zustand** + TypeScript (as built).
- **Timeline:** custom canvas-rendered timeline (virtualized; never DOM-per-clip).
- ~~**Video preview: native, not webview.**~~ → **Superseded (2026-07): the preview renders
  in the webview**, on a canvas driven by `renderly-core`'s own compositor compiled to
  wasm32 + WebGPU (`renderly-wasm`). Frames go decoder→GPU texture→canvas with no CPU
  readback and no per-frame IPC, measured at 60 fps on 1080p60 with ~0.3 ms draw cost — so
  the "electron-feeling" concern this bullet was written to prevent did not materialize.
  The native child-window path was deleted; see the rationale and evidence in
  [docs/preview-webview.md](docs/preview-webview.md).
- Webview hosts everything: preview, panels, inspectors, dialogs, media bin.

### AI interface (`renderly-mcp`, `renderly-cli`)

MCP server (stdio + optionally HTTP) shipping with the app. Tool surface, roughly:

- **Project:** create/open/save project, get project state (the JSON), list media.
- **Ingest:** import files/folders, probe metadata, generate proxies.
- **Perceive:** transcript (with word timestamps), scene cuts, silence spans, audio peaks, render frame at time T as PNG, render low-res preview clip.
- **Edit:** every command from the command API (split, trim, move, ripple-delete, add track, add caption with style X, apply transition/effect, set keyframe, add TTS voiceover from text…).
- **Deliver:** export with preset (TikTok 9:16, YouTube 16:9, etc.), report progress.

Plus an `AGENTS.md`/instructions file in the repo so any coding agent knows how to drive it well.

### Platforms

Windows first (your machine, fastest dogfooding). Code stays cross-platform-clean (wgpu + FFmpeg + Tauri are all cross-platform); macOS/Linux CI builds turn on in Phase 3–4. Mobile: out of scope; the engine/UI split keeps the door open.

### 3.1 As-built deviations (accepted)

This plan is the original strategy document, kept as written. Where the implementation
knowingly diverged, the decision and its rationale are recorded here rather than silently
drifting.

| Planned | As built | Why |
|---|---|---|
| Native `wgpu` child-window preview | Webview canvas driven by `renderly-wasm` (core's compositor on WebGPU) | OS child windows always composite *above* webview DOM, so direct-manipulation handles were unfixably hidden behind the video. The webview path measured at parity (60 fps 1080p60, ~0.3 ms draw), and sharing one compositor gives preview/export effect parity by construction. ~2,900 lines of per-OS window code deleted. [docs/preview-webview.md](docs/preview-webview.md) |
| `cpal` + `symphonia` + `rubato` audio | FFmpeg premix → `rodio` sink (backend owns the master clock) | Reuses the filtergraph that export already needs; segmented premix fixed the startup latency. Re-evaluated during the preview migration and kept. Revisit if audible trim-drag scrubbing is wanted. |
| `cosmic-text` + `glyphon` GPU text | `ab_glyph` CPU rasterization, composited as a layer | Simpler, and it compiles to wasm32 — which is what lets preview and export share one caption implementation. |
| `ffmpeg-the-third` / linked FFmpeg | FFmpeg CLI subprocess behind a `VideoReader` boundary | Zero build-environment friction across contributors; the boundary keeps the migration open. Deferred until vcpkg/`FFMPEG_DIR` is wired everywhere. |
| Svelte frontend | React 19 + Zustand | Contributor comfort, per the original bullet's own escape clause. |
| MCP over stdio **+ HTTP** | stdio + a **loopback WebSocket bridge to the running app** | HTTP solved remote access, which nobody needed; the bridge solved co-editing, which turned out to be the actual product story — agent edits land in the open GUI and share the human's undo stack. [docs/bridge-protocol.md](docs/bridge-protocol.md) |
| OTIO interop (§2) | Not built | No one has needed it yet; tracked on the parity board. |
| Phase 4 ML models (RVM/BiRefNet) | Heuristics + optional `RENDERLY_SEG_CLI` | WASM plugins can't host these efficiently, and linking ONNX needs AGPL-compatible license review. See the recorded decision in [docs/parity-board.md](docs/parity-board.md). |

## 4. Feature roadmap

Phased for a solo dev + Claude Code, side-project pace. Every phase ends in something you actually use for Ultra Bruno videos.

> **Status (2026-07):** Phases 0–3 shipped; Phase 4 is in progress with heuristic
> implementations landed. Live status per feature: [docs/parity-board.md](docs/parity-board.md).
> Work delivered beyond this roadmap: the live agent bridge (co-editing with shared undo),
> the WASM preview compositor, the project home screen / autosave lifecycle, and the
> design system.

### Phase 0 — Skeleton (~first month)
- Repo, CI (Windows build + test), AGPL license, contributing docs.
- Project schema v0 + command API core.
- Prove the spine: FFmpeg decode → wgpu texture → encode. CLI renders a cuts-only timeline JSON to MP4.

### Phase 1 — Headless AI editor (months 2–4) ⭐ the demo that launches the project
- Multi-track timeline model: video, audio, caption tracks; trim/split/move/ripple.
- Whisper auto-captions with 3–4 built-in TikTok-style presets, burned in via GPU text.
- TTS voiceover track (BYO key + one local voice).
- Audio mixing, fades, background-music ducking.
- MCP server v1 with perception tools.
- **Milestone: you give Claude Code a script + a folder of gameplay recordings, and it produces a finished Ultra Bruno video. Record that as the launch demo.**

### Phase 2 — GUI MVP (months 4–8)
- Tauri app: media bin, canvas timeline, native preview surface, playback with audio scrub.
- Direct manipulation: drag/trim/split/snap, razor tool, zoom.
- Caption editor (edit text, timing, style per word/line).
- Export dialog with platform presets.
- **Milestone: a human can comfortably do final touches on an AI-assembled edit.**

### Phase 3 — Effects & plugin SDK (months 8–12)
- Keyframe animation (transform, opacity, volume) with easing curves.
- Transition system + 10 built-in transitions (WGSL).
- Effect system: color adjustments, LUTs, blur, glitch, speed ramp (with pitch-corrected audio).
- **Plugin API v1 (WASM)** + **asset pack format** + template repos ("write an effect in 30 lines").
- Community registry: start as a curated GitHub index repo, in-app browser later.
- macOS/Linux builds.

### Phase 4 — Parity march (year 2, community-powered)
Advanced features, many as plugins/local models: background removal (RVM/BiRefNet), chroma key, masks, auto-reframe, motion tracking, stabilization, audio denoise, text-to-sticker, templates, multi-cam. Track a public "feature parity board" so contributors pick items off it.

## 5. Extension system

Two tiers, so non-programmers can contribute too:

**Asset packs (no code).** A folder/zip with a `pack.json` manifest + assets. Covers: stickers, LUTs/filters, caption style presets, SFX libraries, declaratively-defined transitions (shader preset + params), text animations, templates. Anyone who can make a JSON file and a PNG can publish one.

**WASM plugins (code, any language that targets WASM).** Sandboxed via wasmtime with a capability-based API: frame effects (pixel/GPU-param access), audio effects, generators, analyzers, and integration plugins (e.g., "fetch assets from X", new export targets). Sandboxing means users can safely install community plugins — a real differentiator vs. native plugin editors.

Registry strategy: start with a `renderly-registry` GitHub repo (PR to publish, CI validates manifests), add in-app browsing/install once the format is stable. Don't build registry infrastructure before there are plugins.

## 6. Community & growth strategy

- **Dogfood loudly.** Every Ultra Bruno video is made with Renderly and says so. "This video was edited by Claude" is irresistible content on its own.
- **Lead with the AI angle.** Kdenlive/Shotcut/Olive already exist as open-source editors; none are AI-drivable. The MCP-first design is your wedge — launch on the strength of the Phase 1 demo (Show HN, r/rust, r/VideoEditing, X, the MCP ecosystem lists).
- **Lower the contribution floor.** Asset packs mean designers/creators contribute without touching Rust. Keep `good-first-issue` stocked; ship plugin/asset templates.
- **Docs as a feature.** The project format spec, command API reference, and MCP instructions are documented from day one — for both human contributors and AI agents.
- **Sustainability (later):** GitHub Sponsors / OpenCollective. Never paywall features; optional paid = hosting/rendering services if ever needed.

## 7. Risks & mitigations

| Risk | Mitigation |
|---|---|
| CapCut parity is enormous | Parity is a *direction*, not a v1 promise. Plugins carry the long tail; your workflow defines the core. |
| Webview UI feels slow | Native preview surface; canvas timeline; measure from day one. If Tauri truly can't keep up, the engine/UI split makes a UI swap survivable. |
| FFmpeg/codec licensing | Dynamically link LGPL FFmpeg; document patent-encumbered codec handling per platform. |
| Solo burnout | Every phase ends in a tool you personally benefit from; launch community at Phase 1, not Phase 4. |
| Name collisions | Trademark-check "Renderly" before the public launch; alternates listed at top. |

## 8. Immediate next steps

> **Status (2026-10):** the original four steps here (scaffold the workspace, schema v0 and
> the first ten commands, the media-spine spike, `AGENTS.md`) are long done. The repo is
> public and `v0.1.0` is out as a pre-release with installers for Windows, macOS and Linux,
> but it has not been launched. The next steps are about getting it in front of people, not
> about more features.

1. **Prove the release on a clean machine.** Install `v0.1.0` from the GitHub release on a
   machine that has never had the dev environment, and take one video from import to export.
   Windows first; macOS and Linux runtime QA is still open on the
   [parity board](docs/parity-board.md).
2. **Fix what a stranger hits first.** The installers are unsigned and not notarized, so
   Windows shows an unknown-publisher warning and macOS Gatekeeper blocks un-notarized
   downloads by default. The macOS build is Apple Silicon only. Either sign and notarize, or
   document the workaround next to the download link.
3. **Clear the name.** Run the trademark check on "Renderly" that §7 requires before a public
   launch.
4. **Record the launch demo.** This is the Phase 1 milestone, upgraded by the live bridge:
   hand Claude a script and a folder of gameplay recordings and let it build a finished Ultra
   Bruno video in the open app, with the human doing the final touches. It is also the first
   real dogfood of the release build. Put the recording at the top of the README.
5. **Stock the repo, then launch.** Open the parity board's good first issues as real GitHub
   issues (there are none yet), then post per §6: Show HN, r/rust, r/VideoEditing, X, the MCP
   ecosystem lists.
6. **Then return to the board.** Engineering work resumes from
   [docs/parity-board.md](docs/parity-board.md), which is the live list; this section should
   not duplicate it. The items that most protect a launch are the test gaps (no frontend
   tests, no golden-frame check that preview and export match) and the effects that render
   in export but not in the preview (generated mattes, pack `.cube` LUTs).
