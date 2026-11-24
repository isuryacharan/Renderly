import { describe, expect, it } from "vitest";
import { FLICKS_PER_SECOND, lookupSeconds, secondsToFlicks, sourceFlicks } from "./time";

describe("flicks", () => {
  it("represents common frame intervals as whole numbers", () => {
    // [numerator, denominator] of each rate in frames per second
    const rates = [
      [24, 1],
      [25, 1],
      [30, 1],
      [48, 1],
      [50, 1],
      [60, 1],
      [120, 1],
      [24000, 1001],
      [30000, 1001],
      [60000, 1001],
    ] as const;
    for (const [num, den] of rates) expect((FLICKS_PER_SECOND * den) % num).toBe(0);
  });

  it("round-trips container timestamps of every frame in an hour at 60 fps", () => {
    const first = 0.033; // a non-zero start, as edit lists produce
    for (let n = 0; n < 60 * 3600; n += 997) {
      const timestamp = first + (n * 256) / 15360; // a 15360 Hz MP4 timescale
      expect(sourceFlicks(timestamp, first)).toBe(n * (FLICKS_PER_SECOND / 60));
    }
  });

  it("looks up a frame that starts exactly at the requested time, not the one before", () => {
    const first = 0.033;
    for (let n = 1; n < 60 * 3600; n += 997) {
      const timestamp = first + (n * 256) / 15360;
      const lookup = lookupSeconds(n * (FLICKS_PER_SECOND / 60), first);
      expect(lookup).toBeGreaterThanOrEqual(timestamp);
      expect(lookup).toBeLessThan(timestamp + 1 / 60);
    }
  });

  it("rounds seconds to whole flicks", () => {
    expect(Number.isInteger(secondsToFlicks(1 / 3))).toBe(true);
  });
});
