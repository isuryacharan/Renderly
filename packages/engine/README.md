# @renderly/engine

The render worker: a WebGL2 context on an `OffscreenCanvas`, fed by a decode worker over a
`MessageChannel`. The main thread sends commands and receives a stats message once a second;
frames never pass through it.

Status: M0 spike (slice S0.3). One clip, no compositing yet (S0.6), and a display-refresh clock
until the audio clock exists (S0.4).

## Use

```ts
import { createPreview } from "@renderly/engine";

const preview = createPreview(canvas); // starts a decode worker and a render worker
const info = await preview.open(file);
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

## Playback start

Starting a hardware decoder stalls the GPU briefly; display refreshes arrive 30-100 ms late for
a moment. The clock therefore waits until the decoder has filled its frames-in-flight budget and
three refreshes in a row were on time (at most 1 s), and for the first 500 ms a late refresh
moves the clock start later instead of skipping frames.

## Context loss

three.js re-creates its GPU objects when the context comes back. The working target's contents
are gone, so the renderer re-seeks the current frame when paused; while playing, the next frame
replaces it.

## Files

```
src/preview.ts        main-thread client: workers, canvas transfer, resize observer
src/renderer.ts       serveRenderer: the worker's loop and message handling
src/render-worker.ts  worker entry point
src/frame-upload.ts   VideoFrame → working target (mipmaps, rotation, contain fit)
src/present.ts        working target → canvas
src/gpu.ts            full-screen GLSL ES 3.00 passes and half-float targets
src/frame-queue.ts    which frame to show at the playhead (pure)
src/layout.ts         working size, rotation and letterbox maths (pure)
src/protocol.ts       message types
```

The measurement page is `apps/spike/preview.html`.
