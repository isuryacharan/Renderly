# @renderly/engine

The render worker: a WebGL2 context on an `OffscreenCanvas`, fed by a pool of decode workers over
`MessageChannel`s. It plays an edit list of cuts across files on the audio clock. The main thread
sends commands and receives a stats message once a second; frames never pass through it.

Status: M0 spike (slices S0.3 to S0.6). Layers with static transforms; no effects yet.

## Use

```ts
import { createPreview } from "@renderly/engine";

const preview = await createPreview(canvas); // decode pool, render worker, audio output
const info = await preview.open(file); // one file as a timeline of one cut
// or an edit list over several files (times in flicks):
await preview.load([fileA, fileB], [
  { source: 0, in: 0, out: 2 * FLICKS_PER_SECOND, start: 0 },
  { source: 1, in: 30 * FLICKS_PER_SECOND, out: 31 * FLICKS_PER_SECOND, start: 2 * FLICKS_PER_SECOND },
]);
preview.play(0); // flicks
preview.pause();
await preview.seek(time); // shows the exact frame
preview.setPreviewScale(0.5); // full, 1/2 or 1/4 of the sequence size
preview.onStats((s) => console.log(s.fps, s.dropped, s.late));
```

## How a frame gets on screen

1. The decode worker transfers a `VideoFrame` to the render worker.
2. On each display refresh the worker takes the frame due at the playhead (`FrameQueue`). Frames
   the playhead has already passed are counted as dropped.
3. `FrameUploader` imports the frame into a texture, builds mipmaps, and draws it into the
   half-float working target at the preview size, applying the clip rotation. The frame is
   closed straight after.
4. `Presenter` draws the working target into the canvas, letterboxed, at device-pixel size.

## Layers

A cut can sit on a layer (0 is the bottom) with a static transform: position, scale and opacity
(`Transform` in `timeline.ts`). Each layer keeps its own render target with its last uploaded
frame; `Compositor` draws the layers bottom to top with premultiplied alpha over black. Sound
comes from layer 0 for now.

The pool has six decoders: five for streams and one kept for keyframe-only updates and seeks.
Current runs get decoders first, top layer first. A layer left without one shows the keyframe at
or before the playhead, refreshed twice a second, instead of stalling the others.

GPU memory: the renderer accounts for its render targets, the upload texture and the decoded
frames on loan (`memory.ts`) against a 1.5 GB ceiling. Targets past the ceiling are refused.

## Cuts and decoder scheduling

Consecutive cuts that continue the same footage are merged into runs (`timeline.ts`), so split
points play straight through. Each run gets a decoder stream: the run at the playhead, the two
after it, and every run starting within 1.5 s (up to eight). A decoder is freed as soon as its run
is fully decoded, so strings of clips a few frames long are prepared one after another. Idle
decoders are reused, preferring one that already has the file open.

Streams prepared while the picture plays are paced: one decode submission every 8 ms. Feeding a
hardware decoder 4K frames in a burst stalls the browser's GPU thread for 12-70 ms and drops
frames on screen; paced, three decoders at once cause no stall (measured in S0.5).

## Playback start

Starting a hardware decoder stalls the GPU briefly; display refreshes arrive 30-100 ms late for
a moment. The clock therefore waits until the decoder has filled its frames-in-flight budget and
three refreshes in a row were on time (at most 1 s), and for the first 500 ms a late refresh
moves the clock start later instead of skipping frames.

## Export

`createExporter()` starts an export worker with its own four decode workers, so an export never
competes with the preview. For each output frame the worker waits until each layer's decoder has
delivered past that frame's time, composites with the same `FrameUploader` and `Compositor` as
the preview, wraps the canvas in a `VideoFrame` and hands it to the encoder (mediabunny over
`VideoEncoder`), awaiting its back-pressure. Audio for the bottom layer is decoded by the same
code as playback into a 2-second ring and encoded in 1,024-sample chunks after each frame. The MP4
streams to a `FileSystemWritableFileStream` with the index at the end; nothing grows with the
length of the export except that index.

Jobs report progress, can be cancelled (the partial file is removed), and resolve with a typed
result. `renderFrame` and the preview's `readFrame` read the composite through the same code for
the parity test: the two paths must give identical bytes.

## Context loss

three.js re-creates its GPU objects when the context comes back. The working target's contents
are gone, so the renderer re-seeks the current frame when paused; while playing, the next frame
replaces it.

## Files

```
src/preview.ts        main-thread client: workers, canvas transfer, resize observer, audio
src/timeline.ts       cuts, runs and which runs to prepare (pure)
src/renderer.ts       serveRenderer: the worker's loop and message handling
src/render-worker.ts  worker entry point
src/frame-upload.ts   VideoFrame → working target (mipmaps, rotation, contain fit)
src/compositor.ts     per-layer targets, the composite pass and pixel readback
src/export.ts         main-thread export client: worker, decoders, jobs
src/exporter.ts       the export worker: decode, composite, encode, write
src/memory.ts         GPU memory accounting against the ceiling (pure)
src/present.ts        composite → canvas
src/gpu.ts            full-screen GLSL ES 3.00 passes and half-float targets
src/frame-queue.ts    which frame to show at the playhead (pure)
src/layout.ts         working size, rotation and letterbox maths (pure)
src/protocol.ts       message types
```

The measurement page is `apps/spike/preview.html`.
