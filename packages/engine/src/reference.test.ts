import { describe, expect, it } from "vitest";
import { layerCoordinate, referenceComposite, sampleBilinear, testPattern } from "./reference";
import { IDENTITY, toTransform } from "./timeline";

describe("the golden-frame reference", () => {
  it("samples a texel centre exactly and blends between centres", () => {
    const image = { width: 2, height: 1, data: new Float32Array([0, 0, 0, 1, 1, 1, 1, 1]) };
    expect(sampleBilinear(image, 0.25, 0.5)[0]).toBeCloseTo(0);
    expect(sampleBilinear(image, 0.5, 0.5)[0]).toBeCloseTo(0.5);
    expect(sampleBilinear(image, 1, 0.5)[0]).toBeCloseTo(1); // clamped at the edge
  });

  it("reproduces a layer unchanged at the identity", () => {
    const image = testPattern(32, 18);
    const out = referenceComposite(32, 18, [{ image, transform: IDENTITY }]);
    for (let i = 0; i < out.data.length; i++)
      expect(out.data[i]).toBeCloseTo(image.data[i] ?? 0, 6);
  });

  it("places the anchor at the position, and turns clockwise about it", () => {
    // Anchor at the layer's right edge, placed at the frame centre: the layer lies left of centre.
    const t = toTransform({ anchorX: 0.5 });
    expect(layerCoordinate(t, 100, 100, 49, 50)).not.toBeNull();
    expect(layerCoordinate(t, 100, 100, 51, 50)).toBeNull();
    // A quarter turn clockwise (square frame): what was left of centre is now above it.
    const turned = toTransform({ anchorX: 0.5, rotation: 90 });
    expect(layerCoordinate(turned, 100, 100, 50, 49)).not.toBeNull();
    expect(layerCoordinate(turned, 100, 100, 50, 51)).toBeNull();
  });

  it("mirrors with flip and halves with scale", () => {
    const flipped = toTransform({ flipX: true });
    const [u] = layerCoordinate(flipped, 100, 100, 10, 50) ?? [0];
    expect(u).toBeCloseTo(1 - 10.5 / 100);
    const half = toTransform({ scaleX: 0.5, scaleY: 0.5 });
    expect(layerCoordinate(half, 100, 100, 10, 50)).toBeNull();
    expect(layerCoordinate(half, 100, 100, 30, 50)).not.toBeNull();
  });

  it("puts a half-opaque layer over another with premultiplied over", () => {
    const red = { width: 1, height: 1, data: new Float32Array([1, 0, 0, 1]) };
    const blue = { width: 1, height: 1, data: new Float32Array([0, 0, 1, 1]) };
    const out = referenceComposite(1, 1, [
      { image: red, transform: IDENTITY },
      { image: blue, transform: toTransform({ opacity: 0.25 }) },
    ]);
    expect([...out.data]).toEqual([0.75, 0, 0.25, 1]);
  });
});
