// Blend modes and the blurred background, shared by the compositor's shaders and the CPU
// reference: the mode numbering, the GLSL, and the blur's sizes and weights. The reference
// implements the blend formulas again on its own (reference.ts), so the two check each other.
//
// Blend formulas are those of W3C Compositing and Blending Level 1, on straight (not
// premultiplied) colour clamped to 0..1, composited with the "source over" rule:
//   co = cs (1 - ab) + cb (1 - as) + as ab B(Cb, Cs),  ao = as + ab (1 - as)
// where cs, cb are premultiplied and Cs, Cb straight. `add` is min(1, Cb + Cs).

import { BLEND_MODES, type BackgroundSettings, type BlendMode } from "@renderly/core";

/** The background before a sequence sets one: black. */
export const BLACK_BACKGROUND: BackgroundSettings = { kind: "color", color: "#000000", blur: 0.5 };

/** The shader's number for a blend mode. */
export function blendIndex(mode: BlendMode): number {
  return Math.max(0, BLEND_MODES.indexOf(mode));
}

/** GLSL ES 3.00: `vec3 blendColor(int mode, vec3 cb, vec3 cs)` on straight colour. */
export const BLEND_GLSL = /* glsl */ `
float lum(vec3 c) { return dot(c, vec3(0.3, 0.59, 0.11)); }
vec3 clipColor(vec3 c) {
  float l = lum(c);
  float n = min(c.r, min(c.g, c.b));
  float x = max(c.r, max(c.g, c.b));
  if (n < 0.0) c = l + (c - l) * l / (l - n);
  if (x > 1.0) c = l + (c - l) * (1.0 - l) / (x - l);
  return c;
}
vec3 setLum(vec3 c, float l) { return clipColor(c + (l - lum(c))); }
float sat(vec3 c) { return max(c.r, max(c.g, c.b)) - min(c.r, min(c.g, c.b)); }
vec3 setSat(vec3 c, float s) {
  float n = min(c.r, min(c.g, c.b));
  float x = max(c.r, max(c.g, c.b));
  return x > n ? (c - n) * s / (x - n) : vec3(0.0);
}
float softLightD(float b) {
  return b <= 0.25 ? ((16.0 * b - 12.0) * b + 4.0) * b : sqrt(b);
}
float blendChannel(int mode, float b, float s) {
  if (mode == 1) return b * s;                                   // multiply
  if (mode == 2) return b + s - b * s;                           // screen
  if (mode == 3) return b <= 0.5 ? 2.0 * s * b : 1.0 - 2.0 * (1.0 - s) * (1.0 - b); // overlay
  if (mode == 4) return s <= 0.5                                 // soft light
      ? b - (1.0 - 2.0 * s) * b * (1.0 - b)
      : b + (2.0 * s - 1.0) * (softLightD(b) - b);
  if (mode == 5) return min(1.0, b + s);                         // add
  if (mode == 6) return min(b, s);                               // darken
  if (mode == 7) return max(b, s);                               // lighten
  if (mode == 8) return abs(b - s);                              // difference
  if (mode == 9) return b == 0.0 ? 0.0 : s >= 1.0 ? 1.0 : min(1.0, b / (1.0 - s)); // dodge
  if (mode == 10) return b >= 1.0 ? 1.0 : s <= 0.0 ? 0.0 : 1.0 - min(1.0, (1.0 - b) / s); // burn
  return s;                                                      // normal
}
vec3 blendColor(int mode, vec3 b, vec3 s) {
  if (mode == 11) return setLum(setSat(s, sat(b)), lum(b));      // hue
  if (mode == 12) return setLum(setSat(b, sat(s)), lum(b));      // saturation
  if (mode == 13) return setLum(s, lum(b));                      // colour
  if (mode == 14) return setLum(b, lum(s));                      // luminosity
  return vec3(blendChannel(mode, b.r, s.r), blendChannel(mode, b.g, s.g), blendChannel(mode, b.b, s.b));
}`;

/** The blurred background is worked out at this fraction of the frame size, then scaled up. */
export const BLUR_DOWNSCALE = 8;
/** Sub-samples per axis when shrinking the picture into the small target. */
export const BLUR_TAPS = 4;
const MAX_RADIUS = 48;

export interface BlurPlan {
  width: number;
  height: number;
  /** Gaussian weights from the centre outwards (index 0 is the centre), summing to 1 over ±radius. */
  weights: number[];
}

/**
 * Sizes and weights of the blur for a frame of `width` × `height` and an amount from 0 to 1. The
 * blur's size is a share of the frame width, so it looks the same at any preview resolution.
 */
export function blurPlan(width: number, height: number, amount: number): BlurPlan {
  const w = Math.max(2, Math.round(width / BLUR_DOWNSCALE));
  const h = Math.max(2, Math.round(height / BLUR_DOWNSCALE));
  const sigma = Math.max(0.5, (0.005 + 0.045 * Math.min(1, Math.max(0, amount))) * w);
  const radius = Math.min(MAX_RADIUS, Math.ceil(3 * sigma));
  const raw = Array.from({ length: radius + 1 }, (_, i) =>
    Math.exp(-(i * i) / (2 * sigma * sigma)),
  );
  const total = raw.reduce((sum, v, i) => sum + (i === 0 ? v : 2 * v), 0);
  return { width: w, height: h, weights: raw.map((v) => v / total) };
}

/** How much to magnify a picture covering `box` of the frame so that it covers all of it. */
export function fillZoom(box: readonly [number, number]): number {
  return Math.max(1 / Math.max(1e-6, box[0]), 1 / Math.max(1e-6, box[1]));
}

/** `#rrggbb` → straight RGB, 0 to 1. Anything else is black. */
export function hexColor(hex: string): [number, number, number] {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!m) return [0, 0, 0];
  return [1, 2, 3].map((i) => parseInt(m[i] as string, 16) / 255) as [number, number, number];
}
