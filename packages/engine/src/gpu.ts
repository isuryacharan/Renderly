// Small helpers for full-screen shader passes. Colour is kept in display space: textures and
// targets use NoColorSpace so three.js never converts anything behind our back. Working targets
// are half-float so later effects can hold values above 1.

import * as THREE from "three";

const VERTEX = /* glsl */ `
in vec3 position;
out vec2 vUv;
void main() {
  vUv = position.xy * 0.5 + 0.5;
  gl_Position = vec4(position.xy, 0.0, 1.0);
}`;

/** One triangle that covers the whole viewport. */
const fullScreen = new THREE.BufferGeometry();
fullScreen.setAttribute(
  "position",
  new THREE.Float32BufferAttribute([-1, -1, 0, 3, -1, 0, -1, 3, 0], 3),
);
const camera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1);

/** A fragment shader (GLSL ES 3.00) drawn over a whole target. */
export class Pass {
  readonly material: THREE.RawShaderMaterial;
  private readonly scene = new THREE.Scene();

  constructor(fragment: string, uniforms: Record<string, THREE.IUniform>) {
    this.material = new THREE.RawShaderMaterial({
      glslVersion: THREE.GLSL3,
      vertexShader: `precision highp float;\n${VERTEX}`,
      fragmentShader: `precision highp float;\nprecision highp int;\n${fragment}`,
      uniforms,
      depthTest: false,
      depthWrite: false,
      blending: THREE.NoBlending,
    });
    const mesh = new THREE.Mesh(fullScreen, this.material);
    mesh.frustumCulled = false;
    this.scene.add(mesh);
  }

  get uniforms(): Record<string, THREE.IUniform> {
    return this.material.uniforms;
  }

  run(renderer: THREE.WebGLRenderer, target: THREE.WebGLRenderTarget | null): void {
    renderer.setRenderTarget(target);
    renderer.render(this.scene, camera);
  }

  dispose(): void {
    this.material.dispose();
  }
}

export function makeTarget(width: number, height: number): THREE.WebGLRenderTarget {
  const target = new THREE.WebGLRenderTarget(width, height, {
    type: THREE.HalfFloatType,
    minFilter: THREE.LinearFilter,
    magFilter: THREE.LinearFilter,
    depthBuffer: false,
    generateMipmaps: false,
  });
  target.texture.colorSpace = THREE.NoColorSpace;
  return target;
}
