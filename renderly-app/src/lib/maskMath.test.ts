import { describe, expect, it } from "vitest";
import {
  applyMaskHandleDrag,
  assertUvPxRoundTrip,
  boundsToEllipseKind,
  boundsToRectKind,
  clampMaskBounds,
  coverUv,
  defaultMask,
  hitTestMaskHandle,
  inverseTransformNdc,
  localNdcToUv,
  maskCornerUvs,
  normalizeRectUv,
  previewPxToSourceUv,
  shapeBounds,
  sourceUvToPreviewPx,
  switchMaskShape,
  uvToLocalNdc,
} from "./maskMath";
import { transformNdcPoint } from "./previewMath";
import { IDENTITY_TRANSFORM, type ClipTransform } from "./types";

const tf = (over: Partial<ClipTransform>): ClipTransform => ({ ...IDENTITY_TRANSFORM, ...over });
const square = coverUv(100, 100, 100, 100);

describe("coverUv", () => {
  it("is the identity when aspects match", () => {
    expect(square).toEqual({ scale: [1, 1], offset: [0, 0] });
  });
  it("crops the sides of a wider source (16:9 into 1:1)", () => {
    const c = coverUv(1920, 1080, 1080, 1080);
    expect(c.scale[0]).toBeCloseTo(1080 / 1920, 10);
    expect(c.scale[1]).toBe(1);
    expect(c.offset[0]).toBeCloseTo((1 - 1080 / 1920) / 2, 10);
    expect(c.offset[1]).toBe(0);
  });
  it("crops top/bottom of a taller source (9:16 into 16:9)", () => {
    const c = coverUv(1080, 1920, 1920, 1080);
    expect(c.scale[0]).toBe(1);
    expect(c.scale[1]).toBeCloseTo((1080 / 1920) / (1920 / 1080), 10);
    expect(c.offset[1]).toBeCloseTo((1 - c.scale[1]) / 2, 10);
  });
  it("never produces NaN for zero-sized layers", () => {
    const c = coverUv(100, 0, 100, 100);
    expect(c.scale.every(Number.isFinite)).toBe(true);
  });
});

describe("local NDC <-> UV", () => {
  const cover = coverUv(2000, 1000, 1000, 1000); // scale [0.5, 1], offset [0.25, 0]
  it("maps the NDC corners onto the visible UV window (V down)", () => {
    expect(localNdcToUv({ x: -1, y: 1 }, cover)).toEqual({ u: 0.25, v: 0 });
    expect(localNdcToUv({ x: 1, y: -1 }, cover)).toEqual({ u: 0.75, v: 1 });
    expect(localNdcToUv({ x: 0, y: 0 }, cover)).toEqual({ u: 0.5, v: 0.5 });
  });
  it("round-trips", () => {
    for (const p of [{ x: 0.3, y: -0.6 }, { x: -1, y: 1 }, { x: 0, y: 0 }]) {
      const back = uvToLocalNdc(localNdcToUv(p, cover), cover);
      expect(back.x).toBeCloseTo(p.x, 10);
      expect(back.y).toBeCloseTo(p.y, 10);
    }
  });
});

describe("inverseTransformNdc", () => {
  it("inverts transformNdcPoint for scale+rotation+translation", () => {
    const t = tf({ x: 0.3, y: -0.2, scale_x: 0.7, scale_y: 1.4, rotation_deg: 37 });
    for (const p of [{ x: 0, y: 0 }, { x: 0.8, y: -0.4 }, { x: -1, y: 1 }]) {
      const back = inverseTransformNdc(transformNdcPoint(p, t), t);
      expect(back.x).toBeCloseTo(p.x, 9);
      expect(back.y).toBeCloseTo(p.y, 9);
    }
  });
  it("stays finite with a zero scale", () => {
    const r = inverseTransformNdc({ x: 0.5, y: 0.5 }, tf({ scale_x: 0, scale_y: 0 }));
    expect(Number.isFinite(r.x) && Number.isFinite(r.y)).toBe(true);
  });
});

describe("UV <-> preview px", () => {
  it("maps the UV square onto the preview for an untransformed same-aspect clip", () => {
    expect(sourceUvToPreviewPx({ u: 0, v: 0 }, square, null, 200, 100)).toEqual({ x: 0, y: 0 });
    expect(sourceUvToPreviewPx({ u: 1, v: 1 }, square, undefined, 200, 100)).toEqual({ x: 200, y: 100 });
    expect(sourceUvToPreviewPx({ u: 0.5, v: 0.5 }, square, null, 200, 100)).toEqual({ x: 100, y: 50 });
  });
  it("follows the clip transform (half-size clip pinned to the right)", () => {
    const t = tf({ scale_x: 0.5, scale_y: 0.5, x: 0.5 });
    // UV (0,0) is local NDC (-1, 1) -> (0, 0.5) world -> px (100, 25) on a 200x100 canvas
    const p = sourceUvToPreviewPx({ u: 0, v: 0 }, square, t, 200, 100);
    expect(p.x).toBeCloseTo(100, 9);
    expect(p.y).toBeCloseTo(25, 9);
  });
  it("previewPx -> UV inverts UV -> previewPx incl. rotation and cover crop", () => {
    const cover = coverUv(1920, 1080, 1080, 1920);
    const t = tf({ x: -0.1, y: 0.2, scale_x: 0.8, scale_y: 0.8, rotation_deg: 25 });
    for (const uv of [{ u: 0.5, v: 0.5 }, { u: 0.4, v: 0.1 }, { u: 0.6, v: 0.9 }]) {
      const back = previewPxToSourceUv(sourceUvToPreviewPx(uv, cover, t, 360, 640), cover, t, 360, 640);
      expect(back.u).toBeCloseTo(uv.u, 9);
      expect(back.v).toBeCloseTo(uv.v, 9);
    }
  });
  it("assertUvPxRoundTrip agrees", () => {
    expect(assertUvPxRoundTrip(coverUv(1920, 1080, 1080, 1080), tf({ rotation_deg: 90, scale_x: 0.5 }), 300, 300)).toBe(true);
  });
  it("tolerates a zero-sized preview without NaN", () => {
    const uv = previewPxToSourceUv({ x: 10, y: 10 }, square, null, 0, 0);
    expect(Number.isFinite(uv.u) && Number.isFinite(uv.v)).toBe(true);
  });
});

describe("shape helpers", () => {
  it("shapeBounds for rect and ellipse; null otherwise", () => {
    expect(shapeBounds({ type: "rect", x: 0.1, y: 0.2, width: 0.3, height: 0.4 })).toEqual({
      x: 0.1,
      y: 0.2,
      width: 0.3,
      height: 0.4,
    });
    const e = shapeBounds({ type: "ellipse", cx: 0.5, cy: 0.5, rx: 0.2, ry: 0.1 })!;
    expect(e.x).toBeCloseTo(0.3, 10);
    expect(e.y).toBeCloseTo(0.4, 10);
    expect(e.width).toBeCloseTo(0.4, 10);
    expect(e.height).toBeCloseTo(0.2, 10);
    expect(shapeBounds({ type: "none" })).toBeNull();
    expect(shapeBounds({ type: "raster", path: "p" })).toBeNull();
    expect(shapeBounds({ type: "generated", cache_dir: "d" })).toBeNull();
  });

  it("normalizeRectUv orders corners regardless of drag direction", () => {
    expect(normalizeRectUv(0.8, 0.9, 0.2, 0.1)).toEqual({ x: 0.2, y: 0.1, width: 0.8 - 0.2, height: 0.9 - 0.1 });
  });

  it("clampMaskBounds keeps the box inside the unit square", () => {
    expect(clampMaskBounds({ x: 0.8, y: 0.8, width: 0.5, height: 0.5 })).toEqual({
      x: 0.8,
      y: 0.8,
      width: expect.closeTo(0.2, 10),
      height: expect.closeTo(0.2, 10),
    });
    expect(clampMaskBounds({ x: -1, y: 2, width: -3, height: 5 })).toEqual({ x: 0, y: 1, width: 0, height: 0 });
  });

  it("boundsToRectKind / boundsToEllipseKind clamp then convert", () => {
    expect(boundsToRectKind({ x: 0.1, y: 0.2, width: 0.3, height: 0.4 })).toEqual({
      type: "rect",
      x: 0.1,
      y: 0.2,
      width: 0.3,
      height: 0.4,
    });
    const e = boundsToEllipseKind({ x: 0.2, y: 0.2, width: 0.4, height: 0.2 });
    expect(e).toMatchObject({ type: "ellipse", rx: expect.closeTo(0.2, 10), ry: expect.closeTo(0.1, 10) });
    if (e.type === "ellipse") {
      expect(e.cx).toBeCloseTo(0.4, 10);
      expect(e.cy).toBeCloseTo(0.3, 10);
    }
    expect(boundsToRectKind({ x: 0.9, y: 0, width: 0.5, height: 1 })).toMatchObject({ type: "rect", width: expect.closeTo(0.1, 10) });
  });

  it("switchMaskShape preserves bounds across rect <-> ellipse and refuses non-shape kinds", () => {
    const rect = { type: "rect", x: 0.1, y: 0.2, width: 0.5, height: 0.4 } as const;
    const ell = switchMaskShape(rect, "ellipse")!;
    expect(ell.type).toBe("ellipse");
    const back = switchMaskShape(ell, "rect")!;
    expect(back).toMatchObject({ type: "rect" });
    if (back.type === "rect") {
      expect(back.x).toBeCloseTo(0.1, 10);
      expect(back.width).toBeCloseTo(0.5, 10);
    }
    expect(switchMaskShape({ type: "none" }, "rect")).toBeNull();
    expect(switchMaskShape({ type: "raster", path: "x" }, "ellipse")).toBeNull();
  });

  it("defaultMask is enabled, non-inverted, feathered", () => {
    expect(defaultMask({ type: "none" })).toEqual({ enabled: true, invert: false, feather: 0.05, kind: { type: "none" } });
    expect(defaultMask({ type: "none" }, 0.2).feather).toBe(0.2);
  });

  it("maskCornerUvs lists sw, se, ne, nw in V-down UV", () => {
    expect(maskCornerUvs({ x: 0.25, y: 0.25, width: 0.5, height: 0.5 })).toEqual([
      { u: 0.25, v: 0.75 },
      { u: 0.75, v: 0.75 },
      { u: 0.75, v: 0.25 },
      { u: 0.25, v: 0.25 },
    ]);
  });
});

describe("hitTestMaskHandle (200x200, identity cover/transform)", () => {
  const b = { x: 0.25, y: 0.25, width: 0.5, height: 0.5 }; // px box 50..150
  const hit = (x: number, y: number) => hitTestMaskHandle({ x, y }, b, square, null, 200, 200);
  it("corners", () => {
    expect(hit(52, 148)).toBe("sw");
    expect(hit(148, 148)).toBe("se");
    expect(hit(148, 52)).toBe("ne");
    expect(hit(52, 52)).toBe("nw");
  });
  it("edges", () => {
    expect(hit(100, 52)).toBe("n");
    expect(hit(100, 148)).toBe("s");
    expect(hit(148, 100)).toBe("e");
    expect(hit(52, 100)).toBe("w");
  });
  it("body and outside", () => {
    expect(hit(100, 100)).toBe("move");
    expect(hit(10, 10)).toBeNull();
    expect(hit(190, 100)).toBeNull();
  });
  it("tracks the clip transform", () => {
    const t = tf({ x: 0.5 }); // shifts everything right by 50px on a 200px canvas
    expect(hitTestMaskHandle({ x: 150, y: 100 }, b, square, t, 200, 200)).toBe("move");
    expect(hitTestMaskHandle({ x: 60, y: 100 }, b, square, t, 200, 200)).toBeNull();
  });
});

describe("applyMaskHandleDrag", () => {
  const start = { x: 0.25, y: 0.25, width: 0.5, height: 0.5 };
  const drag = (kind: Parameters<typeof applyMaskHandleDrag>[0], du: number, dv: number) =>
    applyMaskHandleDrag(kind, start, { u: 0.5, v: 0.5 }, { u: 0.5 + du, v: 0.5 + dv });

  it("move translates and keeps the size", () => {
    const r = drag("move", 0.1, -0.05);
    expect(r.x).toBeCloseTo(0.35, 10);
    expect(r.y).toBeCloseTo(0.2, 10);
    expect(r).toMatchObject({ width: 0.5, height: 0.5 });
  });

  it("move slides against the frame edge instead of shrinking the box", () => {
    expect(drag("move", 0.9, 0.9)).toEqual({ x: 0.5, y: 0.5, width: 0.5, height: 0.5 });
    expect(drag("move", -0.9, -0.9)).toEqual({ x: 0, y: 0, width: 0.5, height: 0.5 });
  });

  it("corner handles move two edges", () => {
    const se = drag("se", 0.1, 0.1);
    expect(se.x).toBe(0.25);
    expect(se.width).toBeCloseTo(0.6, 10);
    expect(se.height).toBeCloseTo(0.6, 10);
    const nw = drag("nw", 0.1, 0.1);
    expect(nw.x).toBeCloseTo(0.35, 10);
    expect(nw.y).toBeCloseTo(0.35, 10);
    expect(nw.width).toBeCloseTo(0.4, 10);
  });

  it("edge handles move one edge only", () => {
    const e = drag("e", 0.2, 0.3);
    expect(e.width).toBeCloseTo(0.7, 10);
    expect(e.height).toBe(0.5);
    const n = drag("n", 0.3, -0.1);
    expect(n.y).toBeCloseTo(0.15, 10);
    expect(n.height).toBeCloseTo(0.6, 10);
    expect(n.width).toBe(0.5);
  });

  it("dragging an edge past the opposite edge flips the box instead of going negative", () => {
    const r = drag("w", 0.75, 0); // left edge 0.25 -> 1.0, past right edge 0.75
    expect(r.x).toBeCloseTo(0.75, 10);
    expect(r.width).toBeCloseTo(0.25, 10);
  });

  it("resizes clamp to the unit square", () => {
    const r = drag("se", 5, 5);
    expect(r.x + r.width).toBeCloseTo(1, 10);
    expect(r.y + r.height).toBeCloseTo(1, 10);
    expect(drag("nw", -5, -5)).toMatchObject({ x: 0, y: 0 });
  });

  it("a handle dragged off-frame leaves the anchored edges where they were", () => {
    const r = drag("nw", -5, -5); // anchored edges: right 0.75, bottom 0.75
    expect(r.x + r.width).toBeCloseTo(0.75, 10);
    expect(r.y + r.height).toBeCloseTo(0.75, 10);
  });
});
