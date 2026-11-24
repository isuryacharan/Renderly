import { describe, expect, it } from "vitest";
import { FLICKS_PER_SECOND } from "@renderly/media";
import { cutFrames, type AudioCut } from "./cut-frames";

const RATE = 48_000;
const frame60 = FLICKS_PER_SECOND / 60;

describe("cutFrames", () => {
  it("places a whole cut on the timeline grid", () => {
    const cut: AudioCut = {
      source: 0,
      in: 10 * FLICKS_PER_SECOND,
      out: 11 * FLICKS_PER_SECOND,
      start: 0,
    };
    expect(cutFrames(cut, 0, RATE)).toEqual({
      timelineStart: 0,
      count: 48_000,
      sourceStart: 480_000,
    });
  });

  it("starts part-way through a cut when playback starts inside it", () => {
    const cut: AudioCut = { source: 0, in: 0, out: FLICKS_PER_SECOND, start: FLICKS_PER_SECOND };
    expect(cutFrames(cut, 1.25 * FLICKS_PER_SECOND, RATE)).toEqual({
      timelineStart: 60_000,
      count: 36_000,
      sourceStart: 12_000,
    });
    expect(cutFrames(cut, 3 * FLICKS_PER_SECOND, RATE)).toBeNull();
  });

  it("leaves no gaps or overlaps across many cuts with awkward lengths", () => {
    // 400 cuts of 1 to 89 frames at 60 fps; some lengths are not whole samples at 48 kHz.
    let start = 0;
    let expectedStart = 0;
    let total = 0;
    for (let i = 0; i < 400; i++) {
      const length = (1 + ((i * 37) % 89)) * frame60 + (i % 3);
      const range = cutFrames({ source: 0, in: 0, out: length, start }, 0, RATE);
      expect(range?.timelineStart).toBe(expectedStart);
      expectedStart += range?.count ?? 0;
      total += range?.count ?? 0;
      start += length;
    }
    expect(total).toBe(Math.round((start * RATE) / FLICKS_PER_SECOND));
  });
});
