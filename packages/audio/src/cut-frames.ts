// Where a cut's audio goes, in samples. Pure.
//
// Every boundary is rounded once from its timeline position onto the timeline's sample grid, so
// the sample count of a cut is end - start on that grid. Rounding each cut's length separately
// instead would drift by up to half a sample per cut.

import { FLICKS_PER_SECOND, type Flicks } from "@renderly/media";

/** A cut as the audio side sees it: source time [in, out) at timeline time `start`. */
export interface AudioCut {
  source: number;
  in: Flicks;
  out: Flicks;
  start: Flicks;
}

export interface CutFrames {
  /** First timeline sample to write for this cut. */
  timelineStart: number;
  /** Samples to write. */
  count: number;
  /** Source sample (from source time 0) that the first written sample comes from. */
  sourceStart: number;
}

export function toFrames(time: Flicks, rate: number): number {
  return Math.round((time * rate) / FLICKS_PER_SECOND);
}

/** The samples of `cut` from timeline time `from` on, or null if the cut ends before it. */
export function cutFrames(cut: AudioCut, from: Flicks, rate: number): CutFrames | null {
  const a = toFrames(cut.start, rate);
  const b = toFrames(cut.start + (cut.out - cut.in), rate);
  const begin = Math.max(a, toFrames(from, rate));
  if (begin >= b) return null;
  return {
    timelineStart: begin,
    count: b - begin,
    sourceStart: toFrames(cut.in, rate) + (begin - a),
  };
}
