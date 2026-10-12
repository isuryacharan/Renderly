import { describe, expect, it } from "vitest";
import { project as makeProject, track, videoClip } from "../test/fixtures";
import {
  activeTracks,
  breadcrumbItems,
  containerDurationSecs,
  containerOfTrack,
  containerView,
  findTrackAnywhere,
  isNestedClip,
  MIN_ADJUSTMENT_SECS,
  newTrackKindForDrop,
  normalizeSequencePath,
  pathAfterOpen,
  pathForBreadcrumb,
  replaceActiveTracks,
  resolveActiveSequenceId,
  selectActiveTracks,
  selectActiveView,
  sequenceOfMedia,
  trackAcceptsMedia,
  trimAdjustment,
  updateClip,
} from "./container";
import type { AdjustmentClip, Project } from "./types";

const adj: AdjustmentClip = {
  type: "adjustment",
  id: "adj1",
  position_secs: 1,
  duration_secs: 4,
  enabled: true,
};

/** Root: v1 [A 0..2, N (nested "Intro") 3..6]. Intro: iv1 [IA 0..3, adj 1..5]. Inner: sub-sequence "Titles" in Intro. */
function sample(): Project {
  const p = makeProject([
    track("v1", "video", [videoClip("A", 0, 0, 2), videoClip("N", 3, 0, 3, { media_id: "m-intro" })]),
  ]);
  p.media = [
    { id: "m-a", path: "a.mp4", kind: "video", duration_secs: 10, width: 1, height: 1, fps: 30 },
    { id: "m-intro", path: "", kind: "sequence", sequence_id: "s-intro", duration_secs: null, width: null, height: null, fps: null },
    { id: "m-titles", path: "", kind: "sequence", sequence_id: "s-titles", duration_secs: null, width: null, height: null, fps: null },
  ];
  p.sequences = [
    {
      id: "s-intro",
      name: "Intro",
      tracks: [track("iv1", "video", [videoClip("IA", 0, 0, 3), adj])],
    },
    { id: "s-titles", name: "Titles", tracks: [track("tv1", "video", [videoClip("TA", 0, 0, 1)])] },
  ];
  return p;
}

describe("activeTracks", () => {
  it("returns the root tracks for null and the sequence's tracks otherwise", () => {
    const p = sample();
    expect(activeTracks(p, null)).toBe(p.tracks);
    expect(activeTracks(p, "s-intro")).toBe(p.sequences![0]!.tracks);
  });

  it("falls back to the root when the sequence is gone", () => {
    const p = sample();
    expect(activeTracks(p, "missing")).toBe(p.tracks);
    expect(resolveActiveSequenceId(p, "missing")).toBeNull();
    expect(resolveActiveSequenceId(p, "s-intro")).toBe("s-intro");
  });

  it("is empty for no project and works as a store selector", () => {
    expect(activeTracks(null, "s-intro")).toEqual([]);
    const p = sample();
    expect(selectActiveTracks({ project: p, activeSequenceId: "s-titles" })[0]!.id).toBe("tv1");
    expect(selectActiveView({ project: null, activeSequenceId: null })).toBeNull();
  });
});

describe("containerView", () => {
  it("is the project itself for the root and a cached track-swapped copy for a sequence", () => {
    const p = sample();
    expect(containerView(p, null)).toBe(p);
    const v = containerView(p, "s-intro");
    expect(v).not.toBe(p);
    expect(v.tracks).toBe(p.sequences![0]!.tracks);
    expect(v.media).toBe(p.media);
    expect(containerView(p, "s-intro")).toBe(v);
    // A new project object (any edit) gets a fresh view.
    const p2 = { ...p };
    expect(containerView(p2, "s-intro")).not.toBe(v);
    // Unknown sequence -> root.
    expect(containerView(p, "nope")).toBe(p);
  });
});

describe("replaceActiveTracks / updateClip", () => {
  it("writes edited tracks back into the open container only", () => {
    const p = sample();
    const edited = [track("iv1", "video", [])];
    const root = replaceActiveTracks(p, null, [track("x", "video", [])]);
    expect(root.tracks[0]!.id).toBe("x");
    expect(root.sequences).toBe(p.sequences);
    const seq = replaceActiveTracks(p, "s-intro", edited);
    expect(seq.tracks).toBe(p.tracks);
    expect(seq.sequences![0]!.tracks).toBe(edited);
    expect(seq.sequences![1]).toBe(p.sequences![1]);
  });

  it("updateClip finds the clip in a sequence and keeps untouched containers by identity", () => {
    const p = sample();
    const next = updateClip(p, "iv1", "IA", (c) => ({ ...c, position_secs: 9 }));
    expect(next.sequences![0]!.tracks[0]!.clips[0]!.position_secs).toBe(9);
    expect(next.tracks).toBe(p.tracks);
    expect(next.sequences![1]).toBe(p.sequences![1]);
    expect(updateClip(p, "nope", "IA", (c) => c)).toBe(p);
  });
});

describe("lookups", () => {
  it("finds tracks and their container across root and sequences", () => {
    const p = sample();
    expect(findTrackAnywhere(p, "tv1")?.id).toBe("tv1");
    expect(findTrackAnywhere(p, "zzz")).toBeUndefined();
    expect(containerOfTrack(p, "v1")).toBeNull();
    expect(containerOfTrack(p, "tv1")).toBe("s-titles");
    expect(containerOfTrack(p, "zzz")).toBeUndefined();
  });

  it("maps sequence media to its sequence and detects nested clips", () => {
    const p = sample();
    expect(sequenceOfMedia(p, "m-intro")?.name).toBe("Intro");
    expect(sequenceOfMedia(p, "m-a")).toBeUndefined();
    const nested = p.tracks[0]!.clips[1]!;
    expect(isNestedClip(p, nested)).toBe(true);
    expect(isNestedClip(p, p.tracks[0]!.clips[0]!)).toBe(false);
    expect(isNestedClip(p, adj)).toBe(false);
  });

  it("measures the open container's duration", () => {
    const p = sample();
    expect(containerDurationSecs(p, null)).toBe(6);
    // Intro: IA 0..3 and the adjustment layer 1..5.
    expect(containerDurationSecs(p, "s-intro")).toBe(5);
    expect(containerDurationSecs(p, "s-titles")).toBe(1);
  });
});

describe("breadcrumb and path", () => {
  it("builds Root > Sequence > ... and marks the open one current", () => {
    const p = sample();
    const items = breadcrumbItems(p, ["s-intro", "s-titles"]);
    expect(items.map((i) => i.label)).toEqual(["test", "Intro", "Titles"]);
    expect(items.map((i) => i.current)).toEqual([false, false, true]);
    expect(items[0]!.sequenceId).toBeNull();
    expect(breadcrumbItems(p, [])).toEqual([{ sequenceId: null, label: "test", current: true }]);
  });

  it("drops path entries whose sequence disappeared, keeping the surviving ancestors", () => {
    const p = sample();
    expect(normalizeSequencePath(p, ["s-intro", "s-titles"])).toEqual(["s-intro", "s-titles"]);
    expect(normalizeSequencePath(p, ["s-intro", "gone"])).toEqual(["s-intro"]);
    expect(normalizeSequencePath(p, ["gone", "s-titles"])).toEqual([]);
    expect(normalizeSequencePath(null, ["s-intro"])).toEqual([]);
    expect(breadcrumbItems(p, ["s-intro", "gone"]).map((i) => i.label)).toEqual(["test", "Intro"]);
  });

  it("computes the path after navigating and opening", () => {
    expect(pathForBreadcrumb(["a", "b", "c"], null)).toEqual([]);
    expect(pathForBreadcrumb(["a", "b", "c"], "b")).toEqual(["a", "b"]);
    expect(pathForBreadcrumb(["a", "b"], "zzz")).toEqual(["a", "b"]);
    expect(pathAfterOpen(["a"], "b")).toEqual(["a", "b"]);
    // Re-opening an ancestor truncates instead of growing a loop.
    expect(pathAfterOpen(["a", "b"], "a")).toEqual(["a"]);
  });
});

describe("trimAdjustment", () => {
  it("right edge keeps the start and clamps to the minimum length", () => {
    expect(trimAdjustment("right", 2, 4, 9)).toEqual({ position: 2, duration: 7 });
    expect(trimAdjustment("right", 2, 4, 2.01).duration).toBeCloseTo(MIN_ADJUSTMENT_SECS);
    expect(trimAdjustment("right", 2, 4, 0).duration).toBeCloseTo(MIN_ADJUSTMENT_SECS);
  });

  it("left edge keeps the end fixed, never goes below 0 and keeps the minimum length", () => {
    expect(trimAdjustment("left", 2, 4, 3)).toEqual({ position: 3, duration: 3 });
    expect(trimAdjustment("left", 2, 4, -5)).toEqual({ position: 0, duration: 6 });
    const t = trimAdjustment("left", 2, 4, 50);
    expect(t.position).toBeCloseTo(6 - MIN_ADJUSTMENT_SECS);
    expect(t.position + t.duration).toBeCloseTo(6);
  });
});

describe("drop helpers", () => {
  it("lets sequences land on video or audio tracks but never captions", () => {
    expect(trackAcceptsMedia("video", "sequence")).toBe(true);
    expect(trackAcceptsMedia("audio", "sequence")).toBe(true);
    expect(trackAcceptsMedia("caption", "sequence")).toBe(false);
    expect(trackAcceptsMedia("audio", "video")).toBe(false);
    expect(trackAcceptsMedia("video", "audio")).toBe(false);
    expect(trackAcceptsMedia("video", "image")).toBe(true);
  });

  it("creates a video track above the lanes and an audio track below for a sequence drop", () => {
    expect(newTrackKindForDrop("sequence", true)).toBe("video");
    expect(newTrackKindForDrop("sequence", false)).toBe("audio");
    expect(newTrackKindForDrop("audio", true)).toBe("audio");
    expect(newTrackKindForDrop("video", false)).toBe("video");
  });
});
