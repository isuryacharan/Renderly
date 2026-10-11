import { describe, expect, it } from "vitest";
import {
  applyHandleDrag,
  cursorForHandle,
  hitTestHandle,
  ndcToPx,
  pxToNdc,
  resolveTransform,
  rotationHandleNdc,
  transformedCorners,
  transformNdcPoint,
  type HandleKind,
  type NdcPoint,
} from "./previewMath";
import { IDENTITY_TRANSFORM, type ClipTransform } from "./types";

const id = IDENTITY_TRANSFORM;
const tf = (over: Partial<ClipTransform>): ClipTransform => ({ ...id, ...over });
const closeP = (a: NdcPoint, b: NdcPoint, d = 9) => {
  expect(a.x).toBeCloseTo(b.x, d);
  expect(a.y).toBeCloseTo(b.y, d);
};

describe("transformNdcPoint", () => {
  it("is the identity for the identity transform", () => {
    closeP(transformNdcPoint({ x: 0.3, y: -0.7 }, id), { x: 0.3, y: -0.7 });
  });
  it("scales, rotates, then translates (same order as the shader)", () => {
    // (1,0) -> scale_x 2 -> (2,0) -> rotate 90deg -> (0,2) -> translate (0.5, 0)
    closeP(transformNdcPoint({ x: 1, y: 0 }, tf({ scale_x: 2, rotation_deg: 90, x: 0.5 })), { x: 0.5, y: 2 });
  });
  it("rotates counter-clockwise in +Y-up space", () => {
    closeP(transformNdcPoint({ x: 1, y: 0 }, tf({ rotation_deg: 90 })), { x: 0, y: 1 });
    closeP(transformNdcPoint({ x: 1, y: 0 }, tf({ rotation_deg: 180 })), { x: -1, y: 0 });
  });
  it("translation is unaffected by scale or rotation", () => {
    closeP(transformNdcPoint({ x: 0, y: 0 }, tf({ scale_x: 3, rotation_deg: 33, x: 0.2, y: -0.4 })), { x: 0.2, y: -0.4 });
  });
});

describe("resolveTransform / corners", () => {
  it("falls back to identity for null/undefined", () => {
    expect(resolveTransform(null)).toBe(id);
    expect(resolveTransform(undefined)).toBe(id);
    const t = tf({ x: 1 });
    expect(resolveTransform(t)).toBe(t);
  });
  it("identity corners are the unit square, sw/se/ne/nw order", () => {
    expect(transformedCorners(id)).toEqual([
      { x: -1, y: -1 },
      { x: 1, y: -1 },
      { x: 1, y: 1 },
      { x: -1, y: 1 },
    ]);
  });
  it("a half-scale box translated right has the expected corners", () => {
    const c = transformedCorners(tf({ scale_x: 0.5, scale_y: 0.5, x: 0.5 }));
    closeP(c[0], { x: 0, y: -0.5 });
    closeP(c[2], { x: 1, y: 0.5 });
  });
});

describe("ndc <-> px", () => {
  it("maps the NDC extremes to the canvas corners (+Y up -> px Y down)", () => {
    expect(ndcToPx({ x: -1, y: 1 }, 200, 100)).toEqual({ x: 0, y: 0 });
    expect(ndcToPx({ x: 1, y: -1 }, 200, 100)).toEqual({ x: 200, y: 100 });
    expect(ndcToPx({ x: 0, y: 0 }, 200, 100)).toEqual({ x: 100, y: 50 });
  });
  it("round-trips", () => {
    const p = { x: 0.37, y: -0.81 };
    closeP(pxToNdc(ndcToPx(p, 640, 360), 640, 360), p);
  });
});

describe("rotationHandleNdc", () => {
  it("sits a fixed offset above the top-middle edge", () => {
    closeP(rotationHandleNdc(id), { x: 0, y: 1.12 });
  });
  it("follows the box rotation", () => {
    closeP(rotationHandleNdc(tf({ rotation_deg: 90 })), { x: -1.12, y: 0 });
  });
  it("stays finite for a degenerate (zero-height) box", () => {
    const h = rotationHandleNdc(tf({ scale_y: 0 }));
    expect(Number.isFinite(h.x) && Number.isFinite(h.y)).toBe(true);
  });
});

describe("hitTestHandle (200x200 canvas, identity box)", () => {
  const hit = (x: number, y: number, t = id) => hitTestHandle({ x, y }, t, 200, 200);
  it("finds corner handles (sw is bottom-left on screen)", () => {
    expect(hit(1, 199)).toBe("scale-sw");
    expect(hit(199, 199)).toBe("scale-se");
    expect(hit(199, 1)).toBe("scale-ne");
    expect(hit(1, 1)).toBe("scale-nw");
  });
  it("finds edge handles", () => {
    expect(hit(100, 2)).toBe("scale-n");
    expect(hit(100, 198)).toBe("scale-s");
    expect(hit(198, 100)).toBe("scale-e");
    expect(hit(2, 100)).toBe("scale-w");
  });
  it("finds the rotation handle above the box", () => {
    expect(hit(100, -12)).toBe("rotate");
  });
  it("is 'move' inside the box and null outside everything", () => {
    expect(hit(100, 100)).toBe("move");
    expect(hit(-50, 100)).toBeNull();
    expect(hit(100, 260)).toBeNull();
  });
  it("hit-tests the rotated polygon, not its bounding box", () => {
    const rotated = tf({ rotation_deg: 45, scale_x: 0.5, scale_y: 0.5 });
    // Point inside the axis-aligned bbox of the diamond but outside the diamond itself.
    expect(hit(100 + 60, 100 + 60, rotated)).toBeNull();
    expect(hit(100, 100, rotated)).toBe("move");
  });
});

describe("applyHandleDrag: move", () => {
  const drag = (dx: number, dy: number, mods = { shift: false, alt: false }) =>
    applyHandleDrag("move", id, { x: 0, y: 0 }, { x: dx, y: dy }, mods);

  it("translates by the pointer delta", () => {
    const r = drag(0.2, -0.3);
    expect(r.transform).toMatchObject({ x: 0.2, y: -0.3 });
    expect(r.guideV || r.guideH).toBe(false);
  });
  it("snaps each axis independently to canvas centre and raises guides", () => {
    const r = drag(0.02, 0.5);
    expect(r.transform.x).toBe(0);
    expect(r.transform.y).toBe(0.5);
    expect(r).toMatchObject({ guideV: true, guideH: false });
    expect(drag(0.5, -0.01)).toMatchObject({ guideV: false, guideH: true });
  });
  it("Alt disables snapping", () => {
    const r = drag(0.02, 0.02, { shift: false, alt: true });
    expect(r.transform).toMatchObject({ x: 0.02, y: 0.02 });
    expect(r.guideV || r.guideH).toBe(false);
  });
  it("clamps how far off-canvas the clip centre may go", () => {
    expect(drag(50, -50).transform).toMatchObject({ x: 2, y: -2 });
  });
  it("preserves other transform fields", () => {
    const r = applyHandleDrag("move", tf({ opacity: 0.4, rotation_deg: 10 }), { x: 0, y: 0 }, { x: 0.5, y: 0.5 });
    expect(r.transform).toMatchObject({ opacity: 0.4, rotation_deg: 10 });
  });
});

describe("applyHandleDrag: rotate", () => {
  const rot = (deg: number, mods = { shift: false, alt: false }, start = id) => {
    const a = (deg * Math.PI) / 180;
    return applyHandleDrag("rotate", start, { x: 1, y: 0 }, { x: Math.cos(a), y: Math.sin(a) }, mods).transform
      .rotation_deg;
  };
  it("rotates by the swept angle", () => {
    expect(rot(40)).toBeCloseTo(40, 6);
    expect(rot(-25)).toBeCloseTo(-25, 6);
  });
  it("soft-snaps to multiples of 90 within 3 degrees", () => {
    expect(rot(91.5)).toBe(90);
    expect(rot(-2)).toBeCloseTo(0, 9); // may be -0
    expect(rot(180 - 1)).toBe(180);
    expect(rot(94)).toBeCloseTo(94, 6);
  });
  it("Shift steps by 15 degrees", () => {
    expect(rot(40, { shift: true, alt: false })).toBe(45);
    expect(rot(-52, { shift: true, alt: false })).toBe(-45);
  });
  it("wraps into (-180, 180]", () => {
    expect(rot(20, { shift: false, alt: false }, tf({ rotation_deg: 170 }))).toBeCloseTo(-170, 6);
  });
  it("reports exactly -180 as +180", () => {
    const r = applyHandleDrag("rotate", id, { x: 1, y: 0 }, { x: -1, y: -0 });
    expect(r.transform.rotation_deg).toBe(180);
  });
  it("measures the angle about the box centre, not the origin", () => {
    const start = tf({ x: 0.5, y: 0.5 });
    const r = applyHandleDrag("rotate", start, { x: 1.5, y: 0.5 }, { x: 0.5, y: 1.5 });
    expect(r.transform.rotation_deg).toBe(90);
  });
});

describe("applyHandleDrag: scale", () => {
  it("uniform corner scale keeps the opposite corner anchored", () => {
    const r = applyHandleDrag("scale-se", id, { x: 1, y: -1 }, { x: 3, y: -3 }).transform;
    expect(r.scale_x).toBeCloseTo(2, 9);
    expect(r.scale_y).toBeCloseTo(2, 9);
    // nw corner (-1, 1) must not move
    closeP(transformNdcPoint({ x: -1, y: 1 }, r), { x: -1, y: 1 });
  });
  it("Alt scales from the centre", () => {
    const r = applyHandleDrag("scale-se", id, { x: 1, y: -1 }, { x: 3, y: -3 }, { shift: false, alt: true }).transform;
    expect(r.scale_x).toBeCloseTo(2, 9);
    expect(r).toMatchObject({ x: 0, y: 0 });
  });
  it("Shift on a corner distorts each axis independently", () => {
    const r = applyHandleDrag("scale-se", id, { x: 1, y: -1 }, { x: 2, y: -4 }, { shift: true, alt: false }).transform;
    expect(r.scale_x).toBeCloseTo(1.5, 9);
    expect(r.scale_y).toBeCloseTo(2.5, 9);
  });
  it("edge handles only change one axis", () => {
    const r = applyHandleDrag("scale-e", id, { x: 1, y: 0 }, { x: 2, y: 0 }).transform;
    expect(r.scale_x).toBeCloseTo(1.5, 9);
    expect(r.scale_y).toBe(1);
    expect(r.x).toBeCloseTo(0.5, 9);
    const v = applyHandleDrag("scale-n", id, { x: 0, y: 1 }, { x: 0, y: 2 }).transform;
    expect(v.scale_y).toBeCloseTo(1.5, 9);
    expect(v.scale_x).toBe(1);
  });
  it("clamps scale to [0.05, 8]", () => {
    expect(applyHandleDrag("scale-e", id, { x: 1, y: 0 }, { x: -1, y: 0 }).transform.scale_x).toBe(0.05);
    expect(applyHandleDrag("scale-e", id, { x: 1, y: 0 }, { x: 500, y: 0 }).transform.scale_x).toBe(8);
  });
  it("leaves scale alone when the drag starts on the anchor (zero lever arm)", () => {
    const r = applyHandleDrag("scale-e", id, { x: -1, y: 0 }, { x: 5, y: 0 }).transform;
    expect(r.scale_x).toBe(1);
  });
  it("keeps the anchor fixed for every scale handle on a rotated, offset box", () => {
    const start = tf({ x: 0.1, y: -0.2, scale_x: 0.6, scale_y: 0.9, rotation_deg: 30 });
    const units: Record<string, NdcPoint> = {
      "scale-nw": { x: -1, y: 1 },
      "scale-ne": { x: 1, y: 1 },
      "scale-sw": { x: -1, y: -1 },
      "scale-se": { x: 1, y: -1 },
      "scale-n": { x: 0, y: 1 },
      "scale-s": { x: 0, y: -1 },
      "scale-e": { x: 1, y: 0 },
      "scale-w": { x: -1, y: 0 },
    };
    for (const [kind, hu] of Object.entries(units)) {
      const startNdc = transformNdcPoint(hu, start);
      const cur = { x: startNdc.x + 0.15, y: startNdc.y + 0.1 };
      const out = applyHandleDrag(kind as HandleKind, start, startNdc, cur).transform;
      const anchor = { x: -hu.x, y: -hu.y };
      closeP(transformNdcPoint(anchor, out), transformNdcPoint(anchor, start), 7);
      expect(out.rotation_deg).toBe(30);
    }
  });
  it("returns the start transform untouched for an unknown scale handle", () => {
    const r = applyHandleDrag("bogus" as HandleKind, id, { x: 0, y: 0 }, { x: 1, y: 1 });
    expect(r.transform).toBe(id);
  });
});

describe("cursorForHandle", () => {
  const c = { x: 100, y: 100 };
  it("fixed cursors for move/rotate", () => {
    expect(cursorForHandle("move", c, c)).toBe("move");
    expect(cursorForHandle("rotate", c, c)).toBe("grab");
  });
  it("buckets by direction from the centre; opposites share a cursor", () => {
    expect(cursorForHandle("scale-e", { x: 200, y: 100 }, c)).toBe("ew-resize");
    expect(cursorForHandle("scale-w", { x: 0, y: 100 }, c)).toBe("ew-resize");
    expect(cursorForHandle("scale-s", { x: 100, y: 200 }, c)).toBe("ns-resize");
    expect(cursorForHandle("scale-se", { x: 200, y: 200 }, c)).toBe("nwse-resize");
    expect(cursorForHandle("scale-nw", { x: 0, y: 0 }, c)).toBe("nwse-resize");
    expect(cursorForHandle("scale-ne", { x: 200, y: 0 }, c)).toBe("nesw-resize");
    expect(cursorForHandle("scale-sw", { x: 0, y: 200 }, c)).toBe("nesw-resize");
  });
});
