import { describe, expect, it } from "vitest";
import {
  containRect,
  containUvScale,
  fitUvScale,
  pictureBox,
  rotatedSize,
  viewRect,
  workingSize,
} from "./layout";

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

describe("fitUvScale and pictureBox", () => {
  const wide = { width: 1920, height: 1080 };
  const tall = { width: 1080, height: 1920 };
  it("letterboxes with fit, crops with fill, and covers exactly with stretch", () => {
    const fit = fitUvScale(wide, tall, "fit");
    expect(pictureBox(fit)[0]).toBe(1);
    expect(pictureBox(fit)[1]).toBeCloseTo((1080 * 1080) / 1920 / 1920, 9);
    const fill = fitUvScale(wide, tall, "fill");
    expect(fill[0]).toBeCloseTo((1080 / 1920) ** 2, 9); // only the middle of the width shows
    expect(pictureBox(fill)).toEqual([1, 1]);
    expect(fitUvScale(wide, tall, "stretch")).toEqual([1, 1]);
    expect(fitUvScale(wide, wide, "fill")).toEqual([1, 1]);
  });
});

describe("viewRect", () => {
  const content = { width: 1920, height: 1080 };
  const viewport = { width: 1000, height: 800 };
  it("fits when there is no zoom", () => {
    expect(viewRect(content, viewport, { zoom: null, pan: { x: 0, y: 0 } })).toEqual(
      containRect(content, viewport),
    );
  });
  it("centres the zoomed picture and pans it (GL y is up)", () => {
    expect(viewRect(content, viewport, { zoom: 1, pan: { x: 0, y: 0 } })).toEqual({
      x: -460,
      y: -140,
      width: 1920,
      height: 1080,
    });
    expect(viewRect(content, viewport, { zoom: 0.5, pan: { x: 100, y: 50 } })).toEqual({
      x: 120,
      y: 80,
      width: 960,
      height: 540,
    });
  });
});
