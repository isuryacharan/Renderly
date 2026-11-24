// Shared between the main thread and the AudioWorklet processor. Kept apart from processor.ts,
// whose top-level code only runs inside an AudioWorkletGlobalScope.

export const PROCESSOR_NAME = "renderly-playback";

export type ToProcessor =
  | {
      type: "start";
      session: number;
      ring: SharedArrayBuffer;
      channels: number;
      capacity: number;
    }
  | { type: "stop" };
