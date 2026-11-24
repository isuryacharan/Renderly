// The audio clock, shared between the AudioWorklet (writer), the main thread (writes the
// output-time mapping) and the render worker (reader), in one SharedArrayBuffer.
//
// The worklet publishes, after every render quantum, how many session frames it has consumed and
// the context frame where the last of them ends (the quantum end in steady play). The main thread publishes
// `getOutputTimestamp()`: which context time is reaching the speakers at which wall-clock time.
// Together they give the session time being heard at any wall-clock time, which is what the
// picture must match. Float fields are guarded by sequence counters (a seqlock), since a reader
// may run while the writer is mid-update.

import { FLICKS_PER_SECOND, type Flicks } from "@renderly/media";

// Int32 fields
const CLOCK_SEQ = 0;
const SESSION = 1; // session the worklet is playing or priming
const GO = 2; // session the render worker is ready to start
const STATE = 3;
const UNDERRUNS = 4;
const MAP_SEQ = 5;
const INTS = 8;
// Float64 fields
const QUANTUM_END_FRAME = 0; // context frame where the last consumed sample ends
const CONSUMED = 1; // session frames consumed by then
const SAMPLE_RATE = 2;
const MAP_CONTEXT_TIME = 3; // context time reaching the speakers...
const MAP_WALL_MS = 4; // ...at this wall-clock time (performance.timeOrigin + now), in ms
const MAX_STEP = 5; // largest sample-to-sample change in the output during steady play
const MAX_STEP_TRANSITION = 6; // the same, around starts and stops
const FLOATS = 8;

export const PlaybackState = {
  Idle: 0,
  /** A session is set; waiting for the render worker's go and enough buffered audio. */
  Priming: 1,
  Playing: 2,
  /** Fading out before going idle. */
  Stopping: 3,
} as const;
export type PlaybackState = (typeof PlaybackState)[keyof typeof PlaybackState];

export interface ClockReading {
  session: number;
  state: PlaybackState;
  quantumEndFrame: number;
  consumed: number;
  sampleRate: number;
}

export interface OutputMapping {
  contextTime: number;
  wallMs: number;
}

export class ClockBuffer {
  static readonly BYTES = INTS * 4 + FLOATS * 8;
  private readonly ints: Int32Array;
  private readonly floats: Float64Array;

  constructor(readonly buffer: SharedArrayBuffer = new SharedArrayBuffer(ClockBuffer.BYTES)) {
    this.ints = new Int32Array(buffer, 0, INTS);
    this.floats = new Float64Array(buffer, INTS * 4, FLOATS);
  }

  // --- worklet side ---

  setSession(session: number, state: PlaybackState): void {
    Atomics.store(this.ints, SESSION, session);
    Atomics.store(this.ints, STATE, state);
  }

  setState(state: PlaybackState): void {
    Atomics.store(this.ints, STATE, state);
  }

  publish(quantumEndFrame: number, consumed: number, sampleRate: number): void {
    Atomics.add(this.ints, CLOCK_SEQ, 1); // odd: writing
    this.floats[QUANTUM_END_FRAME] = quantumEndFrame;
    this.floats[CONSUMED] = consumed;
    this.floats[SAMPLE_RATE] = sampleRate;
    Atomics.add(this.ints, CLOCK_SEQ, 1); // even: done
  }

  countUnderrun(): void {
    Atomics.add(this.ints, UNDERRUNS, 1);
  }

  recordSteps(steady: number, transition: number): void {
    this.floats[MAX_STEP] = Math.max(this.floats[MAX_STEP] ?? 0, steady);
    this.floats[MAX_STEP_TRANSITION] = Math.max(this.floats[MAX_STEP_TRANSITION] ?? 0, transition);
  }

  goFor(): number {
    return Atomics.load(this.ints, GO);
  }

  // --- render worker side ---

  /** Tells the worklet that the picture is ready for `session` to start. */
  setGo(session: number): void {
    Atomics.store(this.ints, GO, session);
  }

  read(): ClockReading | null {
    for (let attempt = 0; attempt < 4; attempt++) {
      const before = Atomics.load(this.ints, CLOCK_SEQ);
      if (before % 2 !== 0) continue;
      const reading = {
        session: Atomics.load(this.ints, SESSION),
        state: Atomics.load(this.ints, STATE) as PlaybackState,
        quantumEndFrame: this.floats[QUANTUM_END_FRAME] ?? 0,
        consumed: this.floats[CONSUMED] ?? 0,
        sampleRate: this.floats[SAMPLE_RATE] ?? 0,
      };
      if (Atomics.load(this.ints, CLOCK_SEQ) === before) return reading;
    }
    return null;
  }

  // --- main thread side ---

  writeMapping(contextTime: number, wallMs: number): void {
    Atomics.add(this.ints, MAP_SEQ, 1);
    this.floats[MAP_CONTEXT_TIME] = contextTime;
    this.floats[MAP_WALL_MS] = wallMs;
    Atomics.add(this.ints, MAP_SEQ, 1);
  }

  readMapping(): OutputMapping | null {
    for (let attempt = 0; attempt < 4; attempt++) {
      const before = Atomics.load(this.ints, MAP_SEQ);
      if (before === 0 || before % 2 !== 0) continue;
      const mapping = {
        contextTime: this.floats[MAP_CONTEXT_TIME] ?? 0,
        wallMs: this.floats[MAP_WALL_MS] ?? 0,
      };
      if (Atomics.load(this.ints, MAP_SEQ) === before) return mapping;
    }
    return null;
  }

  diagnostics(): { underruns: number; maxStep: number; maxStepTransition: number } {
    return {
      underruns: Atomics.load(this.ints, UNDERRUNS),
      maxStep: this.floats[MAX_STEP] ?? 0,
      maxStepTransition: this.floats[MAX_STEP_TRANSITION] ?? 0,
    };
  }

  resetDiagnostics(): void {
    Atomics.store(this.ints, UNDERRUNS, 0);
    this.floats[MAX_STEP] = 0;
    this.floats[MAX_STEP_TRANSITION] = 0;
  }
}

/**
 * Session time being heard at wall-clock time `wallMs`, in flicks from the session start, or null
 * if nothing of the session has been heard yet.
 *
 * Frames consumed by the worklet but not yet at the speakers are the ones between the context
 * frame being heard (from the output mapping) and the end of the last processed quantum.
 */
export function heardSessionTime(
  clock: ClockReading,
  mapping: OutputMapping,
  wallMs: number,
): Flicks | null {
  if (clock.sampleRate <= 0) return null;
  const heardFrame = (mapping.contextTime + (wallMs - mapping.wallMs) / 1000) * clock.sampleRate;
  const notYetHeard = clock.quantumEndFrame - heardFrame;
  const heard = Math.min(clock.consumed, clock.consumed - notYetHeard);
  if (heard < 0) return null;
  return Math.round((heard * FLICKS_PER_SECOND) / clock.sampleRate);
}
