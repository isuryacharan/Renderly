// Exact seek: the frame showing at a given time, decoded from the keyframe before it.
//
// This drives VideoDecoder directly instead of using VideoSampleSink.getSample so it can skip
// H.264 frames that nothing references (see nal.ts) and close every frame except the target as
// soon as it leaves the decoder. Holding them would exhaust the hardware decoder's output pool.

import type { EncodedPacketSink } from "mediabunny";
import { avcLengthSize, isNonReferenceAvc } from "./nal";

export interface ExactSeeker {
  /**
   * Resolves with the frame whose presentation interval contains `seconds`, null if there is
   * none (before the first frame) or `cancelled()` became true, or "fallback" if this file
   * cannot be handled here and the caller should use the general path.
   */
  seek(seconds: number, cancelled: () => boolean): Promise<SeekedFrame | null | "fallback">;
  /** Non-reference frames skipped and frames decoded, since the seeker was created. */
  readonly counts: { skipped: number; decoded: number };
}

export interface SeekedFrame {
  frame: VideoFrame;
  /** Container timestamp and duration of the frame, in seconds. */
  timestamp: number;
  duration: number;
}

/** Packets decoded in presentation order past the target before giving up on finding it. */
const SEARCH_LIMIT_SECONDS = 10;

export function createExactSeeker(
  packets: EncodedPacketSink,
  config: VideoDecoderConfig,
  codec: string,
): ExactSeeker {
  const description = config.description;
  const avcC =
    codec === "avc" && description
      ? ArrayBuffer.isView(description)
        ? new Uint8Array(description.buffer, description.byteOffset, description.byteLength)
        : new Uint8Array(description)
      : null;
  const lengthSize = avcC ? avcLengthSize(avcC) : null;
  const counts = { skipped: 0, decoded: 0 };

  async function seek(seconds: number, cancelled: () => boolean) {
    const target = await packets.getPacket(seconds, { metadataOnly: true });
    let packet = await packets.getKeyPacket(seconds);
    if (!target || !packet || cancelled()) return null;

    // Output arrives in presentation order. Keep the latest frame at or before the target and
    // close everything else at once. The 1 µs margin absorbs microsecond rounding of timestamps.
    const limit = target.timestamp * 1e6 + 1;
    let best: VideoFrame | null = null;
    let failure: DOMException | null = null;
    const decoder = new VideoDecoder({
      output: (frame) => {
        if (frame.timestamp <= limit) {
          best?.close();
          best = frame;
        } else {
          frame.close();
        }
      },
      error: (error) => (failure = error),
    });

    try {
      decoder.configure(config);
      let reached = false;
      while (packet && !cancelled()) {
        const isTarget = packet.timestamp === target.timestamp;
        const skippable =
          lengthSize !== null &&
          !isTarget &&
          packet.type === "delta" &&
          packet.timestamp < target.timestamp &&
          isNonReferenceAvc(packet.data, lengthSize);
        if (skippable) {
          counts.skipped++;
        } else {
          decoder.decode(packet.toEncodedVideoChunk());
          counts.decoded++;
        }
        if (isTarget) {
          reached = true;
          break;
        }
        if (packet.timestamp > target.timestamp + SEARCH_LIMIT_SECONDS) break;
        packet = await packets.getNextPacket(packet);
      }
      if (cancelled()) return null;
      if (!reached) return "fallback";
      await decoder.flush();

      const frame = best as VideoFrame | null;
      if (cancelled()) return null;
      if (failure || !frame || Math.abs(frame.timestamp - target.timestamp * 1e6) > 1) {
        return "fallback";
      }
      best = null; // ownership passes to the caller
      return { frame, timestamp: target.timestamp, duration: target.duration };
    } catch (error) {
      if (failure || error instanceof DOMException) return "fallback";
      throw error;
    } finally {
      // Closing the decoder first guarantees no more output callbacks can set `best`.
      if (decoder.state !== "closed") decoder.close();
      (best as VideoFrame | null)?.close();
    }
  }

  return { seek, counts };
}
