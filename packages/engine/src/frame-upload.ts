// Decoded frame → working-size texture, on the GPU. Ported from the effects POC.
//
// The decoder's frame (3840×2160 for 4K) is uploaded once into a texture (the browser imports it
// directly), a mipmap chain is built, and one pass samples it trilinearly into the working target
// while applying the clip rotation and the contain fit. Scaling large frames through a 2D canvas
// instead was measured in the POC to load the GPU far more for the same quality.

import * as THREE from "three";
import { Pass } from "./gpu";
import { containUvScale, rotatedSize } from "./layout";

const FRAGMENT = /* glsl */ `
uniform sampler2D tFrame;
uniform int uRotation;   // clockwise quarter turns
uniform vec2 uScale;     // contain fit, about the centre
in vec2 vUv;
out vec4 fragColor;

void main() {
  // Output pixel → upright display coordinates (x right, y down), then the contain fit.
  vec2 d = 0.5 + (vec2(vUv.x, 1.0 - vUv.y) - 0.5) * uScale;
  if (any(lessThan(d, vec2(0.0))) || any(greaterThan(d, vec2(1.0)))) {
    fragColor = vec4(0.0, 0.0, 0.0, 1.0);
    return;
  }
  // Display coordinates → coded-frame coordinates (undo the clockwise rotation).
  vec2 r = d;
  if (uRotation == 1) r = vec2(d.y, 1.0 - d.x);
  else if (uRotation == 2) r = vec2(1.0 - d.x, 1.0 - d.y);
  else if (uRotation == 3) r = vec2(1.0 - d.y, d.x);
  fragColor = vec4(texture(tFrame, vec2(r.x, 1.0 - r.y)).rgb, 1.0);
}`;

export class FrameUploader {
  private readonly texture = new THREE.VideoFrameTexture();
  private readonly pass: Pass;

  constructor(private readonly renderer: THREE.WebGLRenderer) {
    const t = this.texture;
    t.colorSpace = THREE.NoColorSpace;
    t.minFilter = THREE.LinearMipmapLinearFilter;
    t.magFilter = THREE.LinearFilter;
    t.generateMipmaps = true;
    this.pass = new Pass(FRAGMENT, {
      tFrame: { value: t },
      uRotation: { value: 0 },
      uScale: { value: new THREE.Vector2(1, 1) },
    });
  }

  /**
   * Draws `frame` into `target`, upright and contain-fitted. The caller closes the frame right
   * after: the pixels are on the GPU once this returns.
   */
  upload(frame: VideoFrame, rotation: number, target: THREE.WebGLRenderTarget): void {
    const display = rotatedSize(
      { width: frame.displayWidth, height: frame.displayHeight },
      rotation,
    );
    const [sx, sy] = containUvScale(display, { width: target.width, height: target.height });
    const u = this.pass.uniforms;
    (u["uScale"]?.value as THREE.Vector2).set(sx, sy);
    (u["uRotation"] as THREE.IUniform).value = Math.round(rotation / 90) % 4;
    this.texture.setFrame(frame);
    this.pass.run(this.renderer, target);
  }

  dispose(): void {
    this.texture.dispose();
    this.pass.dispose();
  }
}
