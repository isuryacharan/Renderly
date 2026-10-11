import { describe, expect, it } from "vitest";
import { audioClip, project, track, videoClip } from "../test/fixtures";
import {
  collectSnapCandidates,
  SNAP_PRIORITY_EDGE,
  SNAP_PRIORITY_GRID,
  SNAP_PRIORITY_PLAYHEAD,
  SNAP_THRESHOLD_PX,
  snapTime,
} from "./snap";

// A: [2.5, 5.5]   B: [6.25, 8.25]   -> timeline duration 8.25, fps 30
const A = videoClip("A", 2.5, 0, 3);
const B = videoClip("B", 6.25, 0, 2);
const proj = project([track("v1", "video", [A, B])]);

function byTime(p = proj, playhead = 0, exclude?: string) {
  return new Map(collectSnapCandidates(p, playhead, exclude).map((c) => [c.time, c.priority]));
}

describe("collectSnapCandidates", () => {
  it("offers 0, the timeline end and every clip edge as top-priority edges", () => {
    const m = byTime();
    for (const t of [0, 2.5, 5.5, 6.25, 8.25]) expect(m.get(t)).toBe(SNAP_PRIORITY_EDGE);
  });

  it("adds the playhead at its own priority", () => {
    expect(byTime(proj, 4.1).get(4.1)).toBe(SNAP_PRIORITY_PLAYHEAD);
  });

  it("adds a whole-second grid strictly inside the timeline at lowest priority", () => {
    const m = byTime();
    for (const t of [1, 2, 3, 4, 5, 6, 7, 8]) expect(m.get(t)).toBe(SNAP_PRIORITY_GRID);
    expect(m.has(9)).toBe(false);
  });

  it("keeps the better priority when sources coincide (playhead on an edge, edge on a grid line)", () => {
    expect(byTime(proj, 2.5).get(2.5)).toBe(SNAP_PRIORITY_EDGE);
    const gridEdge = project([track("v", "video", [videoClip("X", 3, 0, 2)])]);
    expect(byTime(gridEdge).get(3)).toBe(SNAP_PRIORITY_EDGE);
    expect(byTime(gridEdge).get(5)).toBe(SNAP_PRIORITY_EDGE); // end of X and of the timeline
    expect(byTime(gridEdge).get(4)).toBe(SNAP_PRIORITY_GRID);
  });

  it("excludes the dragged clip's own edges but still counts it for timeline length", () => {
    const m = byTime(proj, 0, "A");
    expect(m.has(2.5)).toBe(false);
    expect(m.has(5.5)).toBe(false);
    expect(m.get(6.25)).toBe(SNAP_PRIORITY_EDGE);
  });

  it("collects edges across all tracks", () => {
    const p = project([track("v", "video", [A]), track("a", "audio", [audioClip("C", 10, 0, 1)])]);
    const m = byTime(p);
    expect(m.get(10)).toBe(SNAP_PRIORITY_EDGE);
    expect(m.get(11)).toBe(SNAP_PRIORITY_EDGE);
  });

  it("an empty project still has 0 and the 1s minimum duration", () => {
    const m = byTime(project([]));
    expect([...m.keys()].sort()).toEqual([0, 1]);
  });
});

describe("snapTime", () => {
  const snap = (secs: number, px = 100, playhead = 0, enabled = true, exclude?: string) =>
    snapTime(secs, proj, playhead, px, enabled, exclude);

  it("with snapping off, only quantises to the frame grid", () => {
    expect(snap(2.51, 100, 0, false)).toBeCloseTo(75 / 30, 10); // 2.5s sits between frames 75
    expect(snap(2.4999, 100, 0, false)).toBeCloseTo(2.5, 10);
    expect(snapTime(1 / 60, proj, 0, 100, false)).toBeCloseTo(1 / 30, 10); // half-frame rounds up
  });

  it("clamps negative input to 0", () => {
    expect(snap(-5, 100, 0, false)).toBe(0);
    expect(snap(-0.01)).toBe(0);
  });

  it("magnets to a clip edge inside the pixel threshold", () => {
    expect(snap(2.55)).toBe(2.5); // 5px away
    expect(snap(2.43)).toBe(2.5); // 7px away
  });

  it("does not magnet outside the threshold; falls back to the frame grid", () => {
    // 2.5 is 15px away, no other candidate within 10px
    expect(snap(2.65)).toBeCloseTo(Math.round(2.65 * 30) / 30, 10);
  });

  it("threshold is measured in pixels: same time delta snaps at low zoom, not at high zoom", () => {
    expect(snap(2.55, 100)).toBe(2.5); // 5px
    expect(snap(2.55, 400)).not.toBe(2.5); // 20px
  });

  it("still snaps at very high zoom where 10px is below one frame period", () => {
    // 10px at 1000px/s = 10ms < 33ms frame. 8px away must still hit the edge.
    expect(snap(2.508, 1000)).toBe(2.5);
    expect(snap(2.52, 1000)).not.toBe(2.5); // 20px
  });

  it("snaps to the timeline start (0)", () => {
    expect(snap(0.04)).toBe(0);
  });

  it("snaps to the timeline end", () => {
    expect(snap(8.3)).toBe(8.25);
  });

  it("a clip edge beats a closer playhead when both are within threshold", () => {
    // edge 5.5, playhead 5.54, query 5.535: playhead is 0.5px away, edge 3.5px
    expect(snap(5.535, 100, 5.54)).toBe(5.5);
  });

  it("the playhead beats the second grid when both are within threshold", () => {
    // grid 3, playhead 3.05, query 3.04
    expect(snap(3.04, 100, 3.05)).toBe(3.05);
  });

  it("among equal priorities the nearer candidate wins", () => {
    const p = project([track("v", "video", [videoClip("X", 2, 0, 0.1), videoClip("Y", 2.08, 0, 5)])]);
    // edges 2.0, 2.1, 2.08 all within 10px at 100px/s of 2.07 -> 2.08 is nearest
    expect(snapTime(2.07, p, 0, 100, true)).toBe(2.08);
  });

  it("excludeClipId removes that clip's own edges as snap targets", () => {
    expect(snap(2.52, 100, 0, true, "A")).toBeCloseTo(Math.round(2.52 * 30) / 30, 10);
    expect(snap(2.52)).toBe(2.5);
  });

  it("falls back to 30fps when settings.fps is 0", () => {
    const p = { ...proj, settings: { ...proj.settings, fps: 0 } };
    expect(snapTime(0.5, p, 0, 1, false)).toBeCloseTo(0.5, 10);
    expect(snapTime(0.52, p, 0, 1, false)).toBeCloseTo(16 / 30, 10);
  });

  it("exports a 10px threshold", () => expect(SNAP_THRESHOLD_PX).toBe(10));
});
