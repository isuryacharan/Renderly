import { describe, expect, it } from "vitest";
import { MOTION_TARGET_SELF, motionTargetChoices, motionTargetRef } from "./motionTarget";
import type { MediaClip, Project, Track } from "./types";

function clip(id: string, position: number, duration: number): MediaClip {
  return {
    type: "video",
    id,
    media_id: "m1",
    position_secs: position,
    source_in_secs: 0,
    source_out_secs: duration,
    gain_db: 0,
    enabled: true,
    fade_in_secs: 0,
    fade_out_secs: 0,
  } as MediaClip;
}

function track(id: string, kind: "video" | "audio", clips: MediaClip[]): Track {
  return { id, kind, name: id.toUpperCase(), clips } as unknown as Track;
}

const project = {
  media: [{ id: "m1", kind: "video", path: "C:\\footage\\sticker.mp4" }],
  sequences: [],
} as unknown as Project;

describe("motionTargetChoices", () => {
  it("defaults to the first overlapping video clip on the nearest track above", () => {
    // Stack (first = bottom): v1 (tracked), v2 above, v3 above that, audio below.
    const v1 = clip("c1", 2, 4); // [2, 6)
    const v2 = clip("c2", 5, 2); // [5, 7) overlaps
    const v3a = clip("c3", 0, 1); // no overlap
    const v3b = clip("c4", 5.5, 1); // overlaps, but on a higher track
    const tracks = [
      track("v1", "video", [v1]),
      track("v2", "video", [v2]),
      track("v3", "video", [v3a, v3b]),
      track("a1", "audio", [clip("c5", 2, 4)]),
    ];
    const choices = motionTargetChoices(project, tracks, tracks[0], v1);
    expect(choices.defaultValue).toBe("v2|c2");
    expect(choices.options.map((o) => o.value)).toEqual([MOTION_TARGET_SELF, "v2|c2", "v3|c4"]);
    expect(choices.options[1].label).toContain("sticker");
  });

  it("falls back to this clip when the only overlapping clip is on a track below", () => {
    const v1 = clip("c1", 2, 4);
    // v0 (below) overlaps; v1 is the tracked clip on top, so nothing above overlaps.
    const tracks = [track("v0", "video", [clip("c0", 3, 1)]), track("v1", "video", [v1])];
    const choices = motionTargetChoices(project, tracks, tracks[1], v1);
    expect(choices.options.map((o) => o.value)).toEqual([MOTION_TARGET_SELF, "v0|c0"]);
    expect(choices.defaultValue).toBe(MOTION_TARGET_SELF);
  });

  it("ignores clips that only touch the tracked clip's end", () => {
    const v1 = clip("c1", 2, 4); // ends at 6
    const tracks = [track("v1", "video", [v1]), track("v2", "video", [clip("c2", 6, 1)])];
    expect(motionTargetChoices(project, tracks, tracks[0], v1).options).toHaveLength(1);
  });

  it("maps picker values to the TrackMotion target, or null for this clip", () => {
    expect(motionTargetRef(MOTION_TARGET_SELF)).toBeNull();
    expect(motionTargetRef("v2|c2")).toEqual({ trackId: "v2", clipId: "c2" });
  });
});
