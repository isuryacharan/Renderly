# 0001. The WebCodecs engine: go

- Status: Proposed
- Date: 2026-11-25
- Slice: S0.9

## Context

The spec's engine design (§3.4, §3.5) rests on something the proofs of concept had not shown:
several video layers decoded with WebCodecs, composited by WebGL2 in a worker, kept in sync by an
audio clock, and exported by streaming to disk. M0 built that engine as a spike (S0.1–S0.8) to
find out before anything else is built on it. Spec §15 names the fallbacks if it did not hold:
lower preview resolution, proxies, or the `<video>`-element path for single-layer ranges.

## Results against spec §6.4

Measured with `pnpm bench` (headless Edge 158, NVIDIA RTX 5060 through Direct3D 11, Windows 11,
commit `f0809b3`). The JSON is in the benchmark artifacts; slice measurements are in `MEMORY.md`.

| Measure | Target | Result |
|---|---|---|
| Import of a 6 GB file to usable | < 1 s | **47 ms** (6.31 GB, index at the end of the file) |
| Main-thread heap after import | < 50 MB | **8.6 MB** |
| 4K60 single-layer playback | 60 fps, < 0.1% dropped | **59.6 fps** lowest second, **0 dropped** of 3,538 frames, 0 of 9,728 refreshes out of sync |
| 1080p, 3 video layers | 60 fps | **59.6 fps**, 0 dropped (no effects yet) |
| Exact seek, 4K long-GOP | median < 120 ms | **111 ms** (86 ms in a visible tab), 0 wrong frames |
| Scrub picture update while dragging | ≥ 30 per second | **40** per second, worst UI frame 6.1 ms, exact frame at rest |
| Timeline redraw, 500 clips | < 2 ms | Not measured: no timeline until M1 |
| Non-AI effect stack per frame, 720p | < 4 ms | Not measured: no effects until M2 |
| Export, 1080p30, no AI effects | faster than real time | **15.4×** real time |
| Cold start to interactive | < 2 s, warm cache | **165 ms** for the spike page |

Also measured in M0:

- Cuts: a 400-cut list over H.264 and HEVC 4K60 files played with 0 dropped frames and every
  cut on its exact frame; the benchmark's 100-cut run had 0 holds and 0 dropped.
- Two 4K layers: about 60 fps at full preview resolution (the plan asked for 30), 60 fps with 0
  dropped at half. GPU memory accounted by the renderer peaked at 378 MB of a 1.5 GB ceiling.
- Sync: over a 10-minute play, 0 of 99,167 refreshes had picture and sound more than a frame
  apart; audio decodes to the sample, matching ffmpeg's decode.
- Export: a 30-minute 1080p30 export has exactly 54,000 frames, 1,800.000 s of audio and video,
  and every test beep within 0.21 ms; preview and export render 20 of 20 sampled frames
  byte-identical.

## Known limits

- **Scrubbing after idle** (accepted, decision 2026-11-25): a 4K drag that starts after the GPU
  has idled shows UI frames up to ~55 ms for its first ~0.4 s, while the GPU wakes. Proxies (M6)
  are the fix.
- **Export memory** (accepted, decision 2026-11-25): during a 30-minute export the page's
  renderer process grows ~370–430 MB before levelling off and releases it afterwards; the main
  heap and accounted GPU memory stay flat. The nightly benchmark should track process memory.
- **4K playback start**: two 4K layers at full resolution drop a few frames (9 here) in the first
  second after play, on the audio clock, while new decoders warm up. Single-layer 4K dropped none
  in this run.
- **Headless versus visible**: the benchmark runs headless at a 60 Hz frame rate; exact seek is
  ~25 ms slower there than in a visible tab on a 160 Hz display.
- **One machine, one browser family**: every number comes from one RTX 5060 machine on Chromium.
  Other GPUs, integrated graphics, Safari and Firefox are unmeasured (R.3).
- **Not yet exercised**: effects, text, the timeline UI, more than one audio track (sound comes
  from the bottom layer), and sample rates other than 48 kHz.

## Options

1. **Go**: build M1 on this engine as designed.
2. **Go with a fallback in place**: build on the engine, but plan proxies or a `<video>` path now.
3. **No-go**: re-plan around a fallback from §15.

## Decision

**Go.** Every M0 target that can be measured today is met on the reference machine, mostly with
room to spare, and the two misses are understood and accepted. Proxies stay where they are in the
plan (M6) as the answer to the after-idle scrub hitch and to heavier sources; nothing in M1 needs
re-planning.

## Consequences

- M1 can start once this record is accepted. The engine packages (`media`, `audio`, `engine`)
  become the base for the editor instead of a throwaway spike.
- The nightly benchmark guards these numbers. It needs a self-hosted runner labelled `gpu`
  before it runs; until then `pnpm bench` is run by hand before merging engine changes.
- Revisit when the timeline (M1) and effects (M2) can be measured, when proxies land (M6), and
  when other GPUs and browsers are tested (R.3).
