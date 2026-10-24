// Pure geometry helpers shared by renderer.ts (drawing) and interactions.ts (hit-testing) —
// keeping both in agreement about where things are on the canvas.

import { pxToUs, usToPx, US_PER_SEC } from "../lib/time";
import {
  clipDurationUs,
  clipEndUs,
  type Clip,
  type ClipTransition,
  type MediaClip,
  type Project,
  type TextClip,
  type Track,
} from "../lib/types";

export const TRACK_LABEL_W = 88;
export const RULER_H = 24;
export const TRACK_H = 48;
export const TRACK_GAP = 6;
export const CONTENT_PAD_X = 8;
/// Clip card insets within the lane (approved redesign: labels live inside the card, so
/// the card fills most of the lane). Shared by renderer.ts drawing and hit-testing here.
export const CLIP_TOP_PAD = 6;
export const CLIP_BOTTOM_PAD = 4;

/// Canvas x of timeline time `us` (µs). `pxPerSec` is the zoom (a UI scale, not stored time).
export function clipLeft(us: number, pxPerSec: number, scrollX = 0): number {
  return TRACK_LABEL_W + CONTENT_PAD_X + usToPx(us, pxPerSec) - scrollX;
}

/// Timeline time (integer µs, rounded) under canvas x, clamped at 0.
export function usFromCanvasX(x: number, pxPerSec: number, scrollX = 0): number {
  return Math.max(0, pxToUs(x - TRACK_LABEL_W - CONTENT_PAD_X + scrollX, pxPerSec));
}

/// End of the last clip in µs, at least one second so an empty timeline still has a ruler.
export function timelineDurationUs(project: Project): number {
  let end = 0;
  for (const track of project.tracks) {
    for (const clip of track.clips) {
      end = Math.max(end, clipEndUs(clip));
    }
  }
  return Math.max(end, US_PER_SEC);
}

export function contentWidthPx(project: Project, pxPerSec: number): number {
  // Extra pad so the playhead / last clip aren't flush against the right edge.
  return TRACK_LABEL_W + CONTENT_PAD_X + usToPx(timelineDurationUs(project), pxPerSec) + 200;
}

export function contentHeightPx(trackCount: number): number {
  return RULER_H + 8 + Math.max(trackCount, 1) * (TRACK_H + TRACK_GAP) + 40;
}

export function maxScrollX(project: Project, pxPerSec: number, viewportW: number): number {
  return Math.max(0, contentWidthPx(project, pxPerSec) - viewportW);
}

export function maxScrollY(trackCount: number, viewportH: number): number {
  return Math.max(0, contentHeightPx(trackCount) - viewportH);
}

export function trackLayout(_viewportH: number, _trackCount: number, scrollY = 0) {
  const trackH = TRACK_H;
  return {
    trackH,
    laneTop: (i: number) => RULER_H + 4 + i * (trackH + TRACK_GAP) - scrollY,
  };
}

export interface DisplayTrack {
  /** Index into `project.tracks` — the array the compositor reads bottom→top. */
  trackIndex: number;
  track: Track;
}

/// CapCut-style visual mapping (issue 2). `project.tracks` is the core's compositing
/// order — array position = bottom→top, the LAST track composites on top (see
/// AGENTS.md §0.2 / docs/architecture.md) — but that is the OPPOSITE of how every
/// mainstream editor displays a timeline (topmost overlay lane visually on top). This is
/// the ONE place that reconciles the two: video/caption ("overlay") tracks are shown in
/// REVERSE array order, so the last-in-array track (the topmost compositing layer) is
/// visual row 0. Audio tracks have no z-order in the compositor — they're mixed, not
/// stacked — so they're kept in natural array order beneath the overlay group: since
/// `AddTrack` always appends, a newly created audio track lands at the BOTTOM of the
/// audio zone, i.e. exactly where a user dropping audio there expects the new lane to
/// appear (documented choice — see `dropTargetAtY`, which relies on this for its
/// "append-only" new-track semantics to land in the right visual spot for both zones).
///
/// EVERY piece of code that gives `project.tracks` a visual row meaning — renderer lane
/// drawing, hit-testing, drag/trim interactions, junction/transition badges, TrackHeaders
/// row order, drop-target math, scrollbar content — MUST iterate this (or its inverse
/// `rowForTrackIndex`) instead of indexing `project.tracks` positionally.
export function displayOrder(project: Project): DisplayTrack[] {
  const overlay: DisplayTrack[] = [];
  const audio: DisplayTrack[] = [];
  project.tracks.forEach((track, trackIndex) => {
    (track.kind === "audio" ? audio : overlay).push({ trackIndex, track });
  });
  overlay.reverse();
  return [...overlay, ...audio];
}

/// Inverse of `displayOrder`: `project.tracks` index -> visual row index, or -1 if the
/// track isn't found. O(track count) — timeline track counts are small enough this is
/// fine to call per-lookup rather than caching.
export function rowForTrackIndex(project: Project, trackIndex: number): number {
  return displayOrder(project).findIndex((d) => d.trackIndex === trackIndex);
}

/// Visual row bounds `[start, end]` (inclusive) of the zone a track of `kind` reorders
/// within — CapCut behavior: video/caption ("overlay") tracks only reorder among
/// themselves, audio tracks only among themselves (see `displayOrder`'s doc comment for
/// why the two groups are visually adjacent but never interleaved). Used to clamp a
/// drag-reorder's target row so a header can never be dropped into the other zone.
export function zoneRowBounds(project: Project, kind: Track["kind"]): [number, number] {
  const order = displayOrder(project);
  const overlayCount = order.filter((d) => d.track.kind !== "audio").length;
  if (kind === "audio") {
    return [overlayCount, Math.max(order.length - 1, overlayCount)];
  }
  return [0, Math.max(overlayCount - 1, 0)];
}

/// Computes the `Command::MoveTrack` `new_index` that lands `trackId` at visual row
/// `targetRow` after the move, or `null` if `trackId` isn't found or `targetRow` is
/// already where it sits (no-op). `new_index` is an index into the PRE-move
/// `project.tracks` under remove-then-insert-at-that-index semantics (see `move_track` in
/// renderly-core/src/commands/mod.rs) — rather than hand-deriving that remap, this
/// brute-forces every candidate index and re-derives `displayOrder` for each (track
/// counts are always small enough that this is cheap), so it can never drift from the
/// core's actual semantics.
export function moveTrackNewIndex(
  project: Project,
  trackId: string,
  targetRow: number,
): number | null {
  const idx = project.tracks.findIndex((t) => t.id === trackId);
  if (idx === -1) return null;
  const len = project.tracks.length;
  for (let candidate = 0; candidate < len; candidate++) {
    if (candidate === idx) continue;
    const copy = project.tracks.slice();
    const [moved] = copy.splice(idx, 1);
    copy.splice(candidate, 0, moved);
    const hypothetical: Project = { ...project, tracks: copy };
    if (rowForTrackIndex(hypothetical, candidate) === targetRow) return candidate;
  }
  return null;
}

export interface RowDropTarget {
  /** `project.tracks` index of an existing lane to drop into, or `null` to auto-create a
   *  new track (CapCut-style drop above the overlay group / below the audio group). */
  trackIndex: number | null;
  /** Visual row to render the ghost/insertion-line at: a real row index for an existing
   *  target, `-1` for "new track above everything", or the display row count for "new
   *  track below everything". */
  row: number;
}

/// Which visual row `y` falls in, resolved all the way to a `project.tracks` index via
/// `displayOrder` — the single source of truth for both drag-and-drop targeting
/// (editorStore.dropMediaOnTimeline / TimelineCanvas's drag-ghost preview) and cross-track
/// clip moves (interactions.ts). `y` above the topmost lane signals "new overlay track on
/// top" (CapCut's drop-above-the-top-lane gesture — appending a video/image track always
/// lands it there per `displayOrder`'s reversal); `y` below the bottommost lane signals
/// "new track appended at the bottom of the audio zone" (or, if there are no audio tracks
/// yet, the bottom of the overlay zone) — both cases return `trackIndex: null` and let the
/// caller decide the new track's kind from what's being dropped.
export function dropTargetAtY(
  project: Project,
  canvasHeight: number,
  y: number,
  scrollY = 0,
): RowDropTarget {
  const order = displayOrder(project);
  const { trackH, laneTop } = trackLayout(canvasHeight, Math.max(order.length, 1), scrollY);
  if (order.length > 0 && y < laneTop(0) - trackH / 2) {
    return { trackIndex: null, row: -1 };
  }
  for (let i = 0; i < order.length; i++) {
    if (y < laneTop(i) + trackH + 3) return { trackIndex: order[i]!.trackIndex, row: i };
  }
  return { trackIndex: null, row: order.length };
}

export interface ClipHit {
  trackId: string;
  clip: Clip;
  trackIndex: number;
  edge: "left" | "right" | "body";
}

const TRIM_HANDLE_PX = 8;

export function hitTestClip(
  project: Project,
  x: number,
  y: number,
  canvasHeight: number,
  pxPerSec: number,
  scrollX = 0,
  scrollY = 0,
): ClipHit | null {
  const order = displayOrder(project);
  const { trackH, laneTop } = trackLayout(canvasHeight, order.length, scrollY);

  for (let row = 0; row < order.length; row++) {
    const { trackIndex, track } = order[row]!;
    const y0 = laneTop(row);
    if (y < y0 || y > y0 + trackH) continue;
    for (const clip of track.clips) {
      const cx = clipLeft(clip.start_us, pxPerSec, scrollX);
      const cw = usToPx(clipDurationUs(clip), pxPerSec);
      if (y < y0 + CLIP_TOP_PAD || x < cx || x > cx + cw) continue;
      if (x <= cx + TRIM_HANDLE_PX) {
        return { trackId: track.id, clip, trackIndex, edge: "left" };
      }
      if (x >= cx + cw - TRIM_HANDLE_PX) {
        return { trackId: track.id, clip, trackIndex, edge: "right" };
      }
      return { trackId: track.id, clip, trackIndex, edge: "body" };
    }
  }
  return null;
}

/// A clip that can carry an outgoing transition: a video clip or a title.
export type TransitionLayer = MediaClip | TextClip;

export function isTransitionLayer(clip: Clip): clip is TransitionLayer {
  return clip.type === "video" || clip.type === "text";
}

/// Next enabled video clip or title on the same track at or after `clip`'s timeline end (C4
/// junction target; the same pairing core's `SetClipTransition` validates against).
export function nextVideoClipOnTrack(track: Track, clip: TransitionLayer): TransitionLayer | null {
  const end = clip.start_us + clipDurationUs(clip);
  return (
    [...track.clips]
      .filter((c): c is TransitionLayer => isTransitionLayer(c) && c.enabled)
      .sort((a, b) => a.start_us - b.start_us)
      .find((c) => c.id !== clip.id && c.start_us >= end - 1) ?? null
  );
}

export interface TransitionJunction {
  trackId: string;
  trackIndex: number;
  outgoingClipId: string;
  junctionUs: number;
  junctionX: number;
  laneY: number;
  laneH: number;
  bodyY: number;
  bodyH: number;
  transition: ClipTransition | null;
  overlapPx: number;
}

const JUNCTION_HIT_HALF_W = 10;

export function listTransitionJunctions(
  project: Project,
  canvasHeight: number,
  pxPerSec: number,
  scrollX = 0,
  scrollY = 0,
): TransitionJunction[] {
  const order = displayOrder(project);
  const { trackH, laneTop } = trackLayout(canvasHeight, order.length, scrollY);
  const out: TransitionJunction[] = [];

  for (let row = 0; row < order.length; row++) {
    const { trackIndex, track } = order[row]!;
    if (track.kind !== "video") continue;
    const y = laneTop(row);
    const bodyY = y + CLIP_TOP_PAD;
    const bodyH = trackH - CLIP_TOP_PAD - CLIP_BOTTOM_PAD;

    const videos = [...track.clips].filter(isTransitionLayer).sort((a, b) => a.start_us - b.start_us);

    for (const clip of videos) {
      const next = nextVideoClipOnTrack(track, clip);
      if (!next) continue;
      const junctionUs = clip.start_us + clipDurationUs(clip);
      const junctionX = clipLeft(junctionUs, pxPerSec, scrollX);
      const transition = clip.outgoing_transition ?? null;
      const overlapPx = transition ? Math.max(4, usToPx(transition.duration_us, pxPerSec)) : 0;
      out.push({
        trackId: track.id,
        trackIndex,
        outgoingClipId: clip.id,
        junctionUs,
        junctionX,
        laneY: y,
        laneH: trackH,
        bodyY,
        bodyH,
        transition,
        overlapPx,
      });
    }
  }
  return out;
}

export interface TransitionJunctionHit {
  trackId: string;
  clipId: string;
  trackIndex: number;
}

export function hitTestTransitionJunction(
  project: Project,
  x: number,
  y: number,
  canvasHeight: number,
  pxPerSec: number,
  scrollX = 0,
  scrollY = 0,
): TransitionJunctionHit | null {
  for (const j of listTransitionJunctions(project, canvasHeight, pxPerSec, scrollX, scrollY)) {
    if (y < j.bodyY || y > j.bodyY + j.bodyH) continue;
    // Badge sits just inside the outgoing clip — avoid the right-edge trim handle.
    if (x >= j.junctionX - 16 && x <= j.junctionX) {
      return { trackId: j.trackId, clipId: j.outgoingClipId, trackIndex: j.trackIndex };
    }
    if (Math.abs(x - j.junctionX) <= JUNCTION_HIT_HALF_W && y >= j.bodyY + 4 && y <= j.bodyY + j.bodyH - 4) {
      return { trackId: j.trackId, clipId: j.outgoingClipId, trackIndex: j.trackIndex };
    }
  }
  return null;
}
