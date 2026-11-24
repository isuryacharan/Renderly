import fc from "fast-check";
import { describe, expect, it } from "vitest";
import {
  FLICKS_PER_SECOND,
  SUPPORTED_RATES,
  SUPPORTED_SAMPLE_RATES,
  convertFrame,
  flicksPerFrame,
  flicksToFrame,
  flicksToSample,
  formatTimecode,
  framesToFlicks,
  isSupportedRate,
  samplesToFlicks,
  snapToFrame,
  type Rate,
} from "./time";

const rate = fc.constantFrom(...SUPPORTED_RATES);
/** A rate and a frame anywhere in the range a project may use (about ±147 days). */
const rateAndFrame = rate.chain((r) => {
  const limit = Math.floor(Number.MAX_SAFE_INTEGER / flicksPerFrame(r));
  return fc.tuple(fc.constant(r), fc.integer({ min: -limit, max: limit }));
});

describe("frame rates", () => {
  it("includes every whole rate and its NTSC variant", () => {
    expect(SUPPORTED_RATES).toHaveLength(16);
    expect(isSupportedRate([30000, 1001])).toBe(true);
    expect(isSupportedRate([60, 2])).toBe(true); // 30 fps written another way
    expect(isSupportedRate([29, 1])).toBe(false);
  });

  it("has a whole number of flicks per frame at every supported rate", () => {
    for (const r of SUPPORTED_RATES) expect(Number.isInteger(flicksPerFrame(r))).toBe(true);
  });

  it("refuses an unsupported rate", () => {
    expect(() => flicksPerFrame([29, 1])).toThrow(RangeError);
  });

  it("refuses a time past the exact range instead of rounding it", () => {
    expect(() => framesToFlicks(306_367_322, [24, 1])).toThrow(RangeError);
  });
});

describe("frames and flicks (property tests)", () => {
  it("round-trips frame → flicks → frame exactly at every supported rate", () => {
    fc.assert(
      fc.property(rateAndFrame, ([r, f]) => {
        const t = framesToFlicks(f, r);
        return Number.isSafeInteger(t) && flicksToFrame(t, r) === f;
      }),
      { numRuns: 20_000 },
    );
  });

  it("maps every time inside a frame back to that frame", () => {
    fc.assert(
      fc.property(
        rateAndFrame,
        fc.double({ min: 0, max: 0.999999, noNaN: true }),
        ([r, f], within) => {
          const t = framesToFlicks(f, r) + Math.floor(within * flicksPerFrame(r));
          // The very last frame of the range has no room inside it for an exact time.
          if (!Number.isSafeInteger(t)) return true;
          return flicksToFrame(t, r) === f;
        },
      ),
      { numRuns: 20_000 },
    );
  });

  it("snaps onto the frame grid and leaves grid times unchanged", () => {
    fc.assert(
      fc.property(rate, fc.integer({ min: -1e15, max: 1e15 }), (r, t) => {
        const snapped = snapToFrame(t, r);
        return snapped % flicksPerFrame(r) === 0 && snapToFrame(snapped, r) === snapped;
      }),
      { numRuns: 20_000 },
    );
  });

  it("keeps frame starts in order across rates when converting", () => {
    fc.assert(
      fc.property(rate, rate, fc.integer({ min: 0, max: 1e7 }), (from, to, f) => {
        const converted = convertFrame(f, from, to);
        return framesToFlicks(converted, to) <= framesToFlicks(f, from);
      }),
    );
  });
});

describe("samples", () => {
  it("round-trips samples exactly at the supported sample rates", () => {
    fc.assert(
      fc.property(
        fc.constantFrom(...SUPPORTED_SAMPLE_RATES),
        fc.integer({ min: 0, max: 1e11 }),
        (sr, s) => {
          return flicksToSample(samplesToFlicks(s, sr), sr) === s;
        },
      ),
      { numRuns: 20_000 },
    );
  });

  it("has an hour of 48 kHz audio at exactly an hour", () => {
    expect(samplesToFlicks(48_000 * 3600, 48_000)).toBe(FLICKS_PER_SECOND * 3600);
  });
});

describe("timecode", () => {
  const ntsc: Rate = [30000, 1001];
  const ntsc60: Rate = [60000, 1001];

  it("formats non-drop timecode", () => {
    expect(formatTimecode(framesToFlicks(90_061, [25, 1]), [25, 1])).toBe("01:00:02:11");
    expect(formatTimecode(framesToFlicks(-30, [30, 1]), [30, 1])).toBe("-00:00:01:00");
  });

  it("skips frame numbers 00 and 01 at each minute in 29.97 drop-frame, except every tenth", () => {
    expect(formatTimecode(framesToFlicks(1799, ntsc), ntsc)).toBe("00:00:59;29");
    expect(formatTimecode(framesToFlicks(1800, ntsc), ntsc)).toBe("00:01:00;02");
    expect(formatTimecode(framesToFlicks(17_982, ntsc), ntsc)).toBe("00:10:00;00");
  });

  it("stays within a frame of the clock over an hour of 29.97", () => {
    // One hour of 29.97 is 107,892 frames and reads 01:00:00;00 in drop-frame.
    expect(formatTimecode(framesToFlicks(107_892, ntsc), ntsc)).toBe("01:00:00;00");
  });

  it("drops four frame numbers a minute at 59.94", () => {
    expect(formatTimecode(framesToFlicks(3600, ntsc60), ntsc60)).toBe("00:01:00;04");
    expect(formatTimecode(framesToFlicks(215_784, ntsc60), ntsc60)).toBe("01:00:00;00");
  });

  it("uses colon timecode for 23.976, which has no drop-frame", () => {
    const film: Rate = [24000, 1001];
    expect(formatTimecode(framesToFlicks(24, film), film)).toBe("00:00:01:00");
  });
});
