# MEMORY.md

Project memory for Renderly. Keep it short and current: decisions, open questions, status.
Detail belongs in `docs/decisions/` (committed) or in the local spec and plans (git-ignored).

Last updated: 2026-11-24

## Status

- Phase: planning complete, no code yet.
- Next slice: **S0.1 Repository scaffold**, then the rest of M0 (engine spike).
- Nothing after M0 should be built on the engine until the M0 go / no-go record exists.

## Decisions

| Date | Decision |
|---|---|
| 2026-11-24 | Renderly 2.0 is a browser-only rewrite in TypeScript. It does not carry over the earlier codebase or its copyleft licence. |
| 2026-11-24 | Browsers: Chromium-first. Chrome and Edge get every feature; Safari and Firefox are best-effort with fallbacks. |
| 2026-11-24 | Store: git registry plus CDN first. Formats must allow a hosted service (accounts, ratings) later without breaking packs. |
| 2026-11-24 | Packs: two tiers. Declarative (JSON, GLSL, assets) and sandboxed scripted packs with stricter review. |
| 2026-11-24 | Licence: permissive. Apache-2.0 is assumed until the owner confirms Apache-2.0 or MIT. |
| 2026-11-24 | Fonts: the Google Fonts catalogue for text and captions. Fonts used by a project are stored with the project. |
| 2026-11-24 | Stack: mediabunny + WebCodecs, WebGL2 via three.js, GLSL ES 3.00 for packs, Web Audio with an AudioWorklet clock, File System Access + OPFS, React for panels and canvas for per-frame surfaces. |
| 2026-11-24 | Time is stored as integer flicks (1/705,600,000 s). |
| 2026-11-24 | The store is free. No paid items for now. |
| 2026-11-24 | No other video editors are named anywhere in the repository. |
| 2026-11-24 | `SPEC.md` and `docs/plans/` are git-ignored and kept locally. |

## Open questions

| # | Question | Blocks |
|---|---|---|
| 1 | Apache-2.0 or MIT for the code? | Licence file in S0.1 (Apache-2.0 assumed) |
| 2 | Hosting and domain for the app, store site and pack CDN | S3.7, S3.8, R.6 |
| 3 | Allow optional cloud AI with the user's own key later? (On-device only is assumed for v1.) | Nothing before M7 |
| 4 | Should the assistant move earlier than M8? | Position of S8.3–S8.5 |
| 5 | Scripted-pack sandbox mechanism | Decided by prototype in S3.9 |
| 6 | How much launch content is required at 1.0? | R.5 |
| 7 | Registry governance: who approves pull requests, and the content guidelines | S3.7 |

## Unproven assumptions

These come from the spec and have no proof of concept behind them yet.

- Multi-layer WebCodecs playback on an audio clock, rendered in a worker (M0 exists to test it).
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

- 2026-11-24 — Spec written. Implementation plans sliced into 74 slices across M0–M8 and the 1.0
  release. Repository initialised.
