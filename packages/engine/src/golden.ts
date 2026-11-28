// Golden frames on the GPU: test pictures loaded straight into the real compositor's layer targets
// (no video decoding), composited with the real shader, read back as 8-bit RGBA (rows bottom
// first). `pnpm test:golden` compares the result with the CPU reference (reference.ts).

import type { BackgroundSettings } from "@renderly/core";
import * as THREE from "three";
import { Compositor } from "./compositor";
import { Pass } from "./gpu";
import { MemoryBudget } from "./memory";
import type { ReferenceLayer } from "./reference";

const COPY = /* glsl */ `
uniform sampler2D tImage;
in vec2 vUv;
out vec4 fragColor;
void main() {
  fragColor = texture(tImage, vUv);
}`;

/** Renders `layers` (bottom first) over `background` at `width` × `height` with the compositor. */
export function renderGolden(
  canvas: HTMLCanvasElement | OffscreenCanvas,
  width: number,
  height: number,
  layers: readonly ReferenceLayer[],
  background?: BackgroundSettings,
): Uint8Array {
  const renderer = new THREE.WebGLRenderer({ canvas, antialias: false, alpha: false });
  renderer.setPixelRatio(1);
  renderer.autoClear = false;
  const compositor = new Compositor(renderer, new MemoryBudget(Infinity));
  const copy = new Pass(COPY, { tImage: { value: null } });
  try {
    compositor.resize({ width, height });
    if (background) compositor.setBackground(background);
    layers.forEach(({ image, box }, layer) => {
      if (box) compositor.setPictureBox(layer, box);
      const target = compositor.layerTarget(layer);
      if (!target) throw new Error("No layer target");
      // GL rows go bottom first: flip the picture's rows on the way in.
      const rows = new Float32Array(image.data.length);
      const stride = image.width * 4;
      for (let y = 0; y < image.height; y++)
        rows.set(
          image.data.subarray(y * stride, (y + 1) * stride),
          (image.height - 1 - y) * stride,
        );
      const texture = new THREE.DataTexture(
        rows,
        image.width,
        image.height,
        THREE.RGBAFormat,
        THREE.FloatType,
      );
      texture.minFilter = THREE.NearestFilter;
      texture.magFilter = THREE.NearestFilter;
      texture.colorSpace = THREE.NoColorSpace;
      texture.needsUpdate = true;
      (copy.uniforms["tImage"] as THREE.IUniform).value = texture;
      copy.run(renderer, target);
      texture.dispose();
    });
    compositor.composite(layers.map(({ transform }, layer) => ({ layer, transform })));
    const pixels = compositor.readPixels();
    if (!pixels) throw new Error("Nothing composited");
    return pixels;
  } finally {
    copy.dispose();
    compositor.dispose();
    renderer.dispose();
  }
}
