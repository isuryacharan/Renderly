# Renderly

An open source video editor that runs entirely in the browser. The aim is native-class editing
(multi-GB 4K60 sources, long timelines, frame-exact export) with no server: your media and
projects stay on your device. Effects, transitions, caption styles and templates will be
community-made packs, distributed through a free store.

> **Status: early engine spike (milestone M0).** There is no editor yet. The code here proves
> the playback engine: WebCodecs decoding and WebGL2 rendering in workers, on an audio clock. The
> editor itself is built only after the engine passes its go / no-go measurements.

## What works so far

| Slice | What it does | Measured (Chrome, RTX 5060) |
|---|---|---|
| S0.1 Scaffold | pnpm workspace, cross-origin-isolated dev server, CI | CI green |
| S0.2 Decode worker | Opens files by byte range, streams frames, exact seek | 6.3 GB file opens in 26 ms; 4K long-GOP exact seek median 86 ms |
| S0.3 Render worker | One clip on a worker-owned WebGL2 canvas | 4K60 for 2 minutes: 0 dropped frames, main thread idle |
| S0.4 Audio clock | Sound plays from an AudioWorklet; the picture follows it | 10 minutes: 0 of 99,167 refreshes out of sync, no clicks on pause or resume |
| S0.5 Cuts across files | An edit list over several files, decoders prepared ahead | 400 cuts over H.264 and HEVC 4K60: 0 dropped frames, every cut on its exact frame |

## Run it

You need Node.js 22 or later and pnpm 10 (`corepack enable` installs the pinned version), and
Chrome or Edge on a desktop. Safari and Firefox are not supported yet.

```sh
pnpm install
pnpm dev
```

Then open:

- http://localhost:5173/ for a check of the browser features the engine needs
- http://localhost:5173/preview.html to play a clip through the render worker
- http://localhost:5173/decode.html for the decoder measurements

Choose a video file with the file picker, or generate synthetic test media with timecode
burned in (needs ffmpeg) and pick it from the list:

```sh
node scripts/make-test-media.mjs
```

Other commands: `pnpm test`, `pnpm typecheck`, `pnpm lint`, `pnpm build`.

## Repository layout

```
apps/spike/          test pages for the engine spike
packages/media/      decode worker: demux, decode, exact seek
packages/engine/     render worker: WebGL2 upload, presentation, playback clock
packages/audio/      audio decode worker, AudioWorklet playback, the audio clock
packages/ui/         design system: tokens and component styles
docs/design-system/  interface rules and a live preview page
docs/decisions/      decision records
scripts/             test media generation
```

## Built with

TypeScript, Vite, [mediabunny](https://github.com/Vanilagy/mediabunny) over WebCodecs, and
[three.js](https://threejs.org) on WebGL2.

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md) first. Commits are signed off
under the Developer Certificate of Origin.

## Licence

[Apache License 2.0](LICENSE).
