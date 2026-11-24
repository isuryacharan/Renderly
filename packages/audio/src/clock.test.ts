import { describe, expect, it } from "vitest";
import { FLICKS_PER_SECOND } from "@renderly/media";
import { ClockBuffer, heardSessionTime, PlaybackState } from "./clock";

describe("ClockBuffer", () => {
  it("round-trips the clock and the output mapping", () => {
    const clock = new ClockBuffer();
    expect(clock.readMapping()).toBeNull();
    clock.setSession(7, PlaybackState.Playing);
    clock.publish(48_128, 4_800, 48_000);
    clock.writeMapping(0.9, 1_000_000.5);
    expect(clock.read()).toEqual({
      session: 7,
      state: PlaybackState.Playing,
      quantumEndFrame: 48_128,
      consumed: 4_800,
      sampleRate: 48_000,
    });
    expect(clock.readMapping()).toEqual({ contextTime: 0.9, wallMs: 1_000_000.5 });
  });

  it("passes the go flag from the render worker to the worklet", () => {
    const clock = new ClockBuffer();
    clock.setGo(3);
    expect(clock.goFor()).toBe(3);
  });
});

describe("heardSessionTime", () => {
  const reading = {
    session: 1,
    state: PlaybackState.Playing,
    quantumEndFrame: 96_000, // 2.0 s of context time processed
    consumed: 24_000, // 0.5 s of the session consumed
    sampleRate: 48_000,
  };

  it("subtracts audio that is processed but not yet at the speakers", () => {
    // 1.9 s of context time is reaching the speakers now: 0.1 s is still in flight.
    const heard = heardSessionTime(reading, { contextTime: 1.9, wallMs: 5_000 }, 5_000);
    expect(heard).toBe(0.4 * FLICKS_PER_SECOND);
  });

  it("advances with wall-clock time between updates", () => {
    const heard = heardSessionTime(reading, { contextTime: 1.9, wallMs: 5_000 }, 5_050);
    expect(heard).toBe(0.45 * FLICKS_PER_SECOND);
  });

  it("is null before the first session sample is heard", () => {
    expect(heardSessionTime(reading, { contextTime: 1.4, wallMs: 0 }, 0)).toBeNull();
  });

  it("never runs ahead of what was consumed", () => {
    const heard = heardSessionTime(reading, { contextTime: 2.5, wallMs: 0 }, 0);
    expect(heard).toBe(0.5 * FLICKS_PER_SECOND);
  });
});
