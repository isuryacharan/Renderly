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
import type { Transform } from "./timeline";

const FRAGMENT = /* glsl */ `
uniform sampler2D tLayer;
uniform vec2 uOffset;   // layer centre offset, as a fraction of the frame (x right, y down)
uniform float uScale;
uniform float uOpacity;
in vec2 vUv;
out vec4 fragColor;

void main() {
  vec2 d = vec2(vUv.x, 1.0 - vUv.y);          // frame coordinates, y down
  vec2 p = (d - 0.5 - uOffset) / uScale + 0.5; // layer coordinates
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
      uOffset: { value: new THREE.Vector2() },
      uScale: { value: 1 },
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
    if (this.budget.reserve("composite", halfFloatTargetBytes(size.width, size.height))) {
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
    if (!this.budget.reserve(`layer ${layer}`, halfFloatTargetBytes(width, height))) {
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
    this.budget.release(`layer ${layer}`);
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
      (u["uOffset"]?.value as THREE.Vector2).set(transform.x, transform.y);
      (u["uScale"] as THREE.IUniform).value = transform.scale;
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
    this.budget.release("composite");
    this.pass.dispose();
  }
}
