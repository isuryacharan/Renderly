# AGENTS.md

Guidance for AI coding agents and human contributors working on Renderly.

## What Renderly is

An open source video editor that runs entirely in the browser. It aims for native-class editing
(multi-GB 4K60 sources, long timelines, frame-exact export) with no server. Effects, transitions,
caption styles and templates are community-made **packs** distributed through a free store.

## Where things are

| Path | Contents |
|---|---|
| `SPEC.md` | Product and technical specification. **Local only, git-ignored.** |
| `docs/plans/` | Sliced implementation plans (M0–M8). **Local only, git-ignored.** Start at `docs/plans/README.md`. |
| `docs/decisions/` | Decision records. Committed. |
| `docs/schema/` | JSON Schema of the project file, generated from `packages/core`. Committed; never edit by hand. |
| `docs/design-system/` | Design system rules, component guidelines and a live preview page. Committed. |
| `packages/ui/` | Design system as code: `tokens/tokens.json` (source of truth), generated `tokens.css` and `tokens.ts`, base and component styles. Committed. |
| `MEMORY.md` | Project memory: decisions made, open questions, current status. Committed. |
| `apps/`, `packages/`, `packs/` | Code (layout in the spec, §13). `apps/spike` is the M0 test page; `packs/` does not exist yet. |

If `SPEC.md` or `docs/plans/` are missing in your checkout, ask the project owner for them. Do
not recreate them from guesswork. The rules below are enough to work safely without them.

## Rules that must not be broken

1. **One command API.** Every change to a project goes through a typed command. UI code,
   shortcuts and tools never mutate the project directly. A new kind of edit means a new command
   with tests first, then the UI.
2. **The core has no UI.** `packages/core` has no DOM, React or GPU dependency. It must run in a
   worker and in plain unit tests.
3. **Media never lives in JS memory.** Read files by byte range through handles. Never call
   `arrayBuffer()` on a media file. Close every `VideoFrame` as soon as it is uploaded to the GPU.
4. **Every effect is a pure function of time.** Output depends only on inputs, parameters and
   timeline time. No accumulated per-frame state, no `Math.random()`, no wall-clock time. Effects
   that need history must declare it and use fixed-step re-simulation.
5. **Preview equals export.** Both use the same frame graph and the same compositor code. Do not
   add a preview-only or export-only render path.
6. **Time is an integer.** All times are in flicks (1/705,600,000 s). No floating-point time in
   the project model.
7. **Never mutate a shipped pack version.** Projects pin `id@version` plus a content hash. Fixes
   are new versions.
8. **Third-party pack code is sandboxed.** It never receives the GPU context, the DOM, the
   network or storage.
9. **Stored data changes need a schema version** and a small, pure, tested upgrade function.
10. **Long jobs use the shared job protocol**: progress, cancel, typed result.

## Technology

- TypeScript (strict), ES modules, Vite, pnpm workspaces.
- mediabunny over WebCodecs for demux, decode, encode and mux.
- WebGL2 through three.js, half-float render targets. Pack shaders are GLSL ES 3.00.
- Web Audio with an AudioWorklet as the playback clock; the same DSP graph runs in a worker for
  export.
- File System Access API and OPFS for projects; IndexedDB for handles; Web Locks and
  BroadcastChannel for multi-tab safety.
- React for panels. Canvas (not React) for anything redrawn per frame: timeline, viewer overlays,
  scopes, waveforms, curve editors.
- Vitest for units, Playwright for end to end, headless Chrome on a real GPU for golden frames
  and benchmarks.

Target browsers: Chrome and Edge desktop get every feature. Safari and Firefox are best-effort
with graceful fallbacks; probe capabilities at run time instead of assuming.

## How to work

- **Work one slice at a time** from `docs/plans/`. A slice is done only when every item in its
  "done when" list is verified. Do not start M1 or later until the M0 engine decision is recorded.
- **Tests come with the change.** Commands get unit tests; render changes get golden frames
  (cases in `apps/spike/src/golden-page.ts`, compared with the CPU reference in
  `packages/engine/src/reference.ts` by `pnpm test:golden`);
  pack items must pass the determinism check (frame N rendered cold equals frame N after playing
  from 0); performance work is done when its benchmark runs in CI.
- **Record decisions** in `docs/decisions/` when a slice settles a design question, and add a
  line to `MEMORY.md`.
- **Verify locally before every push: `pnpm verify` must pass.** The owner's rule is to keep CI
  failures, and the fixes they need, to a minimum: CI confirms a change, it does not find its
  bugs. `pnpm verify` runs everything CI runs, in the same order, from a cold Vite cache, then the
  timeline and viewer suite again with every decoder seek slowed down (races that only open on a
  slow machine). For changes to playback, the engine, timing or the browser suites, run
  `pnpm verify --repeat 3`. Do not push after a red or skipped step, and do not push on the
  strength of one lucky run: a check that fails even once has a cause, so find it first. Put the
  verify summary in the pull request. If CI still fails, find out why `pnpm verify` missed it and
  make it catch that kind of failure too. In browser checks, wait for a condition (with a
  timeout), never for a fixed time before asserting: fixed waits pass on a fast machine only.
- **Report honestly.** If a test fails or a step was skipped, say so with the output.
- **Do not commit or push unless asked.**
- **Keep it small.** Prefer the simplest change that satisfies the slice. No speculative
  abstractions, no unrequested dependencies.

## Interface work

Read `docs/design-system/README.md` before building or changing any interface.

- **Use tokens, never literals.** Every colour, size, radius and shadow comes from a variable in
  `packages/ui/src/styles/tokens.css`, or from `packages/ui/src/tokens.ts` for canvas drawing.
  If a token is missing, add it to `packages/ui/tokens/tokens.json` with a usage note and run
  `node packages/ui/scripts/build-tokens.mjs`. Never edit the generated files.
- **Buttons, menu items and shortcuts are actions.** Add each new one to the actions registry
  (`apps/editor/src/actions/editor-actions.ts`: id, label, icon, default keys, enabled-when) and
  render it with `ActionButton`, `ActionSegment` or `actionItem`, never with its own handler. A
  default key must not clash with another global key (a unit test checks).
- **Use the existing component styles** (`.r-*` classes in `packages/ui/src/styles/components/`)
  and follow the matching guideline in `docs/design-system/components/`. Every numeric property
  in the inspector uses the property row.
- **One accent, amber.** Text on an accent fill uses `--on-accent`, which is not always white.
  Marks drawn on clips or on the picture use `--selection` with a `--selection-edge` line.
- **Both themes.** Check any interface change in dark and light. After changing tokens or
  component styles, run `node packages/ui/scripts/build-preview.mjs` and open
  `docs/design-system/preview/index.html`.
- **Audio clips always show waveforms and video clips always show thumbnails.**
- Status colours always come with an icon or a word. Focus rings are never removed.

## Dependencies and licences

- The code licence is permissive (Apache-2.0 assumed; see `MEMORY.md` for the open question).
- Dependencies and models must be compatible with it. **No GPL or AGPL code or models.**
- AI models run on device, are downloaded only with user consent, and must be permissively
  licensed.
- Pack licences are limited to an allow-list: MIT, Apache-2.0, BSD, CC0, CC-BY-4.0, and OFL-1.1
  for fonts.

## Writing and naming rules

- **Do not name other video editors** anywhere in this repository: not in code, comments, docs,
  commit messages, pack names or UI strings. Say "native editors" or "other editors" instead.
- **Clean-room rule.** Design lessons from studying other products are allowed. Copying,
  converting or importing their code, shaders, scripts, LUTs, models, fonts, icons or effect
  bundles is not. Implement techniques from public standards and literature.
- Renderly is never described as a clone of, compatible with, or affiliated with another product.
  Its visual identity is its own.
- Write documentation in plain, direct English.

## Privacy defaults

- Media, projects and transcripts stay on the device. No uploads.
- No telemetry unless the user opts in.
- Network use is limited to the app itself, the store, fonts and model downloads.
