// eslint-disable-next-line @typescript-eslint/triple-slash-reference -- Vite's ambient `?worker&url` module type, needed by every package that imports this file
/// <reference path="./vite-env.d.ts" />
// Main-thread side of audio playback. The AudioContext must live here (it does not exist in
// workers); after setup the main thread only forwards play and stop and, every 100 ms, writes
// the output-time mapping into the clock buffer.

import type { Flicks, MediaInput } from "@renderly/media";
import {
  OUTPUT_SAMPLE_RATE,
  type AudioOpenResult,
  type FromAudioDecoder,
  type ToAudioDecoder,
} from "./audio-decoder";
import { ClockBuffer } from "./clock";
import type { AudioCut } from "./cut-frames";
import processorUrl from "./processor?worker&url";
import { PROCESSOR_NAME, type ToProcessor } from "./processor-protocol";
import { AudioRing } from "./ring";

/** Ring size: enough to ride out a busy decode worker, small enough to stay cheap. */
const RING_SECONDS = 2;
const MAPPING_INTERVAL_MS = 100;

export interface FillResult {
  session: number;
  frames: number;
  error: string | null;
}

export interface AudioOutput {
  /** Shared with the render worker, which follows it. */
  readonly clock: SharedArrayBuffer;
  /** Opens every source's audio; one result per source. */
  load(sources: MediaInput[]): Promise<AudioOpenResult[]>;
  /**
   * Starts session `session` at timeline time `from` over `cuts`. It plays once the render
   * worker sets the go flag.
   */
  play(session: number, from: Flicks, cuts: AudioCut[]): void;
  /** Fades out and stops. */
  stop(): void;
  diagnostics(): ReturnType<ClockBuffer["diagnostics"]>;
  /** How the most recent session's decode ended: samples written, or the error. */
  lastFill(): FillResult | null;
  resetDiagnostics(): void;
  readonly context: AudioContext;
  dispose(): void;
}

export async function createAudioOutput(): Promise<AudioOutput> {
  const context = new AudioContext({ sampleRate: OUTPUT_SAMPLE_RATE, latencyHint: "interactive" });
  await context.audioWorklet.addModule(processorUrl);
  const clock = new ClockBuffer();
  const node = new AudioWorkletNode(context, PROCESSOR_NAME, {
    numberOfInputs: 0,
    outputChannelCount: [2],
    processorOptions: { clock: clock.buffer },
  });
  node.connect(context.destination);
  const toProcessor = (message: ToProcessor) => node.port.postMessage(message);

  const worker = new Worker(new URL("./audio-worker.ts", import.meta.url), {
    type: "module",
    name: "audio-decode",
  });
  const toDecoder = (message: ToAudioDecoder) => worker.postMessage(message);
  let nextId = 1;
  const pending = new Map<number, (results: AudioOpenResult[] | Error) => void>();
  let channels = 2;
  let lastFill: FillResult | null = null;
  worker.onmessage = (event: MessageEvent<FromAudioDecoder>) => {
    const message = event.data;
    if (message.type === "loaded") {
      pending.get(message.id)?.(message.results);
      pending.delete(message.id);
    } else if (message.type === "error" && message.id !== undefined) {
      pending.get(message.id)?.(new Error(message.message));
      pending.delete(message.id);
    } else if (message.type === "filled") {
      lastFill = { session: message.session, frames: message.frames, error: null };
    } else if (message.type === "error" && message.session !== undefined) {
      lastFill = { session: message.session, frames: 0, error: message.message };
    }
  };

  const mapping = setInterval(() => {
    if (context.state !== "running") return;
    const stamp = context.getOutputTimestamp();
    if (stamp.contextTime === undefined || stamp.performanceTime === undefined) return;
    clock.writeMapping(stamp.contextTime, performance.timeOrigin + stamp.performanceTime);
  }, MAPPING_INTERVAL_MS);

  return {
    clock: clock.buffer,
    context,
    async load(sources) {
      const id = nextId++;
      const results = await new Promise<AudioOpenResult[] | Error>((resolve) => {
        pending.set(id, resolve);
        toDecoder({ type: "load", id, sources });
      });
      if (results instanceof Error) throw results;
      channels = Math.max(1, ...results.map((result) => (result.ok ? result.channels : 1)));
      return results;
    },
    play(session, from, cuts) {
      void context.resume();
      const capacity = RING_SECONDS * OUTPUT_SAMPLE_RATE;
      const ring = AudioRing.create(channels, capacity);
      toProcessor({ type: "start", session, ring: ring.buffer, channels, capacity });
      toDecoder({ type: "fill", session, ring: ring.buffer, channels, capacity, from, cuts });
    },
    stop() {
      toProcessor({ type: "stop" });
      toDecoder({ type: "stop" });
    },
    diagnostics: () => clock.diagnostics(),
    lastFill: () => lastFill,
    resetDiagnostics: () => clock.resetDiagnostics(),
    dispose() {
      clearInterval(mapping);
      worker.terminate();
      void context.close();
    },
  };
}
