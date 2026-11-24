// The spike's edit list and the decoder scheduling decisions on it. Pure: no workers, no GPU.
//
// A cut shows source time [in, out) of one file at timeline time [start, start + out - in).
// Consecutive cuts that continue the same footage (same file, next in = previous out, no gap)
// are merged into one run, so the decoder plays straight through split points instead of
// seeking. Each run needs its own decoder stream.

import type { Flicks } from "@renderly/media";

export interface Cut {
  /** Index into the list of source files. */
  source: number;
  in: Flicks;
  out: Flicks;
  start: Flicks;
}

export interface Run {
  source: number;
  in: Flicks;
  out: Flicks;
  start: Flicks;
  end: Flicks;
}

/** Sorts cuts by start and merges continuous ones into runs. Rejects overlaps and empty cuts. */
export function toRuns(cuts: readonly Cut[]): Run[] {
  const sorted = [...cuts].sort((a, b) => a.start - b.start);
  const runs: Run[] = [];
  for (const cut of sorted) {
    if (cut.out <= cut.in) throw new RangeError(`Empty cut at ${cut.start}`);
    const end = cut.start + (cut.out - cut.in);
    const last = runs[runs.length - 1];
    if (last && cut.start < last.end) throw new RangeError(`Cuts overlap at ${cut.start}`);
    if (last && last.source === cut.source && last.out === cut.in && last.end === cut.start) {
      last.out = cut.out;
      last.end = end;
    } else {
      runs.push({ source: cut.source, in: cut.in, out: cut.out, start: cut.start, end });
    }
  }
  return runs;
}

/** Index of the run that covers timeline time `t`, or -1 in a gap or past the end. */
export function runAt(runs: readonly Run[], t: Flicks): number {
  let lo = 0;
  let hi = runs.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const run = runs[mid] as Run;
    if (t < run.start) hi = mid - 1;
    else if (t >= run.end) lo = mid + 1;
    else return mid;
  }
  return -1;
}

export interface PlanOptions {
  /** Runs after the current one that always get a decoder ready. */
  minAhead: number;
  /** Also prepare every run starting within this much time of the playhead... */
  lookahead: Flicks;
  /** ...up to this many decoder streams in total. */
  maxStreams: number;
}

/**
 * Which runs should have a decoder stream now: the run at `t` (or the next one, in a gap), the
 * `minAhead` runs after it, and any further runs starting within `lookahead`, capped at
 * `maxStreams`. Short clips shift the window by time, so a run of clips a few frames long never
 * leaves the next cut without a prepared decoder.
 */
export function plan(runs: readonly Run[], t: Flicks, options: PlanOptions): number[] {
  let first = runAt(runs, t);
  if (first < 0) first = runs.findIndex((run) => run.start > t);
  if (first < 0) return [];
  const wanted: number[] = [];
  for (let i = first; i < runs.length && wanted.length < options.maxStreams; i++) {
    const ahead = i - first;
    if (ahead > options.minAhead && (runs[i] as Run).start >= t + options.lookahead) break;
    wanted.push(i);
  }
  return wanted;
}

/** Source time shown at timeline time `t` within `run`. */
export function sourceTime(run: Run, t: Flicks): Flicks {
  return run.in + (t - run.start);
}

/** Timeline time of source time `s` within `run`. */
export function timelineTime(run: Run, s: Flicks): Flicks {
  return run.start + (s - run.in);
}
