import fc from "fast-check";
import { describe, expect, it } from "vitest";
import "./timeline-commands";
import type { Command } from "./commands";
import { valueAt, type Key } from "./keyframes";
import { serialiseProject } from "./migrate";
import { projectSchema, type Clip, type Project, type Track } from "./schema";
import { ProjectStore } from "./store";
import {
  checkTimeline,
  clipAt,
  clipsInRange,
  sequenceDuration,
  snapTargets,
  snapTo,
} from "./timeline";

const id = (prefix: string) => prefix.padEnd(16, "0");
const SEC = 705_600_000;
const F = SEC / 30; // one frame at 30 fps
const s = (seconds: number) => Math.round(seconds * SEC);

const SEQ = id("sq");
const NEST = id("sn");
const V1 = id("tv1"); // magnetic video
const V2 = id("tv2"); // video
const A1 = id("ta1"); // audio
const T1 = id("tt1"); // text
const AV = id("mav"); // 60 s video + audio
const AUD = id("maa"); // 30 s audio only

const clip = (name: string, start: number, duration: number, extra: Partial<Clip> = {}): Clip => ({
  id: id(`c${name}`),
  media: AV,
  start: s(start),
  duration: s(duration),
  in: 0,
  speed: 1,
  enabled: true,
  components: [],
  ...extra,
});

const track = (trackId: string, kind: Track["kind"], clips: Clip[], magnetic = false): Track => ({
  id: trackId,
  kind,
  name: "",
  locked: false,
  hidden: false,
  muted: false,
  magnetic,
  clips,
});

/** V1 (magnetic): a [0,2) b [2,5) · V2: c [1,3) d [5,6) · A1: e [0,4) · T1 empty. */
function project(): Project {
  return projectSchema().parse({
    format: "renderly",
    version: 2,
    id: id("pj"),
    name: "Timeline",
    revision: 0,
    media: [
      {
        id: AV,
        name: "av.mp4",
        link: "handle",
        path: "av.mp4",
        fingerprint: { size: 1, modified: 1, head: "x" },
        duration: s(60),
        video: { width: 1920, height: 1080, fps: [30, 1], codec: "avc" },
        audio: { sampleRate: 48000, channels: 2, codec: "aac" },
      },
      {
        id: AUD,
        name: "a.wav",
        link: "handle",
        path: "a.wav",
        fingerprint: { size: 1, modified: 1, head: "x" },
        duration: s(30),
        audio: { sampleRate: 48000, channels: 2, codec: "pcm" },
      },
    ],
    sequences: [
      {
        id: SEQ,
        name: "Main",
        width: 1920,
        height: 1080,
        fps: [30, 1],
        tracks: [
          track(V1, "video", [clip("a", 0, 2), clip("b", 2, 3, { in: s(10) })], true),
          track(V2, "video", [clip("c", 1, 2, { in: s(5) }), clip("d", 5, 1)]),
          track(A1, "audio", [clip("e", 0, 4)]),
          track(T1, "text", []),
        ],
        markers: [{ id: id("k1"), t: s(7), name: "", color: "accent" }],
      },
      {
        id: NEST,
        name: "Nested",
        width: 1920,
        height: 1080,
        fps: [30, 1],
        tracks: [track(id("tn1"), "video", [clip("n", 0, 3)])],
      },
    ],
  }) as Project;
}

function setup() {
  const store = new ProjectStore(project());
  const ok = (command: Command) => {
    const result = store.dispatch(command);
    if (!result.ok) throw new Error(JSON.stringify(result.error));
    expect(checkTimeline(store.project)).toEqual([]);
    return result;
  };
  const fails = (command: Command) => {
    const before = store.project;
    const result = store.dispatch(command);
    expect(result.ok).toBe(false);
    expect(store.project).toBe(before);
    return result.ok ? undefined : result.error;
  };
  /** Clips of a track as [name, start s, end s, in s], names from the first letters of the id. */
  const layout = (trackId: string) => {
    const t = store.project.sequences.flatMap((q) => q.tracks).find((x) => x.id === trackId);
    return (t?.clips ?? []).map((c) => [
      c.id.replace(/0+$/, "").slice(1) || c.id,
      c.start / SEC,
      (c.start + c.duration) / SEC,
      c.in / SEC,
    ]);
  };
  const trackOf = (trackId: string) =>
    store.project.sequences.flatMap((q) => q.tracks).find((x) => x.id === trackId);
  return { store, ok, fails, layout, trackOf };
}

it("the starting timeline keeps every rule", () => {
  expect(checkTimeline(project())).toEqual([]);
});

describe("track.add / track.remove / track.set", () => {
  it("adds a track at an index, or at the end when the index is past it", () => {
    const { store, ok } = setup();
    ok({ type: "track.add", payload: { sequence: SEQ, id: id("tnew"), kind: "video", index: 1 } });
    ok({ type: "track.add", payload: { sequence: SEQ, id: id("tend"), kind: "audio", index: 99 } });
    expect(store.project.sequences[0]?.tracks.map((t) => t.id)).toEqual([
      V1,
      id("tnew"),
      V2,
      A1,
      T1,
      id("tend"),
    ]);
  });

  it("removes several tracks, but not a locked one", () => {
    const { store, ok, fails } = setup();
    ok({ type: "track.remove", payload: { tracks: [V2, T1] } });
    expect(store.project.sequences[0]?.tracks.map((t) => t.id)).toEqual([V1, A1]);
    ok({ type: "track.set", payload: { tracks: [A1], locked: true } });
    expect(fails({ type: "track.remove", payload: { tracks: [A1] } })?.code).toBe("rejected");
    expect(fails({ type: "track.remove", payload: { tracks: [id("tmxssxng")] } })?.code).toBe(
      "not-found",
    );
  });

  it("sets flags on several tracks; turning magnetic on closes the gaps", () => {
    const { ok, fails, layout, trackOf } = setup();
    ok({ type: "track.set", payload: { tracks: [V2, A1], muted: true, name: "Side" } });
    expect([trackOf(V2)?.muted, trackOf(A1)?.name]).toEqual([true, "Side"]);
    ok({ type: "track.set", payload: { tracks: [V2], magnetic: true } });
    expect(layout(V2)).toEqual([
      ["c", 0, 2, 5],
      ["d", 2, 3, 0],
    ]);
    expect(fails({ type: "track.set", payload: { tracks: [V2] } })?.code).toBe("invalid-payload");
  });
});

describe("clip.add", () => {
  it("overwrites: a new clip inside another cuts it in two", () => {
    const { ok, layout } = setup();
    ok({
      type: "clip.add",
      payload: { track: V2, clips: [{ id: id("cx"), media: AV, start: s(1.5), duration: s(1) }] },
    });
    const rows = layout(V2);
    expect(rows.map((r) => r.slice(1))).toEqual([
      [1, 1.5, 5],
      [1.5, 2.5, 0],
      [2.5, 3, 6.5],
      [5, 6, 0],
    ]);
    expect(rows[1]?.[0]).toBe("x");
  });

  it("inserts: clips from the insert point move later", () => {
    const { ok, layout } = setup();
    ok({
      type: "clip.add",
      payload: {
        track: V2,
        mode: "insert",
        clips: [{ id: id("cx"), media: AV, start: s(2), duration: s(1) }],
      },
    });
    expect(layout(V2).map((r) => r.slice(1))).toEqual([
      [1, 2, 5],
      [2, 3, 0],
      [3, 4, 6],
      [6, 7, 0],
    ]);
  });

  it("goes in at the nearest clip boundary on a magnetic track", () => {
    const { ok, layout } = setup();
    // 3.0 s is before the middle of b (3.5 s), so it lands between a and b.
    ok({
      type: "clip.add",
      payload: { track: V1, clips: [{ id: id("cx"), media: AV, start: s(3), duration: s(1) }] },
    });
    expect(layout(V1).map((r) => r[0])).toEqual(["a", "x", "b"]);
    expect(layout(V1).map((r) => r.slice(1, 3))).toEqual([
      [0, 2],
      [2, 3],
      [3, 6],
    ]);
  });

  it("adds several clips at once and snaps them to the frame grid", () => {
    const { store, ok, layout } = setup();
    ok({
      type: "clip.add",
      payload: {
        track: A1,
        clips: [
          { id: id("cx"), media: AUD, start: s(10) + F / 3, duration: s(1) - F / 3 },
          { id: id("cy"), media: AV, start: s(12), duration: s(2) },
        ],
      },
    });
    expect(layout(A1).map((r) => r.slice(0, 3))).toEqual([
      ["e", 0, 4],
      ["x", 10, 11],
      ["y", 12, 14],
    ]);
    expect(sequenceDuration(store.project.sequences[0] as never)).toBe(s(14));
  });

  it("refuses clips that do not fit", () => {
    const { ok, fails } = setup();
    const add = (track: string, c: Record<string, unknown>) =>
      fails({ type: "clip.add", payload: { track, clips: [{ id: id("cx"), start: 0, ...c }] } });
    expect(add(V2, { media: AV, duration: s(61) })).toMatchObject({ code: "rejected" });
    expect(add(V2, { media: AV, in: s(59), duration: s(2) })).toMatchObject({ code: "rejected" });
    expect(add(V2, { media: AUD, duration: s(1) })).toMatchObject({ code: "rejected" });
    expect(add(T1, { media: AV, duration: s(1) })).toMatchObject({ code: "rejected" });
    expect(add(V2, { media: AV, duration: F / 3 })).toMatchObject({ code: "rejected" });
    expect(add(V2, { media: id("mnnne"), duration: s(1) })).toMatchObject({ code: "not-found" });
    expect(add(V2, { sequence: SEQ, duration: s(1) })).toMatchObject({ code: "rejected" });
    expect(
      fails({
        type: "clip.add",
        payload: {
          track: V2,
          clips: [
            { id: id("cx"), media: AV, start: 0, duration: s(2) },
            { id: id("cy"), media: AV, start: s(1), duration: s(2) },
          ],
        },
      }),
    ).toMatchObject({ code: "rejected" });
    // A compound clip may nest another sequence, but not one that contains this one.
    ok({
      type: "clip.add",
      payload: {
        track: V2,
        clips: [{ id: id("cz"), sequence: NEST, start: s(8), duration: s(3) }],
      },
    });
    const nestTrack = id("tn1");
    expect(
      fails({
        type: "clip.add",
        payload: {
          track: nestTrack,
          clips: [{ id: id("cw"), sequence: SEQ, start: s(5), duration: s(1) }],
        },
      }),
    ).toMatchObject({ code: "rejected", path: "clips[0].sequence" });
  });
});

describe("clip.move", () => {
  it("moves within a track, overwriting what it lands on", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.move", payload: { clips: [id("cd")], delta: s(-3.5) } });
    // d lands inside c: c keeps [1, 1.5), and its tail [2.5, 3) becomes a new clip.
    const rows = layout(V2);
    expect(rows.map((r) => r.slice(1))).toEqual([
      [1, 1.5, 5],
      [1.5, 2.5, 0],
      [2.5, 3, 6.5],
    ]);
    expect(rows.map((r) => r[0]).slice(0, 2)).toEqual(["c", "d"]);
    expect(rows[2]?.[0]).not.toBe("c");
  });

  it("inserts, pushing later clips along", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.move", payload: { clips: [id("cd")], delta: s(-4), mode: "insert" } });
    expect(layout(V2).map((r) => r.slice(0, 3))).toEqual([
      ["d", 1, 2],
      ["c", 2, 4],
    ]);
  });

  it("moves several clips across tracks, keeping their spacing, and stops at 0", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.move", payload: { clips: [id("cc"), id("cd")], delta: s(-10), tracks: -1 } });
    // Clamped to -1 s (c started at 1 s); V1 is magnetic, so they go in at its start, packed.
    expect(layout(V1).map((r) => r.slice(0, 3))).toEqual([
      ["c", 0, 2],
      ["d", 2, 3],
      ["a", 3, 5],
      ["b", 5, 8],
    ]);
    expect(layout(V2)).toEqual([]);
  });

  it("reorders on a magnetic track", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.move", payload: { clips: [id("ca")], delta: s(4) } });
    expect(layout(V1).map((r) => r.slice(0, 3))).toEqual([
      ["b", 0, 3],
      ["a", 3, 5],
    ]);
  });

  it("refuses locked tracks, other kinds and missing tracks", () => {
    const { ok, fails } = setup();
    expect(
      fails({ type: "clip.move", payload: { clips: [id("ce")], delta: 0, tracks: -1 } }),
    ).toMatchObject({ code: "rejected" });
    expect(
      fails({ type: "clip.move", payload: { clips: [id("ca")], delta: 0, tracks: -1 } }),
    ).toMatchObject({ code: "rejected" });
    ok({ type: "track.set", payload: { tracks: [V2], locked: true } });
    expect(fails({ type: "clip.move", payload: { clips: [id("cd")], delta: s(1) } })).toMatchObject(
      { code: "rejected", path: "tracks" },
    );
    expect(
      fails({ type: "clip.move", payload: { clips: [id("ca")], delta: 0, tracks: 1 } }),
    ).toMatchObject({ code: "rejected" });
  });
});

describe("clip.trim", () => {
  it("trims either edge; the in-point follows the start edge", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.trim", payload: { clips: [id("cc")], edge: "start", delta: s(0.5) } });
    ok({ type: "clip.trim", payload: { clips: [id("cd")], edge: "end", delta: s(2) } });
    expect(layout(V2)).toEqual([
      ["c", 1.5, 3, 5.5],
      ["d", 5, 8, 0],
    ]);
  });

  it("stops at neighbours, the source and one frame", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.trim", payload: { clips: [id("cc")], edge: "end", delta: s(10) } });
    expect(layout(V2)[0]).toEqual(["c", 1, 5, 5]); // stops at d
    ok({ type: "clip.trim", payload: { clips: [id("cd")], edge: "start", delta: s(-10) } });
    expect(layout(V2)[1]).toEqual(["d", 5, 6, 0]); // d's source starts at 0: nothing to reveal
    ok({ type: "clip.trim", payload: { clips: [id("cc")], edge: "start", delta: s(-10) } });
    expect(layout(V2)[0]).toEqual(["c", 0, 5, 4]); // stops at 0
    ok({ type: "clip.trim", payload: { clips: [id("cd")], edge: "end", delta: s(100) } });
    expect(layout(V2)[1]).toEqual(["d", 5, 65, 0]); // stops at the 60 s source end
    ok({ type: "clip.trim", payload: { clips: [id("cd")], edge: "end", delta: s(-100) } });
    expect(layout(V2)[1]?.slice(1, 3)).toEqual([5, 5 + 1 / 30]); // one frame left
  });

  it("ripples on a magnetic track and trims several clips at once", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.trim", payload: { clips: [id("ca"), id("cb")], edge: "start", delta: s(1) } });
    expect(layout(V1)).toEqual([
      ["a", 0, 1, 1],
      ["b", 1, 3, 11],
    ]);
    ok({ type: "clip.trim", payload: { clips: [id("ca")], edge: "end", delta: s(0.5) } });
    expect(layout(V1)).toEqual([
      ["a", 0, 1.5, 1],
      ["b", 1.5, 3.5, 11],
    ]);
  });
});

describe("clip.split", () => {
  it("splits several clips at one time, with deterministic ids and shifted keyframes", () => {
    const { store, ok, layout } = setup();
    ok({
      type: "clip.add",
      payload: {
        track: V2,
        clips: [
          {
            id: id("ck"),
            media: AV,
            start: s(10),
            duration: s(4),
            components: [
              {
                type: "transform",
                position: [0, 0],
                scale: [1, 1],
                rotation: 0,
                opacity: 1,
                keys: {
                  opacity: [
                    { t: 0, v: 0, ease: "auto" },
                    { t: s(3), v: 1, ease: "auto" },
                  ],
                },
              },
            ],
          },
        ],
      },
    });
    ok({
      type: "clip.split",
      payload: { clips: [id("ca"), id("cb"), id("ce"), id("ck")], t: s(1) },
    });
    expect(layout(V1).map((r) => r.slice(1))).toEqual([
      [0, 1, 0],
      [1, 2, 1],
      [2, 5, 10],
    ]);
    ok({ type: "clip.split", payload: { clips: [id("ck")], t: s(12) } });
    const right = store.project.sequences[0]?.tracks[1]?.clips.at(-1);
    expect(right?.start).toBe(s(12));
    expect(right?.in).toBe(s(2));
    // The right half gets a key at its new start, and its fade is the same as before the split.
    const keys = (right?.components[0] as { keys: { opacity: Key[] } }).keys.opacity;
    expect(keys[0]?.t).toBe(0);
    expect(keys.at(-1)).toMatchObject({ t: s(1), v: 1 });
    const original: Key[] = [
      { t: 0, v: 0, ease: "auto" },
      { t: s(3), v: 1, ease: "auto" },
    ];
    for (const at of [0, 0.25, 0.5, 0.75, 1, 1.5])
      expect(valueAt(keys, s(at))).toBeCloseTo(valueAt(original, s(at + 2)) as number, 9);

    // The same split on a fresh store gives the same ids.
    const again = setup();
    again.ok({ type: "clip.split", payload: { clips: [id("ca")], t: s(1) } });
    expect(again.store.project.sequences[0]?.tracks[0]?.clips[1]?.id).toBe(
      store.project.sequences[0]?.tracks[0]?.clips[1]?.id,
    );
  });

  it("refuses a time on a clip's edge or outside every clip", () => {
    const { fails } = setup();
    expect(fails({ type: "clip.split", payload: { clips: [id("ca")], t: s(2) } })?.code).toBe(
      "rejected",
    );
    expect(
      fails({ type: "clip.split", payload: { clips: [id("ca"), id("cd")], t: s(9) } })?.code,
    ).toBe("rejected");
  });
});

describe("clip.delete / clip.rippleDelete / track.closeGap", () => {
  it("delete leaves a gap, except on a magnetic track", () => {
    const { ok, layout } = setup();
    ok({ type: "clip.delete", payload: { clips: [id("cc"), id("ca")] } });
    expect(layout(V2).map((r) => r.slice(0, 3))).toEqual([["d", 5, 6]]);
    expect(layout(V1).map((r) => r.slice(0, 3))).toEqual([["b", 0, 3]]);
  });

  it("ripple delete closes the space each clip took", () => {
    const { ok, layout } = setup();
    ok({
      type: "clip.add",
      payload: { track: V2, clips: [{ id: id("cx"), media: AV, start: s(8), duration: s(1) }] },
    });
    ok({ type: "clip.rippleDelete", payload: { clips: [id("cc"), id("cd")] } });
    // c (2 s) and d (1 s) are gone; x moves 3 s earlier; the gap before c stays.
    expect(layout(V2).map((r) => r.slice(0, 3))).toEqual([["x", 5, 6]]);
  });

  it("closes the gap at a time on several tracks, including a leading gap", () => {
    const { ok, fails, layout } = setup();
    ok({ type: "track.closeGap", payload: { tracks: [V2], t: s(4) } });
    expect(layout(V2).map((r) => r.slice(0, 3))).toEqual([
      ["c", 1, 3],
      ["d", 3, 4],
    ]);
    ok({ type: "track.closeGap", payload: { tracks: [V2, A1], t: s(0.5) } });
    expect(layout(V2).map((r) => r.slice(0, 3))).toEqual([
      ["c", 0, 2],
      ["d", 2, 3],
    ]);
    expect(fails({ type: "track.closeGap", payload: { tracks: [V2], t: s(1) } })?.code).toBe(
      "rejected",
    );
    expect(fails({ type: "track.closeGap", payload: { tracks: [V2], t: s(20) } })?.code).toBe(
      "rejected",
    );
  });
});

describe("queries", () => {
  it("finds clips in a range and at a time, snap targets and the duration", () => {
    const sequence = project().sequences[0];
    if (!sequence) throw new Error("no sequence");
    const names = (refs: { clip: Clip }[]) => refs.map((r) => r.clip.id[1]);
    expect(names(clipsInRange(sequence, s(2), s(5)))).toEqual(["b", "c", "e"]);
    expect(names(clipsInRange(sequence, s(2), s(5), [V2]))).toEqual(["c"]);
    expect(names(clipsInRange(sequence, s(3), s(5), [V2]))).toEqual([]); // ends are open
    const v2 = sequence.tracks[1];
    if (!v2) throw new Error("no track");
    expect(clipAt(v2, s(1))?.id).toBe(id("cc"));
    expect(clipAt(v2, s(3))).toBeUndefined();
    expect(snapTargets(sequence, { exclude: [id("cd")], extra: [s(9)] })).toEqual(
      [0, 1, 2, 3, 4, 5, 7, 9].map(s),
    );
    expect(snapTo([0, s(1), s(2)], s(1.1), s(0.2))).toBe(s(1));
    expect(snapTo([0, s(1), s(2)], s(1.5), s(0.2))).toBe(s(1.5));
    expect(sequenceDuration(sequence)).toBe(s(6));
  });
});

// --- fuzz: random edits never break a timeline rule, and undo restores everything ---

type Pick = { a: number; b: number; c: number };
const pickArb = fc.record({ a: fc.nat(), b: fc.nat(), c: fc.nat() });
const editArb = fc.tuple(
  fc.constantFrom(
    "add",
    "insert",
    "move",
    "moveInsert",
    "moveTrack",
    "trimStart",
    "trimEnd",
    "split",
    "delete",
    "ripple",
    "closeGap",
    "magnetic",
    "lock",
    "undo",
  ),
  pickArb,
  fc.integer({ min: -s(8), max: s(8) }),
  fc.boolean(),
);

function toCommand(
  project: Project,
  [kind, pick, amount, flag]: [string, Pick, number, boolean],
  serial: number,
): Command | "undo" {
  const sequence = project.sequences[0];
  if (!sequence) throw new Error("no sequence");
  const tracks = sequence.tracks.filter((t) => t.kind === "video" || t.kind === "audio");
  const track = tracks[pick.a % tracks.length] as Track;
  const clips = sequence.tracks.flatMap((t) => t.clips);
  const some = () => {
    if (clips.length === 0) return [id("cnnne")];
    const first = clips[pick.b % clips.length] as Clip;
    const second = clips[pick.c % clips.length] as Clip;
    return flag && first.id !== second.id ? [first.id, second.id] : [first.id];
  };
  const t = Math.abs(amount) + (pick.c % 7) * F;
  switch (kind) {
    case "add":
    case "insert":
      return {
        type: "clip.add",
        payload: {
          track: track.id,
          mode: kind === "add" ? "overwrite" : "insert",
          clips: [
            {
              id: id(`f${serial.toString(32).replace(/[ilou]/g, "x")}`),
              media: AV,
              start: t,
              duration: 1 + ((pick.b * F) % s(5)),
              in: (pick.c % 20) * F,
            },
          ],
        },
      };
    case "move":
    case "moveInsert":
      return {
        type: "clip.move",
        payload: { clips: some(), delta: amount, mode: kind === "move" ? "overwrite" : "insert" },
      };
    case "moveTrack":
      return {
        type: "clip.move",
        payload: { clips: some(), delta: amount, tracks: flag ? 1 : -1 },
      };
    case "trimStart":
    case "trimEnd":
      return {
        type: "clip.trim",
        payload: { clips: some(), edge: kind === "trimStart" ? "start" : "end", delta: amount },
      };
    case "split":
      return { type: "clip.split", payload: { clips: some(), t } };
    case "delete":
      return { type: "clip.delete", payload: { clips: some() } };
    case "ripple":
      return { type: "clip.rippleDelete", payload: { clips: some() } };
    case "closeGap":
      return { type: "track.closeGap", payload: { tracks: [track.id], t } };
    case "magnetic":
      return { type: "track.set", payload: { tracks: [track.id], magnetic: flag } };
    case "lock":
      return {
        type: "track.set",
        payload: { tracks: [track.id], locked: flag && pick.c % 4 === 0 },
      };
    default:
      return "undo";
  }
}

it("no random sequence of edits breaks a timeline rule, and undo restores the start", () => {
  const counts = new Map<string, number>();
  fc.assert(
    fc.property(fc.array(editArb, { minLength: 1, maxLength: 40 }), (edits) => {
      const store = new ProjectStore(project());
      const original = serialiseProject(store.project);
      edits.forEach((edit, serial) => {
        const command = toCommand(store.project, edit, serial);
        if (command === "undo") store.undo();
        else {
          const result = store.dispatch(command);
          const key = `${command.type} ${result.ok ? "ok" : result.error.code}`;
          counts.set(key, (counts.get(key) ?? 0) + 1);
        }
        const problems = checkTimeline(store.project);
        if (problems.length > 0) throw new Error(`${JSON.stringify(edit)}: ${problems.join("; ")}`);
      });
      while (store.undo().ok);
      expect(serialiseProject({ ...store.project, revision: 0 })).toBe(original);
    }),
    { numRuns: 1000 },
  );
  // Most edits should succeed; a fuzz test that only exercises refusals proves little. Each type
  // must succeed often enough to matter (closing a gap is the rarest: about 45-70 per 1,000 runs).
  const succeeded = [...counts].filter(([k]) => k.endsWith(" ok")).reduce((n, [, v]) => n + v, 0);
  const total = [...counts.values()].reduce((n, v) => n + v, 0);
  expect(succeeded / total).toBeGreaterThan(0.5);
  for (const type of [
    "clip.add",
    "clip.move",
    "clip.trim",
    "clip.split",
    "clip.delete",
    "clip.rippleDelete",
    "track.closeGap",
  ])
    expect(counts.get(`${type} ok`) ?? 0).toBeGreaterThan(20);
});
