import { describe, expect, it } from "vitest";
import { containRect, containUvScale, rotatedSize, workingSize } from "./layout";

describe("workingSize", () => {
  it("scales the sequence and keeps sizes even", () => {
    expect(workingSize({ width: 3840, height: 2160 }, 1)).toEqual({ width: 3840, height: 2160 });
    expect(workingSize({ width: 3840, height: 2160 }, 0.5)).toEqual({ width: 1920, height: 1080 });
    expect(workingSize({ width: 1080, height: 1350 }, 0.25)).toEqual({ width: 270, height: 338 });
  });
});

describe("rotatedSize", () => {
  it("swaps width and height for quarter turns only", () => {
    const landscape = { width: 1920, height: 1080 };
    expect(rotatedSize(landscape, 90)).toEqual({ width: 1080, height: 1920 });
    expect(rotatedSize(landscape, 270)).toEqual({ width: 1080, height: 1920 });
    expect(rotatedSize(landscape, 180)).toEqual(landscape);
  });
});

describe("containRect", () => {
  it("pillarboxes portrait content in a landscape viewport", () => {
    expect(containRect({ width: 1080, height: 1920 }, { width: 1600, height: 900 })).toEqual({
      x: 547,
      y: 0,
      width: 506,
      height: 900,
    });
  });

  it("fills a viewport of the same aspect ratio exactly", () => {
    expect(containRect({ width: 3840, height: 2160 }, { width: 1280, height: 720 })).toEqual({
      x: 0,
      y: 0,
      width: 1280,
      height: 720,
    });
  });
});

describe("containUvScale", () => {
  it("is 1:1 when the aspect ratios match", () => {
    expect(containUvScale({ width: 3840, height: 2160 }, { width: 1920, height: 1080 })).toEqual([
      1, 1,
    ]);
  });

  it("widens the horizontal range for narrower content", () => {
    const [sx, sy] = containUvScale({ width: 1080, height: 1920 }, { width: 1920, height: 1080 });
    expect(sy).toBe(1);
    expect(sx).toBeCloseTo(1920 / 1080 / (1080 / 1920));
  });
});
