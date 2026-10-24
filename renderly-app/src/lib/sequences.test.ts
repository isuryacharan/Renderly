import { describe, expect, it } from "vitest";
import {
  clipDurationUs,
  mediaDurationUs,
  sequenceDurationUs,
  sequenceMediaName,
  type AdjustmentClip,
  type MediaItem,
  type Project,
} from "./types";

const adjustment: AdjustmentClip = {
  type: "adjustment",
  id: "a1",
  start_us: 2_000_000,
  duration_us: 4_000_000,
  enabled: true,
};

function project(): Project {
  return {
    schema_version: 7,
    id: "p",
    name: "p",
    settings: { fps: { num: 30, den: 1 }, width: 1920, height: 1080, sample_rate: 48000, duck_db: -12 },
    media: [
      { id: "m1", path: "a.mp4", kind: "video", duration_us: 10_000_000, width: 1, height: 1, fps: 30 },
      {
        id: "sm",
        path: "",
        kind: "sequence",
        sequence_id: "s1",
        duration_us: null,
        width: null,
        height: null,
        fps: null,
      },
    ],
    tracks: [],
    sequences: [
      {
        id: "s1",
        name: "Intro",
        tracks: [
          {
            id: "t1",
            kind: "video",
            name: "V",
            muted: false,
            locked: false,
            hidden: false,
            clips: [
              adjustment,
              {
                type: "video",
                id: "c1",
                media_id: "m1",
                start_us: 1_000_000,
                source_in_us: 0,
                source_out_us: 3_000_000,
                gain_db: 0,
                enabled: true,
                fade_in_us: 0,
                fade_out_us: 0,
              },
            ],
          },
        ],
      },
    ],
  };
}

describe("sequence helpers (mirror the engine's derived media fields)", () => {
  it("adjustment clips use their own duration", () => {
    expect(clipDurationUs(adjustment)).toBe(4_000_000);
  });

  it("sequence duration is the end of the last enabled clip, adjustment layers included", () => {
    const p = project();
    expect(sequenceDurationUs(p, "s1")).toBe(6_000_000);
    // Disabling the adjustment layer shortens it to the media clip's end (1 + 3).
    adjustment.enabled = false;
    expect(sequenceDurationUs(p, "s1")).toBe(4_000_000);
    adjustment.enabled = true;
    expect(sequenceDurationUs(p, "nope")).toBe(0);
  });

  it("media duration and name are derived for sequence media only", () => {
    const p = project();
    const [file, seq] = p.media as [MediaItem, MediaItem];
    expect(mediaDurationUs(p, file)).toBe(10_000_000);
    expect(mediaDurationUs(p, seq)).toBe(6_000_000);
    expect(sequenceMediaName(p, file)).toBeUndefined();
    expect(sequenceMediaName(p, seq)).toBe("Intro");
  });
});
