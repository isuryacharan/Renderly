// The golden-frame reference: the compositor's result worked out on the CPU, from the same
// definitions but a separate implementation (plain loops, no shaders), so the two check each
// other. Images are RGBA floats, premultiplied, rows top first, values 0 to 1.
//
// It follows the GPU exactly where that matters: pixel centres at (i + 0.5) / size, bilinear
// sampling with edges clamped (as a linear-filtered render target samples), layers outside their
// rectangle (less the crop) not drawn, the background (a colour, or the bottom picture blurred to
// fill the frame) under everything, and each layer combined by its blend mode with the W3C
// "source over" rule. The blend formulas are written out again here, not shared with the shader.

import type { BackgroundSettings, BlendMode } from "@renderly/core";
import { BLACK_BACKGROUND, BLUR_TAPS, blurPlan, fillZoom, hexColor } from "./blend";
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
 * coordinates (0..1, y down), or null when the pixel is outside the layer's picture less its crop.
 * `box` is the share of the layer the picture covers, per axis, about its centre.
 */
export function layerCoordinate(
  t: Transform,
  width: number,
  height: number,
  px: number,
  py: number,
  box: readonly [number, number] = [1, 1],
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
  const [bx, by] = box;
  const left = 0.5 - bx / 2 + t.cropLeft * bx;
  const right = 0.5 + bx / 2 - t.cropRight * bx;
  const top = 0.5 - by / 2 + t.cropTop * by;
  const bottom = 0.5 + by / 2 - t.cropBottom * by;
  if (u < left || u > right || v < top || v > bottom) return null;
  return [u, v];
}

type RGB = [number, number, number];

const lum = ([r, g, b]: RGB) => 0.3 * r + 0.59 * g + 0.11 * b;

function clipColor(c: RGB): RGB {
  const l = lum(c);
  const n = Math.min(...c);
  const x = Math.max(...c);
  let out = c;
  if (n < 0) out = out.map((v) => l + ((v - l) * l) / (l - n)) as RGB;
  if (x > 1) out = out.map((v) => l + ((v - l) * (1 - l)) / (x - l)) as RGB;
  return out;
}

const setLum = (c: RGB, l: number): RGB => clipColor(c.map((v) => v + (l - lum(c))) as RGB);
const sat = (c: RGB) => Math.max(...c) - Math.min(...c);

/** Scales `c` so its saturation is `s`: the largest channel becomes s, the smallest 0. */
function setSat(c: RGB, s: number): RGB {
  const n = Math.min(...c);
  const x = Math.max(...c);
  return x > n ? (c.map((v) => ((v - n) * s) / (x - n)) as RGB) : [0, 0, 0];
}

/** W3C separable blend functions, backdrop `b` and source `s`, straight colour. */
const SEPARABLE: Partial<Record<BlendMode, (b: number, s: number) => number>> = {
  multiply: (b, s) => b * s,
  screen: (b, s) => b + s - b * s,
  // HardLight(Cs, Cb): multiply by 2b in the dark half, screen with 2b - 1 in the light half.
  overlay: (b, s) => (b <= 0.5 ? s * (2 * b) : s + (2 * b - 1) - s * (2 * b - 1)),
  softLight: (b, s) => {
    if (s <= 0.5) return b - (1 - 2 * s) * b * (1 - b);
    const d = b <= 0.25 ? ((16 * b - 12) * b + 4) * b : Math.sqrt(b);
    return b + (2 * s - 1) * (d - b);
  },
  add: (b, s) => Math.min(1, b + s),
  darken: (b, s) => Math.min(b, s),
  lighten: (b, s) => Math.max(b, s),
  difference: (b, s) => Math.abs(b - s),
  colorDodge: (b, s) => (b === 0 ? 0 : s >= 1 ? 1 : Math.min(1, b / (1 - s))),
  colorBurn: (b, s) => (b >= 1 ? 1 : s <= 0 ? 0 : 1 - Math.min(1, (1 - b) / s)),
};

/** The blend function B(Cb, Cs) for `mode`. */
export function blendReference(mode: BlendMode, b: RGB, s: RGB): RGB {
  switch (mode) {
    case "hue":
      return setLum(setSat(s, sat(b)), lum(b));
    case "saturation":
      return setLum(setSat(b, sat(s)), lum(b));
    case "color":
      return setLum(s, lum(b));
    case "luminosity":
      return setLum(b, lum(s));
    default: {
      const f = SEPARABLE[mode];
      return f ? (b.map((v, i) => f(v, s[i] ?? 0)) as RGB) : s;
    }
  }
}

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));

export interface ReferenceLayer {
  image: Image;
  transform: Transform;
  /** The share of the layer the picture covers (the uploader's fit); all of it when left out. */
  box?: readonly [number, number];
}

/** The bottom picture magnified to cover the frame, shrunk, blurred and scaled up again. */
function blurredFill(width: number, height: number, layer: ReferenceLayer, amount: number): Image {
  const plan = blurPlan(width, height, amount);
  const { width: w, height: h, weights } = plan;
  const box = layer.box ?? [1, 1];
  const zoom = fillZoom(box);
  // Samples stay a texel inside the picture, so its edge never mixes in the empty bars.
  const inside = (c: number, share: number, size: number) =>
    Math.min(0.5 + share / 2 - 1 / size, Math.max(0.5 - share / 2 + 1 / size, c));
  let small = new Float32Array(w * h * 4);
  for (let j = 0; j < h; j++)
    for (let i = 0; i < w; i++) {
      const sum = [0, 0, 0];
      for (let b = 0; b < BLUR_TAPS; b++)
        for (let a = 0; a < BLUR_TAPS; a++) {
          const u = (i + (a + 0.5) / BLUR_TAPS) / w;
          const v = (j + (b + 0.5) / BLUR_TAPS) / h;
          const c = sampleBilinear(
            layer.image,
            inside(0.5 + (u - 0.5) / zoom, box[0], layer.image.width),
            inside(0.5 + (v - 0.5) / zoom, box[1], layer.image.height),
          );
          for (let k = 0; k < 3; k++) sum[k] = (sum[k] ?? 0) + (c[k] ?? 0);
        }
      const o = (j * w + i) * 4;
      for (let k = 0; k < 3; k++) small[o + k] = (sum[k] ?? 0) / (BLUR_TAPS * BLUR_TAPS);
      small[o + 3] = 1;
    }
  const radius = weights.length - 1;
  for (const [dx, dy] of [
    [1, 0],
    [0, 1],
  ] as const) {
    const next = new Float32Array(small.length);
    for (let j = 0; j < h; j++)
      for (let i = 0; i < w; i++) {
        const o = (j * w + i) * 4;
        for (let k = -radius; k <= radius; k++) {
          const x = Math.min(w - 1, Math.max(0, i + dx * k));
          const y = Math.min(h - 1, Math.max(0, j + dy * k));
          const weight = weights[Math.abs(k)] ?? 0;
          const from = (y * w + x) * 4;
          for (let c = 0; c < 3; c++)
            next[o + c] = (next[o + c] ?? 0) + (small[from + c] ?? 0) * weight;
        }
        next[o + 3] = 1;
      }
    small = next;
  }
  const blurred: Image = { width: w, height: h, data: small };
  const data = new Float32Array(width * height * 4);
  for (let py = 0; py < height; py++)
    for (let px = 0; px < width; px++) {
      const c = sampleBilinear(blurred, (px + 0.5) / width, (py + 0.5) / height);
      const o = (py * width + px) * 4;
      data.set([c[0], c[1], c[2], 1], o);
    }
  return { width, height, data };
}

/** Composites `layers` (bottom first) over the background, as the compositor does. */
export function referenceComposite(
  width: number,
  height: number,
  layers: readonly ReferenceLayer[],
  background: BackgroundSettings = BLACK_BACKGROUND,
): Image {
  const bottom = layers[0];
  let data: Float32Array;
  if (background.kind === "blur" && bottom) {
    data = blurredFill(width, height, bottom, background.blur).data;
  } else {
    data = new Float32Array(width * height * 4);
    const [r, g, b] = hexColor(background.color);
    for (let o = 0; o < data.length; o += 4) data.set([r, g, b, 1], o);
  }
  for (const { image, transform, box } of layers) {
    if (transform.opacity <= 0) continue;
    for (let py = 0; py < height; py++)
      for (let px = 0; px < width; px++) {
        const uv = layerCoordinate(transform, width, height, px, py, box);
        if (!uv) continue;
        const sampled = sampleBilinear(image, uv[0], uv[1]);
        const s = sampled.map((v) => v * transform.opacity);
        const o = (py * width + px) * 4;
        const sa = s[3] ?? 0;
        const ba = data[o + 3] ?? 0;
        if (transform.blend === "normal") {
          for (let c = 0; c < 4; c++) data[o + c] = (s[c] ?? 0) + (data[o + c] ?? 0) * (1 - sa);
          continue;
        }
        if (sa <= 0) continue;
        const cs = [0, 1, 2].map((c) => clamp01((s[c] ?? 0) / sa)) as RGB;
        const cb = [0, 1, 2].map((c) => (ba > 0 ? clamp01((data[o + c] ?? 0) / ba) : 0)) as RGB;
        const mixed = blendReference(transform.blend, cb, cs);
        for (let c = 0; c < 3; c++)
          data[o + c] =
            (s[c] ?? 0) * (1 - ba) + (data[o + c] ?? 0) * (1 - sa) + sa * ba * (mixed[c] ?? 0);
        data[o + 3] = sa + ba * (1 - sa);
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
