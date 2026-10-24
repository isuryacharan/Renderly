// Magnetic tracks (schema v8). Core owns the ripple: on a magnetic track every command that
// changes a track's clips ends by packing it, and the UI sends the command and re-reads the
// project. The one thing the UI needs of its own is a drag *indicator* (where a dragged clip
// will land), which follows core's insert-by-centre rule; it never writes clip positions.

import type { Track } from "./types";
import { clipDurationUs } from "./types";

/**
 * Where on the timeline (µs) a clip dragged to `startUs` will land on the magnetic `track`:
 * the boundary after every other clip whose centre is at or before the dragged clip's centre
 * (renderly-core `magnetic::insert`). Indicator only.
 */
export function magneticInsertBoundaryUs(
  track: Track,
  clipId: string,
  startUs: number,
  durationUs: number,
): number {
  const centre = startUs + Math.floor(durationUs / 2);
  const others = track.clips
    .filter((c) => c.id !== clipId)
    .sort((a, b) => a.start_us - b.start_us);
  let at = 0;
  while (at < others.length) {
    const c = others[at]!;
    if (c.start_us + Math.floor(clipDurationUs(c) / 2) > centre) break;
    at += 1;
  }
  if (at === 0) return 0;
  const prev = others[at - 1]!;
  return prev.start_us + clipDurationUs(prev);
}

/** The video track the timeline-toolbar magnet acts on, see `magnetTargetTrack` (lib/text.ts). */
export function isMagneticTrack(track: Track | undefined): boolean {
  return !!track && track.magnetic === true;
}
