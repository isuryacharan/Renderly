import { describe, expect, it } from "vitest";
import { plan, runAt, sourceTime, toRuns, type Cut } from "./timeline";

const cut = (source: number, inT: number, out: number, start: number): Cut => ({
  source,
  in: inT,
  out,
  start,
});

describe("toRuns", () => {
  it("merges cuts that continue the same footage", () => {
    const runs = toRuns([cut(0, 0, 10, 0), cut(0, 10, 25, 10), cut(1, 0, 5, 25)]);
    expect(runs).toEqual([
      { source: 0, in: 0, out: 25, start: 0, end: 25 },
      { source: 1, in: 0, out: 5, start: 25, end: 30 },
    ]);
  });

  it("keeps a jump within one file as two runs", () => {
    expect(toRuns([cut(0, 0, 10, 0), cut(0, 50, 60, 10)])).toHaveLength(2);
  });

  it("sorts by start and rejects overlaps and empty cuts", () => {
    expect(toRuns([cut(1, 0, 5, 10), cut(0, 0, 10, 0)])[0]?.source).toBe(0);
    expect(() => toRuns([cut(0, 0, 10, 0), cut(1, 0, 10, 5)])).toThrow(RangeError);
    expect(() => toRuns([cut(0, 5, 5, 0)])).toThrow(RangeError);
  });
});

describe("runAt and sourceTime", () => {
  const runs = toRuns([cut(0, 100, 110, 0), cut(1, 0, 10, 10), cut(0, 300, 310, 30)]);

  it("finds the run covering a time, with exclusive ends", () => {
    expect(runAt(runs, 0)).toBe(0);
    expect(runAt(runs, 9)).toBe(0);
    expect(runAt(runs, 10)).toBe(1);
    expect(runAt(runs, 25)).toBe(-1); // gap
    expect(runAt(runs, 40)).toBe(-1); // past the end
  });

  it("maps timeline time to source time", () => {
    expect(sourceTime(runs[1]!, 13)).toBe(3);
    expect(sourceTime(runs[2]!, 35)).toBe(305);
  });
});

describe("plan", () => {
  // Ten one-unit clips alternating between two files, then a long one.
  const short = Array.from({ length: 10 }, (_, i) => cut(i % 2, i * 100, i * 100 + 1, i));
  const runs = toRuns([...short, cut(0, 5000, 5100, 10)]);

  it("prepares the current run and the minimum ahead", () => {
    expect(plan(runs, 0, { minAhead: 2, lookahead: 0, maxStreams: 8 })).toEqual([0, 1, 2]);
  });

  it("looks further ahead in time when clips are short", () => {
    expect(plan(runs, 0, { minAhead: 2, lookahead: 5, maxStreams: 8 })).toEqual([0, 1, 2, 3, 4]);
  });

  it("never exceeds the stream cap", () => {
    expect(plan(runs, 0, { minAhead: 2, lookahead: 100, maxStreams: 4 })).toHaveLength(4);
  });

  it("plans from the next run in a gap and nothing past the end", () => {
    const gappy = toRuns([cut(0, 0, 10, 0), cut(1, 0, 10, 20)]);
    expect(plan(gappy, 15, { minAhead: 2, lookahead: 0, maxStreams: 4 })).toEqual([1]);
    expect(plan(gappy, 40, { minAhead: 2, lookahead: 0, maxStreams: 4 })).toEqual([]);
  });
});
