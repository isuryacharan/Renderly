// Shared builders for unit tests (not imported by app code, so never bundled).
// Time arguments are in SECONDS for readability; the builders store integer µs (schema v8).

import { fpsFromNumber, secsToUs, type Fps } from "../lib/time";
import type { CaptionClip, MediaClip, Project, TextClip, Track } from "../lib/types";

export function videoClip(id: string, position: number, srcIn: number, srcOut: number, extra: Partial<MediaClip> = {}): MediaClip {
  return {
    type: "video",
    id,
    media_id: `m-${id}`,
    start_us: secsToUs(position),
    source_in_us: secsToUs(srcIn),
    source_out_us: secsToUs(srcOut),
    gain_db: 0,
    enabled: true,
    fade_in_us: 0,
    fade_out_us: 0,
    ...extra,
  };
}

export function audioClip(id: string, position: number, srcIn: number, srcOut: number): MediaClip {
  return { ...videoClip(id, position, srcIn, srcOut), type: "audio" };
}

export function captionClip(id: string, position: number, duration: number, text = "hi"): CaptionClip {
  return {
    type: "caption",
    id,
    start_us: secsToUs(position),
    duration_us: secsToUs(duration),
    body: { text, runs: [], words: [] },
    style: null,
    animations: {},
  };
}

export function textClip(id: string, position: number, duration: number, text = "Title"): TextClip {
  return {
    type: "text",
    id,
    start_us: secsToUs(position),
    duration_us: secsToUs(duration),
    enabled: true,
    body: { text, runs: [], words: [] },
    style: { ref: "builtin:tiktok-minimal@1", params: {}, overrides: { anchor: "center" } },
    animations: {},
  };
}

export function track(id: string, kind: Track["kind"], clips: Track["clips"] = []): Track {
  return { id, kind, name: id, clips, muted: false, locked: false, hidden: false };
}

export function project(tracks: Track[], fps: number | Fps = 30): Project {
  return {
    schema_version: 8,
    id: "p",
    name: "test",
    settings: { fps: typeof fps === "number" ? fpsFromNumber(fps) : fps, width: 1920, height: 1080, sample_rate: 48000, duck_db: -12 },
    media: [],
    tracks,
  };
}
