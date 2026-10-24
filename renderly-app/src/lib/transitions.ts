// Limits and defaults for the outgoing transition of a video clip.

import type { CatalogueEntry } from "./ipc";
import { nextVideoClipOnTrack, type TransitionLayer } from "../timeline/layout";
import { clipDurationUs, type Track } from "./types";

/** Shortest transition (µs). */
export const MIN_TRANSITION_US = 50_000;
/** Duration a new transition starts with (µs), before clamping to the clips. */
export const DEFAULT_TRANSITION_US = 500_000;

/** The video clip that follows `clip` on its track (the transition's incoming side). */
export const nextVideoClip = nextVideoClipOnTrack;

/** Longest transition the pair of clips allows: half of the shorter one. */
export function maxTransitionUs(track: Track, clip: TransitionLayer): number {
  const next = nextVideoClip(track, clip);
  const own = clipDurationUs(clip);
  return next ? Math.min(own, clipDurationUs(next)) / 2 : own / 2;
}

/** Duration for a newly applied transition: the clip's current one if any, else the default,
 *  never beyond what the clips allow and never below the minimum. */
export function transitionDurationFor(
  track: Track,
  clip: TransitionLayer,
  preferredUs: number = DEFAULT_TRANSITION_US,
): number {
  const current = clip.outgoing_transition?.duration_us;
  const max = Math.max(MIN_TRANSITION_US, maxTransitionUs(track, clip));
  return Math.round(Math.min(current ?? preferredUs, max));
}

export function transitionEntries(catalogue: readonly CatalogueEntry[]): CatalogueEntry[] {
  return catalogue.filter((e) => e.kind === "transition" && e.state === "ok");
}
