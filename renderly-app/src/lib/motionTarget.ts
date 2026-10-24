import { findSequence } from "./container";
import type { Clip, MediaClip, Project, Track } from "./types";
import { clipEndUs } from "./types";

/**
 * "Apply to" choices for TrackMotion: the tracked clip itself, or a video clip on another
 * video track that overlaps it in timeline time. Higher track index is drawn on top, so the
 * default is the first overlapping clip on a track above (nearest track first), else the
 * clip itself.
 */
export const MOTION_TARGET_SELF = "self";

export interface MotionTargetChoices {
  options: { value: string; label: string }[];
  /** Selected when the user has not picked one. */
  defaultValue: string;
}

function clipLabel(project: Project | null, clip: MediaClip): string {
  if (!project) return "Clip";
  const media = project.media.find((m) => m.id === clip.media_id);
  if (media?.kind === "sequence") {
    return findSequence(project, media.sequence_id ?? "")?.name ?? "Sequence";
  }
  const file = media?.path.split(/[\\/]/).pop() ?? "";
  return file.replace(/\.[^.]+$/, "") || "Clip";
}

/** `TrackMotion` target for a picker value: null when tracking the clip itself. */
export function motionTargetRef(value: string): { trackId: string; clipId: string } | null {
  if (value === MOTION_TARGET_SELF) return null;
  const sep = value.indexOf("|");
  return { trackId: value.slice(0, sep), clipId: value.slice(sep + 1) };
}

/**
 * `tracks` is the container the clip lives in (`activeTracks`), so every target is on the
 * same timeline as the tracked clip.
 */
export function motionTargetChoices(
  project: Project | null,
  tracks: Track[],
  track: Track,
  clip: MediaClip,
): MotionTargetChoices {
  const options = [{ value: MOTION_TARGET_SELF, label: "This clip" }];
  const own = tracks.findIndex((t) => t.id === track.id);
  const start = clip.start_us;
  const end = clipEndUs(clip);
  const n = tracks.length;
  const distance = (index: number) => (index > own ? index - own : n + own - index);
  // Nearest track above first, then nearest below.
  const order = tracks
    .map((t, index) => ({ t, index }))
    .filter(({ t, index }) => t.kind === "video" && index !== own)
    .sort((a, b) => distance(a.index) - distance(b.index));
  let defaultValue = MOTION_TARGET_SELF;
  for (const { t, index } of order) {
    const overlapping = t.clips
      .filter(
        (c: Clip): c is MediaClip =>
          c.type === "video" && c.start_us < end && clipEndUs(c) > start,
      )
      .sort((a, b) => a.start_us - b.start_us);
    for (const c of overlapping) {
      const value = `${t.id}|${c.id}`;
      options.push({
        value,
        label: `${clipLabel(project, c)} (${t.name}, at ${(c.start_us / 1_000_000).toFixed(1)} s)`,
      });
      if (defaultValue === MOTION_TARGET_SELF && index > own) defaultValue = value;
    }
  }
  return { options, defaultValue };
}
