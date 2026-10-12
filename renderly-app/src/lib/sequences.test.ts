import { describe, expect, it } from "vitest";
import {
  clipDurationSecs,
  mediaDurationSecs,
  sequenceDurationSecs,
  sequenceMediaName,
  type AdjustmentClip,
  type MediaItem,
  type Project,
} from "./types";

const adjustment: AdjustmentClip = {
  type: "adjustment",
  id: "a1",
  position_secs: 2,
  duration_secs: 4,
  enabled: true,
};

function project(): Project {
  return {
    schema_version: 7,
    id: "p",
    name: "p",
    settings: { fps: 30, width: 1920, height: 1080, sample_rate: 48000, duck_db: -12 },
    media: [
      { id: "m1", path: "a.mp4", kind: "video", duration_secs: 10, width: 1, height: 1, fps: 30 },
      {
        id: "sm",
        path: "",
        kind: "sequence",
        sequence_id: "s1",
        duration_secs: null,
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
                position_secs: 1,
                source_in_secs: 0,
                source_out_secs: 3,
                gain_db: 0,
                enabled: true,
                fade_in_secs: 0,
                fade_out_secs: 0,
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
    expect(clipDurationSecs(adjustment)).toBe(4);
  });

  it("sequence duration is the end of the last enabled clip, adjustment layers included", () => {
    const p = project();
    expect(sequenceDurationSecs(p, "s1")).toBe(6);
    // Disabling the adjustment layer shortens it to the media clip's end (1 + 3).
    adjustment.enabled = false;
    expect(sequenceDurationSecs(p, "s1")).toBe(4);
    adjustment.enabled = true;
    expect(sequenceDurationSecs(p, "nope")).toBe(0);
  });

  it("media duration and name are derived for sequence media only", () => {
    const p = project();
    const [file, seq] = p.media as [MediaItem, MediaItem];
    expect(mediaDurationSecs(p, file)).toBe(10);
    expect(mediaDurationSecs(p, seq)).toBe(6);
    expect(sequenceMediaName(p, file)).toBeUndefined();
    expect(sequenceMediaName(p, seq)).toBe("Intro");
  });
});
