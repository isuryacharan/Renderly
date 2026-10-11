// Shared builders for unit tests (not imported by app code, so never bundled).

import type { CaptionClip, MediaClip, Project, Track } from "../lib/types";

export function videoClip(id: string, position: number, srcIn: number, srcOut: number, extra: Partial<MediaClip> = {}): MediaClip {
  return {
    type: "video",
    id,
    media_id: `m-${id}`,
    position_secs: position,
    source_in_secs: srcIn,
    source_out_secs: srcOut,
    gain_db: 0,
    enabled: true,
    fade_in_secs: 0,
    fade_out_secs: 0,
    ...extra,
  };
}

export function audioClip(id: string, position: number, srcIn: number, srcOut: number): MediaClip {
  return { ...videoClip(id, position, srcIn, srcOut), type: "audio" };
}

export function captionClip(id: string, position: number, duration: number): CaptionClip {
  return { type: "caption", id, text: "hi", position_secs: position, duration_secs: duration, style_id: "tiktok-minimal" };
}

export function track(id: string, kind: Track["kind"], clips: Track["clips"] = []): Track {
  return { id, kind, name: id, clips, muted: false, locked: false, hidden: false };
}

export function project(tracks: Track[], fps = 30): Project {
  return {
    schema_version: 6,
    id: "p",
    name: "test",
    settings: { fps, width: 1920, height: 1080, sample_rate: 48000, duck_db: -12 },
    media: [],
    tracks,
  };
}
