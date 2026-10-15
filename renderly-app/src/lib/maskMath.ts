// Source-UV ↔ preview-pixel mapping for clip masks.
// Mirrors cover_uv (compose/mod.rs) + composite.wgsl vertex UV / user transform.
// Mask coordinates stay in source-frame UV (0..1), V down; preview overlay is CSS px.

import type { ClipMask, ClipMaskKind, ClipTransform } from "./types";
import { resolveTransform, transformNdcPoint, type NdcPoint, type PxPoint } from "./previewMath";

export type UvPoint = { u: number; v: number };

export type CoverUv = {
  scale: [number, number];
  offset: [number, number];
};

/**
 * Same cover-fit as renderly-core `cover_uv` / `cover_window`. `fitX`/`fitY` (-1..1, the clip's
 * `fit_x`/`fit_y`) slide the window across the cropped axis: offset = (1 - scale)(1 + fit)/2.
 */
export function coverUv(
  layerW: number,
  layerH: number,
  outW: number,
  outH: number,
  fitX = 0,
  fitY = 0,
): CoverUv {
  const layerAspect = layerW / Math.max(layerH, 1e-9);
  const outAspect = outW / Math.max(outH, 1e-9);
  const scale: [number, number] =
    layerAspect > outAspect
      ? [outAspect / layerAspect, 1]
      : [1, layerAspect / outAspect];
  const fx = Math.max(-1, Math.min(1, fitX));
  const fy = Math.max(-1, Math.min(1, fitY));
  return {
    scale,
    offset: [((1 - scale[0]) * (1 + fx)) / 2, ((1 - scale[1]) * (1 + fy)) / 2],
  };
}

/** Local (pre-user-transform) NDC → source UV via cover remap (matches WGSL). */
export function localNdcToUv(ndc: NdcPoint, cover: CoverUv): UvPoint {
  const uParam = (ndc.x + 1) / 2;
  const vParam = (1 - ndc.y) / 2;
  return {
    u: cover.offset[0] + cover.scale[0] * uParam,
    v: cover.offset[1] + cover.scale[1] * vParam,
  };
}

/** Source UV → local NDC (inverse of localNdcToUv). */
export function uvToLocalNdc(uv: UvPoint, cover: CoverUv): NdcPoint {
  const sx = Math.max(cover.scale[0], 1e-9);
  const sy = Math.max(cover.scale[1], 1e-9);
  const uParam = (uv.u - cover.offset[0]) / sx;
  const vParam = (uv.v - cover.offset[1]) / sy;
  return {
    x: uParam * 2 - 1,
    y: 1 - vParam * 2,
  };
}

/** Inverse of transformNdcPoint (translate → unrotate → unscale). */
export function inverseTransformNdc(p: NdcPoint, t: ClipTransform): NdcPoint {
  const ox = p.x - t.x;
  const oy = p.y - t.y;
  const rad = (-t.rotation_deg * Math.PI) / 180;
  const c = Math.cos(rad);
  const s = Math.sin(rad);
  const rx = ox * c - oy * s;
  const ry = ox * s + oy * c;
  const sx = Math.abs(t.scale_x) < 1e-9 ? 1e-9 : t.scale_x;
  const sy = Math.abs(t.scale_y) < 1e-9 ? 1e-9 : t.scale_y;
  return { x: rx / sx, y: ry / sy };
}

export function sourceUvToPreviewPx(
  uv: UvPoint,
  cover: CoverUv,
  transform: ClipTransform | null | undefined,
  previewW: number,
  previewH: number,
): PxPoint {
  const local = uvToLocalNdc(uv, cover);
  const screen = transformNdcPoint(local, resolveTransform(transform));
  return {
    x: ((screen.x + 1) / 2) * previewW,
    y: ((1 - screen.y) / 2) * previewH,
  };
}

export function previewPxToSourceUv(
  px: PxPoint,
  cover: CoverUv,
  transform: ClipTransform | null | undefined,
  previewW: number,
  previewH: number,
): UvPoint {
  const screen: NdcPoint = {
    x: (px.x / Math.max(previewW, 1e-9)) * 2 - 1,
    y: 1 - (px.y / Math.max(previewH, 1e-9)) * 2,
  };
  const local = inverseTransformNdc(screen, resolveTransform(transform));
  return localNdcToUv(local, cover);
}

export type MaskBoundsUv = { x: number; y: number; width: number; height: number };

export function shapeBounds(kind: ClipMaskKind): MaskBoundsUv | null {
  if (kind.type === "rect") {
    return { x: kind.x, y: kind.y, width: kind.width, height: kind.height };
  }
  if (kind.type === "ellipse") {
    return {
      x: kind.cx - kind.rx,
      y: kind.cy - kind.ry,
      width: kind.rx * 2,
      height: kind.ry * 2,
    };
  }
  return null;
}

export function normalizeRectUv(x0: number, y0: number, x1: number, y1: number): MaskBoundsUv {
  const x = Math.min(x0, x1);
  const y = Math.min(y0, y1);
  return {
    x,
    y,
    width: Math.abs(x1 - x0),
    height: Math.abs(y1 - y0),
  };
}

export function clampMaskBounds(b: MaskBoundsUv): MaskBoundsUv {
  const x = Math.min(1, Math.max(0, b.x));
  const y = Math.min(1, Math.max(0, b.y));
  const width = Math.min(1 - x, Math.max(0, b.width));
  const height = Math.min(1 - y, Math.max(0, b.height));
  return { x, y, width, height };
}

export function boundsToRectKind(b: MaskBoundsUv): ClipMaskKind {
  const c = clampMaskBounds(b);
  return { type: "rect", x: c.x, y: c.y, width: c.width, height: c.height };
}

export function boundsToEllipseKind(b: MaskBoundsUv): ClipMaskKind {
  const c = clampMaskBounds(b);
  return {
    type: "ellipse",
    cx: c.x + c.width / 2,
    cy: c.y + c.height / 2,
    rx: c.width / 2,
    ry: c.height / 2,
  };
}

/** Switch shape type while preserving axis-aligned bounds. */
export function switchMaskShape(
  kind: ClipMaskKind,
  to: "rect" | "ellipse",
): ClipMaskKind | null {
  const b = shapeBounds(kind);
  if (!b) return null;
  return to === "rect" ? boundsToRectKind(b) : boundsToEllipseKind(b);
}

export function defaultMask(kind: ClipMaskKind, feather = 0.05): ClipMask {
  return { enabled: true, invert: false, feather, kind };
}

export type MaskHandleKind =
  | "move"
  | "nw"
  | "ne"
  | "sw"
  | "se"
  | "n"
  | "s"
  | "e"
  | "w";

function dist(a: PxPoint, b: PxPoint): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

function pointInBoundsPx(p: PxPoint, corners: PxPoint[]): boolean {
  // Axis-aligned in UV but may be rotated on screen — use polygon hit on the 4 corners.
  let inside = false;
  for (let i = 0, j = corners.length - 1; i < corners.length; j = i++) {
    const xi = corners[i].x;
    const yi = corners[i].y;
    const xj = corners[j].x;
    const yj = corners[j].y;
    const intersect =
      yi > p.y !== yj > p.y && p.x < ((xj - xi) * (p.y - yi)) / (yj - yi + 1e-12) + xi;
    if (intersect) inside = !inside;
  }
  return inside;
}

export function maskCornerUvs(b: MaskBoundsUv): UvPoint[] {
  return [
    { u: b.x, v: b.y + b.height }, // sw in image V-down → bottom-left
    { u: b.x + b.width, v: b.y + b.height }, // se
    { u: b.x + b.width, v: b.y }, // ne
    { u: b.x, v: b.y }, // nw
  ];
}

export function hitTestMaskHandle(
  localPx: PxPoint,
  bounds: MaskBoundsUv,
  cover: CoverUv,
  transform: ClipTransform | null | undefined,
  w: number,
  h: number,
  handlePx = 10,
): MaskHandleKind | null {
  const cornersUv = maskCornerUvs(bounds);
  const corners = cornersUv.map((uv) => sourceUvToPreviewPx(uv, cover, transform, w, h));
  const labels: MaskHandleKind[] = ["sw", "se", "ne", "nw"];
  for (let i = 0; i < 4; i++) {
    if (dist(localPx, corners[i]) <= handlePx) return labels[i];
  }
  const mid = (a: PxPoint, b: PxPoint): PxPoint => ({
    x: (a.x + b.x) / 2,
    y: (a.y + b.y) / 2,
  });
  const edges: { kind: MaskHandleKind; p: PxPoint }[] = [
    { kind: "s", p: mid(corners[0], corners[1]) },
    { kind: "e", p: mid(corners[1], corners[2]) },
    { kind: "n", p: mid(corners[2], corners[3]) },
    { kind: "w", p: mid(corners[3], corners[0]) },
  ];
  for (const e of edges) {
    if (dist(localPx, e.p) <= handlePx) return e.kind;
  }
  if (pointInBoundsPx(localPx, corners)) return "move";
  return null;
}

export function applyMaskHandleDrag(
  kind: MaskHandleKind,
  start: MaskBoundsUv,
  startUv: UvPoint,
  curUv: UvPoint,
): MaskBoundsUv {
  const du = curUv.u - startUv.u;
  const dv = curUv.v - startUv.v;
  if (kind === "move") {
    // Slide-clamp: keep the size and stop at the frame edge (clampMaskBounds alone would
    // shrink the box as it crosses the edge).
    return clampMaskBounds({
      x: Math.min(Math.max(0, start.x + du), Math.max(0, 1 - start.width)),
      y: Math.min(Math.max(0, start.y + dv), Math.max(0, 1 - start.height)),
      width: start.width,
      height: start.height,
    });
  }

  let x0 = start.x;
  let y0 = start.y;
  let x1 = start.x + start.width;
  let y1 = start.y + start.height;

  switch (kind) {
    case "nw":
      x0 = start.x + du;
      y0 = start.y + dv;
      break;
    case "ne":
      x1 = start.x + start.width + du;
      y0 = start.y + dv;
      break;
    case "sw":
      x0 = start.x + du;
      y1 = start.y + start.height + dv;
      break;
    case "se":
      x1 = start.x + start.width + du;
      y1 = start.y + start.height + dv;
      break;
    case "n":
      y0 = start.y + dv;
      break;
    case "s":
      y1 = start.y + start.height + dv;
      break;
    case "w":
      x0 = start.x + du;
      break;
    case "e":
      x1 = start.x + start.width + du;
      break;
  }

  // Clamp each edge before normalizing so a handle dragged off-frame pins its own edge
  // at 0/1 without dragging the anchored (opposite) edge outward with it.
  const c01 = (v: number) => Math.min(1, Math.max(0, v));
  return clampMaskBounds(normalizeRectUv(c01(x0), c01(y0), c01(x1), c01(y1)));
}

/** Round-trip identity check for cover-fit + optional transform (used by smoke asserts). */
export function assertUvPxRoundTrip(
  cover: CoverUv,
  transform: ClipTransform,
  previewW: number,
  previewH: number,
  samples: UvPoint[] = [
    { u: 0.5, v: 0.5 },
    { u: 0.25, v: 0.25 },
    { u: 0.75, v: 0.8 },
  ],
  eps = 1e-4,
): boolean {
  for (const uv of samples) {
    const px = sourceUvToPreviewPx(uv, cover, transform, previewW, previewH);
    const back = previewPxToSourceUv(px, cover, transform, previewW, previewH);
    if (Math.abs(back.u - uv.u) > eps || Math.abs(back.v - uv.v) > eps) return false;
  }
  return true;
}
