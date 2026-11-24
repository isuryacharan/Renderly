// Working target → canvas, letterboxed, scaled with linear filtering.

import * as THREE from "three";
import { Pass } from "./gpu";
import type { Rect } from "./layout";

const FRAGMENT = /* glsl */ `
uniform sampler2D tImage;
in vec2 vUv;
out vec4 fragColor;
void main() {
  fragColor = vec4(texture(tImage, vUv).rgb, 1.0);
}`;

export class Presenter {
  private readonly pass = new Pass(FRAGMENT, { tImage: { value: null } });

  constructor(private readonly renderer: THREE.WebGLRenderer) {}

  present(image: THREE.Texture, rect: Rect): void {
    const r = this.renderer;
    (this.pass.uniforms["tImage"] as THREE.IUniform).value = image;
    r.setRenderTarget(null);
    r.setScissorTest(false);
    r.setClearColor(0x000000, 1);
    r.clear(true, false, false);
    r.setViewport(rect.x, rect.y, rect.width, rect.height);
    this.pass.run(r, null);
    const size = r.getDrawingBufferSize(new THREE.Vector2());
    r.setViewport(0, 0, size.x, size.y);
  }

  dispose(): void {
    this.pass.dispose();
  }
}
