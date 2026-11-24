// The decode service that runs inside a worker. One service holds one open file.
//
// Media is read by byte range (BlobSource / UrlSource) and never loaded whole. Each decoded
// frame is transferred, not copied, to the client. The client returns a credit for every frame
// it closes (see CreditGate).

import {
  ALL_FORMATS,
  BlobSource,
  EncodedPacketSink,
  Input,
  UnsupportedInputFormatError,
  UrlSource,
  VideoSampleSink,
} from "mediabunny";
import { CreditGate } from "./credits";
import { createExactSeeker, type ExactSeeker } from "./exact-seek";
import type {
  DecodeStats,
  FromWorker,
  MediaInput,
  OpenResult,
  SeekMode,
  ToWorker,
} from "./protocol";
import { flicksToSeconds, lookupSeconds, secondsToFlicks, sourceFlicks, type Flicks } from "./time";

/** The parts of a Worker scope or MessagePort that the decoder and its client use. */
export interface Endpoint {
  postMessage(message: unknown, transfer: Transferable[]): void;
  onmessage: ((event: MessageEvent) => void) | null;
}

interface OpenFile {
  input: Input;
  samples: VideoSampleSink;
  packets: EncodedPacketSink;
  seeker: ExactSeeker | null;
  /** Timestamp of the first video frame, in seconds. Source time 0 maps to it. */
  first: number;
  credits: CreditGate;
}

export function serveDecoder(endpoint: Endpoint): void {
  let file: OpenFile | null = null;
  /** Id of the play or seek that may send frames. Anything else stops at its next step. */
  let current = 0;
  let framesSent = 0;
  let framesHeld = 0;

  const post = (message: FromWorker, transfer: Transferable[] = []) =>
    endpoint.postMessage(message, transfer);

  function stopCurrent(): void {
    current = 0;
    file?.credits.cancelWaiters();
  }

  function closeFile(): void {
    stopCurrent();
    file?.input.dispose();
    file = null;
  }

  async function open(input: MediaInput, maxFramesInFlight: number): Promise<OpenResult> {
    closeFile();
    const source = input instanceof Blob ? new BlobSource(input) : new UrlSource(input.url);
    const media = new Input({ source, formats: ALL_FORMATS });
    try {
      const tracks = await media.getTracks();
      const video = await media.getPrimaryVideoTrack();
      if (!video) {
        media.dispose();
        return { ok: false, reason: "no-video-track" };
      }
      const codec = await video.getCodec();
      const config = await video.getDecoderConfig();
      if (!codec || !config || !(await video.canDecode())) {
        media.dispose();
        return { ok: false, reason: "unsupported-codec", codec };
      }
      const first = await video.getFirstTimestamp();
      const end = await video.computeDuration();
      const packets = new EncodedPacketSink(video);
      file = {
        input: media,
        samples: new VideoSampleSink(video),
        packets,
        seeker: createExactSeeker(packets, config, codec),
        first,
        credits: new CreditGate(maxFramesInFlight),
      };
      return {
        ok: true,
        duration: secondsToFlicks(end - first),
        width: await video.getDisplayWidth(),
        height: await video.getDisplayHeight(),
        rotation: await video.getRotation(),
        codec,
        tracks: await Promise.all(
          tracks.map(async (t) => ({
            kind: t.isVideoTrack() ? ("video" as const) : ("audio" as const),
            codec: await t.getCodec(),
          })),
        ),
      };
    } catch (error) {
      media.dispose();
      if (error instanceof UnsupportedInputFormatError) {
        return { ok: false, reason: "unsupported-format", detail: error.message };
      }
      throw error;
    }
  }

  /**
   * Sends one frame if `id` is still current and a credit is available. Takes ownership of
   * `frame`: it is either transferred or closed.
   */
  async function send(
    f: OpenFile,
    id: number,
    frame: VideoFrame,
    timestamp: number,
    duration: number,
  ): Promise<boolean> {
    framesHeld++;
    let sent = false;
    try {
      if (current !== id) return false;
      const granted = await f.credits.acquire();
      if (current !== id) {
        if (granted) f.credits.release();
        return false;
      }
      post(
        {
          type: "frame",
          id,
          frame,
          time: sourceFlicks(timestamp, f.first),
          duration: secondsToFlicks(duration),
        },
        [frame],
      );
      framesSent++;
      sent = true;
      return true;
    } finally {
      if (!sent) frame.close();
      framesHeld--;
    }
  }

  async function play(f: OpenFile, id: number, from: Flicks, to: Flicks): Promise<number> {
    let frames = 0;
    // The end is exclusive: a frame starting exactly at `to` belongs to whatever comes next.
    const samples = f.samples.samples(lookupSeconds(from, f.first), f.first + flicksToSeconds(to));
    for await (const sample of samples) {
      const frame = sample.toVideoFrame();
      const { timestamp, duration } = sample;
      sample.close();
      if (!(await send(f, id, frame, timestamp, duration))) break;
      frames++;
    }
    return frames;
  }

  async function seek(f: OpenFile, id: number, time: Flicks, mode: SeekMode): Promise<number> {
    let target = lookupSeconds(time, f.first);
    const cancelled = () => current !== id;

    if (mode === "exact" && f.seeker) {
      const result = await f.seeker.seek(target, cancelled);
      if (result === null) return 0;
      if (result !== "fallback") {
        return (await send(f, id, result.frame, result.timestamp, result.duration)) ? 1 : 0;
      }
    }

    if (mode === "keyframe") {
      const key = await f.packets.getKeyPacket(target, { metadataOnly: true });
      if (key) target = key.timestamp;
    }
    if (cancelled()) return 0;
    const sample = await f.samples.getSample(target);
    if (!sample) return 0;
    const frame = sample.toVideoFrame();
    const { timestamp, duration } = sample;
    sample.close();
    return (await send(f, id, frame, timestamp, duration)) ? 1 : 0;
  }

  async function run(id: number, job: (f: OpenFile) => Promise<number>): Promise<void> {
    if (!file) throw new Error("No file is open");
    stopCurrent();
    current = id;
    try {
      post({ type: "done", id, frames: await job(file) });
    } finally {
      if (current === id) current = 0;
    }
  }

  function stats(): DecodeStats {
    return {
      framesInFlight: file?.credits.inFlight ?? 0,
      framesSent,
      framesHeld,
      seekFramesSkipped: file?.seeker?.counts.skipped ?? 0,
      seekFramesDecoded: file?.seeker?.counts.decoded ?? 0,
    };
  }

  async function handle(message: ToWorker): Promise<void> {
    switch (message.type) {
      case "open":
        post({
          type: "opened",
          id: message.id,
          result: await open(message.input, message.maxFramesInFlight),
        });
        return;
      case "play":
        return run(message.id, (f) => play(f, message.id, message.from, message.to));
      case "seek":
        return run(message.id, (f) => seek(f, message.id, message.time, message.mode));
      case "stop":
        if (current === message.id) stopCurrent();
        return;
      case "credit":
        file?.credits.release(message.count);
        return;
      case "stats":
        post({ type: "stats", id: message.id, stats: stats() });
        return;
      case "close":
        closeFile();
        post({ type: "closed", id: message.id });
        return;
    }
  }

  endpoint.onmessage = (event: MessageEvent<ToWorker>) => {
    const message = event.data;
    handle(message).catch((error: unknown) => {
      const id = "id" in message ? message.id : 0;
      post({ type: "error", id, message: error instanceof Error ? error.message : String(error) });
    });
  };
}
