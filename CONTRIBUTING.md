# Contributing to Renderly

Thanks for considering contributing. Renderly is pre-alpha but the foundations are stable:
the command API, project schema, plugin SDK, and asset-pack format are all shipped, and the
preview renders through the same compositor as export.

**Current phase: 4 — the parity march** ([PLAN.md](PLAN.md) §4). Heuristic implementations
have landed for background removal, chroma key, masks, auto-reframe, motion tracking,
stabilization, denoise, stickers, templates, and multicam; upgrades and new features are
open for contribution. See the [feature parity board](docs/parity-board.md) for what's
claimed, what's open, and what's deliberately deferred.

## Three ways in, two of them code-free

- **Asset packs** — stickers, LUTs/filters, caption style presets, SFX, transitions,
  templates. A folder of JSON plus media files, no code. Start from
  [`examples/packs/starter`](examples/packs/starter) and the format spec in PLAN.md §5.
- **Plugins** — sandboxed WebAssembly, written in any language that compiles to wasm32.
  Start from [`examples/plugins/invert`](examples/plugins/invert) (an effect in ~30 lines).
- **Code** — Rust for `renderly-core`/`renderly-cli`/`renderly-mcp`/`renderly-wasm`,
  TypeScript + React for `renderly-app`'s UI.

Packs and plugins are listed in the curated index at
[`examples/registry`](examples/registry) — open a PR adding your entry.

## Ground rules

Read [AGENTS.md](AGENTS.md) first — it is written for AI agents, but its architecture rules,
repo layout, and definition of done apply identically to human PRs. The short version:

1. **Every edit operation goes through the command API in `renderly-core`.** Never implement
   editing logic in the CLI, the MCP server, or the app.
2. **One compositor.** Preview and export share `renderly-core`'s compositor (compiled to
   wasm32 for the preview). Never reimplement an effect in JavaScript.
3. `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, and
   `cargo test --workspace` must pass. For frontend changes, `npx tsc --noEmit` and
   `npm run build` from `renderly-app/`.
4. Schema or command-set changes update [docs/project-schema.md](docs/project-schema.md) or
   [docs/command-api.md](docs/command-api.md) **in the same PR**.
5. Keep PRs scoped to one roadmap item or issue.

Before a release-minded PR, walk [docs/qa-checklist.md](docs/qa-checklist.md).

## Setup

Prereqs: Rust via [rustup](https://rustup.rs) (the version is pinned in
`rust-toolchain.toml` and installs itself on the first `cargo` command), Node 20+,
`ffmpeg` and `ffprobe` on PATH, and `wasm-pack` (`cargo install wasm-pack`) if you're
touching the preview compositor.

```sh
git clone https://github.com/isuryacharan/Renderly.git
cd Renderly
cargo build --workspace
cargo test --workspace

# Desktop app
cd renderly-app
npm install
npm run build:wasm     # after any renderly-core/renderly-wasm change
npm run tauri dev
```

**FFmpeg**: Windows — `winget install Gyan.FFmpeg` (or scoop/choco); macOS —
`brew install ffmpeg`; Linux — your distro's `ffmpeg` package. Verify with
`ffmpeg -version && ffprobe -version`. Optional AI extras: `whisper-cli` for captions
(`RENDERLY_WHISPER_MODEL` points at a `ggml-*.bin`), `piper` for local TTS.

### Working on the UI without launching the desktop app

`renderly-app` has a browser harness that runs the real UI with Tauri IPC mocked and a
sample project loaded:

```sh
cd renderly-app
npx vite --config vite.harness.config.ts   # http://localhost:5188
```

`window.__store` exposes the Zustand store and `window.__previewStats` reports the live
preview mode and frame cost — both are the fastest way to verify a change.

## Reporting issues / proposing features

Open an issue describing the problem or the parity-board item you want to pick up. For
anything not already on the roadmap, briefly explain how it fits the non-negotiables in
[AGENTS.md](AGENTS.md) §0 before sending a large PR — it saves everyone rework.

## License

By contributing, you agree your contribution is licensed under AGPL-3.0 (see [LICENSE](LICENSE)).
