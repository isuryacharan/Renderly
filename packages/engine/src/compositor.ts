// Layers → one picture. Each layer keeps its own target holding its last uploaded frame, so a
// layer that updates less often (keyframe-only under decoder pressure) still shows its last
// picture. Compositing fills the composite target with the sequence's background (a colour, or
// the bottom layer's picture blurred to fill the frame), then draws the layers bottom to top,
// each with its transform, crop, opacity and blend mode.
//
// Layer targets hold premultiplied colour: the uploader writes it that way, and any input with
// straight alpha must be premultiplied on its way in. Normal layers are drawn with the GPU's
// premultiplied "over" blending. Other blend modes need the colour underneath, which a shader
// cannot read from the target it draws to, so they draw into a second composite target reading
// the first, and the two swap.
//
// Every target is reserved in the MemoryBudget first; past the ceiling a layer gets no target
// and is not drawn, and a blend mode without its second target draws as normal.

import type { BackgroundSettings } from "@renderly/core";
import * as THREE from "three";
import {
  BLACK_BACKGROUND,
  BLEND_GLSL,
  BLUR_TAPS,
  blendIndex,
  blurPlan,
  fillZoom,
  hexColor,
} from "./blend";
import { makeTarget, Pass } from "./gpu";
import type { Size } from "./layout";
import { halfFloatTargetBytes, type MemoryBudget } from "./memory";
import { LruKeys } from "./scrub";
import type { Transform } from "./timeline";

/** Composited frames kept for scrubbing. */
export const FRAME_CACHE_ENTRIES = 12;

/**
 * Where an output pixel samples its layer. Each output pixel is mapped back to the layer: undo
 * the position, then the rotation (in pixels, so a non-square frame does not shear), then the
 * scale, add the anchor, mirror. Pixels outside the picture's box less the crop sample nothing.
 * The same mapping, written separately on the CPU, is the golden-frame reference (reference.ts).
 */
const MAPPING = /* glsl */ `
uniform sampler2D tLayer;
uniform vec2 uSize;      // composite size in pixels
uniform vec2 uOffset;    // where the anchor sits, as a fraction of the frame from its centre (y down)
uniform vec2 uScale;     // per axis
uniform float uRotation; // radians, clockwise on screen
uniform vec2 uAnchor;    // fraction of the layer from its centre
uniform vec2 uFlip;      // 1 or -1 per axis
uniform float uOpacity;
uniform vec2 uBox;       // share of the layer the picture covers, per axis (about the centre)
uniform vec4 uCrop;      // left, top, right, bottom, as a fraction of the picture
in vec2 vUv;
out vec4 fragColor;

/** The layer's colour (premultiplied, times the opacity) at this pixel; false outside it. */
bool layerAt(out vec4 color) {
  vec2 d = vec2(vUv.x, 1.0 - vUv.y);                  // frame coordinates, y down
  vec2 q = (d - 0.5 - uOffset) * uSize;               // pixels from the anchor's place
  float c = cos(uRotation);
  float s = sin(uRotation);
  q = vec2(c * q.x + s * q.y, -s * q.x + c * q.y);     // undo a clockwise rotation (y down)
  vec2 l = (q / (uScale * uSize) + uAnchor) * uFlip;  // layer, from its centre
  vec2 p = l + 0.5;
  vec2 lo = 0.5 - 0.5 * uBox + uCrop.xy * uBox;
  vec2 hi = 0.5 + 0.5 * uBox - uCrop.zw * uBox;
  if (any(lessThan(p, lo)) || any(greaterThan(p, hi))) return false;
  color = texture(tLayer, vec2(p.x, 1.0 - p.y)) * uOpacity;
  return true;
}`;

/** A normal layer: drawn with the GPU's premultiplied blending. */
const NORMAL = /* glsl */ `
${MAPPING}
void main() {
  vec4 color;
  if (!layerAt(color)) discard;
  fragColor = color;
}`;

/** Any other blend mode: reads the composite so far and writes the whole result. */
const BLENDED = /* glsl */ `
uniform sampler2D tBackdrop;
uniform int uMode;
${MAPPING}
${BLEND_GLSL}
void main() {
  vec4 b = texture(tBackdrop, vUv);
  vec4 s;
  if (!layerAt(s) || s.a <= 0.0) {
    fragColor = b;
    return;
  }
  vec3 cs = clamp(s.rgb / s.a, 0.0, 1.0);
  vec3 cb = b.a > 0.0 ? clamp(b.rgb / b.a, 0.0, 1.0) : vec3(0.0);
  vec3 mixed = blendColor(uMode, cb, cs);
  fragColor = vec4(s.rgb * (1.0 - b.a) + b.rgb * (1.0 - s.a) + s.a * b.a * mixed,
                   s.a + b.a * (1.0 - s.a));
}`;

/** The bottom picture magnified to cover the frame and shrunk into the small blur target. */
const BLUR_FILL = /* glsl */ `
uniform sampler2D tLayer;
uniform vec2 uSmall;   // small target size in pixels
uniform float uZoom;   // magnification that makes the picture cover the frame
uniform vec2 uBox;     // share of the layer the picture covers
in vec2 vUv;
out vec4 fragColor;
void main() {
  vec2 cell = floor(vUv * uSmall);
  // Samples stay a texel inside the picture, so its edge never mixes in the empty bars.
  vec2 texel = 1.0 / vec2(textureSize(tLayer, 0));
  vec2 lo = 0.5 - 0.5 * uBox + texel;
  vec2 hi = 0.5 + 0.5 * uBox - texel;
  vec4 sum = vec4(0.0);
  for (int j = 0; j < ${BLUR_TAPS}; j++)
    for (int i = 0; i < ${BLUR_TAPS}; i++) {
      vec2 d = (cell + (vec2(float(i), float(j)) + 0.5) / ${BLUR_TAPS}.0) / uSmall;
      sum += texture(tLayer, clamp(0.5 + (d - 0.5) / uZoom, lo, hi));
    }
  fragColor = vec4(sum.rgb / ${BLUR_TAPS * BLUR_TAPS}.0, 1.0);
}`;

/** One direction of a Gaussian blur, on texel centres with edges clamped. */
const BLUR = /* glsl */ `
uniform sampler2D tImage;
uniform ivec2 uStep;       // (1, 0) or (0, 1)
uniform int uRadius;
uniform float uWeights[49];
in vec2 vUv;
out vec4 fragColor;
void main() {
  ivec2 size = textureSize(tImage, 0);
  ivec2 at = ivec2(gl_FragCoord.xy);
  vec4 sum = texelFetch(tImage, at, 0) * uWeights[0];
  for (int i = 1; i <= uRadius; i++) {
    sum += texelFetch(tImage, clamp(at + uStep * i, ivec2(0), size - 1), 0) * uWeights[i];
    sum += texelFetch(tImage, clamp(at - uStep * i, ivec2(0), size - 1), 0) * uWeights[i];
  }
  fragColor = vec4(sum.rgb, 1.0);
}`;

const COPY = /* glsl */ `
uniform sampler2D tImage;
in vec2 vUv;
out vec4 fragColor;
void main() {
  fragColor = texture(tImage, vUv);
}`;

export interface LayerDraw {
  layer: number;
  transform: Transform;
}

function mappingUniforms(): Record<string, THREE.IUniform> {
  return {
    tLayer: { value: null },
    uSize: { value: new THREE.Vector2(2, 2) },
    uOffset: { value: new THREE.Vector2() },
    uScale: { value: new THREE.Vector2(1, 1) },
    uRotation: { value: 0 },
    uAnchor: { value: new THREE.Vector2() },
    uFlip: { value: new THREE.Vector2(1, 1) },
    uOpacity: { value: 1 },
    uBox: { value: new THREE.Vector2(1, 1) },
    uCrop: { value: new THREE.Vector4() },
  };
}

export class Compositor {
  private size: Size = { width: 2, height: 2 };
  private output: THREE.WebGLRenderTarget | null = null;
  /** The second composite target, made on the first non-normal blend. */
  private spare: THREE.WebGLRenderTarget | null = null;
  private blurTargets: [THREE.WebGLRenderTarget, THREE.WebGLRenderTarget] | null = null;
  private readonly layers = new Map<number, THREE.WebGLRenderTarget>();
  /** Per layer: the share of its target the picture covers (set at upload). */
  private readonly boxes = new Map<number, readonly [number, number]>();
  private background: BackgroundSettings = BLACK_BACKGROUND;
  private readonly normal = new Pass(NORMAL, mappingUniforms(), { premultiplied: true });
  private readonly blended = new Pass(BLENDED, {
    ...mappingUniforms(),
    tBackdrop: { value: null },
    uMode: { value: 0 },
  });
  private readonly blurFill = new Pass(BLUR_FILL, {
    tLayer: { value: null },
    uSmall: { value: new THREE.Vector2(2, 2) },
    uZoom: { value: 1 },
    uBox: { value: new THREE.Vector2(1, 1) },
  });
  private readonly blur = new Pass(BLUR, {
    tImage: { value: null },
    uStep: { value: new THREE.Vector2(1, 0) },
    uRadius: { value: 0 },
    uWeights: { value: new Array<number>(49).fill(0) },
  });
  private readonly copy = new Pass(COPY, { tImage: { value: null } });
  /** Layers refused a target because of the memory ceiling, since the last reset. */
  refused = 0;

  constructor(
    private readonly renderer: THREE.WebGLRenderer,
    private readonly budget: MemoryBudget,
    /** Prefix for this compositor's budget entries, when there is more than one. */
    private readonly name = "",
  ) {}

  get texture(): THREE.Texture | null {
    return this.output?.texture ?? null;
  }

  /** Sets the working size. Existing layer pictures are dropped; the caller redraws them. */
  resize(size: Size): void {
    this.size = size;
    for (const layer of [...this.layers.keys()]) this.releaseLayer(layer);
    this.output?.dispose();
    this.output = null;
    this.releaseSpare();
    this.releaseBlur();
    if (
      this.budget.reserve(`${this.name}composite`, halfFloatTargetBytes(size.width, size.height))
    ) {
      this.output = makeTarget(size.width, size.height);
    } else {
      this.refused++;
    }
  }

  /** What fills the frame behind the layers. */
  setBackground(background: BackgroundSettings): void {
    this.background = { ...background };
    if (background.kind !== "blur") this.releaseBlur();
  }

  /** The target for `layer`'s picture, allocated on first use; null past the memory ceiling. */
  layerTarget(layer: number): THREE.WebGLRenderTarget | null {
    const existing = this.layers.get(layer);
    if (existing) return existing;
    const { width, height } = this.size;
    if (!this.budget.reserve(`${this.name}layer ${layer}`, halfFloatTargetBytes(width, height))) {
      this.refused++;
      return null;
    }
    const target = makeTarget(width, height);
    this.layers.set(layer, target);
    return target;
  }

  /** Records how much of `layer`'s target its picture covers (what the uploader returned). */
  setPictureBox(layer: number, box: readonly [number, number]): void {
    this.boxes.set(layer, box);
  }

  releaseLayer(layer: number): void {
    this.layers.get(layer)?.dispose();
    this.layers.delete(layer);
    this.boxes.delete(layer);
    this.budget.release(`${this.name}layer ${layer}`);
  }

  /** Draws `draws` (bottom first) over the background into the composite target. */
  composite(draws: readonly LayerDraw[]): void {
    if (!this.output) return;
    const r = this.renderer;
    r.setRenderTarget(this.output);
    const [red, green, blue] = hexColor(this.background.color);
    r.setClearColor(new THREE.Color(red, green, blue), 1);
    r.clear(true, false, false);
    const bottom = draws.find((d) => this.layers.has(d.layer));
    if (this.background.kind === "blur" && bottom) this.drawBlurredFill(bottom.layer);
    for (const draw of draws) {
      const target = this.layers.get(draw.layer);
      const t = draw.transform;
      if (!target || t.opacity <= 0) continue;
      const mode = blendIndex(t.blend);
      const spare = mode === 0 ? null : this.spareTarget();
      const pass = spare ? this.blended : this.normal;
      this.setMapping(pass.uniforms, target, draw);
      if (spare) {
        (pass.uniforms["tBackdrop"] as THREE.IUniform).value = this.output.texture;
        (pass.uniforms["uMode"] as THREE.IUniform).value = mode;
        pass.run(r, spare);
        this.spare = this.output;
        this.output = spare;
      } else {
        pass.run(r, this.output);
      }
    }
  }

  private setMapping(
    u: Record<string, THREE.IUniform>,
    target: THREE.WebGLRenderTarget,
    { layer, transform: t }: LayerDraw,
  ): void {
    const box = this.boxes.get(layer) ?? [1, 1];
    (u["tLayer"] as THREE.IUniform).value = target.texture;
    (u["uSize"]?.value as THREE.Vector2).set(this.size.width, this.size.height);
    (u["uOffset"]?.value as THREE.Vector2).set(t.x, t.y);
    (u["uScale"]?.value as THREE.Vector2).set(t.scaleX, t.scaleY);
    (u["uRotation"] as THREE.IUniform).value = (t.rotation * Math.PI) / 180;
    (u["uAnchor"]?.value as THREE.Vector2).set(t.anchorX, t.anchorY);
    (u["uFlip"]?.value as THREE.Vector2).set(t.flipX ? -1 : 1, t.flipY ? -1 : 1);
    (u["uOpacity"] as THREE.IUniform).value = t.opacity;
    (u["uBox"]?.value as THREE.Vector2).set(box[0], box[1]);
    (u["uCrop"]?.value as THREE.Vector4).set(t.cropLeft, t.cropTop, t.cropRight, t.cropBottom);
  }

  /** The second composite target; null (draw as normal) past the memory ceiling. */
  private spareTarget(): THREE.WebGLRenderTarget | null {
    if (this.spare) return this.spare;
    const { width, height } = this.size;
    if (!this.budget.reserve(`${this.name}composite 2`, halfFloatTargetBytes(width, height))) {
      this.refused++;
      return null;
    }
    this.spare = makeTarget(width, height);
    return this.spare;
  }

  private releaseSpare(): void {
    this.spare?.dispose();
    this.spare = null;
    this.budget.release(`${this.name}composite 2`);
  }

  /** Fills the composite with `layer`'s picture, magnified to cover the frame and blurred. */
  private drawBlurredFill(layer: number): void {
    const target = this.layers.get(layer);
    const output = this.output;
    if (!target || !output) return;
    const plan = blurPlan(this.size.width, this.size.height, this.background.blur);
    if (
      !this.blurTargets ||
      this.blurTargets[0].width !== plan.width ||
      this.blurTargets[0].height !== plan.height
    ) {
      this.releaseBlur();
      if (
        !this.budget.reserve(`${this.name}blur`, 2 * halfFloatTargetBytes(plan.width, plan.height))
      )
        return;
      this.blurTargets = [makeTarget(plan.width, plan.height), makeTarget(plan.width, plan.height)];
    }
    const [a, b] = this.blurTargets;
    const r = this.renderer;
    const fill = this.blurFill.uniforms;
    (fill["tLayer"] as THREE.IUniform).value = target.texture;
    (fill["uSmall"]?.value as THREE.Vector2).set(plan.width, plan.height);
    const box = this.boxes.get(layer) ?? [1, 1];
    (fill["uZoom"] as THREE.IUniform).value = fillZoom(box);
    (fill["uBox"]?.value as THREE.Vector2).set(box[0], box[1]);
    this.blurFill.run(r, a);
    const u = this.blur.uniforms;
    const weights = u["uWeights"]?.value as number[];
    weights.fill(0);
    plan.weights.forEach((w, i) => (weights[i] = w));
    (u["uRadius"] as THREE.IUniform).value = plan.weights.length - 1;
    (u["tImage"] as THREE.IUniform).value = a.texture;
    (u["uStep"]?.value as THREE.Vector2).set(1, 0);
    this.blur.run(r, b);
    (u["tImage"] as THREE.IUniform).value = b.texture;
    (u["uStep"]?.value as THREE.Vector2).set(0, 1);
    this.blur.run(r, a);
    (this.copy.uniforms["tImage"] as THREE.IUniform).value = a.texture;
    this.copy.run(r, output);
  }

  private releaseBlur(): void {
    if (!this.blurTargets) return;
    for (const target of this.blurTargets) target.dispose();
    this.blurTargets = null;
    this.budget.release(`${this.name}blur`);
  }

  /**
   * The composite's pixels as 8-bit RGBA, rows bottom first (GL order). Preview and export both
   * read through here for the parity test.
   */
  readPixels(): Uint8Array | null {
    if (!this.output) return null;
    const { width, height } = this.size;
    const target = makeTarget(width, height, true);
    try {
      (this.copy.uniforms["tImage"] as THREE.IUniform).value = this.output.texture;
      this.copy.run(this.renderer, target);
      const pixels = new Uint8Array(width * height * 4);
      this.renderer.readRenderTargetPixels(target, 0, 0, width, height, pixels);
      return pixels;
    } finally {
      target.dispose();
    }
  }

  dispose(): void {
    for (const layer of [...this.layers.keys()]) this.releaseLayer(layer);
    this.output?.dispose();
    this.budget.release(`${this.name}composite`);
    this.releaseSpare();
    this.releaseBlur();
    for (const pass of [this.normal, this.blended, this.blurFill, this.blur, this.copy])
      pass.dispose();
  }
}

/**
 * A small cache of recently composited exact frames, for scrubbing back and forth. Entries are
 * 8-bit at half the working size: they stand in while dragging, and a resting playhead always
 * gets a full exact render.
 */
export class FrameCache {
  private readonly entries = new Map<number, THREE.WebGLRenderTarget>();
  private readonly order = new LruKeys<number>(FRAME_CACHE_ENTRIES);
  private readonly copy = new Pass(COPY, { tImage: { value: null } });
  private size: Size = { width: 2, height: 2 };
  hits = 0;

  constructor(
    private readonly renderer: THREE.WebGLRenderer,
    private readonly budget: MemoryBudget,
  ) {}

  /** Sets the working size; drops every entry. */
  resize(working: Size): void {
    this.clear();
    this.size = {
      width: Math.max(2, Math.round(working.width / 2)),
      height: Math.max(2, Math.round(working.height / 2)),
    };
  }

  get(key: number): THREE.Texture | null {
    const entry = this.entries.get(key);
    if (!entry) return null;
    this.order.touch(key);
    this.hits++;
    return entry.texture;
  }

  /** Stores a copy of `image` under `key`. Does nothing past the memory ceiling. */
  store(key: number, image: THREE.Texture): void {
    let target = this.entries.get(key) ?? null;
    if (!target) {
      const bytes = this.size.width * this.size.height * 4;
      if (!this.budget.reserve(`cache ${key}`, bytes)) return;
      target = makeTarget(this.size.width, this.size.height, true);
      this.entries.set(key, target);
      const evicted = this.order.add(key);
      if (evicted !== null) this.drop(evicted);
    } else {
      this.order.touch(key);
    }
    (this.copy.uniforms["tImage"] as THREE.IUniform).value = image;
    this.copy.run(this.renderer, target);
  }

  clear(): void {
    for (const key of this.order.clear()) this.drop(key);
  }

  private drop(key: number): void {
    this.entries.get(key)?.dispose();
    this.entries.delete(key);
    this.budget.release(`cache ${key}`);
  }
}
