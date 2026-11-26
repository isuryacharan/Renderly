import fc from "fast-check";
import { describe, expect, it } from "vitest";
import {
  applyEase,
  curveProgress,
  insertKey,
  shiftKeys,
  spanCurve,
  valueAt,
  type Ease,
  type Key,
  type KeyValue,
} from "./keyframes";

const S = 705_600_000;
const near = (a: KeyValue, b: KeyValue, tolerance = 1e-7) => {
  const x = typeof a === "number" ? [a] : a;
  const y = typeof b === "number" ? [b] : b;
  return x.length === y.length && x.every((v, i) => Math.abs(v - (y[i] ?? NaN)) <= tolerance);
};

describe("interpolation", () => {
  it("holds the first value before the first key and the last after the last", () => {
    const keys: Key[] = [
      { t: S, v: 10, ease: "linear" },
      { t: 2 * S, v: 20, ease: "linear" },
    ];
    expect(valueAt(keys, 0)).toBe(10);
    expect(valueAt(keys, 3 * S)).toBe(20);
  });

  it("hold keeps the value until the next key", () => {
    const keys: Key[] = [
      { t: 0, v: 1, ease: "hold" },
      { t: S, v: 5, ease: "hold" },
    ];
    expect(valueAt(keys, S - 1)).toBe(1);
    expect(valueAt(keys, S)).toBe(5);
  });

  it("linear moves at a steady rate, per component", () => {
    const keys: Key[] = [
      { t: 0, v: [0, 10], ease: "linear" },
      { t: 4 * S, v: [8, -10], ease: "linear" },
    ];
    expect(valueAt(keys, S)).toEqual([2, 5]);
  });

  it("a bézier key without handles eases out flat; with handles it follows them", () => {
    const eased: Key[] = [
      { t: 0, v: 0, ease: "bezier" },
      { t: S, v: 1, ease: "bezier" },
    ];
    // Flat at both ends and symmetric: half way at half time, slow near the keys.
    expect(valueAt(eased, S / 2) as number).toBeCloseTo(0.5, 9);
    expect(valueAt(eased, S / 10) as number).toBeLessThan(0.1);
    const custom: Key[] = [
      { t: 0, v: 0, ease: "bezier", out: [S / 3, 1 / 3] },
      { t: S, v: 1, ease: "linear", in: [-S / 3, -1 / 3] },
    ];
    // Handles on the straight line are a straight line.
    expect(valueAt(custom, 0.3 * S) as number).toBeCloseTo(0.3, 9);
  });

  it("auto is the Catmull-Rom curve through the keys, flat at the ends", () => {
    const keys: Key[] = [
      { t: 0, v: 0, ease: "auto" },
      { t: S, v: 10, ease: "auto" },
      { t: 2 * S, v: 40, ease: "auto" },
      { t: 3 * S, v: 50, ease: "auto" },
    ];
    // The middle span as a cubic Hermite with Catmull-Rom tangents (value per second).
    const m1 = (40 - 0) / 2;
    const m2 = (50 - 10) / 2;
    const hermite = (u: number) =>
      (2 * u ** 3 - 3 * u ** 2 + 1) * 10 +
      (u ** 3 - 2 * u ** 2 + u) * m1 +
      (-2 * u ** 3 + 3 * u ** 2) * 40 +
      (u ** 3 - u ** 2) * m2;
    for (const u of [0.1, 0.25, 0.5, 0.8])
      expect(valueAt(keys, S + u * S)).toBeCloseTo(hermite(u), 6);
    // The ends ease in and out flat.
    expect(spanCurve(keys, 0).y1).toBe(0);
    expect(spanCurve(keys, 2).y2).toBe(1);
  });

  it("progress overshoots when the handles do, and solves time exactly", () => {
    const curve = { x1: 0.2, y1: 1.4, x2: 0.8, y2: 1.2 };
    expect(curveProgress(curve, 0)).toBeCloseTo(0, 12);
    expect(curveProgress(curve, 1)).toBeCloseTo(1, 12);
    expect(curveProgress(curve, 0.5)).toBeGreaterThan(1);
  });
});

// --- random keys for the property tests ---

const ease = fc.constantFrom<Ease>("linear", "auto", "bezier", "hold");
const handle = (sign: 1 | -1) =>
  fc
    .tuple(
      fc.double({ min: 0, max: 0.45, noNaN: true }),
      fc.double({ min: -0.6, max: 1.2, noNaN: true }),
    )
    .map(([dt, dp]) => [sign * dt, sign * dp] as [number, number]);
const keyList = (dimensions: 1 | 2) =>
  fc
    .uniqueArray(fc.integer({ min: 0, max: 600 }), { minLength: 2, maxLength: 6 })
    .chain((frames) =>
      fc.tuple(
        ...frames
          .sort((a, b) => a - b)
          .map((frame) =>
            fc.record({
              t: fc.constant((frame * S) / 60),
              v:
                dimensions === 1
                  ? fc.double({ min: -100, max: 100, noNaN: true })
                  : fc.tuple(
                      fc.double({ min: -100, max: 100, noNaN: true }),
                      fc.double({ min: -100, max: 100, noNaN: true }),
                    ),
              ease,
              in: fc.option(handle(-1), { nil: undefined }),
              out: fc.option(handle(1), { nil: undefined }),
            }),
          ),
      ),
    )
    .map((keys) =>
      keys.map((k) => {
        const key: Key = { t: k.t, v: k.v as KeyValue, ease: k.ease };
        // Handles are in flicks: scale the shares by a typical span.
        if (k.in) key.in = [k.in[0] * S, k.in[1]];
        if (k.out) key.out = [k.out[0] * S, k.out[1]];
        return key;
      }),
    );

/**
 * Values here go up to ±100. Handle times are stored as whole flicks, which moves a span's curve
 * by up to about 4e-8 of its length on the shortest spans: far below anything visible, but more
 * than 1e-6 on large values.
 */
const TOLERANCE = 1e-4;

/** Sample times over the keys' range and a little past it. */
const samples = (keys: readonly Key[], from = -S) => {
  const last = keys[keys.length - 1]?.t ?? 0;
  const list: number[] = [];
  for (let i = 0; i <= 200; i++) list.push(from + ((last + S - from) * i) / 200);
  return list;
};

/** A span whose curve returns to its start value (progress 0) cannot be split exactly. */
const splittable = (keys: readonly Key[], t: number) => {
  const sorted = [...keys].sort((a, b) => a.t - b.t);
  const i = sorted.findIndex((k, j) => k.t <= t && t < (sorted[j + 1]?.t ?? -Infinity));
  if (i < 0) return true;
  const left = sorted[i] as Key;
  const right = sorted[i + 1] as Key;
  if (left.ease === "hold" || left.ease === "linear") return true;
  const p = curveProgress(spanCurve(sorted, i), (t - left.t) / (right.t - left.t));
  return Math.abs(p) > 1e-3 && Math.abs(1 - p) > 1e-3;
};

describe("editing keeps the motion", () => {
  for (const dimensions of [1, 2] as const)
    it(`inserting a key anywhere leaves every value unchanged (${dimensions}D)`, () => {
      fc.assert(
        fc.property(keyList(dimensions), fc.integer({ min: -60, max: 700 }), (keys, frame) => {
          const t = (frame * S) / 60;
          fc.pre(splittable(keys, t));
          const after = insertKey(keys, t);
          expect(after.length).toBe(keys.some((k) => k.t === t) ? keys.length : keys.length + 1);
          for (const at of samples(keys))
            expect(near(valueAt(after, at), valueAt(keys, at), TOLERANCE)).toBe(true);
        }),
        { numRuns: 1500 },
      );
    });

  it("shifting keys (a trim or a split) keeps the motion from 0 on", () => {
    fc.assert(
      fc.property(keyList(2), fc.integer({ min: 1, max: 600 }), (keys, frame) => {
        const by = (frame * S) / 60;
        fc.pre(splittable(keys, by));
        const after = shiftKeys(keys, by);
        expect(after.every((k) => k.t >= 0)).toBe(true);
        for (const at of samples(keys, by).filter((x) => x >= by))
          expect(near(valueAt(after, at - by), valueAt(keys, at), TOLERANCE)).toBe(true);
      }),
      { numRuns: 1500 },
    );
  });

  it("does not change the keys it was given", () => {
    const keys: Key[] = [
      { t: 0, v: 0, ease: "auto" },
      { t: S, v: 5, ease: "auto" },
      { t: 2 * S, v: 1, ease: "auto" },
    ];
    const copy = JSON.stringify(keys);
    insertKey(keys, S / 2);
    shiftKeys(keys, S / 2);
    applyEase(keys, 1, "ease-in-out");
    expect(JSON.stringify(keys)).toBe(copy);
  });
});

describe("ease presets", () => {
  const keys: Key[] = [
    { t: 0, v: 0, ease: "linear" },
    { t: S, v: 10, ease: "linear" },
    { t: 2 * S, v: 20, ease: "linear" },
  ];
  it("ease out slows the departure; ease in the arrival", () => {
    const out = applyEase(keys, 1, "ease-out");
    expect(valueAt(out, S + S / 10) as number).toBeLessThan(11); // slow just after the key
    expect(valueAt(out, S / 2)).toBeCloseTo(5, 9); // the span before is still straight
    const into = applyEase(keys, 1, "ease-in");
    expect(valueAt(into, S - S / 10) as number).toBeGreaterThan(9); // slow just before the key
  });

  it("hold and linear shape the span after the key", () => {
    expect(valueAt(applyEase(keys, 1, "hold"), 1.5 * S)).toBe(10);
    expect(valueAt(applyEase(keys, 0, "linear"), S / 2)).toBe(5);
  });
});
