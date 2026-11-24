// Audio decode service, run in a worker. Decodes a file's audio from a source time onward and
// writes it, sample-exact, into the session's ring buffer. Media is read by byte range.
//
// Source time 0 is the first video frame (as in @renderly/media), so audio and video share one
// time base even when the container starts the tracks at different timestamps.

import {
  ALL_FORMATS,
  AudioSampleSink,
  BlobSource,
  Input,
  UnsupportedInputFormatError,
  UrlSource,
} from "mediabunny";
import { FLICKS_PER_SECOND, type Endpoint, type Flicks, type MediaInput } from "@renderly/media";
import { AudioRing } from "./ring";

/** The rate the output runs at. Other source rates are refused for now (no resampler yet). */
export const OUTPUT_SAMPLE_RATE = 48_000;

export type AudioOpenResult =
  | { ok: true; sampleRate: number; channels: number; codec: string | null }
  | { ok: false; reason: "no-audio-track" | "unsupported-codec" | "unsupported-format" }
  | { ok: false; reason: "unsupported-sample-rate"; sampleRate: number };

export type ToAudioDecoder =
  | { type: "open"; id: number; input: MediaInput }
  /** Decodes from `from` into the ring until it ends or another fill or stop arrives. */
  | {
      type: "fill";
      session: number;
      ring: SharedArrayBuffer;
      channels: number;
      capacity: number;
      from: Flicks;
    }
  | { type: "stop" };

export type FromAudioDecoder =
  | { type: "opened"; id: number; result: AudioOpenResult }
  | { type: "filled"; session: number; frames: number }
  | { type: "error"; id?: number; session?: number; message: string };

interface OpenAudio {
  input: Input;
  sink: AudioSampleSink;
  /** Timestamp of the first video frame, in seconds: source time 0. */
  origin: number;
  channels: number;
}

const FULL_RING_WAIT_MS = 10;

/** Source time in flicks to a frame index at `rate`, rounded to the nearest frame. */
export function flicksToFrames(time: Flicks, rate: number): number {
  return Math.round((time * rate) / FLICKS_PER_SECOND);
}

export function serveAudioDecoder(endpoint: Endpoint): void {
  let audio: OpenAudio | null = null;
  let current = 0;

  const post = (message: FromAudioDecoder) => endpoint.postMessage(message, []);

  async function open(input: MediaInput): Promise<AudioOpenResult> {
    current = 0;
    audio?.input.dispose();
    audio = null;
    const source = input instanceof Blob ? new BlobSource(input) : new UrlSource(input.url);
    const media = new Input({ source, formats: ALL_FORMATS });
    try {
      const track = await media.getPrimaryAudioTrack();
      if (!track) {
        media.dispose();
        return { ok: false, reason: "no-audio-track" };
      }
      const codec = await track.getCodec();
      if (!(await track.canDecode())) {
        media.dispose();
        return { ok: false, reason: "unsupported-codec" };
      }
      const sampleRate = await track.getSampleRate();
      if (sampleRate !== OUTPUT_SAMPLE_RATE) {
        media.dispose();
        return { ok: false, reason: "unsupported-sample-rate", sampleRate };
      }
      const video = await media.getPrimaryVideoTrack();
      const origin = video ? await video.getFirstTimestamp() : await track.getFirstTimestamp();
      const channels = Math.min(2, await track.getNumberOfChannels());
      audio = { input: media, sink: new AudioSampleSink(track), origin, channels };
      return { ok: true, sampleRate, channels, codec };
    } catch (error) {
      media.dispose();
      if (error instanceof UnsupportedInputFormatError)
        return { ok: false, reason: "unsupported-format" };
      throw error;
    }
  }

  /** Writes all of `planes` into the ring, waiting while it is full. False if cancelled. */
  async function writeAll(
    ring: AudioRing,
    planes: Float32Array[],
    frames: number,
    session: number,
  ) {
    let done = 0;
    while (done < frames) {
      if (current !== session) return false;
      done += ring.write(planes, done, frames - done);
      if (done < frames) await new Promise((r) => setTimeout(r, FULL_RING_WAIT_MS));
    }
    return true;
  }

  async function fill(session: number, ring: AudioRing, from: Flicks): Promise<number> {
    if (!audio) throw new Error("No file is open");
    const { sink, origin } = audio;
    const rate = OUTPUT_SAMPLE_RATE;
    // Frame index (from source time 0) of the next frame the ring expects.
    let next = flicksToFrames(from, rate);
    let written = 0;

    for await (const sample of sink.samples(origin + next / rate)) {
      try {
        if (current !== session) break;
        const start = Math.round((sample.timestamp - origin) * rate);
        // A gap before this sample (e.g. audio that starts after the video) is silence.
        if (start > next) {
          const gap = start - next;
          let filled = 0;
          while (filled < gap && current === session) {
            filled += ring.writeSilence(gap - filled);
            if (filled < gap) await new Promise((r) => setTimeout(r, FULL_RING_WAIT_MS));
          }
          next = start;
        }
        const skip = next - start; // frames of this sample before the position we need
        const count = sample.numberOfFrames - skip;
        if (count <= 0) continue;
        const planes = Array.from({ length: ring.channels }, (_, c) => {
          const plane = new Float32Array(count);
          sample.copyTo(plane, {
            planeIndex: c,
            format: "f32-planar",
            frameOffset: skip,
            frameCount: count,
          });
          return plane;
        });
        if (!(await writeAll(ring, planes, count, session))) break;
        next += count;
        written += count;
      } finally {
        sample.close();
      }
    }
    return written;
  }

  endpoint.onmessage = (event: MessageEvent<ToAudioDecoder>) => {
    const message = event.data;
    switch (message.type) {
      case "open":
        open(message.input).then(
          (result) => post({ type: "opened", id: message.id, result }),
          (error: unknown) => post({ type: "error", id: message.id, message: String(error) }),
        );
        return;
      case "fill": {
        current = message.session;
        const ring = new AudioRing(message.ring, message.channels, message.capacity);
        fill(message.session, ring, message.from).then(
          (frames) => post({ type: "filled", session: message.session, frames }),
          (error: unknown) =>
            post({ type: "error", session: message.session, message: String(error) }),
        );
        return;
      }
      case "stop":
        current = 0;
        return;
    }
  };
}
