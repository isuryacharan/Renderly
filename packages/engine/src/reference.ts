// The golden-frame reference: the compositor's result worked out on the CPU, from the same
// definitions but a separate implementation (plain loops, no shaders), so the two check each
// other. Images are RGBA floats, premultiplied, rows top first, values 0 to 1.
//
// It follows the GPU exactly where that matters: pixel centres at (i + 0.5) / size, bilinear
// sampling with edges clamped (as a linear-filtered render target samples), layers outside their
// rectangle not drawn, and premultiplied "over" onto opaque black.

import type { Transform } from "./timeline";

export interface Image {
  width: number;
  height: number;
  /** RGBA, premultiplied, rows top first. */
  data: Float32Array;
}

/** Bilinear sample of `image` at texture coordinates (u, v) in 0..1, v down. */
export function sampleBilinear(
  image: Image,
  u: number,
  v: number,
): [number, number, number, number] {
  const { width, height, data } = image;
  const x = u * width - 0.5;
  const y = v * height - 0.5;
  const x0 = Math.floor(x);
  const y0 = Math.floor(y);
  const fx = x - x0;
  const fy = y - y0;
  const clampX = (i: number) => Math.min(width - 1, Math.max(0, i));
  const clampY = (i: number) => Math.min(height - 1, Math.max(0, i));
  const at = (ix: number, iy: number, c: number) =>
    data[(clampY(iy) * width + clampX(ix)) * 4 + c] ?? 0;
  const out: [number, number, number, number] = [0, 0, 0, 0];
  for (let c = 0; c < 4; c++) {
    const top = at(x0, y0, c) * (1 - fx) + at(x0 + 1, y0, c) * fx;
    const bottom = at(x0, y0 + 1, c) * (1 - fx) + at(x0 + 1, y0 + 1, c) * fx;
    out[c] = top * (1 - fy) + bottom * fy;
  }
  return out;
}

/**
 * Where output pixel (px, py) samples layer `t` in a frame of `width` × `height`, as layer texture
 * coordinates (0..1, y down), or null when the pixel is outside the layer.
 */
export function layerCoordinate(
  t: Transform,
  width: number,
  height: number,
  px: number,
  py: number,
): [number, number] | null {
  // Pixels from the place the anchor is put.
  const dx = (px + 0.5) / width - 0.5 - t.x;
  const dy = (py + 0.5) / height - 0.5 - t.y;
  let qx = dx * width;
  let qy = dy * height;
  // Undo a clockwise rotation (y down).
  const a = (t.rotation * Math.PI) / 180;
  const rx = Math.cos(a) * qx + Math.sin(a) * qy;
  const ry = -Math.sin(a) * qx + Math.cos(a) * qy;
  // Undo the scale, add the anchor, mirror.
  qx = (rx / (t.scaleX * width) + t.anchorX) * (t.flipX ? -1 : 1);
  qy = (ry / (t.scaleY * height) + t.anchorY) * (t.flipY ? -1 : 1);
  const u = qx + 0.5;
  const v = qy + 0.5;
  if (u < 0 || u > 1 || v < 0 || v > 1) return null;
  return [u, v];
}

/** Composites `layers` (bottom first) over opaque black, as the compositor does. */
export function referenceComposite(
  width: number,
  height: number,
  layers: readonly { image: Image; transform: Transform }[],
): Image {
  const data = new Float32Array(width * height * 4);
  for (let i = 3; i < data.length; i += 4) data[i] = 1;
  for (const { image, transform } of layers) {
    if (transform.opacity <= 0) continue;
    for (let py = 0; py < height; py++)
      for (let px = 0; px < width; px++) {
        const uv = layerCoordinate(transform, width, height, px, py);
        if (!uv) continue;
        const s = sampleBilinear(image, uv[0], uv[1]);
        const o = (py * width + px) * 4;
        const alpha = (s[3] ?? 0) * transform.opacity;
        for (let c = 0; c < 4; c++)
          data[o + c] = (s[c] ?? 0) * transform.opacity + (data[o + c] ?? 0) * (1 - alpha);
      }
  }
  return { width, height, data };
}

export interface Difference {
  /** Largest difference of any channel, in 8-bit steps. */
  max: number;
  /** Share of pixels with any channel off by more than `tolerance` steps. */
  outliers: number;
  /** Mean absolute difference per channel, in 8-bit steps. */
  mean: number;
}

/**
 * Compares 8-bit RGBA pixels from the GPU (rows bottom first, as WebGL reads them) with a
 * reference image (rows top first).
 */
export function compare(gpu: Uint8Array, reference: Image, tolerance: number): Difference {
  const { width, height, data } = reference;
  let max = 0;
  let sum = 0;
  let outliers = 0;
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) {
      const g = ((height - 1 - y) * width + x) * 4;
      const r = (y * width + x) * 4;
      let worst = 0;
      for (let c = 0; c < 4; c++) {
        const expected = Math.round(Math.min(1, Math.max(0, data[r + c] ?? 0)) * 255);
        const d = Math.abs((gpu[g + c] ?? 0) - expected);
        worst = Math.max(worst, d);
        sum += d;
      }
      max = Math.max(max, worst);
      if (worst > tolerance) outliers++;
    }
  return { max, outliers: outliers / (width * height), mean: sum / (width * height * 4) };
}

/** A smooth test picture (no hard edges inside), premultiplied: gradients and a soft disc. */
export function testPattern(width: number, height: number, alpha = false): Image {
  const data = new Float32Array(width * height * 4);
  for (let y = 0; y < height; y++)
    for (let x = 0; x < width; x++) {
      const u = (x + 0.5) / width;
      const v = (y + 0.5) / height;
      const d = Math.hypot(u - 0.35, (v - 0.4) * (height / width));
      const disc = Math.max(0, Math.min(1, (0.22 - d) / 0.08));
      const a = alpha ? 0.35 + 0.65 * disc : 1;
      const r = 0.15 + 0.7 * u;
      const g = 0.2 + 0.6 * v;
      const b = 0.25 + 0.6 * disc;
      const o = (y * width + x) * 4;
      data[o] = r * a;
      data[o + 1] = g * a;
      data[o + 2] = b * a;
      data[o + 3] = a;
    }
  return { width, height, data };
}
