# MEMORY.md

Project memory for Renderly. Keep it short and current: decisions, open questions, status.
Detail belongs in `docs/decisions/` (committed) or in the local spec and plans (git-ignored).

Last updated: 2026-11-24

## Status

- Phase: M0 engine spike. S0.1 to S0.4 are merged into `main` (pull requests #1 to #3).
- S0.5 (cuts across files) is built on `s0.5-cuts`; both "done when" items pass. In review as a
  pull request.
- Next slice: **S0.6 Multiple layers**.
- The owner has authorised completing and merging M0 slices without asking (2026-11-25), as long as
  each slice's "done when" items pass. Stop and report if a target is missed. The go / no-go
  decision record (S0.9) is the owner's to accept; nightly GPU CI needs a self-hosted runner from
  the owner.
- Nothing after M0 should be built on the engine until the M0 go / no-go record exists.

## Decisions

| Date | Decision |
|---|---|
| 2026-11-24 | Renderly 2.0 is a browser-only rewrite in TypeScript. It does not carry over the earlier codebase or its copyleft licence. |
| 2026-11-24 | Browsers: Chromium-first. Chrome and Edge get every feature; Safari and Firefox are best-effort with fallbacks. |
| 2026-11-24 | Store: git registry plus CDN first. Formats must allow a hosted service (accounts, ratings) later without breaking packs. |
| 2026-11-24 | Packs: two tiers. Declarative (JSON, GLSL, assets) and sandboxed scripted packs with stricter review. |
| 2026-11-24 | Licence: Apache-2.0, chosen over MIT for its explicit patent grant and trademark clause. Contributions are signed off with the DCO, not a CLA. |
| 2026-11-24 | Fonts: the Google Fonts catalogue for text and captions. Fonts used by a project are stored with the project. |
| 2026-11-24 | Stack: mediabunny + WebCodecs, WebGL2 via three.js, GLSL ES 3.00 for packs, Web Audio with an AudioWorklet clock, File System Access + OPFS, React for panels and canvas for per-frame surfaces. |
| 2026-11-24 | Time is stored as integer flicks (1/705,600,000 s). |
| 2026-11-24 | The store is free. No paid items for now. |
| 2026-11-24 | No other video editors are named anywhere in the repository. |
| 2026-11-24 | `SPEC.md` and `docs/plans/` are git-ignored and kept locally. |
| 2026-11-24 | Toolchain: Node 22+, pnpm 10 workspaces, TypeScript 6.0 (7.0 waits for typescript-eslint support), Vite, Vitest, ESLint, Prettier. Prettier formats code only; Markdown and the preview templates keep their authored layout. |
| 2026-11-24 | Exact seek drives `VideoDecoder` directly and skips H.264 frames with `nal_ref_idc = 0` before the target (about a third of the frames on NVENC test media). Median 4K long-GOP seek went from 145–154 ms (mediabunny `getSample`) to 86 ms with identical pixels. HEVC still uses the full decode. |
| 2026-11-24 | Decode back-pressure is by credit: a worker sends at most N frames (default 6) until the receiver closes some, so a leak stalls decoding instead of filling GPU memory. |
| 2026-11-25 | Render worker playback start: the clock waits until the decoder has filled its frames-in-flight budget and three display refreshes in a row were on time (at most 1 s), and for the first 500 ms a late refresh delays the clock instead of skipping frames. Starting a hardware decoder stalls the GPU for 30-100 ms; without this, 4K starts dropped 1-8 frames. |
| 2026-11-25 | The decode worker is reached through a MessagePort, so the render worker receives frames directly. |
| 2026-11-25 | Audio clock: the AudioWorklet publishes the context frame and session frames consumed after every quantum; the main thread writes `getOutputTimestamp()` into the same SharedArrayBuffer every 100 ms; the render worker computes the session time heard when its next refresh reaches the screen (assumed one refresh ahead) and shows the frame for it. Clips without audio keep the display-refresh clock. |
| 2026-11-25 | Audio and video start together: the worklet holds a session until 100 ms is buffered and the render worker sets the session's go flag after its video pre-roll. Every start and stop is a 5 ms gain ramp. |
| 2026-11-25 | A paused playhead snaps to the start of the frame on screen; resume starts there. |
| 2026-11-25 | Audio output runs at 48 kHz; other source rates are refused until there is a resampler (M2). |
| 2026-11-25 | Edit-list playback: consecutive cuts of continuous footage merge into runs; each run gets a decoder stream (the current run, two ahead, and any run starting within 1.5 s, up to eight). A pool of four decode workers is shared; a decoder is freed as soon as its run is fully decoded. |
| 2026-11-25 | Decoders preparing upcoming cuts submit one frame every 8 ms. Bursts of 4K decode stall Chrome's GPU thread for 12-70 ms and drop frames on screen; paced decoding does not, even three at once. Playback decoding now runs in our own loop (`range-decode.ts`) instead of mediabunny's sample iterator, and skips non-reference H.264 frames before the first wanted frame. |
| 2026-11-25 | The audio clock publishes where the last consumed sample ended, not the latest quantum, so the heard time reaches the end of the audio. A ring the decoder has marked complete running dry is the end, not an underrun. |
| 2026-11-24 | Design system v1: dark-first with a full light theme, strictly neutral grey chrome, one amber accent (chosen over blue, violet and magenta; warning is yellow), Geist for the interface and Geist Mono for numbers (both OFL, shipped with the app), 4px grid, 22–28px controls, shadows only on floating surfaces and knobs. It lives in the repository: rules in `docs/design-system/`, tokens and styles in `packages/ui/`. `tokens.json` is the source of truth. |

## Open questions

| # | Question | Blocks |
|---|---|---|
| 2 | Hosting and domain for the app, store site and pack CDN. The code is at github.com/isuryacharan/Renderly. | S3.7, S3.8, R.6 |
| 3 | Allow optional cloud AI with the user's own key later? (On-device only is assumed for v1.) | Nothing before M7 |
| 4 | Should the assistant move earlier than M8? | Position of S8.3–S8.5 |
| 5 | Scripted-pack sandbox mechanism | Decided by prototype in S3.9 |
| 6 | How much launch content is required at 1.0? | R.5 |
| 7 | Registry governance: who approves pull requests, and the content guidelines | S3.7 |

## Unproven assumptions

These come from the spec and have no proof of concept behind them yet.

- Multi-layer WebCodecs playback on an audio clock, rendered in a worker. Single-layer playback on
  the audio clock is proven (S0.4); several layers and cuts are S0.5 and S0.6.
- Streaming export to disk with flat memory (S0.8).
- A real audio engine: many tracks, effects, pitch-preserving speed, chunked offline mix (S2.4, S2.5).
- The text engine: HarfBuzz in WASM with a GPU glyph atlas (S4.1).
- The scripted-pack sandbox at 60 fps (S3.9).
- Disk-folder projects surviving a browser restart (manual check in S1.4).
- Safari and Firefox WebCodecs coverage has not been verified against current releases (R.3).

## Reference material (outside the repository)

- Four proofs of concept: timeline and playback, effects, transitions, storage. Their READMEs
  hold the measurements the spec quotes.
- The owner's teardown notes on a leading native editor. Lessons only; the clean-room rule in
  AGENTS.md applies.

Ask the project owner for the locations.

## Log

- 2026-11-25 — S0.5 cuts: a seeded 400-cut list (320 s) over two H.264 and one HEVC 4K60 file played with 308 decoder switches, 0 cuts held, 0 inexact, 0 frames dropped, 0 late, 19,221 frames presented, 0 of 52,778 refreshes out of sync, 0 audio underruns. Strings of 2, 3 and 4 three-frame clips between 1 s clips: 0 held, 0 inexact. Found and fixed on the way: start-of-run decoders started at the run start instead of the playhead; burst decoding stalling the GPU; a flush deadlock when the decoder's output surfaces were all held; one-at-a-time packet reads; the audio clock stopping one output latency short of the end. Known limit: hard cuts between different audio make a small step (no edit-point fades until M2).
- 2026-11-25 — S0.4 audio clock: a 10-minute play of 1080p60 with AAC audio (Chrome 156) gave 36,060 frames, 0 dropped, 0 late, 0 of 99,167 refreshes out of sync, 0 audio underruns, main thread 0 long tasks. Beep onsets decoded through our path match ffmpeg's decode of the same file to the sample, from four start positions. 10 of 10 pause/resume cycles resumed on the frame shown at pause; the largest output step at fades (0.065) stays below normal playback (0.246). Not measured acoustically: output latency comes from the browser's `getOutputTimestamp()`, and display latency is assumed to be one refresh.
- 2026-11-25 — S0.3 render worker measured in Chrome 156 (RTX 5060, ~160 Hz display): 4K60 at full preview resolution played 2 minutes with 7,576 frames, 0 dropped, 0 late, 59.6-61 fps, worker work 0.15 ms per refresh; main thread had 0 long tasks and at most 9 ms of timer lag (not a DevTools trace). 10 starts over four files: 0 dropped, start latency 19-251 ms. Phone clips tagged 90° and 270° display upright. Context loss (simulated with WEBGL_lose_context, paused and mid-play) recovers without a reload.
- 2026-11-24 — S0.1 CI passed on pull request #1.
- 2026-11-24 — S0.2 decode worker measured in Chrome 156 (RTX 5060) on a 6.31 GB 4K60 H.264 file at 120 Mbps with a 2 s GOP and the index at the end: open 26 ms; main heap 11.9 → 12.0 MB; exact seek median 86 ms (p90 167 ms), 0 wrong frames, 0 pixel mismatches against a full decode; 5-minute soak (779 plays, 333 seeks, 88,066 frames) left the leak counter, worker in-flight and held frames all at 0. Measured through the dev server's HTTP range endpoint; the `File` path was checked for correctness only. Test media comes from `scripts/make-test-media.mjs`.
- 2026-11-24 — S0.1 scaffold: pnpm workspace, strict tsconfig, Vite spike page with COOP/COEP (all probes true in Chrome 156), Vitest, ESLint, Prettier, CI workflow, LICENSE, NOTICE, CONTRIBUTING with DCO, decision-record template. Geist font packages are now installed.
- 2026-11-24 — Design system written to the repository (`packages/ui`, `docs/design-system`). Font packages are declared but not installed; there is no workspace root yet (slice S0.1).
- 2026-11-24 — Spec written. Implementation plans sliced into 74 slices across M0–M8 and the 1.0
  release. Repository initialised.
