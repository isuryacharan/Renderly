// Timeline geometry: where tracks and clips sit on screen, what is under the pointer, which clips
// are visible, ruler steps, zoom limits and wheel input. Pure functions of the sequence and the
// view, so they are unit tested without a canvas.
//
// x is measured from the left edge of the track area, y from the top of the first track row
// (below the ruler). Times are flicks; zoom is flicks per CSS pixel.

import {
  clipEnd,
  flicksPerFrame,
  FLICKS_PER_SECOND,
  formatTimecode,
  type Clip,
  type Sequence,
  type Track,
} from "@renderly/core";
import { size } from "@renderly/ui/tokens";

export const RULER_HEIGHT = 24;
/** Pointer distance from a clip edge that grabs the edge for trimming. */
export const EDGE_PX = 6;
/** Pointer distance within which edits snap to a target. */
export const SNAP_PX = 8;
/** Inset of a clip inside its track row, top and bottom. */
export const CLIP_INSET = 3;
/** Most zoomed in: pixels per frame. */
export const MAX_FRAME_PX = 24;

export interface Row {
  track: Track;
  /** Position of the track in `sequence.tracks`. */
  index: number;
  y: number;
  height: number;
}

export const trackHeight = (kind: Track["kind"]) =>
  kind === "video" ? size.trackVideo : kind === "audio" ? size.trackAudio : size.trackAux;

/**
 * Display order: picture tracks (video, text, effect) with the highest index on top, since
 * higher tracks draw over lower ones, then audio tracks in index order below them.
 */
export function layoutRows(sequence: Sequence): Row[] {
  const picture: Row[] = [];
  const audio: Row[] = [];
  sequence.tracks.forEach((track, index) => {
    const row = { track, index, y: 0, height: trackHeight(track.kind) };
    (track.kind === "audio" ? audio : picture).push(row);
  });
  picture.reverse();
  const rows = [...picture, ...audio];
  let y = 0;
  for (const row of rows) {
    row.y = y;
    y += row.height + 1; // 1px border line between rows
  }
  return rows;
}

export const rowsHeight = (rows: readonly Row[]) => {
  const last = rows[rows.length - 1];
  return last ? last.y + last.height + 1 : 0;
};

/** The row at content y (already including vertical scroll), or undefined. */
export function rowAt(rows: readonly Row[], y: number): Row | undefined {
  return rows.find((row) => y >= row.y && y < row.y + row.height + 1);
}

/** Index of the first clip that ends after `from`. Clips on a track are sorted and never overlap. */
export function firstVisible(clips: readonly Clip[], from: number): number {
  let lo = 0;
  let hi = clips.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const clip = clips[mid];
    if (clip && clipEnd(clip) <= from) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

export interface View {
  /** Time at the left edge of the track area. */
  t: number;
  /** Flicks per pixel. */
  zoom: number;
  /** Vertical scroll of the track rows, in pixels. */
  y: number;
}

export const timeToX = (view: View, t: number) => (t - view.t) / view.zoom;
export const xToTime = (view: View, x: number) => view.t + x * view.zoom;

export type Zone = "start" | "end" | "body";

export interface Hit {
  row: Row;
  clip: Clip;
  zone: Zone;
}

/** The clip under content point (x, y) and which part of it: an edge or the body. */
export function hitClip(rows: readonly Row[], view: View, x: number, y: number): Hit | undefined {
  const row = rowAt(rows, y);
  if (!row) return undefined;
  if (y < row.y + CLIP_INSET || y > row.y + row.height - CLIP_INSET) return undefined;
  const clips = row.track.clips;
  const t = xToTime(view, x);
  const pad = EDGE_PX * view.zoom;
  // Candidates: the clip under t, and neighbours whose edge is within reach.
  const i = firstVisible(clips, t - pad);
  let best: Clip | undefined;
  for (let k = i; k < clips.length; k++) {
    const clip = clips[k];
    if (!clip || clip.start > t + pad) break;
    if (!best || (clip.start <= t && t < clipEnd(clip))) best = clip;
  }
  if (!best) return undefined;
  const x0 = timeToX(view, best.start);
  const x1 = timeToX(view, clipEnd(best));
  const wide = x1 - x0 > EDGE_PX * 3;
  if (Math.abs(x - x0) <= EDGE_PX && (wide || x < (x0 + x1) / 2))
    return { row, clip: best, zone: "start" };
  if (Math.abs(x - x1) <= EDGE_PX) return { row, clip: best, zone: "end" };
  if (x < x0 || x >= x1) return undefined;
  return { row, clip: best, zone: "body" };
}

/** Ids of clips that intersect the content rectangle, in display order. */
export function clipsInBox(
  rows: readonly Row[],
  view: View,
  box: { x0: number; y0: number; x1: number; y1: number },
): string[] {
  const left = Math.min(box.x0, box.x1);
  const right = Math.max(box.x0, box.x1);
  const top = Math.min(box.y0, box.y1);
  const bottom = Math.max(box.y0, box.y1);
  const from = xToTime(view, left);
  const to = xToTime(view, right);
  const ids: string[] = [];
  for (const row of rows) {
    if (row.y + row.height - CLIP_INSET < top || row.y + CLIP_INSET > bottom) continue;
    const clips = row.track.clips;
    for (let k = firstVisible(clips, from); k < clips.length; k++) {
      const clip = clips[k];
      if (!clip || clip.start > to) break;
      ids.push(clip.id);
    }
  }
  return ids;
}

/** Snaps `t` to the nearest target within `tolerance`. `hit` is the target, or null. */
export function snapNearest(
  targets: readonly number[],
  t: number,
  tolerance: number,
): { t: number; hit: number | null } {
  let hit: number | null = null;
  let distance = tolerance;
  for (const target of targets) {
    const d = Math.abs(target - t);
    if (d <= distance) {
      hit = target;
      distance = d;
    }
  }
  return hit === null ? { t, hit } : { t: hit, hit };
}

/**
 * Snap for a moving span [start, start + length): whichever edge is closer to a target wins.
 * Returns the time offset to apply and the target hit.
 */
export function snapSpan(
  targets: readonly number[],
  start: number,
  length: number,
  tolerance: number,
): { shift: number; hit: number | null } {
  const a = snapNearest(targets, start, tolerance);
  const b = snapNearest(targets, start + length, tolerance);
  const da = a.hit === null ? Infinity : Math.abs(a.t - start);
  const db = b.hit === null ? Infinity : Math.abs(b.t - (start + length));
  if (da === Infinity && db === Infinity) return { shift: 0, hit: null };
  return da <= db
    ? { shift: a.t - start, hit: a.hit }
    : { shift: b.t - start - length, hit: b.hit };
}

// --- zoom ---

/** Zoom limits for a sequence shown `width` pixels wide: single frames up to the whole thing. */
export function zoomLimits(sequence: Sequence, duration: number, width: number) {
  const frame = flicksPerFrame(sequence.fps);
  const min = frame / MAX_FRAME_PX;
  const content = Math.max(duration, 60 * FLICKS_PER_SECOND) * 1.15;
  const max = Math.max(min, content / Math.max(width, 1));
  return { min, max };
}

/** The zoom that shows the whole sequence (with a little room after it). */
export function zoomToFit(sequence: Sequence, duration: number, width: number): number {
  const frame = flicksPerFrame(sequence.fps);
  const content = Math.max(duration, 10 * FLICKS_PER_SECOND) * 1.05;
  return Math.max(frame / MAX_FRAME_PX, content / Math.max(width, 1));
}

/** Zooms to `zoom` keeping the time under `anchorX` where it is. */
export function zoomAround(view: View, zoom: number, anchorX: number): View {
  const at = xToTime(view, anchorX);
  return { ...view, zoom, t: Math.max(0, at - anchorX * zoom) };
}

// --- ruler ---

const SECOND = FLICKS_PER_SECOND;
const STEPS_SECONDS = [1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200];
/** Room for a label: a timecode with frames is wider than minutes and seconds. */
const MIN_LABEL_PX = { frames: 100, seconds: 64 };

/** Major and minor ruler spacing, in flicks: frames when zoomed in, seconds and minutes out. */
export function rulerSteps(sequence: Sequence, zoom: number): { major: number; minor: number } {
  const frame = flicksPerFrame(sequence.fps);
  const framesPerSecond = sequence.fps[0] / sequence.fps[1];
  const frameSteps = [1, 2, 5, 10].filter((n) => n < framesPerSecond).map((n) => n * frame);
  const steps = [...frameSteps, ...STEPS_SECONDS.map((s) => s * SECOND)];
  const major =
    steps.find(
      (step) => step / zoom >= (step < SECOND ? MIN_LABEL_PX.frames : MIN_LABEL_PX.seconds),
    ) ?? 7200 * SECOND;
  let minor = major < SECOND ? frame : major / 5;
  if (minor / zoom < 6) minor = major;
  return { major, minor };
}

/** 0:00, 1:05, 1:02:03; full timecode (00:01:05:12) when the step is under a second. */
export function rulerLabel(t: number, sequence: Sequence, withFrames: boolean): string {
  if (withFrames) return formatTimecode(t, sequence.fps);
  const seconds = Math.floor(t / SECOND);
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  const s = String(seconds % 60).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

// --- wheel ---

export type WheelIntent =
  { kind: "zoom"; factor: number } | { kind: "scroll"; dx: number; dy: number };

interface WheelLike {
  deltaX: number;
  deltaY: number;
  deltaMode: number;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  /** Legacy, Chromium: multiples of 120 for mouse wheel notches. */
  wheelDeltaY?: number;
  wheelDeltaX?: number;
}

/**
 * True for a notched mouse wheel, false for a trackpad. Wheels report whole lines, or (in
 * Chromium) legacy deltas in multiples of 120 with no sideways movement; trackpads send small,
 * smooth deltas on both axes.
 */
export function isMouseWheel(e: WheelLike): boolean {
  if (e.deltaMode !== 0) return true;
  if (e.deltaX !== 0) return false;
  if (e.wheelDeltaY !== undefined && e.wheelDeltaY !== 0)
    return e.wheelDeltaY % 120 === 0 && (e.wheelDeltaX ?? 0) === 0;
  return Math.abs(e.deltaY) >= 50;
}

/**
 * What a wheel event means on the timeline.
 * - Pinch on a trackpad (reported with ctrlKey) or Ctrl, Cmd or Alt with any wheel: zoom.
 * - Trackpad: two-finger swipe scrolls both ways.
 * - Mouse wheel: scrolls time; with Shift, scrolls the tracks.
 */
export function wheelIntent(e: WheelLike): WheelIntent {
  const lines = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
  const dx = e.deltaX * lines;
  const dy = e.deltaY * lines;
  const mouse = isMouseWheel(e);
  if (e.ctrlKey || e.metaKey || e.altKey) {
    const rate = mouse ? 0.0018 : 0.01;
    return { kind: "zoom", factor: Math.exp(dy * rate) };
  }
  if (!mouse) return { kind: "scroll", dx, dy };
  return e.shiftKey ? { kind: "scroll", dx: 0, dy } : { kind: "scroll", dx: dy || dx, dy: 0 };
}

/** Speed (pixels per frame) to auto-scroll when the pointer is near an edge of [lo, hi]. */
export function edgeScroll(position: number, lo: number, hi: number, zone = 32): number {
  const speed = (depth: number) => Math.min(32, Math.ceil((depth / zone) * 16));
  if (position < lo + zone) return -speed(lo + zone - position);
  if (position > hi - zone) return speed(position - (hi - zone));
  return 0;
}
