// The client side of a decode worker. It can run on the main thread or in another worker (the
// render worker talks to decode workers directly over a MessagePort).

import type { Endpoint } from "./decoder";
import type {
  DecodeStats,
  FromWorker,
  MediaInput,
  OpenResult,
  SeekMode,
  ToWorker,
} from "./protocol";
import type { Flicks } from "./time";

/** A decoded frame on loan from the decoder. Call `release()` as soon as it is on the GPU. */
export interface DecodedFrame {
  readonly frame: VideoFrame;
  /** Source time of the frame, in flicks from the first frame of the file. */
  readonly time: Flicks;
  readonly duration: Flicks;
  /** Closes the frame and lets the decoder send another. Safe to call twice. */
  release(): void;
}

export interface Playback {
  /** Resolves with the number of frames delivered once the range ends or playback is stopped. */
  readonly done: Promise<number>;
  stop(): void;
}

interface Stream {
  onFrame(frame: DecodedFrame): void;
  resolve(frames: number): void;
  reject(error: Error): void;
}

interface Request {
  resolve(message: FromWorker): void;
  reject(error: Error): void;
}

/** Frames the decoder may have out at once. Two on screen plus a few decoded ahead. */
export const DEFAULT_MAX_FRAMES_IN_FLIGHT = 6;

export class DecodeClient {
  private nextId = 1;
  private requests = new Map<number, Request>();
  private streams = new Map<number, Stream>();
  private current = 0;
  private outstandingFrames = 0;

  constructor(
    private readonly endpoint: Endpoint,
    private readonly onTerminate?: () => void,
  ) {
    endpoint.onmessage = (event: MessageEvent<FromWorker>) => this.receive(event.data);
  }

  /**
   * Frames received and not yet released. This is the leak counter: it must return to zero
   * once a consumer is idle.
   */
  get outstanding(): number {
    return this.outstandingFrames;
  }

  async open(
    input: MediaInput,
    maxFramesInFlight = DEFAULT_MAX_FRAMES_IN_FLIGHT,
  ): Promise<OpenResult> {
    const reply = await this.request((id) => ({ type: "open", id, input, maxFramesInFlight }));
    if (reply.type !== "opened") throw new Error(`Unexpected reply: ${reply.type}`);
    return reply.result;
  }

  /** Streams frames from `from` (the frame showing at that time) up to `to` (exclusive). */
  play(from: Flicks, to: Flicks, onFrame: (frame: DecodedFrame) => void): Playback {
    const id = this.nextId++;
    const done = this.stream(id, onFrame);
    this.send({ type: "play", id, from, to });
    return { done, stop: () => this.send({ type: "stop", id }) };
  }

  /**
   * The frame showing at `time` ("exact"), or the keyframe at or before it ("keyframe"), which
   * is much cheaper on long-GOP footage. Resolves null if a newer play or seek replaced it.
   */
  async seek(time: Flicks, mode: SeekMode = "exact"): Promise<DecodedFrame | null> {
    const id = this.nextId++;
    let result: DecodedFrame | null = null;
    const done = this.stream(id, (frame) => {
      result?.release();
      result = frame;
    });
    this.send({ type: "seek", id, time, mode });
    await done;
    return result;
  }

  /** Stops the current play or seek, if any. */
  stop(): void {
    if (this.current) this.send({ type: "stop", id: this.current });
  }

  async stats(): Promise<DecodeStats> {
    const reply = await this.request((id) => ({ type: "stats", id }));
    if (reply.type !== "stats") throw new Error(`Unexpected reply: ${reply.type}`);
    return reply.stats;
  }

  /** Closes the file. The worker stays alive for the next `open`. */
  async close(): Promise<void> {
    await this.request((id) => ({ type: "close", id }));
  }

  /** Ends the worker. Pending calls reject. */
  terminate(): void {
    const error = new Error("Decoder terminated");
    for (const r of this.requests.values()) r.reject(error);
    for (const s of this.streams.values()) s.reject(error);
    this.requests.clear();
    this.streams.clear();
    this.onTerminate?.();
  }

  private send(message: ToWorker): void {
    this.endpoint.postMessage(message, []);
  }

  private request(build: (id: number) => ToWorker): Promise<FromWorker> {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.requests.set(id, { resolve, reject });
      this.send(build(id));
    });
  }

  private stream(id: number, onFrame: (frame: DecodedFrame) => void): Promise<number> {
    // A newer play or seek replaces the current one in the worker.
    this.current = id;
    return new Promise((resolve, reject) => {
      this.streams.set(id, { onFrame, resolve, reject });
    });
  }

  private loan(frame: VideoFrame, time: Flicks, duration: Flicks): DecodedFrame {
    this.outstandingFrames++;
    let released = false;
    return {
      frame,
      time,
      duration,
      release: () => {
        if (released) return;
        released = true;
        frame.close();
        this.outstandingFrames--;
        this.send({ type: "credit", count: 1 });
      },
    };
  }

  private receive(message: FromWorker): void {
    switch (message.type) {
      case "frame": {
        const frame = this.loan(message.frame, message.time, message.duration);
        const stream = this.streams.get(message.id);
        // A frame can arrive after its stream ended (stop raced with delivery): return it.
        if (stream) stream.onFrame(frame);
        else frame.release();
        return;
      }
      case "done":
        this.finishStream(message.id)?.resolve(message.frames);
        return;
      case "error": {
        const error = new Error(message.message);
        const stream = this.finishStream(message.id);
        if (stream) stream.reject(error);
        else this.takeRequest(message.id)?.reject(error);
        return;
      }
      default:
        this.takeRequest(message.id)?.resolve(message);
    }
  }

  private finishStream(id: number): Stream | undefined {
    const stream = this.streams.get(id);
    this.streams.delete(id);
    if (this.current === id) this.current = 0;
    return stream;
  }

  private takeRequest(id: number): Request | undefined {
    const request = this.requests.get(id);
    this.requests.delete(id);
    return request;
  }
}

/** Starts a decode worker and returns its client. */
export function createDecodeClient(): DecodeClient {
  const worker = new Worker(new URL("./decode-worker.ts", import.meta.url), {
    type: "module",
    name: "decode",
  });
  return new DecodeClient(worker as unknown as Endpoint, () => worker.terminate());
}
