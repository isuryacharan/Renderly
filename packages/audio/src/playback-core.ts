// What the AudioWorklet does each render quantum, as a plain class so it can be tested without
// an audio device: read the ring, apply gain ramps, publish the clock.
//
// Every start and stop is a short gain ramp, so pausing, resuming and seeking never cut the
// waveform mid-cycle (an audible click). A new session that arrives while one is playing waits
// for the old one to fade out.

import { ClockBuffer, PlaybackState } from "./clock";
import type { AudioRing } from "./ring";

export const RAMP_SECONDS = 0.005;
/** Audio buffered before a session may start, so the first quanta never underrun. */
export const PREFILL_SECONDS = 0.1;

interface Session {
  id: number;
  ring: AudioRing;
}

export class PlaybackCore {
  private session: Session | null = null;
  private pending: Session | null = null;
  private state: PlaybackState = PlaybackState.Idle;
  private gain = 0;
  private target = 0;
  private consumed = 0;
  /** Context frame where the last consumed sample ends; null before any is consumed. */
  private consumedEnd: number | null = null;
  private readonly step: number;
  private readonly prefill: number;
  private readonly lastSample: number[];
  /** Quanta left during which output changes count as a transition, not steady play. */
  private transitionQuanta = 0;

  constructor(
    private readonly clock: ClockBuffer,
    private readonly sampleRate: number,
    channels = 2,
  ) {
    this.step = 1 / Math.max(1, Math.round(RAMP_SECONDS * sampleRate));
    this.prefill = Math.round(PREFILL_SECONDS * sampleRate);
    this.lastSample = new Array<number>(channels).fill(0);
  }

  /** Starts a session. It plays once the clock's go flag names it and the ring is prefilled. */
  start(id: number, ring: AudioRing): void {
    const next = { id, ring };
    if (this.state === PlaybackState.Playing || this.state === PlaybackState.Stopping) {
      this.pending = next;
      this.fadeOut();
    } else {
      this.activate(next);
    }
  }

  /** Fades out and goes idle. */
  stop(): void {
    this.pending = null;
    if (this.state === PlaybackState.Playing) this.fadeOut();
    else if (this.state === PlaybackState.Priming) this.goIdle();
  }

  process(output: Float32Array[], currentFrame: number): void {
    const frames = output[0]?.length ?? 0;
    const session = this.session;

    if (
      session &&
      this.state === PlaybackState.Priming &&
      this.clock.goFor() === session.id &&
      session.ring.readable >= this.prefill
    ) {
      this.state = PlaybackState.Playing;
      this.target = 1;
      this.transitionQuanta = this.rampQuanta(frames);
    }

    if (
      session &&
      (this.state === PlaybackState.Playing || this.state === PlaybackState.Stopping)
    ) {
      const read = session.ring.read(output, 0, frames);
      for (const channel of output) channel.fill(0, read);
      if (read < frames && this.state === PlaybackState.Playing && !session.ring.complete) {
        this.clock.countUnderrun();
      }
      this.consumed += read;
      if (read > 0) this.consumedEnd = currentFrame + read;
      this.applyGain(output, frames);
      if (this.state === PlaybackState.Stopping && this.gain === 0) {
        this.goIdle();
        if (this.pending) this.activate(this.pending);
      }
    } else {
      for (const channel of output) channel.fill(0);
    }

    this.measureSteps(output, frames);
    if (this.session) {
      // After an underrun or the end, the last consumed sample stays where it was output, so
      // the heard time keeps catching up with it instead of stopping one output latency short.
      this.clock.publish(this.consumedEnd ?? currentFrame + frames, this.consumed, this.sampleRate);
    }
    this.clock.setState(this.state);
  }

  private activate(session: Session): void {
    this.session = session;
    this.pending = null;
    this.state = PlaybackState.Priming;
    this.gain = 0;
    this.target = 0;
    this.consumed = 0;
    this.consumedEnd = null;
    this.clock.setSession(session.id, PlaybackState.Priming);
  }

  private fadeOut(): void {
    this.state = PlaybackState.Stopping;
    this.target = 0;
    this.transitionQuanta = this.rampQuanta(128);
  }

  private goIdle(): void {
    this.session = null;
    this.state = PlaybackState.Idle;
    this.gain = 0;
    this.target = 0;
  }

  private rampQuanta(frames: number): number {
    return Math.ceil(1 / this.step / Math.max(1, frames)) + 2;
  }

  private applyGain(output: Float32Array[], frames: number): void {
    for (let i = 0; i < frames; i++) {
      if (this.gain < this.target) this.gain = Math.min(this.target, this.gain + this.step);
      else if (this.gain > this.target) this.gain = Math.max(this.target, this.gain - this.step);
      for (const channel of output) channel[i] = (channel[i] ?? 0) * this.gain;
    }
  }

  /** Records the largest sample-to-sample change, to detect clicks. */
  private measureSteps(output: Float32Array[], frames: number): void {
    let largest = 0;
    output.forEach((channel, c) => {
      let previous = this.lastSample[c] ?? 0;
      for (let i = 0; i < frames; i++) {
        const sample = channel[i] ?? 0;
        largest = Math.max(largest, Math.abs(sample - previous));
        previous = sample;
      }
      this.lastSample[c] = previous;
    });
    if (this.transitionQuanta > 0) {
      this.transitionQuanta--;
      this.clock.recordSteps(0, largest);
    } else {
      this.clock.recordSteps(largest, 0);
    }
  }
}
