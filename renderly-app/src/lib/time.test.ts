import { describe, expect, it } from "vitest";
import {
  formatFps,
  frameAt,
  frameStart,
  fpsFromNumber,
  isOnFrame,
  nominalFps,
  pxToUs,
  secsToUs,
  snapToFrameUs,
  stepFrames,
  usToPx,
  usToSecs,
} from "./time";

const F24 = { num: 24, den: 1 };
const F25 = { num: 25, den: 1 };
const F2997 = { num: 30000, den: 1001 };
const F30 = { num: 30, den: 1 };
const F60 = { num: 60, den: 1 };

// Reference implementation in BigInt, same formula as renderly-core Fps::frame_start.
function ref(fps: { num: number; den: number }, n: number): number {
  const a = BigInt(n) * 1_000_000n * BigInt(fps.den);
  const b = BigInt(fps.num);
  let q = a / b;
  if (a % b !== 0n && a < 0n) q -= 1n;
  return Number(q);
}

describe("frameStart", () => {
  it("matches known values", () => {
    expect(frameStart(F24, 0)).toBe(0);
    expect(frameStart(F24, 1)).toBe(41666);
    expect(frameStart(F24, 24)).toBe(1_000_000);
    expect(frameStart(F25, 1)).toBe(40000);
    expect(frameStart(F30, 1)).toBe(33333);
    expect(frameStart(F30, 3)).toBe(100000);
    expect(frameStart(F60, 1)).toBe(16666);
    expect(frameStart(F2997, 1)).toBe(33366);
    expect(frameStart(F2997, 30000)).toBe(1_001_000_000);
  });

  it("agrees with the BigInt reference over a range, including >2^53 products", () => {
    for (const fps of [F24, F25, F2997, F30, F60, { num: 60000, den: 1001 }]) {
      for (const n of [0, 1, 2, 3, 7, 23, 24, 29, 30, 1799, 107892, 1_000_000, 10_000_000, 1e9, 9e10]) {
        expect(frameStart(fps, n)).toBe(ref(fps, n));
      }
    }
    // 2^53 / (1e6 * 1001) ~ 9.0e6 frames: beyond this the fast path must not be used.
    expect(Number.isSafeInteger(9_100_000 * 1_000_000 * 1001)).toBe(false);
    expect(frameStart(F2997, 9_100_000)).toBe(ref(F2997, 9_100_000));
  });

  it("floors negative frames", () => {
    expect(frameStart(F30, -1)).toBe(-33334);
  });

  it("is strictly increasing", () => {
    let prev = -1;
    for (let n = 0; n < 5000; n++) {
      const s = frameStart(F2997, n);
      expect(s).toBeGreaterThan(prev);
      prev = s;
    }
  });
});

describe("frameAt / snap", () => {
  it("round-trips every frame boundary", () => {
    for (const fps of [F24, F25, F2997, F30, F60]) {
      for (let n = 0; n < 3000; n += 7) {
        const t = frameStart(fps, n);
        expect(frameAt(fps, t)).toBe(n);
        expect(isOnFrame(fps, t)).toBe(true);
        expect(frameAt(fps, t + 1)).toBe(n);
        if (t > 0) expect(frameAt(fps, t - 1)).toBe(n - 1);
      }
    }
  });

  it("snaps to the nearest boundary, ties to the earlier frame", () => {
    expect(snapToFrameUs(F30, 0)).toBe(0);
    expect(snapToFrameUs(F30, 16000)).toBe(0);
    expect(snapToFrameUs(F30, 17000)).toBe(33333);
    expect(snapToFrameUs(F25, 20000)).toBe(0);
    expect(snapToFrameUs(F25, 20001)).toBe(40000);
    expect(snapToFrameUs(F2997, 1_000_000)).toBe(frameStart(F2997, 30));
    expect(snapToFrameUs(F30, -5)).toBe(0);
  });

  it("snapped values are always on frame", () => {
    for (const fps of [F24, F25, F2997, F30, F60]) {
      for (let t = 0; t < 4_000_000; t += 99_991) {
        expect(isOnFrame(fps, snapToFrameUs(fps, t))).toBe(true);
      }
    }
  });

  it("steps frames", () => {
    expect(stepFrames(F30, 0, 1)).toBe(33333);
    expect(stepFrames(F30, 33333, -1)).toBe(0);
    expect(stepFrames(F30, 0, -1)).toBe(0);
    expect(stepFrames(F2997, 33366, 1)).toBe(frameStart(F2997, 2));
  });
});

describe("conversions", () => {
  it("rounds seconds to µs and back", () => {
    expect(secsToUs(1.5)).toBe(1_500_000);
    expect(secsToUs(0.1 + 0.2)).toBe(300_000);
    expect(usToSecs(2_500_000)).toBe(2.5);
  });

  it("rounds pointer positions", () => {
    expect(pxToUs(100, 100)).toBe(1_000_000);
    expect(pxToUs(1, 3)).toBe(333_333);
    expect(pxToUs(2, 3)).toBe(666_667);
    expect(usToPx(500_000, 200)).toBe(100);
  });

  it("formats rates for display", () => {
    expect(formatFps(F2997)).toBe("29.97");
    expect(formatFps(F30)).toBe("30");
    expect(formatFps({ num: 60000, den: 1001 })).toBe("59.94");
  });

  it("snaps a float rate to a standard rational", () => {
    expect(fpsFromNumber(29.97)).toEqual(F2997);
    expect(fpsFromNumber(30)).toEqual(F30);
    expect(fpsFromNumber(59.94)).toEqual({ num: 60000, den: 1001 });
    expect(fpsFromNumber(12.5)).toEqual({ num: 12500, den: 1000 });
    expect(nominalFps(F2997)).toBe(30);
  });
});
