// Golden-frame cases (`pnpm test:golden`): each renders test pictures through the compositor on
// this machine's GPU and compares them with the CPU reference. New cases go in CASES; later
// slices add colour and masks.

import "@renderly/ui/fonts.css";
import "@renderly/ui/styles.css";
import "./spike.css";
import { BLEND_MODES, type BackgroundSettings } from "@renderly/core";
import {
  compare,
  referenceComposite,
  renderGolden,
  runTransformAt,
  testPattern,
  toTransform,
  type Image,
  type ReferenceLayer,
} from "@renderly/engine";

const WIDTH = 320;
const HEIGHT = 180;

interface Case {
  /** Layers bottom first. */
  layers: ReferenceLayer[];
  background?: BackgroundSettings;
}

const base = testPattern(WIDTH, HEIGHT);
const overlay = testPattern(WIDTH, HEIGHT, true);

/**
 * A picture fitted into the frame with bars above and below, as the uploader leaves a wider clip:
 * the test pattern squeezed into the middle `share` of the rows, transparent outside.
 */
function letterboxed(share: number): Image {
  const picture = testPattern(WIDTH, Math.round(HEIGHT * share));
  const data = new Float32Array(WIDTH * HEIGHT * 4);
  const top = Math.round((HEIGHT - picture.height) / 2);
  data.set(picture.data, top * WIDTH * 4);
  return { width: WIDTH, height: HEIGHT, data };
}

/** A smaller, turned, part-transparent layer over the base, combined by `mode`. */
const blendCase = (mode: (typeof BLEND_MODES)[number]): Case => ({
  layers: [
    { image: base, transform: toTransform({}) },
    {
      image: overlay,
      transform: toTransform({
        x: 0.08,
        y: -0.05,
        scaleX: 0.8,
        scaleY: 0.75,
        rotation: 12,
        flipX: true,
        opacity: 0.9,
        blend: mode,
      }),
    },
  ],
});
const blendCases = Object.fromEntries(
  BLEND_MODES.filter((mode) => mode !== "normal").map((mode) => [
    `blend: ${mode}`,
    blendCase(mode),
  ]),
);

const S = 705_600_000;

/**
 * An animated clip as the engine plays it: position, scale and rotation keyed with eased and auto
 * spans, evaluated by the engine's own `runTransformAt` 1.3 s into a 3 s clip.
 */
const animated = runTransformAt(
  {
    source: 0,
    in: 0,
    out: 3 * S,
    start: 0,
    end: 3 * S,
    layer: 1,
    transform: toTransform({ opacity: 0.85 }),
    keys: {
      position: [
        { t: 0, v: [-0.3, 0.2], ease: "bezier" },
        { t: 2 * S, v: [0.2, -0.1], ease: "auto" },
        { t: 3 * S, v: [0.25, 0], ease: "auto" },
      ],
      scale: [
        { t: 0, v: [0.3, 0.3], ease: "linear" },
        { t: 3 * S, v: [0.7, 0.6], ease: "linear" },
      ],
      rotation: [
        { t: 0, v: -20, ease: "auto" },
        { t: S, v: 10, ease: "auto" },
        { t: 3 * S, v: 45, ease: "auto" },
      ],
    },
  },
  1.3 * S,
);

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
  "keyframes: position, scale and rotation eased, 1.3 s into the clip": {
    layers: [
      { image: base, transform: toTransform({}) },
      { image: overlay, transform: animated },
    ],
  },
  ...blendCases,
  "blend: two blended layers in a row": {
    layers: [
      { image: base, transform: toTransform({}) },
      { image: overlay, transform: toTransform({ scale: 0.7, blend: "screen" }) },
      {
        image: overlay,
        transform: toTransform({ x: 0.2, scale: 0.5, rotation: -20, blend: "difference" }),
      },
    ],
  },
  "crop: edges cut, then moved and turned": {
    layers: [
      { image: base, transform: toTransform({}) },
      {
        image: overlay,
        transform: toTransform({
          x: -0.1,
          scale: 0.7,
          rotation: 15,
          cropLeft: 0.1,
          cropTop: 0.2,
          cropRight: 0.25,
          cropBottom: 0.05,
        }),
      },
    ],
  },
  "crop: within a letterboxed picture": {
    layers: [
      {
        image: letterboxed(0.6),
        transform: toTransform({ cropTop: 0.25, cropLeft: 0.1 }),
        box: [1, 0.6],
      },
    ],
    background: { kind: "color", color: "#2a6f97", blur: 0.5 },
  },
  "background: colour behind a smaller clip": {
    layers: [{ image: base, transform: toTransform({ scale: 0.6, rotation: 8 }) }],
    background: { kind: "color", color: "#d4a017", blur: 0.5 },
  },
  "background: the bottom picture blurred to fill the frame": {
    layers: [
      { image: letterboxed(0.5), transform: toTransform({}), box: [1, 0.5] },
      { image: overlay, transform: toTransform({ x: 0.3, scale: 0.3 }) },
    ],
    background: { kind: "blur", color: "#000000", blur: 0.6 },
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
  const gpu = renderGolden(
    document.createElement("canvas"),
    WIDTH,
    HEIGHT,
    test.layers,
    test.background,
  );
  const reference = referenceComposite(WIDTH, HEIGHT, test.layers, test.background);
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
