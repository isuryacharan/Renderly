// The AudioWorklet processor. Runs on the audio rendering thread; loaded by `createAudioOutput`.
// All logic is in PlaybackCore; this file only adapts it to the worklet API.

import { ClockBuffer } from "./clock";
import { PlaybackCore } from "./playback-core";
import { PROCESSOR_NAME, type ToProcessor } from "./processor-protocol";
import { AudioRing } from "./ring";

/** The parts of AudioWorkletGlobalScope used here (not in TypeScript's DOM types). */
interface WorkletScope {
  AudioWorkletProcessor: new () => { readonly port: MessagePort };
  registerProcessor(name: string, processor: unknown): void;
  currentFrame: number;
  sampleRate: number;
}

const scope = globalThis as unknown as WorkletScope;

class PlaybackProcessor extends scope.AudioWorkletProcessor {
  private readonly core: PlaybackCore;

  constructor(options: { processorOptions: { clock: SharedArrayBuffer } }) {
    super();
    this.core = new PlaybackCore(new ClockBuffer(options.processorOptions.clock), scope.sampleRate);
    this.port.onmessage = (event: MessageEvent<ToProcessor>) => {
      const message = event.data;
      if (message.type === "start") {
        this.core.start(
          message.session,
          new AudioRing(message.ring, message.channels, message.capacity),
        );
      } else {
        this.core.stop();
      }
    };
  }

  process(_inputs: Float32Array[][], outputs: Float32Array[][]): boolean {
    const output = outputs[0];
    if (output) this.core.process(output, scope.currentFrame);
    return true;
  }
}

scope.registerProcessor(PROCESSOR_NAME, PlaybackProcessor);
