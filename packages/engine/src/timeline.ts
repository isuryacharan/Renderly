// The spike's edit list and the decoder scheduling decisions on it. Pure: no workers, no GPU.
//
// A cut shows source time [in, out) of one file at timeline time [start, start + out - in), on a
// layer (0 is the bottom) with a static transform. Consecutive cuts on a layer that continue the
// same footage (same file and transform, next in = previous out, no gap) are merged into one
// run, so the decoder plays straight through split points instead of seeking. Each run needs
// its own decoder stream.

import type { Flicks } from "@renderly/media";

/**
 * Where a layer sits in the frame: core's `LayerTransform`. `x` and `y` place the anchor as a
 * fraction of the frame from its centre (right and down); `scaleX`/`scaleY` 1 fit the layer to the
 * frame; `rotation` is degrees clockwise about the anchor; `anchorX`/`anchorY` are a fraction of
 * the layer from its centre; `flipX`/`flipY` mirror it; `opacity` is 0 to 1.
 */
export interface Transform {
  x: number;
  y: number;
  scaleX: number;
  scaleY: number;
  rotation: number;
  anchorX: number;
  anchorY: number;
  flipX: boolean;
  flipY: boolean;
  opacity: number;
}

export const IDENTITY: Transform = {
  x: 0,
  y: 0,
  scaleX: 1,
  scaleY: 1,
  rotation: 0,
  anchorX: 0,
  anchorY: 0,
  flipX: false,
  flipY: false,
  opacity: 1,
};

/** A transform given in part; `scale` sets both axes. Everything left out is the identity. */
export type TransformInput = Partial<Transform> & { scale?: number };

export function toTransform(input: TransformInput | undefined): Transform {
  if (!input) return IDENTITY;
  const { scale, ...rest } = input;
  return {
    ...IDENTITY,
    ...(scale === undefined ? {} : { scaleX: scale, scaleY: scale }),
    ...rest,
  };
}

export interface Cut {
  /** Index into the list of source files. */
  source: number;
  in: Flicks;
  out: Flicks;
  start: Flicks;
  /** 0 is the bottom layer. Default 0. */
  layer?: number;
  transform?: TransformInput;
  /** Layer 0's cuts give the sound (until the audio engine mixes layers); muted ones do not. */
  muted?: boolean;
}

export interface Run {
  source: number;
  in: Flicks;
  out: Flicks;
  start: Flicks;
  end: Flicks;
  layer: number;
  transform: Transform;
}

const sameTransform = (a: Transform, b: Transform) =>
  (Object.keys(IDENTITY) as (keyof Transform)[]).every((key) => a[key] === b[key]);

/**
 * Sorts one layer's cuts by start and merges continuous ones into runs. Rejects overlaps and
 * empty cuts.
 */
export function toRuns(cuts: readonly Cut[]): Run[] {
  const sorted = [...cuts].sort((a, b) => a.start - b.start);
  const runs: Run[] = [];
  for (const cut of sorted) {
    if (cut.out <= cut.in) throw new RangeError(`Empty cut at ${cut.start}`);
    const end = cut.start + (cut.out - cut.in);
    const last = runs[runs.length - 1];
    if (last && cut.start < last.end) throw new RangeError(`Cuts overlap at ${cut.start}`);
    const layer = cut.layer ?? 0;
    const transform = toTransform(cut.transform);
    if (
      last &&
      last.source === cut.source &&
      last.out === cut.in &&
      last.end === cut.start &&
      sameTransform(last.transform, transform)
    ) {
      last.out = cut.out;
      last.end = end;
    } else {
      const { source, in: inTime, out } = cut;
      runs.push({ source, in: inTime, out, start: cut.start, end, layer, transform });
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

/** All runs, grouped by layer. A run's id is its index in `runs`. */
export interface Timeline {
  runs: Run[];
  /** Run ids per layer, each sorted by start, bottom layer first. */
  layers: { layer: number; ids: number[] }[];
  duration: Flicks;
}

export function toTimeline(cuts: readonly Cut[]): Timeline {
  const byLayer = new Map<number, Cut[]>();
  for (const cut of cuts) {
    const layer = cut.layer ?? 0;
    byLayer.set(layer, [...(byLayer.get(layer) ?? []), cut]);
  }
  const runs: Run[] = [];
  const layers: Timeline["layers"] = [];
  for (const layer of [...byLayer.keys()].sort((a, b) => a - b)) {
    const ids: number[] = [];
    for (const run of toRuns(byLayer.get(layer) ?? [])) {
      ids.push(runs.length);
      runs.push(run);
    }
    layers.push({ layer, ids });
  }
  const duration = runs.reduce((end, run) => Math.max(end, run.end), 0);
  return { runs, layers, duration };
}

function layerRuns(timeline: Timeline, ids: number[]): Run[] {
  return ids.map((id) => timeline.runs[id] as Run);
}

/** Id of the run covering `t` on the layer at `layerIndex` (into `timeline.layers`), or -1. */
export function runIdAt(timeline: Timeline, layerIndex: number, t: Flicks): number {
  const ids = timeline.layers[layerIndex]?.ids ?? [];
  const index = runAt(layerRuns(timeline, ids), t);
  return index < 0 ? -1 : (ids[index] as number);
}

export interface Wanted {
  /** Runs at the playhead, top layer first: they need a decoder most. */
  current: number[];
  /** Runs coming up on any layer, earliest start first. */
  ahead: number[];
}

/**
 * The runs that should have decoder streams, in priority order. Each layer contributes its run
 * at `t` and its upcoming runs (as `plan`). When decoders run short, the caller serves `current`
 * first (top layer first, so the layer most likely to be seen keeps moving), then `ahead`.
 */
export function wantedRuns(timeline: Timeline, t: Flicks, options: PlanOptions): Wanted {
  const current: number[] = [];
  const ahead: number[] = [];
  for (const { ids } of [...timeline.layers].reverse()) {
    const runs = layerRuns(timeline, ids);
    const at = runAt(runs, t);
    for (const index of plan(runs, t, options)) {
      const id = ids[index] as number;
      if (index === at) current.push(id);
      else ahead.push(id);
    }
  }
  ahead.sort((a, b) => (timeline.runs[a] as Run).start - (timeline.runs[b] as Run).start);
  return { current, ahead };
}
