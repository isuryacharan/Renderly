import { describe, expect, it } from "vitest";
import { ClockBuffer, PlaybackState } from "./clock";
import { PlaybackCore, PREFILL_SECONDS } from "./playback-core";
import { AudioRing } from "./ring";

const RATE = 48_000;
const QUANTUM = 128;

/** A full-scale 1 kHz sine, written into a ring. */
function sineRing(seconds: number): AudioRing {
  const frames = Math.round(seconds * RATE);
  const ring = AudioRing.create(1, frames + 1);
  const sine = Float32Array.from({ length: frames }, (_, i) =>
    Math.sin((2 * Math.PI * 1000 * i) / RATE),
  );
  ring.write([sine], 0, frames);
  return ring;
}

function setup() {
  const clock = new ClockBuffer();
  const core = new PlaybackCore(clock, RATE);
  let frame = 0;
  const run = (quanta: number) => {
    const outputs: Float32Array[][] = [];
    for (let q = 0; q < quanta; q++) {
      const out = [new Float32Array(QUANTUM), new Float32Array(QUANTUM)];
      core.process(out, frame);
      frame += QUANTUM;
      outputs.push(out);
    }
    return outputs;
  };
  return { clock, core, run };
}

/** The largest change between consecutive samples a 1 kHz full-scale sine makes. */
const SINE_STEP = 2 * Math.PI * (1000 / RATE);

describe("PlaybackCore", () => {
  it("waits for the go flag before playing", () => {
    const { clock, core, run } = setup();
    core.start(1, sineRing(1));
    run(4);
    expect(clock.read()?.state).toBe(PlaybackState.Priming);
    expect(clock.read()?.consumed).toBe(0);

    clock.setGo(1);
    run(4);
    expect(clock.read()?.state).toBe(PlaybackState.Playing);
    expect(clock.read()?.consumed).toBe(4 * QUANTUM);
  });

  it("waits until the ring is prefilled", () => {
    const { clock, core, run } = setup();
    core.start(1, sineRing(PREFILL_SECONDS / 2));
    clock.setGo(1);
    run(2);
    expect(clock.read()?.state).toBe(PlaybackState.Priming);
  });

  it("starts and stops without a step larger than the signal's own", () => {
    const { clock, core, run } = setup();
    core.start(1, sineRing(1));
    clock.setGo(1);
    run(40);
    core.stop();
    run(10);
    const { maxStep, maxStepTransition } = clock.diagnostics();
    expect(maxStep).toBeLessThanOrEqual(SINE_STEP * 1.01);
    expect(maxStepTransition).toBeLessThanOrEqual(SINE_STEP * 1.01);
    expect(clock.read()?.state).toBe(PlaybackState.Idle);
  });

  it("fades the old session out before a new one starts", () => {
    const { clock, core, run } = setup();
    core.start(1, sineRing(1));
    clock.setGo(1);
    run(20);
    core.start(2, sineRing(1));
    clock.setGo(2);
    run(1);
    expect(clock.read()?.session).toBe(1); // still fading out
    run(10);
    expect(clock.read()?.session).toBe(2);
    expect(clock.diagnostics().maxStepTransition).toBeLessThanOrEqual(SINE_STEP * 1.01);
  });

  it("counts an underrun when the ring runs dry while playing", () => {
    const { clock, core, run } = setup();
    core.start(1, sineRing(PREFILL_SECONDS + 0.001));
    clock.setGo(1);
    run(60);
    expect(clock.diagnostics().underruns).toBeGreaterThan(0);
  });
});

describe("PlaybackCore at the end of the audio", () => {
  it("stops the published clock where the last sample ended, so the heard time can reach it", () => {
    const clock = new ClockBuffer();
    const core = new PlaybackCore(clock, RATE);
    const ring = sineRing(PREFILL_SECONDS + 0.01);
    ring.markComplete();
    core.start(1, ring);
    clock.setGo(1);
    let frame = 0;
    const out = () => [new Float32Array(QUANTUM), new Float32Array(QUANTUM)];
    for (let q = 0; q < 60; q++) {
      core.process(out(), frame);
      frame += QUANTUM;
    }
    const reading = clock.read();
    const total = Math.round((PREFILL_SECONDS + 0.01) * RATE);
    expect(reading?.consumed).toBe(total);
    // The last consumed sample ended partway through an earlier quantum, not at the latest one.
    expect(reading?.quantumEndFrame).toBe(total);
    // A complete ring running dry is the end, not an underrun.
    expect(clock.diagnostics().underruns).toBe(0);
  });
});
