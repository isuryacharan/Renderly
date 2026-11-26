// Layers → one picture. Each layer keeps its own target holding its last uploaded frame, so a
// layer that updates less often (keyframe-only under decoder pressure) still shows its last
// picture. Compositing draws the layers bottom to top with premultiplied alpha, each with its
// static transform and opacity, into the composite target.
//
// Every target is reserved in the MemoryBudget first; past the ceiling a layer gets no target
// and is not drawn.

import * as THREE from "three";
import { makeTarget, Pass } from "./gpu";
import type { Size } from "./layout";
import { halfFloatTargetBytes, type MemoryBudget } from "./memory";
import { LruKeys } from "./scrub";
import type { Transform } from "./timeline";

/** Composited frames kept for scrubbing. */
export const FRAME_CACHE_ENTRIES = 12;

/**
 * Draws one layer with its transform. Each output pixel is mapped back to the layer: undo the
 * position, then the rotation (in pixels, so a non-square frame does not shear), then the scale,
 * add the anchor, mirror. The same mapping, written separately on the CPU, is the golden-frame
 * reference (reference.ts).
 */
const FRAGMENT = /* glsl */ `
uniform sampler2D tLayer;
uniform vec2 uSize;      // composite size in pixels
uniform vec2 uOffset;    // where the anchor sits, as a fraction of the frame from its centre (y down)
uniform vec2 uScale;     // per axis
uniform float uRotation; // radians, clockwise on screen
uniform vec2 uAnchor;    // fraction of the layer from its centre
uniform vec2 uFlip;      // 1 or -1 per axis
uniform float uOpacity;
in vec2 vUv;
out vec4 fragColor;

void main() {
  vec2 d = vec2(vUv.x, 1.0 - vUv.y);                  // frame coordinates, y down
  vec2 q = (d - 0.5 - uOffset) * uSize;               // pixels from the anchor's place
  float c = cos(uRotation);
  float s = sin(uRotation);
  q = vec2(c * q.x + s * q.y, -s * q.x + c * q.y);     // undo a clockwise rotation (y down)
  vec2 l = (q / (uScale * uSize) + uAnchor) * uFlip;  // layer, from its centre
  vec2 p = l + 0.5;
  if (any(lessThan(p, vec2(0.0))) || any(greaterThan(p, vec2(1.0)))) discard;
  fragColor = texture(tLayer, vec2(p.x, 1.0 - p.y)) * uOpacity; // premultiplied
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

export class Compositor {
  private size: Size = { width: 2, height: 2 };
  private output: THREE.WebGLRenderTarget | null = null;
  private readonly layers = new Map<number, THREE.WebGLRenderTarget>();
  private readonly pass = new Pass(
    FRAGMENT,
    {
      tLayer: { value: null },
      uSize: { value: new THREE.Vector2(2, 2) },
      uOffset: { value: new THREE.Vector2() },
      uScale: { value: new THREE.Vector2(1, 1) },
      uRotation: { value: 0 },
      uAnchor: { value: new THREE.Vector2() },
      uFlip: { value: new THREE.Vector2(1, 1) },
      uOpacity: { value: 1 },
    },
    { premultiplied: true },
  );
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
    if (
      this.budget.reserve(`${this.name}composite`, halfFloatTargetBytes(size.width, size.height))
    ) {
      this.output = makeTarget(size.width, size.height);
    } else {
      this.refused++;
    }
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

  releaseLayer(layer: number): void {
    this.layers.get(layer)?.dispose();
    this.layers.delete(layer);
    this.budget.release(`${this.name}layer ${layer}`);
  }

  /** Draws `draws` (bottom first) over opaque black into the composite target. */
  composite(draws: readonly LayerDraw[]): void {
    if (!this.output) return;
    const r = this.renderer;
    r.setRenderTarget(this.output);
    r.setClearColor(0x000000, 1);
    r.clear(true, false, false);
    const u = this.pass.uniforms;
    for (const { layer, transform } of draws) {
      const target = this.layers.get(layer);
      if (!target || transform.opacity <= 0) continue;
      (u["tLayer"] as THREE.IUniform).value = target.texture;
      (u["uSize"]?.value as THREE.Vector2).set(this.size.width, this.size.height);
      (u["uOffset"]?.value as THREE.Vector2).set(transform.x, transform.y);
      (u["uScale"]?.value as THREE.Vector2).set(transform.scaleX, transform.scaleY);
      (u["uRotation"] as THREE.IUniform).value = (transform.rotation * Math.PI) / 180;
      (u["uAnchor"]?.value as THREE.Vector2).set(transform.anchorX, transform.anchorY);
      (u["uFlip"]?.value as THREE.Vector2).set(transform.flipX ? -1 : 1, transform.flipY ? -1 : 1);
      (u["uOpacity"] as THREE.IUniform).value = transform.opacity;
      this.pass.run(r, this.output);
    }
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
    this.pass.dispose();
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
