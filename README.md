<p align="center">
  <img src="assets/logo-with-text-1200.png" alt="Renderly" width="600">
</p>

<p align="center">
  <a href="https://github.com/isuryacharan/Renderly/actions/workflows/ci.yml"><img src="https://github.com/isuryacharan/Renderly/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-blue" alt="License: AGPL-3.0"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%C2%B7%20macOS%2FLinux%20CI-2f7bf2" alt="Platforms">
  <img src="https://img.shields.io/badge/engine-Rust%20%2B%20wgpu%20%2B%20FFmpeg-orange" alt="Engine">
</p>

<h3 align="center">The open-source video editor built for AI agents — and the humans who finish the job.</h3>

<p align="center">
Tell an agent what you want. Watch it edit in your timeline, live. Press Ctrl+Z when you disagree.
</p>

---

## Why Renderly exists

Editing to a script in a commercial editor works — but the features that make it fast sit behind
subscriptions, and none of it is scriptable by the AI tools you already use. Renderly is built
on two bets that reinforce each other:

1. **Every edit is a command.** One command API drives the GUI, the CLI, and the MCP server
   identically — so an agent can do the entire ideate → clip → order → caption → voiceover pass
   on its own, and a human polishes the result with normal editing.
2. **The feature set belongs to the community.** Effects, transitions, caption styles, and
   stickers ship as sandboxed WASM plugins and no-code asset packs — not as a paywall.

Not affiliated with, endorsed by, or a clone of any commercial editor.

## What makes it different

- **Agents edit your open project, live.** With the desktop app running, an MCP-connected agent
  (Claude Code, or anything that speaks MCP) discovers the app over a loopback bridge and edits
  the same session you're looking at: clips land in the timeline instantly, the preview updates,
  its changes share **your undo stack** (Ctrl+Z undoes the agent), and `render_frame` shows the
  agent the exact pixels you see.
- **One compositor, everywhere.** The preview is renderly-core's real compositor — the same
  Rust/WGSL that renders your export — compiled to WebAssembly + WebGPU. Effects, transitions,
  masks, keyframes, and burned-in captions look identical in preview and output, at 60 fps on
  1080p60 footage, because they are the same code.
- **Agent perception, not just mutation.** Scene detection, silence detection, audio peaks,
  transcripts, and frame rendering are first-class MCP tools — an agent can *watch and listen*
  to your footage before deciding where to cut.
- **Local-first AI.** Whisper captions and Piper voiceover run on your machine; cloud TTS is
  strictly bring-your-own-key. Nothing leaves your computer unless you send it.
- **Interop, not lock-in.** Projects are plain JSON, and timelines import from / export to
  [OpenTimelineIO](docs/otio.md) (`.otio`) — in the app, the CLI and MCP — with a lossless
  Renderly round trip and warnings (not failures) for whatever another tool's file contains
  that Renderly cannot represent.
- **A timeline that behaves like a pro NLE.** CapCut-style layer ordering (what stacks higher
  overlaps in frame), drag-to-reorder tracks, magnetic snapping, group operations, transition
  badges, filmstrip thumbnails and waveforms, keyframe curves — in a dark-first UI designed for
  long sessions.

## Status

**Pre-alpha, moving fast.** Phases 0–3 of [PLAN.md](PLAN.md) are complete; Phase 4's parity
march (background removal, motion tracking, stabilization, auto-reframe, templates, multicam)
has heuristic implementations landed, with ML upgrades on hold. Recent highlights: the WebGPU
preview migration is complete end to end — the native per-OS child-window preview and its
Win32/AppKit/X11/Wayland backends are deleted, the webview canvas is the only preview path
(see [docs/preview-webview.md](docs/preview-webview.md)) — plus live caption burn-in in
preview, a project home screen with auto-save and self-refreshing thumbnails, and the live
agent bridge running end-to-end. Current workstreams:
[docs/improvement-plan.md](docs/improvement-plan.md).

Windows is the primary target today; macOS/Linux build in CI and are next in line for runtime QA.

## Architecture

```
renderly-core/     Headless Rust engine — project model, command API, media I/O, compositing.
                   No UI dependencies. The single source of truth every frontend calls into.
renderly-cli/      Thin CLI over the command API — scriptable, the simplest agent surface.
renderly-mcp/      MCP server: full command API + perception tools. Proxies to the running
                   desktop app's live bridge when one is open; headless file mode otherwise.
renderly-wasm/     renderly-core's compositor compiled to wasm32 + WebGPU — the preview.
renderly-app/      Tauri 2 desktop app; React UI around the wasm compositor preview.
```

One command API, one project schema, dispatched identically everywhere — specs in
[docs/command-api.md](docs/command-api.md), [docs/project-schema.md](docs/project-schema.md),
[docs/architecture.md](docs/architecture.md). The preview migration story is
[docs/preview-webview.md](docs/preview-webview.md); the live bridge contract is
[docs/bridge-protocol.md](docs/bridge-protocol.md).

## Getting started

Prereqs: Rust, Node 20+, `ffmpeg`/`ffprobe` on PATH, `wasm-pack` (`cargo install wasm-pack`).

```sh
git clone https://github.com/isuryacharan/Renderly.git
cd Renderly
cargo build --workspace

# Desktop app
cd renderly-app
npm install
npm run build:wasm   # once, and after Rust compositor changes
npm run tauri dev
```

Projects live in `Documents/Renderly` — date-named, auto-saved, with live gallery thumbnails.

### Script it from the CLI

```sh
cargo run -p renderly-cli -- new-project demo.renderly.json --name "my-edit"
cargo run -p renderly-cli -- apply demo.renderly.json '{"command":"AddTrack","kind":"audio","name":"A1"}'
cargo run -p renderly-cli -- export demo.renderly.json out.mp4 --preset tiktok
```

### Connect an agent

```sh
cargo build -p renderly-mcp --release
```

Register `target/release/renderly-mcp` in your agent's MCP settings (for Claude Code:
`claude mcp add --scope user renderly -- <path-to-exe>`). Headless, the agent can create,
edit, caption, voice, and export projects. With the desktop app open on the same project,
the agent's first `get_editor_status` call reports `live: true` — and from then on you're
co-editing. Full tool list and workflow: [docs/mcp-agent-guide.md](docs/mcp-agent-guide.md).

Environment for the AI features: `RENDERLY_WHISPER_MODEL` (captions),
`RENDERLY_PIPER_MODEL` (local TTS), or `OPENAI_API_KEY` (BYO cloud voiceover).

## Roadmap

- **Phase 0–3 — done:** schema + command API, CLI export spine, MCP server with perception,
  Whisper captions, TTS voiceover, Tauri GUI, effects/transitions/keyframes, WASM plugin SDK,
  asset packs.
- **Phase 4 — current:** community feature-parity march; heuristics landed, ML models next.
  In parallel: performance/UX overhaul and the live-agent workflow
  ([docs/improvement-plan.md](docs/improvement-plan.md)).

Full detail and rationale: [PLAN.md](PLAN.md) §4.

## Contributing

Code or no-code, there's a way in — see [CONTRIBUTING.md](CONTRIBUTING.md). Asset packs and
plugins are the fastest path: [`examples/packs/starter`](examples/packs/starter),
[`examples/plugins/invert`](examples/plugins/invert), [`examples/registry`](examples/registry).

**[docs/parity-board.md](docs/parity-board.md)** is the board to pick work from — what's
shipped, what's open, what's deliberately deferred, and a good-first-issue list.

If you're an AI agent working in this repo, read [AGENTS.md](AGENTS.md) first — it is the
enforceable contract for this codebase. (Yes, this project is partly built by the workflow it
ships.)

## License

[AGPL-3.0](LICENSE) — forks and hosted services must stay open source.

## Third-party licences

Installers bundle third-party code, models and fonts under their own licences.
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) lists every bundled Rust crate and npm package
with its SPDX licence and licence text, plus the bundled model and font. The FFmpeg notice is in
[NOTICE-ffmpeg.md](NOTICE-ffmpeg.md). The installers ship these files alongside `LICENSE`.

After adding, removing or upgrading a dependency, regenerate the notices with
`python scripts/third_party_notices.py` (run `npm ci` in `renderly-app` first). CI runs the same
script with `--check` and fails if the committed file is out of date.
