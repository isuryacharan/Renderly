import { describe, expect, it } from "vitest";
import { audioClip, project, track, videoClip } from "../test/fixtures";
import { frameStart, isOnFrame, secsToUs } from "../lib/time";
import {
  collectSnapCandidates,
  SNAP_PRIORITY_EDGE,
  SNAP_PRIORITY_GRID,
  SNAP_PRIORITY_PLAYHEAD,
  SNAP_THRESHOLD_PX,
  snapTime,
} from "./snap";

// Times in these tests are written in seconds and converted to integer µs (the unit snap.ts
// works in) with `U`.
const U = secsToUs;

// A: [2.5, 5.5]   B: [6.25, 8.25]   -> timeline duration 8.25, fps 30
const A = videoClip("A", 2.5, 0, 3);
const B = videoClip("B", 6.25, 0, 2);
const proj = project([track("v1", "video", [A, B])]);
const F30 = { num: 30, den: 1 };

function byTime(p = proj, playhead = 0, exclude?: string) {
  return new Map(collectSnapCandidates(p, U(playhead), exclude).map((c) => [c.time, c.priority]));
}

describe("collectSnapCandidates", () => {
  it("offers 0, the timeline end and every clip edge as top-priority edges", () => {
    const m = byTime();
    for (const t of [0, 2.5, 5.5, 6.25, 8.25]) expect(m.get(U(t))).toBe(SNAP_PRIORITY_EDGE);
  });

  it("adds the playhead at its own priority", () => {
    expect(byTime(proj, 4.1).get(U(4.1))).toBe(SNAP_PRIORITY_PLAYHEAD);
  });

  it("adds a whole-second grid strictly inside the timeline at lowest priority", () => {
    const m = byTime();
    for (const t of [1, 2, 3, 4, 5, 6, 7, 8]) expect(m.get(U(t))).toBe(SNAP_PRIORITY_GRID);
    expect(m.has(U(9))).toBe(false);
  });

  it("keeps the better priority when sources coincide (playhead on an edge, edge on a grid line)", () => {
    expect(byTime(proj, 2.5).get(U(2.5))).toBe(SNAP_PRIORITY_EDGE);
    const gridEdge = project([track("v", "video", [videoClip("X", 3, 0, 2)])]);
    expect(byTime(gridEdge).get(U(3))).toBe(SNAP_PRIORITY_EDGE);
    expect(byTime(gridEdge).get(U(5))).toBe(SNAP_PRIORITY_EDGE); // end of X and of the timeline
    expect(byTime(gridEdge).get(U(4))).toBe(SNAP_PRIORITY_GRID);
  });

  it("excludes the dragged clip's own edges but still counts it for timeline length", () => {
    const m = byTime(proj, 0, "A");
    expect(m.has(U(2.5))).toBe(false);
    expect(m.has(U(5.5))).toBe(false);
    expect(m.get(U(6.25))).toBe(SNAP_PRIORITY_EDGE);
  });

  it("collects edges across all tracks", () => {
    const p = project([track("v", "video", [A]), track("a", "audio", [audioClip("C", 10, 0, 1)])]);
    const m = byTime(p);
    expect(m.get(U(10))).toBe(SNAP_PRIORITY_EDGE);
    expect(m.get(U(11))).toBe(SNAP_PRIORITY_EDGE);
  });

  it("offers markers as top-priority edges, like clip edges (core snap_points)", () => {
    const p = { ...proj, markers: [{ id: "m", time_us: U(7), label: "", color: "", kind: "user" as const }] };
    const m = new Map(collectSnapCandidates(p, 0).map((c) => [c.time, c.priority]));
    expect(m.get(U(7))).toBe(SNAP_PRIORITY_EDGE);
    // The coarse grid still skips a second a marker already covers (the better priority wins).
    expect(m.get(U(3))).toBe(SNAP_PRIORITY_GRID);
  });

  it("an empty project still has 0 and the 1s minimum duration", () => {
    const m = byTime(project([]));
    expect([...m.keys()].sort((a, b) => a - b)).toEqual([0, 1_000_000]);
  });
});

describe("snapTime", () => {
  const snap = (secs: number, px = 100, playhead = 0, enabled = true, exclude?: string) =>
    snapTime(U(secs), proj, U(playhead), px, enabled, exclude);

  it("with snapping off, only quantises to the frame grid", () => {
    expect(snap(2.51, 100, 0, false)).toBe(frameStart(F30, 75)); // 2.5 s is frame 75
    expect(snap(2.4999, 100, 0, false)).toBe(frameStart(F30, 75));
    expect(snapTime(16_666, proj, 0, 100, false)).toBe(0); // under half a frame snaps back
    expect(snapTime(16_667, proj, 0, 100, false)).toBe(33_333); // over half snaps forward
  });

  it("clamps negative input to 0", () => {
    expect(snap(-5, 100, 0, false)).toBe(0);
    expect(snap(-0.01)).toBe(0);
  });

  it("magnets to a marker inside the pixel threshold", () => {
    const p = { ...proj, markers: [{ id: "m", time_us: U(3.3), label: "", color: "", kind: "beat" as const }] };
    expect(snapTime(U(3.34), p, 0, 100, true)).toBe(U(3.3));
    expect(snapTime(U(3.34), proj, 0, 100, true)).not.toBe(U(3.3));
  });

  it("magnets to a clip edge inside the pixel threshold", () => {
    expect(snap(2.55)).toBe(U(2.5)); // 5px away
    expect(snap(2.43)).toBe(U(2.5)); // 7px away
  });

  it("does not magnet outside the threshold; falls back to the frame grid", () => {
    // 2.5 is 15px away, no other candidate within 10px
    const out = snap(2.65);
    expect(isOnFrame(F30, out)).toBe(true);
    expect(out).toBe(frameStart(F30, Math.round(2.65 * 30)));
  });

  it("threshold is measured in pixels: same time delta snaps at low zoom, not at high zoom", () => {
    expect(snap(2.55, 100)).toBe(U(2.5)); // 5px
    expect(snap(2.55, 400)).not.toBe(U(2.5)); // 20px
  });

  it("still snaps at very high zoom where 10px is below one frame period", () => {
    // 10px at 1000px/s = 10ms < 33ms frame. 8px away must still hit the edge.
    expect(snap(2.508, 1000)).toBe(U(2.5));
    expect(snap(2.52, 1000)).not.toBe(U(2.5)); // 20px
  });

  it("snaps to the timeline start (0)", () => {
    expect(snap(0.04)).toBe(0);
  });

  it("snaps to the timeline end", () => {
    expect(snap(8.3)).toBe(U(8.25));
  });

  it("a clip edge beats a closer playhead when both are within threshold", () => {
    // edge 5.5, playhead 5.54, query 5.535: playhead is 0.5px away, edge 3.5px
    expect(snap(5.535, 100, 5.54)).toBe(U(5.5));
  });

  it("the playhead beats the second grid when both are within threshold", () => {
    // grid 3, playhead 3.05, query 3.04
    expect(snap(3.04, 100, 3.05)).toBe(U(3.05));
  });

  it("among equal priorities the nearer candidate wins", () => {
    const p = project([track("v", "video", [videoClip("X", 2, 0, 0.1), videoClip("Y", 2.08, 0, 5)])]);
    // edges 2.0, 2.1, 2.08 all within 10px at 100px/s of 2.07 -> 2.08 is nearest
    expect(snapTime(U(2.07), p, 0, 100, true)).toBe(U(2.08));
  });

  it("excludeClipId removes that clip's own edges as snap targets", () => {
    const out = snap(2.52, 100, 0, true, "A");
    expect(out).toBe(frameStart(F30, Math.round(2.52 * 30)));
    expect(snap(2.52)).toBe(U(2.5));
  });

  it("falls back to 30fps when the rate is invalid", () => {
    const p = { ...proj, settings: { ...proj.settings, fps: { num: 0, den: 0 } } };
    expect(snapTime(U(0.5), p, 0, 1, false)).toBe(U(0.5));
    expect(snapTime(U(0.52), p, 0, 1, false)).toBe(frameStart(F30, 16));
  });

  it("quantises to the NTSC 29.97 grid, not a 30 fps one", () => {
    const ntsc = project([], { num: 30000, den: 1001 });
    const out = snapTime(U(1.0), ntsc, 0, 100, false);
    expect(out).toBe(frameStart({ num: 30000, den: 1001 }, 30)); // 1_001_000 µs: frame 30
    expect(out).toBe(1_001_000);
  });

  it("exports a 10px threshold", () => expect(SNAP_THRESHOLD_PX).toBe(10));
});
