import { describe, expect, it } from "vitest";
import "./basic-commands";
import "./keyframe-commands";
import "./media-commands";
import "./timeline-commands";
import { createId } from "./ids";
import { valueAt, type Key } from "./keyframes";
import { newProject } from "./new-project";
import { ProjectStore } from "./store";

const SEC = 705_600_000;

/** A store with two 4-second clips at 0 and 10 s on a video track. */
function setup() {
  const store = new ProjectStore(newProject("Keys"));
  const sequence = store.project.sequences[0];
  if (!sequence) throw new Error("no sequence");
  const media = createId();
  const track = createId();
  const clips = [createId(), createId()];
  const run = (command: { type: string; payload: unknown }) => {
    const result = store.dispatch(command);
    if (!result.ok) throw new Error(JSON.stringify(result.error));
    return result;
  };
  run({
    type: "media.import",
    payload: {
      items: [
        {
          id: media,
          name: "a.mp4",
          link: "copy",
          path: "media/a.mp4",
          fingerprint: { size: 1, modified: 0, head: "x" },
          duration: 60 * SEC,
          video: { width: 1920, height: 1080, fps: [30, 1], codec: "avc" },
        },
      ],
    },
  });
  run({ type: "track.add", payload: { sequence: sequence.id, id: track, kind: "video" } });
  run({
    type: "clip.add",
    payload: {
      track,
      clips: clips.map((id, i) => ({ id, media, start: i * 10 * SEC, duration: 4 * SEC })),
    },
  });
  const transform = (id: string) =>
    store.project.sequences[0]?.tracks[0]?.clips
      .find((c) => c.id === id)
      ?.components.find((c) => (c as { type: string }).type === "transform") as
      (Record<string, unknown> & { keys?: Record<string, Key[]> }) | undefined;
  return { store, clips, run, transform };
}

describe("keyframe.add", () => {
  it("keys the current value, several fields and clips in one step", () => {
    const { store, clips, run, transform } = setup();
    run({
      type: "component.setMany",
      payload: { clips, component: "transform", field: "rotation", value: 30 },
    });
    const steps = store.undoLabels.length;
    // A group key at 1 s into the first clip (and 1 s before the second: outside it).
    expect(
      store.dispatch({
        type: "keyframe.add",
        payload: { clips, component: "transform", fields: ["rotation"], time: 11 * SEC },
      }).ok,
    ).toBe(false); // the time is outside the first clip
    run({
      type: "keyframe.add",
      payload: {
        clips: [clips[0]],
        component: "transform",
        fields: ["rotation", "position"],
        time: SEC,
      },
    });
    expect(store.undoLabels.length).toBe(steps + 1);
    const keys = transform(clips[0] ?? "")?.keys;
    expect(keys?.["rotation"]).toEqual([{ t: SEC, v: 30, ease: "auto" }]);
    expect(keys?.["position"]).toEqual([{ t: SEC, v: [0, 0], ease: "auto" }]);
  });

  it("adding a key between two keeps the motion", () => {
    const { clips, run, transform } = setup();
    const id = clips[0] ?? "";
    run({
      type: "keyframe.add",
      payload: { clips: [id], component: "transform", fields: ["rotation"], time: 0 },
    });
    run({
      type: "keyframe.add",
      payload: { clips: [id], component: "transform", fields: ["rotation"], time: 4 * SEC },
    });
    run({
      type: "component.setMany",
      payload: { clips: [id], component: "transform", field: "rotation", value: 90, time: 4 * SEC },
    });
    const before = transform(id)?.keys?.["rotation"] ?? [];
    expect(before.map((k) => k.v)).toEqual([0, 90]);
    run({
      type: "keyframe.add",
      payload: { clips: [id], component: "transform", fields: ["rotation"], time: 1.3 * SEC },
    });
    const after = transform(id)?.keys?.["rotation"] ?? [];
    expect(after).toHaveLength(3);
    for (let t = 0; t <= 4 * SEC; t += SEC / 7)
      expect(valueAt(after, t)).toBeCloseTo(valueAt(before, t) as number, 6);
  });
});

describe("component.setMany with a time", () => {
  it("edits the key at that time on an animated field, and the value on a plain one", () => {
    const { clips, run, transform } = setup();
    const id = clips[0] ?? "";
    run({
      type: "keyframe.add",
      payload: { clips: [id], component: "transform", fields: ["position"], time: 0 },
    });
    run({
      type: "component.setMany",
      payload: {
        clips: [id],
        component: "transform",
        field: "position",
        index: 0,
        value: 0.25,
        time: 2 * SEC,
      },
    });
    const keys = transform(id)?.keys?.["position"] ?? [];
    expect(keys.map((k) => [k.t, k.v])).toEqual([
      [0, [0, 0]],
      [2 * SEC, [0.25, 0]],
    ]);
    run({
      type: "component.setMany",
      payload: { clips: [id], component: "transform", field: "opacity", value: 0.5, time: 2 * SEC },
    });
    expect(transform(id)?.["opacity"]).toBe(0.5);
    expect(transform(id)?.keys?.["opacity"]).toBeUndefined();
  });
});

describe("keyframe.remove / move / ease", () => {
  function animated() {
    const s = setup();
    const id = s.clips[0] ?? "";
    for (const [time, value] of [
      [0, 0],
      [2 * SEC, 1],
    ] as const) {
      s.run({
        type: "keyframe.add",
        payload: { clips: [id], component: "transform", fields: ["opacity"], time },
      });
      s.run({
        type: "component.setMany",
        payload: { clips: [id], component: "transform", field: "opacity", value, time },
      });
    }
    return { ...s, id };
  }

  it("removing the last key leaves the property at that value", () => {
    const { id, run, store, transform } = animated();
    run({
      type: "keyframe.remove",
      payload: { clips: [id], component: "transform", fields: ["opacity"], time: 0 },
    });
    run({
      type: "keyframe.remove",
      payload: { clips: [id], component: "transform", fields: ["opacity"], time: 2 * SEC },
    });
    expect(transform(id)?.keys).toBeUndefined();
    expect(transform(id)?.["opacity"]).toBe(1);
    expect(
      store.dispatch({
        type: "keyframe.remove",
        payload: { clips: [id], component: "transform", fields: ["opacity"], time: SEC },
      }),
    ).toMatchObject({ ok: false, error: { code: "not-found" } });
  });

  it("moves a key within the clip, but not onto another key or out of the clip", () => {
    const { id, run, store, transform } = animated();
    run({
      type: "keyframe.move",
      payload: {
        clip: id,
        component: "transform",
        fields: ["opacity"],
        from: 2 * SEC,
        to: 3 * SEC,
      },
    });
    expect(transform(id)?.keys?.["opacity"]?.map((k) => k.t)).toEqual([0, 3 * SEC]);
    const move = (to: number) =>
      store.dispatch({
        type: "keyframe.move",
        payload: { clip: id, component: "transform", fields: ["opacity"], from: 3 * SEC, to },
      });
    expect(move(0)).toMatchObject({ ok: false, error: { code: "rejected" } });
    expect(move(5 * SEC)).toMatchObject({ ok: false, error: { code: "rejected" } });
  });

  it("applies an ease preset to the key at a time", () => {
    const { id, run, transform } = animated();
    run({
      type: "keyframe.ease",
      payload: {
        clips: [id],
        component: "transform",
        fields: ["opacity"],
        time: 0,
        preset: "hold",
      },
    });
    const keys = transform(id)?.keys?.["opacity"] ?? [];
    expect(keys[0]?.ease).toBe("hold");
    expect(valueAt(keys, SEC)).toBe(0);
  });
});

describe("trims keep the motion", () => {
  it("trimming an animated clip's start leaves what is still visible unchanged", () => {
    const { clips, run, store } = setup();
    const id = clips[0] ?? "";
    for (const [time, value] of [
      [0, [0, 0]],
      [SEC, [0.3, -0.2]],
      [3 * SEC, [-0.1, 0.25]],
    ] as const) {
      run({
        type: "keyframe.add",
        payload: { clips: [id], component: "transform", fields: ["position"], time },
      });
      run({
        type: "component.setMany",
        payload: {
          clips: [id],
          component: "transform",
          field: "position",
          value: [...value],
          time,
        },
      });
    }
    const clip = () => store.project.sequences[0]?.tracks[0]?.clips.find((c) => c.id === id);
    const at = (timeline: number) => {
      const c = clip();
      const keys = (c?.components[0] as { keys: { position: Key[] } }).keys.position;
      return valueAt(keys, timeline - (c?.start ?? 0)) as number[];
    };
    const before = Array.from({ length: 30 }, (_, i) => at(1.3 * SEC + (i * 2.7 * SEC) / 29));
    run({ type: "clip.trim", payload: { clips: [id], edge: "start", delta: 1.3 * SEC } });
    expect(clip()?.start).toBe(1.3 * SEC);
    const after = Array.from({ length: 30 }, (_, i) => at(1.3 * SEC + (i * 2.7 * SEC) / 29));
    after.forEach((v, i) => {
      expect(v[0]).toBeCloseTo(before[i]?.[0] ?? NaN, 6);
      expect(v[1]).toBeCloseTo(before[i]?.[1] ?? NaN, 6);
    });
  });
});
