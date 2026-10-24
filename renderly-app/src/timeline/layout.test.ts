import { describe, expect, it } from "vitest";
import { audioClip, captionClip, project, textClip, track, videoClip } from "../test/fixtures";
import {
  CLIP_BOTTOM_PAD,
  CLIP_TOP_PAD,
  clipLeft,
  contentHeightPx,
  contentWidthPx,
  CONTENT_PAD_X,
  displayOrder,
  dropTargetAtY,
  hitTestClip,
  hitTestTransitionJunction,
  listTransitionJunctions,
  maxScrollX,
  maxScrollY,
  moveTrackNewIndex,
  nextVideoClipOnTrack,
  RULER_H,
  rowForTrackIndex,
  timelineDurationUs,
  TRACK_GAP,
  TRACK_H,
  TRACK_LABEL_W,
  trackLayout,
  usFromCanvasX,
  zoneRowBounds,
} from "./layout";

describe("time <-> pixel conversion", () => {
  it("clipLeft offsets by the label gutter and padding", () => {
    expect(clipLeft(0, 100)).toBe(TRACK_LABEL_W + CONTENT_PAD_X);
    expect(clipLeft(2_000_000, 50)).toBe(TRACK_LABEL_W + CONTENT_PAD_X + 100);
  });

  it("clipLeft subtracts scroll", () => {
    expect(clipLeft(1_000_000, 100, 30)).toBe(TRACK_LABEL_W + CONTENT_PAD_X + 70);
  });

  it("usFromCanvasX inverts clipLeft to the exact integer µs", () => {
    for (const [us, px, scroll] of [
      [0, 80, 0],
      [3_250_000, 80, 0],
      [12_500_000, 200, 340],
      [40_000, 400, 10],
      [33_333, 100, 0],
    ] as const) {
      expect(usFromCanvasX(clipLeft(us, px, scroll), px, scroll)).toBe(us);
    }
  });

  it("usFromCanvasX rounds pointer positions to the nearest µs", () => {
    // 1px at 3px/s is 333333.33 µs
    expect(usFromCanvasX(TRACK_LABEL_W + CONTENT_PAD_X + 1, 3)).toBe(333_333);
    expect(usFromCanvasX(TRACK_LABEL_W + CONTENT_PAD_X + 2, 3)).toBe(666_667);
  });

  it("usFromCanvasX never returns negative time (clicks in the label gutter)", () => {
    expect(usFromCanvasX(0, 100)).toBe(0);
    expect(usFromCanvasX(TRACK_LABEL_W - 10, 100)).toBe(0);
  });
});

describe("timelineDurationUs & content extents", () => {
  it("is at least one second for an empty timeline", () => {
    expect(timelineDurationUs(project([]))).toBe(1_000_000);
    expect(timelineDurationUs(project([track("v", "video")]))).toBe(1_000_000);
  });

  it("uses the furthest clip end across all tracks", () => {
    const p = project([
      track("v", "video", [videoClip("a", 0, 0, 3)]),
      track("a", "audio", [audioClip("b", 5, 0, 4)]),
    ]);
    expect(timelineDurationUs(p)).toBe(9_000_000);
  });

  it("honours clip speed and caption durations", () => {
    const fast = videoClip("a", 2, 0, 8, { speed: 4 }); // 2s long
    const slow = videoClip("b", 0, 0, 1, { speed: 0.5 }); // 2s long
    expect(timelineDurationUs(project([track("v", "video", [fast, slow])]))).toBe(4_000_000);
    expect(timelineDurationUs(project([track("c", "caption", [captionClip("x", 1, 6)])]))).toBe(7_000_000);
  });

  it("treats speed <= 0 as 1x rather than dividing by zero", () => {
    const p = project([track("v", "video", [videoClip("a", 0, 0, 4, { speed: 0 })])]);
    expect(timelineDurationUs(p)).toBe(4_000_000);
  });

  it("contentWidthPx / maxScrollX scale with zoom and never go negative", () => {
    const p = project([track("v", "video", [videoClip("a", 0, 0, 10)])]);
    expect(contentWidthPx(p, 100)).toBe(TRACK_LABEL_W + CONTENT_PAD_X + 1000 + 200);
    expect(maxScrollX(p, 100, 500)).toBe(contentWidthPx(p, 100) - 500);
    expect(maxScrollX(p, 10, 5000)).toBe(0);
  });

  it("contentHeightPx / maxScrollY reserve at least one lane", () => {
    expect(contentHeightPx(0)).toBe(contentHeightPx(1));
    expect(contentHeightPx(3)).toBe(RULER_H + 8 + 3 * (TRACK_H + TRACK_GAP) + 40);
    expect(maxScrollY(2, 10_000)).toBe(0);
    expect(maxScrollY(10, 100)).toBe(contentHeightPx(10) - 100);
  });

  it("trackLayout places lanes at a fixed pitch and subtracts scrollY", () => {
    const { laneTop, trackH } = trackLayout(500, 3, 10);
    expect(trackH).toBe(TRACK_H);
    expect(laneTop(1) - laneTop(0)).toBe(TRACK_H + TRACK_GAP);
    expect(laneTop(0)).toBe(RULER_H + 4 - 10);
  });
});

describe("displayOrder / rowForTrackIndex / zoneRowBounds", () => {
  // array order (bottom -> top compositing): a1, v1, c1, v2, a2
  const p = project([
    track("a1", "audio"),
    track("v1", "video"),
    track("c1", "caption"),
    track("v2", "video"),
    track("a2", "audio"),
  ]);

  it("shows overlay tracks top-down in reverse array order, then audio in array order", () => {
    expect(displayOrder(p).map((d) => d.track.id)).toEqual(["v2", "c1", "v1", "a1", "a2"]);
  });

  it("carries the original project.tracks index", () => {
    expect(displayOrder(p).map((d) => d.trackIndex)).toEqual([3, 2, 1, 0, 4]);
  });

  it("rowForTrackIndex is the inverse mapping, -1 when missing", () => {
    expect(rowForTrackIndex(p, 3)).toBe(0);
    expect(rowForTrackIndex(p, 0)).toBe(3);
    expect(rowForTrackIndex(p, 4)).toBe(4);
    expect(rowForTrackIndex(p, 99)).toBe(-1);
  });

  it("zoneRowBounds confines each kind to its own visual zone", () => {
    expect(zoneRowBounds(p, "video")).toEqual([0, 2]);
    expect(zoneRowBounds(p, "caption")).toEqual([0, 2]);
    expect(zoneRowBounds(p, "audio")).toEqual([3, 4]);
  });

  it("zoneRowBounds degrades sanely for single-zone projects", () => {
    const onlyVideo = project([track("v", "video")]);
    expect(zoneRowBounds(onlyVideo, "video")).toEqual([0, 0]);
    expect(zoneRowBounds(onlyVideo, "audio")).toEqual([1, 1]);
    expect(zoneRowBounds(project([]), "video")).toEqual([0, 0]);
  });
});

describe("moveTrackNewIndex", () => {
  const p = project([track("v1", "video"), track("v2", "video"), track("a1", "audio")]);
  // display rows: v2 (0), v1 (1), a1 (2)

  it("returns the MoveTrack index that lands the track on the requested row", () => {
    expect(moveTrackNewIndex(p, "v1", 0)).toBe(1);
    expect(moveTrackNewIndex(p, "v2", 1)).toBe(0);
  });

  it("is consistent with actually applying the move (remove then insert)", () => {
    for (const id of ["v1", "v2"]) {
      for (const row of [0, 1]) {
        const idx = moveTrackNewIndex(p, id, row);
        if (idx === null) continue;
        const tracks = p.tracks.slice();
        const [moved] = tracks.splice(
          tracks.findIndex((t) => t.id === id),
          1,
        );
        tracks.splice(idx, 0, moved);
        const order = displayOrder({ ...p, tracks });
        expect(order[row].track.id).toBe(id);
      }
    }
  });

  it("returns null for a no-op, unknown track, or unreachable row", () => {
    expect(moveTrackNewIndex(p, "v1", 1)).toBeNull();
    expect(moveTrackNewIndex(p, "nope", 0)).toBeNull();
    expect(moveTrackNewIndex(p, "v1", 2)).toBeNull(); // would cross into the audio zone
  });
});

describe("dropTargetAtY", () => {
  const p = project([track("a", "audio"), track("v1", "video"), track("v2", "video")]);
  // rows: v2 (idx2), v1 (idx1), a (idx0). laneTop(i) = 28 + 54 i
  it("above the top lane means a new track on top", () => {
    expect(dropTargetAtY(p, 400, 0)).toEqual({ trackIndex: null, row: -1 });
  });
  it("resolves rows to project.tracks indices through displayOrder", () => {
    expect(dropTargetAtY(p, 400, 40)).toEqual({ trackIndex: 2, row: 0 });
    expect(dropTargetAtY(p, 400, 90)).toEqual({ trackIndex: 1, row: 1 });
    expect(dropTargetAtY(p, 400, 150)).toEqual({ trackIndex: 0, row: 2 });
  });
  it("below the bottom lane means a new track at the bottom", () => {
    expect(dropTargetAtY(p, 400, 1000)).toEqual({ trackIndex: null, row: 3 });
  });
  it("accounts for vertical scroll", () => {
    // scrolled by one lane pitch, y=40 now falls in the second row
    expect(dropTargetAtY(p, 400, 40, TRACK_H + TRACK_GAP)).toEqual({ trackIndex: 1, row: 1 });
  });
  it("with no tracks, everything is 'new track below'", () => {
    expect(dropTargetAtY(project([]), 400, 0)).toEqual({ trackIndex: null, row: 0 });
  });
});

describe("hitTestClip", () => {
  // one clip at 1s, 2s long, 100px/s -> x in [196, 396]; lane 0 y in [28, 76]
  const clip = videoClip("c", 1, 0, 2);
  const p = project([track("v", "video", [clip])]);
  const hit = (x: number, y = 50, scrollX = 0, scrollY = 0) => hitTestClip(p, x, y, 400, 100, scrollX, scrollY);

  it("reports the left trim handle, body and right trim handle", () => {
    expect(hit(198)?.edge).toBe("left");
    expect(hit(300)?.edge).toBe("body");
    expect(hit(392)?.edge).toBe("right");
  });

  it("returns ids and track index", () => {
    expect(hit(300)).toMatchObject({ trackId: "v", trackIndex: 0, clip });
  });

  it("misses outside the clip horizontally", () => {
    expect(hit(150)).toBeNull();
    expect(hit(500)).toBeNull();
  });

  it("misses in the lane's top padding and outside the lane", () => {
    expect(hit(300, 28 + CLIP_TOP_PAD - 1)).toBeNull();
    expect(hit(300, 28 - 1)).toBeNull();
    expect(hit(300, 28 + TRACK_H + 1)).toBeNull();
  });

  it("follows horizontal scroll", () => {
    expect(hit(100, 50, 100)?.edge).toBe("left"); // clip left is now 96
    expect(hit(290, 50, 100)?.edge).toBe("right");
  });

  it("follows vertical scroll", () => {
    expect(hit(300, 50, 0, 54)).toBeNull(); // lane scrolled up out of reach
    expect(hit(300, 50, 0, 0)).not.toBeNull();
  });

  it("hit-tests the visually top lane as the last track in the array", () => {
    const two = project([
      track("bottom", "video", [videoClip("b", 0, 0, 5)]),
      track("top", "video", [videoClip("t", 0, 0, 5)]),
    ]);
    expect(hitTestClip(two, 200, 50, 400, 100)?.trackId).toBe("top");
    expect(hitTestClip(two, 200, 50 + TRACK_H + TRACK_GAP, 400, 100)?.trackId).toBe("bottom");
  });

  it("uses speed-adjusted width", () => {
    const fast = project([track("v", "video", [videoClip("c", 0, 0, 4, { speed: 2 })])]); // 2s -> 200px
    expect(hitTestClip(fast, 88 + 8 + 150, 50, 400, 100)?.edge).toBe("body");
    expect(hitTestClip(fast, 88 + 8 + 250, 50, 400, 100)).toBeNull();
  });
});

describe("nextVideoClipOnTrack", () => {
  const a = videoClip("a", 0, 0, 2);
  const b = videoClip("b", 2, 0, 1);
  const c = videoClip("c", 5, 0, 1);

  it("finds the next video clip even if the track array is unordered", () => {
    expect(nextVideoClipOnTrack(track("v", "video", [c, b, a]), a)?.id).toBe("b");
    expect(nextVideoClipOnTrack(track("v", "video", [c, b, a]), b)?.id).toBe("c");
  });
  it("returns null for the last clip", () => {
    expect(nextVideoClipOnTrack(track("v", "video", [a, b, c]), c)).toBeNull();
  });
  it("skips non-video clips", () => {
    const t = track("v", "video", [a, audioClip("x", 2, 0, 1), captionClip("y", 2, 1), c]);
    expect(nextVideoClipOnTrack(t, a)?.id).toBe("c");
  });
  it("ignores clips that start before this clip ends (overlap)", () => {
    const overlapping = videoClip("o", 1, 0, 2);
    expect(nextVideoClipOnTrack(track("v", "video", [a, overlapping]), a)).toBeNull();
  });
  it("pairs a video clip with a following title and the reverse (both carry transitions)", () => {
    const title = textClip("t", 2, 1);
    expect(nextVideoClipOnTrack(track("v", "video", [a, title]), a)?.id).toBe("t");
    expect(nextVideoClipOnTrack(track("v", "video", [title, c]), title)?.id).toBe("c");
  });
  it("skips a disabled clip, like the engine", () => {
    const off = textClip("off", 2, 1);
    off.enabled = false;
    expect(nextVideoClipOnTrack(track("v", "video", [a, off, c]), a)?.id).toBe("c");
  });
  it("tolerates a one-microsecond gap", () => {
    const nearly = videoClip("n", 2, 0, 1, { start_us: 2_000_000 - 1 });
    expect(nextVideoClipOnTrack(track("v", "video", [a, nearly]), a)?.id).toBe("n");
  });
});

describe("transition junctions", () => {
  const a = videoClip("a", 0, 0, 1, { outgoing_transition: { ref: "builtin:crossfade@1", duration_us: 500_000, params: {}, easing: null } });
  const b = videoClip("b", 1, 0, 1);
  const p = project([track("v", "video", [a, b]), track("au", "audio", [audioClip("x", 0, 0, 1), audioClip("y", 1, 0, 1)])]);

  it("lists one junction per adjacent video pair and none for audio lanes", () => {
    const js = listTransitionJunctions(p, 400, 100);
    expect(js).toHaveLength(1);
    expect(js[0]).toMatchObject({ trackId: "v", outgoingClipId: "a", junctionUs: 1_000_000 });
    expect(js[0].junctionX).toBe(clipLeft(1_000_000, 100));
  });

  it("derives overlapPx from the transition (min 4px) and 0 with no transition", () => {
    expect(listTransitionJunctions(p, 400, 100)[0].overlapPx).toBe(50);
    expect(listTransitionJunctions(p, 400, 2)[0].overlapPx).toBe(4);
    const bare = project([track("v", "video", [videoClip("a", 0, 0, 1), b])]);
    expect(listTransitionJunctions(bare, 400, 100)[0]).toMatchObject({ transition: null, overlapPx: 0 });
  });

  it("body rect excludes the lane padding", () => {
    const j = listTransitionJunctions(p, 400, 100)[0];
    expect(j.bodyY).toBe(j.laneY + CLIP_TOP_PAD);
    expect(j.bodyH).toBe(TRACK_H - CLIP_TOP_PAD - CLIP_BOTTOM_PAD);
  });

  it("hit-tests the badge just inside the outgoing clip and a small zone around the seam", () => {
    const jx = clipLeft(1_000_000, 100);
    const y = 50;
    expect(hitTestTransitionJunction(p, jx - 8, y, 400, 100)).toEqual({ trackId: "v", clipId: "a", trackIndex: 0 });
    expect(hitTestTransitionJunction(p, jx + 5, y, 400, 100)?.clipId).toBe("a");
    expect(hitTestTransitionJunction(p, jx + 30, y, 400, 100)).toBeNull();
    expect(hitTestTransitionJunction(p, jx - 40, y, 400, 100)).toBeNull();
  });

  it("the seam zone (right of the junction) respects vertical insets but the badge zone doesn't", () => {
    const jx = clipLeft(1_000_000, 100);
    const nearTop = 28 + CLIP_TOP_PAD + 1; // inside body, within the 4px inset
    expect(hitTestTransitionJunction(p, jx + 5, nearTop, 400, 100)).toBeNull();
    expect(hitTestTransitionJunction(p, jx - 8, nearTop, 400, 100)).not.toBeNull();
    expect(hitTestTransitionJunction(p, jx - 8, 28 + 2, 400, 100)).toBeNull(); // above body
  });
});
