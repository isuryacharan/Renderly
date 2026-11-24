import { describe, expect, it } from "vitest";
import { AudioRing } from "./ring";

const ramp = (from: number, n: number) => Float32Array.from({ length: n }, (_, i) => from + i);

describe("AudioRing", () => {
  it("round-trips planar data across the wrap point", () => {
    const ring = AudioRing.create(2, 8);
    const out = [new Float32Array(5), new Float32Array(5)];
    expect(ring.write([ramp(0, 5), ramp(100, 5)], 0, 5)).toBe(5);
    expect(ring.read(out, 0, 5)).toBe(5);
    // the next write wraps around the end of the 8-frame buffer
    expect(ring.write([ramp(5, 6), ramp(105, 6)], 0, 6)).toBe(6);
    const wrapped = [new Float32Array(6), new Float32Array(6)];
    expect(ring.read(wrapped, 0, 6)).toBe(6);
    expect(Array.from(wrapped[0]!)).toEqual([5, 6, 7, 8, 9, 10]);
    expect(Array.from(wrapped[1]!)).toEqual([105, 106, 107, 108, 109, 110]);
  });

  it("never overwrites unread data and reports what it wrote", () => {
    const ring = AudioRing.create(1, 4);
    expect(ring.writable).toBe(3);
    expect(ring.write([ramp(0, 10)], 0, 10)).toBe(3);
    expect(ring.readable).toBe(3);
    expect(ring.writable).toBe(0);
  });

  it("reads only what is there", () => {
    const ring = AudioRing.create(1, 8);
    ring.write([ramp(1, 2)], 0, 2);
    const out = [new Float32Array(4)];
    expect(ring.read(out, 0, 4)).toBe(2);
  });

  it("feeds a mono ring to both output channels", () => {
    const ring = AudioRing.create(1, 8);
    ring.write([ramp(1, 3)], 0, 3);
    const out = [new Float32Array(3), new Float32Array(3)];
    ring.read(out, 0, 3);
    expect(Array.from(out[1]!)).toEqual([1, 2, 3]);
  });

  it("writes silence", () => {
    const ring = AudioRing.create(1, 8);
    ring.write([ramp(9, 2)], 0, 2);
    expect(ring.writeSilence(2)).toBe(2);
    const out = [new Float32Array(4)];
    ring.read(out, 0, 4);
    expect(Array.from(out[0]!)).toEqual([9, 10, 0, 0]);
  });
});
