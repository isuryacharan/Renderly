import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import "./media-commands";
import { parseProject } from "./migrate";
import { projectSchema, type MediaItem, type Project } from "./schema";
import { ProjectStore } from "./store";
import { checkTimeline } from "./timeline";
import { MEDIA_RATES, nearestMediaRate } from "./time";

const fixture = readFileSync(
  join(import.meta.dirname, "..", "fixtures", "holiday-edit.project.json"),
  "utf8",
);
function load(): Project {
  const result = parseProject(fixture);
  if (!result.ok) throw new Error(JSON.stringify(result));
  return result.project;
}

const BEACH = "m0a1b2c3d4e5f6g7"; // used by one clip on a magnetic track
const PHONE = "m1h2j3k4m5n6p7q8"; // used by two clips, in both sequences

const item = (id: string, extra: Partial<MediaItem> = {}): MediaItem => ({
  id,
  name: "screen.webm",
  link: "handle",
  path: "screen.webm",
  fingerprint: { size: 1000, modified: 1, head: "sha256-abc" },
  duration: 705_600_000,
  video: { width: 1280, height: 720, fps: [15, 1], codec: "vp9", rotation: 0 },
  audio: { sampleRate: 22_050, channels: 1, codec: "opus" },
  ...extra,
});

const clipIds = (p: Project) =>
  p.sequences.flatMap((s) => s.tracks.flatMap((t) => t.clips.map((c) => c.id)));

describe("media.import", () => {
  it("adds items, including source rates a sequence cannot have", () => {
    const store = new ProjectStore(load());
    const result = store.dispatch({
      type: "media.import",
      payload: { items: [item("mnew000000000000"), item("mnew100000000000")] },
    });
    expect(result).toMatchObject({
      ok: true,
      changes: { added: ["mnew000000000000", "mnew100000000000"] },
    });
    expect(store.project.media).toHaveLength(4);
    expect(store.undoLabels).toEqual(["Import 2 files"]);
  });

  it("refuses an id already in the project, and rates no file can have", () => {
    const store = new ProjectStore(load());
    expect(
      store.dispatch({ type: "media.import", payload: { items: [item(BEACH)] } }),
    ).toMatchObject({
      ok: false,
      error: { code: "rejected", path: "items[0].id" },
    });
    const odd = item("modd000000000000", {
      video: { width: 10, height: 10, fps: [7, 1], codec: "x", rotation: 0 },
    });
    expect(store.dispatch({ type: "media.import", payload: { items: [odd] } })).toMatchObject({
      ok: false,
      error: { code: "invalid-payload" },
    });
  });
});

describe("media.remove", () => {
  it("refuses to remove media that clips use, unless told to remove the clips too", () => {
    const store = new ProjectStore(load());
    const before = store.project;
    expect(store.dispatch({ type: "media.remove", payload: { ids: [PHONE] } })).toMatchObject({
      ok: false,
      error: { code: "rejected", message: "The media is used by 2 clips" },
    });
    expect(store.project).toBe(before);

    const result = store.dispatch({
      type: "media.remove",
      payload: { ids: [BEACH, PHONE], withClips: true },
    });
    expect(result.ok).toBe(true);
    expect(store.project.media).toEqual([]);
    // Only the compound clip (which plays a sequence, not a file) is left.
    expect(clipIds(store.project)).toEqual(["c2r3s4t5v6w7x8y9"]);
    expect(checkTimeline(store.project)).toEqual([]);
    store.undo();
    expect({ ...store.project, revision: 0 }).toEqual({ ...before, revision: 0 }); // undo bumps the revision
  });

  it("removes unused media", () => {
    const store = new ProjectStore(load());
    store.dispatch({ type: "media.import", payload: { items: [item("mxnxsed000000000")] } });
    expect(
      store.dispatch({ type: "media.remove", payload: { ids: ["mxnxsed000000000"] } }).ok,
    ).toBe(true);
    expect(store.project.media.map((m) => m.id)).toEqual([BEACH, PHONE]);
    expect(
      store.dispatch({ type: "media.remove", payload: { ids: ["mxnxsed000000000"] } }),
    ).toMatchObject({
      error: { code: "not-found" },
    });
  });
});

describe("source rates", () => {
  it("snap a measured rate to the rate the file was meant to have", () => {
    expect(nearestMediaRate(29.97003)).toEqual([30000, 1001]);
    expect(nearestMediaRate(30.02)).toEqual([30, 1]);
    expect(nearestMediaRate(15.02)).toEqual([15, 1]);
    expect(nearestMediaRate(59.94)).toEqual([60000, 1001]);
    expect(nearestMediaRate(23.976)).toEqual([24000, 1001]);
    expect(nearestMediaRate(240)).toEqual([120, 1]);
  });

  it("all have whole-flick frames, and the schema accepts them", () => {
    for (const [num, den] of MEDIA_RATES)
      expect(Number.isInteger((705_600_000 * den) / num)).toBe(true);
    const project = load();
    project.media[0] = item(BEACH);
    expect(projectSchema().safeParse(project).success).toBe(true);
  });
});
