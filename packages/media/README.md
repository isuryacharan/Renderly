# @renderly/media

Demux and decode in a worker. A file is read by byte range and never loaded whole; decoded
frames cross to the consumer by transfer and are closed as soon as they are used.

Status: M0 spike (slice S0.2). One worker holds one open file. Decoder pools and scheduling come
in S0.5.

## Use

```ts
import { createDecodeClient, FLICKS_PER_SECOND } from "@renderly/media";

const decoder = createDecodeClient();
const info = await decoder.open(file); // File or Blob; { url } for benchmarks only
if (!info.ok) throw new Error(info.reason); // "unsupported-format" | "no-video-track" | "unsupported-codec"

// The frame showing at 12 s. "keyframe" is cheaper and lands on the keyframe at or before it.
const frame = await decoder.seek(12 * FLICKS_PER_SECOND, "exact");
upload(frame.frame);
frame.release(); // closes the VideoFrame and lets the decoder send another

// Every frame from 10 s up to (not including) 14 s, in order.
const playback = decoder.play(10 * FLICKS_PER_SECOND, 14 * FLICKS_PER_SECOND, (f) => {
  upload(f.frame);
  f.release();
});
await playback.done;
```

All times are integer flicks from the first frame of the file. Conversion to the seconds and
microseconds that mediabunny and WebCodecs use happens only in `time.ts`.

## Rules

- **Release every frame.** `decoder.outstanding` counts frames received and not released. It is
  the leak counter, and it must return to zero when the consumer is idle.
- **Back-pressure is by credit.** The worker sends at most `maxFramesInFlight` frames (default 6)
  before the consumer releases one. A consumer that leaks frames stalls the decoder instead of
  filling GPU memory.
- A newer `play` or `seek` replaces the current one. Frames that arrive for a replaced request
  are closed by the client.

## Exact seek

An exact seek decodes from the keyframe before the target up to the target. For H.264 it skips
frames before the target that no other frame references (`nal_ref_idc = 0`), and closes every
decoded frame except the target as soon as it leaves the decoder. If the target cannot be found
that way, it falls back to mediabunny's `VideoSampleSink.getSample`. HEVC and other codecs
always take the full decode for now.

## Files

```
src/client.ts       DecodeClient: requests, frame loans, the leak counter
src/decoder.ts      serveDecoder: the worker side (open, play, seek, stats, close)
src/decode-worker.ts  worker entry point
src/exact-seek.ts   exact seek with non-reference frame skipping
src/nal.ts          H.264 NAL reading for the skip decision
src/credits.ts      CreditGate (back-pressure)
src/protocol.ts     message types
src/time.ts         flicks and the conversion to container time
```

The measurement page is `apps/spike/decode.html`. Test media comes from
`node scripts/make-test-media.mjs`.
