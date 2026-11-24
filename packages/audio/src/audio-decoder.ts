// Audio decode service, run in a worker. Decodes the audio of an edit list from a timeline time
// onward and writes it, sample-exact, into the session's ring buffer. Each cut contributes
// exactly its samples on the timeline grid (cut-frames.ts); gaps and sources without audio are
// silence. Media is read by byte range.
//
// Source time 0 is the first video frame of each file (as in @renderly/media), so audio and
// video share one time base even when the container starts the tracks at different timestamps.

import {
  ALL_FORMATS,
  AudioSampleSink,
  BlobSource,
  Input,
  UnsupportedInputFormatError,
  UrlSource,
} from "mediabunny";
import type { Endpoint, Flicks, MediaInput } from "@renderly/media";
import { cutFrames, toFrames, type AudioCut } from "./cut-frames";
import { AudioRing } from "./ring";

/** The rate the output runs at. Other source rates are refused for now (no resampler yet). */
export const OUTPUT_SAMPLE_RATE = 48_000;

export type AudioOpenResult =
  | { ok: true; sampleRate: number; channels: number; codec: string | null }
  | { ok: false; reason: "no-audio-track" | "unsupported-codec" | "unsupported-format" }
  | { ok: false; reason: "unsupported-sample-rate"; sampleRate: number };

export type ToAudioDecoder =
  /** Opens every source; replaces anything loaded before. */
  | { type: "load"; id: number; sources: MediaInput[] }
  /** Decodes the cuts from `from` into the ring until done or another fill or stop arrives. */
  | {
      type: "fill";
      session: number;
      ring: SharedArrayBuffer;
      channels: number;
      capacity: number;
      from: Flicks;
      cuts: AudioCut[];
    }
  | { type: "stop" };

export type FromAudioDecoder =
  | { type: "loaded"; id: number; results: AudioOpenResult[] }
  | { type: "filled"; session: number; frames: number }
  | { type: "error"; id?: number; session?: number; message: string };

interface OpenAudio {
  input: Input;
  sink: AudioSampleSink;
  /** Timestamp of the first video frame, in seconds: source time 0. */
  origin: number;
}

const FULL_RING_WAIT_MS = 10;
const RATE = OUTPUT_SAMPLE_RATE;

async function openAudio(
  input: MediaInput,
): Promise<{ result: AudioOpenResult; audio: OpenAudio | null }> {
  const source = input instanceof Blob ? new BlobSource(input) : new UrlSource(input.url);
  const media = new Input({ source, formats: ALL_FORMATS });
  const fail = (result: AudioOpenResult) => {
    media.dispose();
    return { result, audio: null };
  };
  try {
    const track = await media.getPrimaryAudioTrack();
    if (!track) return fail({ ok: false, reason: "no-audio-track" });
    const codec = await track.getCodec();
    if (!(await track.canDecode())) return fail({ ok: false, reason: "unsupported-codec" });
    const sampleRate = await track.getSampleRate();
    if (sampleRate !== RATE)
      return fail({ ok: false, reason: "unsupported-sample-rate", sampleRate });
    const video = await media.getPrimaryVideoTrack();
    const origin = video ? await video.getFirstTimestamp() : await track.getFirstTimestamp();
    const channels = Math.min(2, await track.getNumberOfChannels());
    return {
      result: { ok: true, sampleRate, channels, codec },
      audio: { input: media, sink: new AudioSampleSink(track), origin },
    };
  } catch (error) {
    if (error instanceof UnsupportedInputFormatError)
      return fail({ ok: false, reason: "unsupported-format" });
    media.dispose();
    throw error;
  }
}

export function serveAudioDecoder(endpoint: Endpoint): void {
  let sources: (OpenAudio | null)[] = [];
  let current = 0;

  const post = (message: FromAudioDecoder) => endpoint.postMessage(message, []);
  const wait = () => new Promise((resolve) => setTimeout(resolve, FULL_RING_WAIT_MS));

  async function load(inputs: MediaInput[]): Promise<AudioOpenResult[]> {
    current = 0;
    for (const audio of sources) audio?.input.dispose();
    sources = [];
    const results: AudioOpenResult[] = [];
    for (const input of inputs) {
      const { result, audio } = await openAudio(input);
      results.push(result);
      sources.push(audio);
    }
    return results;
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
      if (done < frames) await wait();
    }
    return true;
  }

  async function writeSilence(ring: AudioRing, frames: number, session: number) {
    let done = 0;
    while (done < frames) {
      if (current !== session) return false;
      done += ring.writeSilence(frames - done);
      if (done < frames) await wait();
    }
    return true;
  }

  /**
   * Writes exactly `count` samples of `audio` starting at source sample `start`. Missing audio
   * (before the track starts or after it ends) is silence. False if cancelled.
   */
  async function writeRange(
    ring: AudioRing,
    audio: OpenAudio,
    start: number,
    count: number,
    session: number,
  ) {
    let next = start;
    const end = start + count;
    for await (const sample of audio.sink.samples(audio.origin + start / RATE)) {
      try {
        if (current !== session) return false;
        const first = Math.round((sample.timestamp - audio.origin) * RATE);
        if (first > next) {
          const gap = Math.min(first, end) - next;
          if (!(await writeSilence(ring, gap, session))) return false;
          next += gap;
        }
        if (next >= end) break;
        const skip = next - first;
        const take = Math.min(sample.numberOfFrames - skip, end - next);
        if (take <= 0) continue;
        const planes = Array.from({ length: ring.channels }, (_, c) => {
          const plane = new Float32Array(take);
          const planeIndex = Math.min(c, sample.numberOfChannels - 1);
          sample.copyTo(plane, {
            planeIndex,
            format: "f32-planar",
            frameOffset: skip,
            frameCount: take,
          });
          return plane;
        });
        if (!(await writeAll(ring, planes, take, session))) return false;
        next += take;
        if (next >= end) break;
      } finally {
        sample.close();
      }
    }
    return next < end ? writeSilence(ring, end - next, session) : true;
  }

  async function fill(
    session: number,
    ring: AudioRing,
    from: Flicks,
    cuts: AudioCut[],
  ): Promise<number> {
    const sorted = [...cuts].sort((a, b) => a.start - b.start);
    let next = toFrames(from, RATE); // next timeline sample the ring expects
    const begin = next;
    for (const cut of sorted) {
      const range = cutFrames(cut, from, RATE);
      if (!range) continue;
      if (
        range.timelineStart > next &&
        !(await writeSilence(ring, range.timelineStart - next, session))
      )
        break;
      next = range.timelineStart;
      const audio = sources[cut.source];
      const ok = audio
        ? await writeRange(ring, audio, range.sourceStart, range.count, session)
        : await writeSilence(ring, range.count, session);
      if (!ok) return next - begin;
      next += range.count;
    }
    if (current === session) ring.markComplete();
    return next - begin;
  }

  endpoint.onmessage = (event: MessageEvent<ToAudioDecoder>) => {
    const message = event.data;
    switch (message.type) {
      case "load":
        load(message.sources).then(
          (results) => post({ type: "loaded", id: message.id, results }),
          (error: unknown) => post({ type: "error", id: message.id, message: String(error) }),
        );
        return;
      case "fill": {
        current = message.session;
        const ring = new AudioRing(message.ring, message.channels, message.capacity);
        fill(message.session, ring, message.from, message.cuts).then(
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
