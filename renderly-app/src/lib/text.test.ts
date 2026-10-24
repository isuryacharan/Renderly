import { describe, expect, it } from "vitest";
import { project, textClip, track, videoClip, captionClip } from "../test/fixtures";
import { secsToUs as U } from "./time";
import { clipEndUs, type Marker, type Transcript } from "./types";
import { containerView } from "./container";
import { magneticInsertBoundaryUs } from "./magnetic";
import {
  animationEntries,
  bodyLabel,
  builtinStyleRef,
  cuttableWordIds,
  effectiveCaptionStyle,
  magnetTargetTrack,
  nearestMarker,
  restorableWordIds,
  snapPoints,
  styleItemId,
  styleLabel,
  wordRange,
} from "./text";

const marker = (id: string, secs: number, kind: Marker["kind"] = "user"): Marker => ({
  id,
  time_us: U(secs),
  label: "",
  color: "",
  kind,
});

describe("snapPoints (mirrors renderly-core Project::snap_points)", () => {
  // Same scenario as core's markers_are_added_sorted_removed_and_used_for_snapping test: one
  // 2 s clip at 0, markers at 3 s, 1 s and 1.5 s -> [0, 1 s, 1.5 s, 2 s, 3 s].
  it("is the sorted, de-duplicated set of clip edges and markers", () => {
    const tracks = [track("v1", "video", [videoClip("A", 0, 0, 2)])];
    const markers = [marker("a", 3), marker("b", 1, "beat"), marker("c", 1.5, "beat")];
    expect(snapPoints(tracks, markers, clipEndUs)).toEqual([0, U(1), U(1.5), U(2), U(3)]);
  });

  it("de-duplicates a marker that sits on a clip edge, counts every track and every clip type", () => {
    const tracks = [
      track("v1", "video", [videoClip("A", 0, 0, 2), textClip("T", 2, 1)]),
      track("c1", "caption", [captionClip("K", 5, 1)]),
    ];
    expect(snapPoints(tracks, [marker("a", 2)], clipEndUs)).toEqual([0, U(2), U(3), U(5), U(6)]);
  });

  it("can leave out the dragged clip's own edges (UI only; core has no such exclusion)", () => {
    const tracks = [track("v1", "video", [videoClip("A", 1, 0, 2), videoClip("B", 5, 0, 1)])];
    expect(snapPoints(tracks, undefined, clipEndUs, "A")).toEqual([U(5), U(6)]);
  });

  it("a nested sequence view has no markers to snap to", () => {
    const p = project([track("v1", "video", [])]);
    p.markers = [marker("a", 1)];
    p.sequences = [{ id: "s1", name: "S", tracks: [track("sv", "video", [videoClip("X", 0, 0, 1)])] }];
    expect(containerView(p, null).markers).toHaveLength(1);
    expect(containerView(p, "s1").markers).toEqual([]);
  });
});

describe("styles", () => {
  it("reads the item id of builtin and pack refs", () => {
    expect(styleItemId("builtin:tiktok-box@1")).toBe("tiktok-box");
    expect(styleItemId("pack:local.neon/glow-pop@2")).toBe("glow-pop");
    expect(builtinStyleRef("tiktok-box")).toBe("builtin:tiktok-box@1");
  });

  it("labels builtins by name and other styles by a readable item id", () => {
    expect(styleLabel("builtin:youtube-lower-thirds@1")).toBe("Lower thirds");
    expect(styleLabel("pack:p/glow_pop@1")).toBe("Glow pop");
  });

  it("a caption follows its own style, else its track style, else the default", () => {
    const cap = captionClip("K", 0, 1);
    const plain = track("c1", "caption", [cap]);
    expect(effectiveCaptionStyle(plain, cap).ref).toBe("builtin:tiktok-bold-yellow@1");
    const styled = { ...plain, caption_style: { ref: "builtin:tiktok-box@1", params: {}, overrides: {} } };
    expect(effectiveCaptionStyle(styled, cap).ref).toBe("builtin:tiktok-box@1");
    const own = { ...cap, style: { ref: "builtin:tiktok-minimal@1", params: {}, overrides: {} } };
    expect(effectiveCaptionStyle(styled, own).ref).toBe("builtin:tiktok-minimal@1");
  });

  it("lists stored animations in phase order", () => {
    expect(animationEntries(undefined)).toEqual([]);
    expect(
      animationEntries({ out: { ref: "builtin:fade@1", params: {} }, in: { ref: "builtin:pop@1", params: {}, duration_us: 400_000 } }),
    ).toEqual([
      { phase: "in", ref: "builtin:pop@1", durationUs: 400_000 },
      { phase: "out", ref: "builtin:fade@1", durationUs: undefined },
    ]);
  });

  it("bodyLabel flattens lines and truncates", () => {
    expect(bodyLabel({ text: "one\ntwo", runs: [], words: [] })).toBe("one two");
    expect(bodyLabel({ text: "abcdef", runs: [], words: [] }, 3)).toBe("abc");
  });
});

describe("markers and the magnet target", () => {
  it("finds the nearest marker within a distance", () => {
    const ms = [marker("a", 1), marker("b", 2)];
    expect(nearestMarker(ms, U(1.04), U(0.1))?.id).toBe("a");
    expect(nearestMarker(ms, U(1.5), U(0.1))).toBeUndefined();
    expect(nearestMarker(undefined, 0, 10)).toBeUndefined();
  });

  it("the magnet acts on the selected video track, else the first video track", () => {
    const tracks = [track("a1", "audio"), track("v1", "video"), track("v2", "video")];
    expect(magnetTargetTrack(tracks, "v2")?.id).toBe("v2");
    expect(magnetTargetTrack(tracks, "a1")?.id).toBe("v1");
    expect(magnetTargetTrack(tracks, null)?.id).toBe("v1");
    expect(magnetTargetTrack([track("a1", "audio")], null)).toBeUndefined();
  });
});

describe("transcript selection", () => {
  const t: Transcript = {
    id: "t",
    media_id: "m",
    language: "en",
    model: "x",
    words: ["a", "b", "c", "d"].map((w, i) => ({
      id: `w-${i + 1}`,
      text: w,
      start_us: i,
      end_us: i + 1,
      deleted: w === "c",
      tags: [],
    })),
  };

  it("splits a selection into words to cut and words to restore", () => {
    const sel = new Set(["w-1", "w-3"]);
    expect(cuttableWordIds(t, sel)).toEqual(["w-1"]);
    expect(restorableWordIds(t, sel)).toEqual(["w-3"]);
  });

  it("selects a range in transcript order whichever way it was dragged", () => {
    expect(wordRange(t, "w-2", "w-4")).toEqual(["w-2", "w-3", "w-4"]);
    expect(wordRange(t, "w-4", "w-2")).toEqual(["w-2", "w-3", "w-4"]);
    expect(wordRange(t, "w-2", "nope")).toEqual([]);
  });
});

describe("magneticInsertBoundaryUs (indicator for core's insert-by-centre)", () => {
  const lane = track("v1", "video", [videoClip("A", 0, 0, 2), videoClip("B", 2, 0, 2), videoClip("C", 4, 0, 2)]);

  it("lands at the start when the dragged centre is before the first clip's centre", () => {
    expect(magneticInsertBoundaryUs(lane, "X", U(-3), U(2))).toBe(0);
  });

  it("lands after every clip whose centre is at or before the dragged clip's centre", () => {
    // Centre 3.2 s: past A (1 s) and B (3 s), before C (5 s) -> boundary at B's end.
    expect(magneticInsertBoundaryUs(lane, "X", U(2.2), U(2))).toBe(U(4));
    // Centre exactly on B's centre counts as after B.
    expect(magneticInsertBoundaryUs(lane, "X", U(2), U(2))).toBe(U(4));
  });

  it("ignores the dragged clip itself and appends past the end", () => {
    expect(magneticInsertBoundaryUs(lane, "B", U(1), U(2))).toBe(U(2));
    expect(magneticInsertBoundaryUs(lane, "X", U(20), U(2))).toBe(U(6));
  });
});
