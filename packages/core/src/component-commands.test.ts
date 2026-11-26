import { describe, expect, it } from "vitest";
import "./basic-commands";
import "./media-commands";
import "./timeline-commands";
import { componentDefaults } from "./components";
import { createId } from "./ids";
import { newProject } from "./new-project";
import { ProjectStore } from "./store";

const SEC = 705_600_000;

/** A store with three clips on a video track, none with a transform yet. */
function setup() {
  const store = new ProjectStore(newProject("Inspector"));
  const sequence = store.project.sequences[0];
  if (!sequence) throw new Error("no sequence");
  const media = createId();
  const track = createId();
  const clips = [createId(), createId(), createId()];
  const run = (command: { type: string; payload: unknown }) => {
    const result = store.dispatch(command);
    if (!result.ok) throw new Error(result.error.message);
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
      clips: clips.map((id, i) => ({ id, media, start: i * 2 * SEC, duration: SEC })),
    },
  });
  const transform = (id: string) =>
    store.project.sequences[0]?.tracks[0]?.clips
      .find((c) => c.id === id)
      ?.components.find((c) => (c as { type: string }).type === "transform") as
      Record<string, unknown> | undefined;
  return { store, clips, transform };
}

describe("component.setMany", () => {
  it("adds the component with its defaults where a clip has none, then sets the field", () => {
    const { store, clips, transform } = setup();
    const before = store.undoLabels.length;
    const result = store.dispatch({
      type: "component.setMany",
      payload: { clips, component: "transform", field: "rotation", value: 45 },
    });
    expect(result.ok).toBe(true);
    for (const id of clips)
      expect(transform(id)).toEqual({ ...componentDefaults("transform"), rotation: 45 });
    expect(store.undoLabels.length).toBe(before + 1); // one step for three clips
    expect(store.undoLabels[0]).toBe("Change rotation on 3 clips");
    store.undo();
    for (const id of clips) expect(transform(id)).toBeUndefined();
  });

  it("gives each clip its own value, or changes one element of a pair", () => {
    const { store, clips, transform } = setup();
    store.dispatch({
      type: "component.setMany",
      payload: {
        clips,
        component: "transform",
        field: "position",
        values: [
          [0.1, 0],
          [0.2, 0.1],
          [0, -0.3],
        ],
      },
    });
    store.dispatch({
      type: "component.setMany",
      payload: { clips, component: "transform", field: "position", index: 1, value: 0.25 },
    });
    expect(clips.map((id) => transform(id)?.["position"])).toEqual([
      [0.1, 0.25],
      [0.2, 0.25],
      [0, 0.25],
    ]);
  });

  it("refuses values the component does not allow, and changes nothing", () => {
    const { store, clips, transform } = setup();
    const set = (payload: Record<string, unknown>) =>
      store.dispatch({
        type: "component.setMany",
        payload: { clips, component: "transform", ...payload },
      });
    expect(set({ field: "opacity", value: 2 })).toMatchObject({
      ok: false,
      error: { code: "invalid-result" },
    });
    expect(set({ field: "scale", value: [0, 1] }).ok).toBe(false);
    expect(set({ field: "wobble", value: 1 })).toMatchObject({ error: { code: "rejected" } });
    expect(set({ field: "rotation", values: [1] })).toMatchObject({
      error: { code: "invalid-payload" },
    });
    expect(set({ field: "rotation", index: 0, value: 1 })).toMatchObject({
      error: { code: "rejected" },
    });
    expect(transform(clips[0] ?? "")).toBeUndefined();
  });
});

describe("component.reset", () => {
  it("puts the component back to its defaults on every clip given", () => {
    const { store, clips, transform } = setup();
    store.dispatch({
      type: "component.setMany",
      payload: { clips, component: "transform", field: "scale", value: [2, 0.5] },
    });
    store.dispatch({
      type: "component.reset",
      payload: { clips: clips.slice(0, 2), component: "transform" },
    });
    expect(transform(clips[0] ?? "")?.["scale"]).toEqual([1, 1]);
    expect(transform(clips[1] ?? "")?.["scale"]).toEqual([1, 1]);
    expect(transform(clips[2] ?? "")?.["scale"]).toEqual([2, 0.5]);
  });
});
