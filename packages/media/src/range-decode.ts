// Decodes a range of frames for playback, driving VideoDecoder directly.
//
// Two things mediabunny's sample iterator cannot do, both needed to prepare upcoming cuts
// without disturbing the picture on screen (measured in S0.5):
// - Pacing. Feeding a hardware decoder a burst of 4K frames stalls the browser's GPU thread for
//   12-70 ms, which delays display refreshes and drops frames. Submitting one frame every 8 ms
//   per decoder causes no stall, even with three decoders working at once.
// - Skipping. Frames before the first wanted frame that nothing references (H.264 nal_ref_idc 0)
//   are not decoded at all, as in exact seek.

import type { EncodedPacketSink } from "mediabunny";
import { avcLengthSize, isNonReferenceAvc } from "./nal";

export interface RangeDecodeOptions {
  packets: EncodedPacketSink;
  config: VideoDecoderConfig;
  codec: string;
  /** Container time of the first wanted frame: the frame showing at this time is the first sent. */
  from: number;
  /** Container time where the range ends (exclusive). */
  end: number;
  /** Minimum time between decode submissions, in ms. 0 decodes as fast as the decoder allows. */
  paceMs: number;
  cancelled: () => boolean;
  /**
   * Receives each wanted frame in presentation order, with its exact container timestamp and
   * duration in seconds, and takes ownership of it. Resolves false to stop decoding.
   */
  deliver: (frame: VideoFrame, timestamp: number, duration: number) => Promise<boolean>;
}

/** Decode requests allowed to wait inside the decoder plus frames waiting to be delivered. */
const MAX_AHEAD = 4;
/** Packets past the end are still fed for this long, so reordered frames before it come out. */
const REORDER_MARGIN_SECONDS = 0.25;

export async function decodeRange(options: RangeDecodeOptions): Promise<number> {
  const { packets, config, codec, from, end, paceMs, cancelled, deliver } = options;
  const first = await packets.getPacket(from, { metadataOnly: true });
  const key = await packets.getKeyPacket(from);
  if (!first || !key || cancelled()) return 0;

  const description = config.description;
  const avcC =
    codec === "avc" && description
      ? ArrayBuffer.isView(description)
        ? new Uint8Array(description.buffer, description.byteOffset, description.byteLength)
        : new Uint8Array(description)
      : null;
  const lengthSize = avcC ? avcLengthSize(avcC) : null;

  // Output arrives in presentation order; keep only frames in [first, end).
  const lowest = first.timestamp * 1e6 - 1;
  const highest = end * 1e6 - 1;
  const ready: VideoFrame[] = [];
  // Frame timestamps are whole microseconds; map them back to the exact packet times.
  const exact = new Map<number, { timestamp: number; duration: number }>();
  let failure: unknown = null;
  let wake: (() => void) | null = null;
  const notify = () => {
    wake?.();
    wake = null;
  };
  /** Waits for decoder output, a dequeue or an error; at most 50 ms so cancellation is seen. */
  const nextEvent = () =>
    new Promise<void>((resolve) => {
      wake = resolve;
      setTimeout(resolve, 50);
    });
  const decoder = new VideoDecoder({
    output: (frame) => {
      if (frame.timestamp < lowest || frame.timestamp > highest) frame.close();
      else ready.push(frame);
      notify();
    },
    error: (error) => {
      failure = error;
      notify();
    },
  });
  decoder.addEventListener("dequeue", notify);

  let delivered = 0;
  let stopped = false;
  /** Sends every ready frame. False when the receiver asked to stop. */
  const drain = async () => {
    while (ready.length > 0 && !stopped) {
      const frame = ready.shift() as VideoFrame;
      const time = exact.get(frame.timestamp);
      exact.delete(frame.timestamp);
      if (cancelled()) {
        frame.close();
        stopped = true;
        return false;
      }
      // deliver owns the frame from here, whatever it returns.
      const ok = await deliver(
        frame,
        time?.timestamp ?? frame.timestamp / 1e6,
        time?.duration ?? (frame.duration ?? 0) / 1e6,
      );
      if (!ok) {
        stopped = true;
        return false;
      }
      delivered++;
    }
    return !stopped;
  };
  const waitForRoom = async () => {
    while (!stopped && !failure && ready.length + decoder.decodeQueueSize >= MAX_AHEAD) {
      if (ready.length > 0) {
        if (!(await drain())) return;
      } else {
        await nextEvent();
      }
    }
  };

  try {
    decoder.configure(config);
    let lastSubmit = -Infinity;
    // packets() preloads ahead of the consumer; fetching one packet at a time instead left the
    // decoder waiting on cold byte-range reads for up to ~180 ms in the S0.5 stress run.
    for await (const packet of packets.packets(key)) {
      if (cancelled() || stopped || failure) break;
      if (packet.timestamp >= end + REORDER_MARGIN_SECONDS) break;
      if (packet.type === "key" && packet.timestamp >= end) break;
      const skippable =
        lengthSize !== null &&
        packet.type === "delta" &&
        packet.timestamp < first.timestamp &&
        isNonReferenceAvc(packet.data, lengthSize);
      if (!skippable) {
        await waitForRoom();
        if (stopped || cancelled()) break;
        const wait = lastSubmit + paceMs - performance.now();
        if (wait > 0) await new Promise((resolve) => setTimeout(resolve, wait));
        const chunk = packet.toEncodedVideoChunk();
        exact.set(chunk.timestamp, { timestamp: packet.timestamp, duration: packet.duration });
        decoder.decode(chunk);
        lastSubmit = performance.now();
      }
      if (ready.length > 0 && !(await drain())) break;
    }
    if (!stopped && !cancelled() && !failure) {
      // Keep delivering while flushing. The decoder has a fixed pool of output surfaces; frames
      // held by the receiver and frames waiting here use them up, so a flush awaited before
      // delivering can wait forever for the receiver to let go.
      let flushed = false;
      decoder.flush().then(
        () => {
          flushed = true;
          notify();
        },
        (error: unknown) => {
          failure ??= error;
          notify();
        },
      );
      while ((!flushed || ready.length > 0) && !failure && !stopped) {
        if (ready.length > 0) {
          if (!(await drain())) break;
        } else {
          await nextEvent();
        }
      }
    }
    if (failure) throw failure;
    return delivered;
  } finally {
    if (decoder.state !== "closed") decoder.close();
    for (const frame of ready.splice(0)) frame.close();
  }
}
