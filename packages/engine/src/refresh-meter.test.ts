import { describe, expect, it } from "vitest";
import { RefreshMeter } from "./refresh-meter";

describe("RefreshMeter", () => {
  it("has no period until it has seen a few refreshes", () => {
    const meter = new RefreshMeter();
    meter.add(16.7);
    expect(meter.period).toBeNull();
    expect(meter.isLate(100)).toBe(false);
  });

  it("finds the period of a 160 Hz display through a stall and the bunched frames after it", () => {
    const meter = new RefreshMeter();
    for (let i = 0; i < 20; i++) meter.add(6.25);
    meter.add(60); // stall
    meter.add(0.4); // catch-up callbacks
    meter.add(0.3);
    expect(meter.period).toBe(6.25);
    expect(meter.isLate(6.5)).toBe(false);
    expect(meter.isLate(12.6)).toBe(true);
  });

  it("follows a change of display rate", () => {
    const meter = new RefreshMeter();
    for (let i = 0; i < 31; i++) meter.add(6.25);
    for (let i = 0; i < 31; i++) meter.add(16.7);
    expect(meter.period).toBe(16.7);
  });

  it("ignores zero and negative intervals", () => {
    const meter = new RefreshMeter();
    for (const interval of [0, -1, 16.7, 16.7, 16.7]) meter.add(interval);
    expect(meter.period).toBe(16.7);
  });
});
