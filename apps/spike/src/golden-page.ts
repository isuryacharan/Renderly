// Golden-frame cases (`pnpm test:golden`): each renders test pictures through the compositor on
// this machine's GPU and compares them with the CPU reference. New cases go in CASES; later
// slices add blend modes, colour and masks.

import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./spike.css";
import {
  compare,
  referenceComposite,
  renderGolden,
  testPattern,
  toTransform,
  type Image,
  type Transform,
} from "@renderly/engine";

const WIDTH = 320;
const HEIGHT = 180;

interface Case {
  /** Layers bottom first. */
  layers: { image: Image; transform: Transform }[];
}

const base = testPattern(WIDTH, HEIGHT);
const overlay = testPattern(WIDTH, HEIGHT, true);

const CASES: Record<string, Case> = {
  "transform: identity": { layers: [{ image: base, transform: toTransform({}) }] },
  "transform: moved, scaled per axis, anchored": {
    layers: [
      {
        image: base,
        transform: toTransform({
          x: 0.15,
          y: -0.1,
          scaleX: 0.6,
          scaleY: 0.45,
          anchorX: 0.2,
          anchorY: -0.15,
        }),
      },
    ],
  },
  "transform: rotated, flipped, 70% over a base layer": {
    layers: [
      { image: base, transform: toTransform({}) },
      {
        image: overlay,
        transform: toTransform({
          x: -0.1,
          y: 0.05,
          scaleX: 0.55,
          scaleY: 0.5,
          rotation: 30,
          flipX: true,
          opacity: 0.7,
        }),
      },
    ],
  },
};

/** 8-bit RGBA, rows top first, as a PNG data URL (for failure reports). */
function png(rgba: Uint8ClampedArray): string {
  const canvas = document.createElement("canvas");
  canvas.width = WIDTH;
  canvas.height = HEIGHT;
  canvas
    .getContext("2d")
    ?.putImageData(new ImageData(rgba as Uint8ClampedArray<ArrayBuffer>, WIDTH, HEIGHT), 0, 0);
  return canvas.toDataURL("image/png");
}

function run(name: string, tolerance: number, images = false) {
  const test = CASES[name];
  if (!test) throw new Error(`No case ${name}`);
  const gpu = renderGolden(document.createElement("canvas"), WIDTH, HEIGHT, test.layers);
  const reference = referenceComposite(WIDTH, HEIGHT, test.layers);
  const difference = compare(gpu, reference, tolerance);
  const result: Record<string, unknown> = { name, ...difference };
  if (images) {
    const top = new Uint8ClampedArray(WIDTH * HEIGHT * 4);
    const ref = new Uint8ClampedArray(WIDTH * HEIGHT * 4);
    const diff = new Uint8ClampedArray(WIDTH * HEIGHT * 4);
    for (let y = 0; y < HEIGHT; y++)
      for (let x = 0; x < WIDTH; x++) {
        const g = ((HEIGHT - 1 - y) * WIDTH + x) * 4;
        const o = (y * WIDTH + x) * 4;
        let worst = 0;
        for (let c = 0; c < 4; c++) {
          top[o + c] = gpu[g + c] ?? 0;
          ref[o + c] = Math.round(Math.min(1, Math.max(0, reference.data[o + c] ?? 0)) * 255);
          worst = Math.max(worst, Math.abs((top[o + c] ?? 0) - (ref[o + c] ?? 0)));
        }
        diff.set([Math.min(255, worst * 16), 0, 0, 255], o);
      }
    Object.assign(result, { gpu: png(top), reference: png(ref), difference: png(diff) });
  }
  const item = document.createElement("li");
  item.textContent = `${name}: max ${difference.max}, outliers ${(difference.outliers * 100).toFixed(2)}%`;
  document.getElementById("results")?.append(item);
  return result;
}

Object.assign(window, {
  golden: { cases: () => Object.keys(CASES), run },
  pageReadyAt: performance.now(),
});
