// Snap candidates + magnet resolver (improvement-plan C2). Extracted from layout.ts so
// geometry helpers stay pure and interactions/canvas import snapping from one place.

import { normalizeFps, snapToFrameUs, US_PER_SEC, usToPx } from "../lib/time";
import { snapPoints } from "../lib/text";
import { clipEndUs, type Project } from "../lib/types";
import { timelineDurationUs } from "./layout";

/// Snap source priority — lower wins ties and, more importantly, wins when a
/// lower-priority candidate happens to sit closer in time but a higher-priority one is
/// still within the pixel threshold (e.g. a clip edge a few frames from the playhead
/// shouldn't lose to the playhead just because it's marginally farther in seconds).
export const SNAP_PRIORITY_EDGE = 0;
export const SNAP_PRIORITY_PLAYHEAD = 1;
export const SNAP_PRIORITY_GRID = 2;

export interface SnapCandidate {
  /** Candidate time in integer µs. */
  time: number;
  priority: number;
}

/// Clip edges, markers (+ timeline start/end) and the playhead are the "magnet" sources — always
/// present regardless of duration. The coarse 1-second grid is the lowest-priority
/// fallback: it used to compete on equal footing with real edges (`collectSnapTimes`'s
/// old flat `number[]`), which flooded the candidate set at low zoom and made clip edges
/// lose to whichever grid line happened to be a hair closer.
export function collectSnapCandidates(
  project: Project,
  playheadUs: number,
  excludeClipId?: string,
): SnapCandidate[] {
  const byTime = new Map<number, number>();
  const offer = (time: number, priority: number) => {
    const existing = byTime.get(time);
    if (existing === undefined || priority < existing) byTime.set(time, priority);
  };

  offer(0, SNAP_PRIORITY_EDGE);
  offer(timelineDurationUs(project), SNAP_PRIORITY_EDGE);
  // Clip edges and markers: the same set as core's `Project::snap_points()` (lib/text.ts).
  for (const time of snapPoints(project.tracks, project.markers, clipEndUs, excludeClipId)) {
    offer(time, SNAP_PRIORITY_EDGE);
  }
  offer(playheadUs, SNAP_PRIORITY_PLAYHEAD);
  // Coarse second grid (not every frame — that floods the set at long durations).
  const duration = timelineDurationUs(project);
  for (let t = US_PER_SEC; t < duration; t += US_PER_SEC) offer(t, SNAP_PRIORITY_GRID);

  return [...byTime.entries()].map(([time, priority]) => ({ time, priority }));
}

/// Magnet threshold in **screen pixels**, deliberately not converted to a time value —
/// `10 / pxPerSec` shrinks below one frame period at high zoom (`pxPerSec` >= ~300 at
/// 30fps), which silently disabled snapping exactly when a pixel-perfect magnet matters
/// most. Comparing in pixels keeps the magnet feel constant across zoom levels.
export const SNAP_THRESHOLD_PX = 10;

/// `us` and the result are integer µs. With snapping off the result is still quantised to the
/// nearest frame boundary of the project rate.
export function snapTime(
  us: number,
  project: Project,
  playheadUs: number,
  pxPerSec: number,
  snapEnabled: boolean,
  excludeClipId?: string,
): number {
  const framed = snapToFrameUs(normalizeFps(project.settings.fps), Math.max(0, us));
  if (!snapEnabled) return framed;

  let best: number | null = null;
  let bestPriority = Infinity;
  let bestPxDist = SNAP_THRESHOLD_PX;
  for (const { time, priority } of collectSnapCandidates(project, playheadUs, excludeClipId)) {
    const pxDist = usToPx(Math.abs(time - us), pxPerSec);
    if (pxDist > SNAP_THRESHOLD_PX) continue;
    if (priority < bestPriority || (priority === bestPriority && pxDist < bestPxDist)) {
      bestPriority = priority;
      bestPxDist = pxDist;
      best = time;
    }
  }
  // Prefer a magnet hit; otherwise keep the frame-quantized value.
  return best !== null ? best : framed;
}
