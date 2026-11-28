import { describe, expect, it } from "vitest";
import { blurPlan, hexColor } from "./blend";
import {
  blendReference,
  layerCoordinate,
  referenceComposite,
  sampleBilinear,
  testPattern,
} from "./reference";
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

  it("crops each edge as a fraction of the picture, inside its box", () => {
    const t = toTransform({ cropLeft: 0.25, cropBottom: 0.5 });
    expect(layerCoordinate(t, 100, 100, 20, 10)).toBeNull(); // left quarter gone
    expect(layerCoordinate(t, 100, 100, 30, 10)).not.toBeNull();
    expect(layerCoordinate(t, 100, 100, 30, 60)).toBeNull(); // bottom half gone
    // A letterboxed picture covering the middle half of the rows: a quarter of the picture is
    // an eighth of the frame.
    const top = toTransform({ cropTop: 0.25 });
    expect(layerCoordinate(top, 100, 100, 50, 30, [1, 0.5])).toBeNull();
    expect(layerCoordinate(top, 100, 100, 50, 40, [1, 0.5])).not.toBeNull();
    expect(layerCoordinate(toTransform({}), 100, 100, 50, 20, [1, 0.5])).toBeNull();
  });

  it("blends by the W3C formulas", () => {
    const b: [number, number, number] = [0.2, 0.5, 0.8];
    const s: [number, number, number] = [0.6, 0.6, 0.1];
    const close = (actual: number[], expected: number[]) =>
      actual.forEach((v, i) => expect(v).toBeCloseTo(expected[i] ?? NaN, 6));
    close(blendReference("normal", b, s), s);
    close(blendReference("multiply", b, s), [0.12, 0.3, 0.08]);
    close(blendReference("screen", b, s), [0.68, 0.8, 0.82]);
    close(blendReference("overlay", b, s), [0.24, 0.6, 0.64]);
    close(blendReference("add", b, s), [0.8, 1, 0.9]);
    close(blendReference("difference", b, s), [0.4, 0.1, 0.7]);
    close(blendReference("colorDodge", b, s), [0.5, 1, 0.8 / 0.9]);
    close(blendReference("colorBurn", b, s), [0, 1 - 0.5 / 0.6, 0]);
    // Luminosity keeps the backdrop's hue and takes the source's lightness.
    const lum = (c: number[]) => 0.3 * (c[0] ?? 0) + 0.59 * (c[1] ?? 0) + 0.11 * (c[2] ?? 0);
    expect(lum(blendReference("luminosity", b, s))).toBeCloseTo(lum(s), 6);
    expect(lum(blendReference("color", b, s))).toBeCloseTo(lum(b), 6);
  });

  it("fills with the background colour, and blurs the bottom picture over the bars", () => {
    const empty = referenceComposite(4, 2, [], { kind: "color", color: "#ff8000", blur: 0.5 });
    expect([...empty.data.slice(0, 4)]).toEqual([1, 128 / 255, 0, 1].map((v) => Math.fround(v)));
    expect(hexColor("nope")).toEqual([0, 0, 0]);
    // A picture in the middle half of the rows, blurred to fill: the bars are no longer black.
    const image = testPattern(64, 36);
    const transparent = {
      ...image,
      data: image.data.map((v, i) => (Math.floor(i / 256) % 36 < 9 ? 0 : v)),
    };
    const out = referenceComposite(
      64,
      36,
      [{ image: transparent, transform: toTransform({}), box: [1, 0.5] }],
      {
        kind: "blur",
        color: "#000000",
        blur: 1,
      },
    );
    expect(out.data[0]).toBeGreaterThan(0.1);
    expect(out.data[3]).toBe(1);
    const plan = blurPlan(1920, 1080, 1);
    expect([plan.width, plan.height]).toEqual([240, 135]);
    const total = plan.weights.reduce((sum, w, i) => sum + (i === 0 ? w : 2 * w), 0);
    expect(total).toBeCloseTo(1, 9);
  });
});
