// Timeline rules shared by the editing commands, and read-only queries on sequences.
//
// Rules every track keeps (checked by `checkTimeline`):
// - clips are sorted by start and never overlap;
// - every start and duration is a whole number of sequence frames, and a clip is at least one
//   frame long;
// - a clip never reads outside its source;
// - a magnetic track has no gaps: its first clip starts at 0 and each clip starts where the
//   previous one ends.

import type { Clip, Project, Sequence, Track } from "./schema";
import { flicksPerFrame } from "./time";

export const clipEnd = (clip: Pick<Clip, "start" | "duration">) => clip.start + clip.duration;

/** Source time a clip covers: its duration times its speed. */
export const sourceSpan = (clip: Pick<Clip, "duration" | "speed">) =>
  Math.round(clip.duration * clip.speed);

/** The end of the last clip on any track; 0 when the sequence is empty. */
export function sequenceDuration(sequence: Sequence): number {
  let end = 0;
  for (const track of sequence.tracks) {
    const last = track.clips[track.clips.length - 1];
    if (last) end = Math.max(end, clipEnd(last));
  }
  return end;
}

/** Length of what a clip plays from: a media item's duration, or a nested sequence's. */
export function sourceDuration(project: Project, clip: Pick<Clip, "media" | "sequence">): number {
  if (clip.media !== undefined) {
    const media = project.media.find((m) => m.id === clip.media);
    return media ? media.duration : 0;
  }
  const sequence = project.sequences.find((s) => s.id === clip.sequence);
  return sequence ? sequenceDuration(sequence) : 0;
}

export interface ClipRef {
  track: Track;
  clip: Clip;
}

/** Clips overlapping [from, to), optionally on some tracks only, in track then time order. */
export function clipsInRange(
  sequence: Sequence,
  from: number,
  to: number,
  trackIds?: readonly string[],
): ClipRef[] {
  const found: ClipRef[] = [];
  for (const track of sequence.tracks) {
    if (trackIds && !trackIds.includes(track.id)) continue;
    for (const clip of track.clips) {
      if (clip.start < to && clipEnd(clip) > from) found.push({ track, clip });
    }
  }
  return found;
}

/** The clip playing at time `t` on a track: start ≤ t < end. */
export function clipAt(track: Track, t: number): Clip | undefined {
  return track.clips.find((clip) => clip.start <= t && t < clipEnd(clip));
}

/**
 * Times an edit can snap to: 0, every clip start and end, and every marker, sorted and unique.
 * Clips in `exclude` (the ones being dragged) do not count; `extra` adds times such as the
 * playhead.
 */
export function snapTargets(
  sequence: Sequence,
  options: { exclude?: readonly string[]; extra?: readonly number[] } = {},
): number[] {
  const exclude = new Set(options.exclude);
  const times = new Set<number>([0, ...(options.extra ?? [])]);
  for (const track of sequence.tracks) {
    for (const clip of track.clips) {
      if (exclude.has(clip.id)) continue;
      times.add(clip.start);
      times.add(clipEnd(clip));
    }
  }
  for (const marker of sequence.markers) times.add(marker.t);
  return [...times].sort((a, b) => a - b);
}

/** The target nearest to `t` within `tolerance`, or `t` itself. */
export function snapTo(targets: readonly number[], t: number, tolerance: number): number {
  let best = t;
  let distance = tolerance;
  for (const target of targets) {
    const d = Math.abs(target - t);
    if (d <= distance) {
      best = target;
      distance = d;
    }
  }
  return best;
}

/** Every broken timeline rule in the project, as readable messages. Empty means valid. */
export function checkTimeline(project: Project): string[] {
  const problems: string[] = [];
  for (const sequence of project.sequences) {
    const frame = flicksPerFrame(sequence.fps);
    for (const track of sequence.tracks) {
      let previousEnd = 0;
      for (const clip of track.clips) {
        const where = `clip ${clip.id} on track ${track.id}`;
        if (clip.duration < frame) problems.push(`${where} is shorter than one frame`);
        if (clip.start % frame !== 0 || clip.duration % frame !== 0)
          problems.push(`${where} is off the frame grid`);
        if (clip.start < previousEnd) problems.push(`${where} overlaps the clip before it`);
        if (track.magnetic && clip.start !== previousEnd)
          problems.push(`${where} leaves a gap on a magnetic track`);
        if (clip.in < 0 || clip.in + sourceSpan(clip) > sourceDuration(project, clip))
          problems.push(`${where} reads outside its source`);
        previousEnd = clipEnd(clip);
      }
    }
  }
  return problems;
}
