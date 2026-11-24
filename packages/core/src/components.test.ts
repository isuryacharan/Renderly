import { describe, expect, it } from "vitest";
import { z } from "zod";
import { componentTypes, registerComponent } from "./components";
import { migrate } from "./migrate";
import { createId } from "./ids";

describe("component registry", () => {
  it("lets a later slice add a component type without touching the clip type", () => {
    registerComponent("testTint", { amount: z.number().min(0).max(1).default(0.5) });
    expect(componentTypes()).toContain("testTint");
    const media = createId();
    const result = migrate({
      format: "renderly",
      version: 1,
      id: createId(),
      name: "Tinted",
      revision: 0,
      media: [
        {
          id: media,
          name: "a.mp4",
          link: "handle",
          path: "a.mp4",
          fingerprint: { size: 1, modified: 1, head: "x" },
          duration: 705_600_000,
        },
      ],
      sequences: [
        {
          id: createId(),
          name: "Main",
          width: 1920,
          height: 1080,
          fps: [30, 1],
          tracks: [
            {
              id: createId(),
              kind: "video",
              clips: [
                {
                  id: createId(),
                  media,
                  start: 0,
                  duration: 23_520_000,
                  in: 0,
                  components: [{ type: "testTint" }],
                },
              ],
            },
          ],
        },
      ],
    });
    if (!result.ok) throw new Error(JSON.stringify(result));
    expect(result.project.sequences[0]?.tracks[0]?.clips[0]?.components[0]).toEqual({
      type: "testTint",
      amount: 0.5,
    });
  });

  it("refuses to register a type twice or reuse reserved fields", () => {
    expect(() => registerComponent("transform", {})).toThrow();
    expect(() => registerComponent("other", { keys: z.string() })).toThrow();
  });
});
