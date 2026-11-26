import { IDENTITY_TRANSFORM, type SequenceGraph } from "@renderly/core";
import { describe, expect, it } from "vitest";
import { cutsFromGraph } from "./project";

const T = IDENTITY_TRANSFORM;

const graph: SequenceGraph = {
  sequence: "s",
  width: 1920,
  height: 1080,
  fps: [30, 1],
  duration: 300,
  skipped: [],
  layers: [
    {
      track: "v1",
      muted: true,
      clips: [
        { clip: "a", media: "m1", in: 10, out: 110, start: 0, transform: T },
        { clip: "b", media: "m2", in: 0, out: 50, start: 100, transform: T },
      ],
    },
    {
      track: "v2",
      muted: false,
      clips: [
        {
          clip: "c",
          media: "m1",
          in: 0,
          out: 20,
          start: 30,
          transform: { ...T, scaleX: 0.5, scaleY: 0.5 },
        },
      ],
    },
  ],
};

describe("cutsFromGraph", () => {
  it("makes one cut per clip with its layer, transform and the layer's mute", () => {
    const index = { m1: 0, m2: 1 } as Record<string, number>;
    expect(cutsFromGraph(graph, (m) => index[m] ?? null)).toEqual([
      { source: 0, in: 10, out: 110, start: 0, layer: 0, transform: T, muted: true },
      { source: 1, in: 0, out: 50, start: 100, layer: 0, transform: T, muted: true },
      {
        source: 0,
        in: 0,
        out: 20,
        start: 30,
        layer: 1,
        transform: { ...T, scaleX: 0.5, scaleY: 0.5 },
        muted: false,
      },
    ]);
  });

  it("leaves out clips whose media has no source", () => {
    const cuts = cutsFromGraph(graph, (m) => (m === "m1" ? 0 : null));
    expect(cuts.map((c) => c.start)).toEqual([0, 30]);
  });
});
