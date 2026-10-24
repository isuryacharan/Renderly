// Timeline mouse interaction state machine: hit-test → drag → commit-on-mouseup. Drag
// mutates the store's `project` immutably for live visual feedback (optimistic local
// mutation), then issues exactly one command on mouseup — see docs/architecture.md.

import { useEffect, useRef, type RefObject } from "react";
import {
  deleteClip,
  moveClip,
  setAdjustmentLayer,
  setCaption,
  setTextTiming,
  splitClip,
  trimClip,
} from "../lib/commands";
import { magneticInsertBoundaryUs } from "../lib/magnetic";
import { nearestMarker } from "../lib/text";
import { activeTracks, replaceActiveTracks, selectActiveView, trimAdjustment } from "../lib/container";
import { normalizeFps, pxToUs, snapToFrameUs } from "../lib/time";
import { clipDurationUs, type CaptionClip, type Clip, type Project, type TextClip } from "../lib/types";
import { useEditorStore } from "../store/editorStore";
import {
  clipLeft,
  dropTargetAtY,
  hitTestClip,
  hitTestTransitionJunction,
  RULER_H,
  TRACK_LABEL_W,
  usFromCanvasX,
} from "./layout";
import { snapTime } from "./snap";

/// Shortest source range a trim may leave (µs) and shortest caption (µs).
const MIN_TRIM_US = 50_000;
const MIN_CAPTION_US = 100_000;

/// Snap a trimmed/derived time to a frame boundary of the project rate.
function frameSnap(project: Project, us: number): number {
  return snapToFrameUs(normalizeFps(project.settings.fps), us);
}

/// Captions and titles are timed by position + duration (no source range): a trim is committed
/// with `SetCaption` / `SetTextTiming`, not `TrimClip`.
function isTimed(clip: Clip): clip is CaptionClip | TextClip {
  return clip.type === "caption" || clip.type === "text";
}

function timingCommand(trackId: string, clip: CaptionClip | TextClip, fields: { startUs?: number; durationUs?: number }) {
  return clip.type === "caption" ? setCaption(trackId, clip.id, fields) : setTextTiming(trackId, clip.id, fields);
}

/// Pixels a click may be off a ruler marker's flag and still hit it.
const MARKER_HIT_PX = 7;

type DragState =
  | {
      mode: "scrub";
      lastScrubMs: number;
    }
  | {
      mode: "pan";
      startClientX: number;
      startClientY: number;
      origScrollX: number;
      origScrollY: number;
    }
  | {
      mode: "move";
      origTrackId: string;
      currentTrackId: string;
      clipId: string;
      startX: number;
      origPos: number;
      /** When length > 1, group-move: same delta on each (no cross-track). */
      group: { trackId: string; clipId: string; origPos: number }[] | null;
    }
  | {
      mode: "trim-left";
      trackId: string;
      clipId: string;
      startX: number;
      origIn: number;
      origOut: number;
      origPos: number;
    }
  | {
      mode: "trim-left-caption";
      trackId: string;
      clipId: string;
      startX: number;
      origPos: number;
      origDur: number;
    }
  | { mode: "trim-right"; trackId: string; clipId: string; startX: number; origOut: number }
  // Adjustment layers have no source range: a trim is position + duration, committed with
  // one SetAdjustmentLayer (TrimClip is rejected for them).
  | {
      mode: "trim-adjustment";
      edge: "left" | "right";
      trackId: string;
      clipId: string;
      startX: number;
      origPos: number;
      origDur: number;
    };

// NOTE: the helpers below operate on the open container's VIEW of the project (tracks =
// `activeTracks`), and the mouse handlers write the result back with `replaceActiveTracks`.
function withClip(project: Project, trackId: string, clipId: string, update: (c: Clip) => Clip): Project {
  return {
    ...project,
    tracks: project.tracks.map((t) =>
      t.id !== trackId
        ? t
        : { ...t, clips: t.clips.map((c) => (c.id === clipId ? update(c) : c)) },
    ),
  };
}

function findClip(project: Project, trackId: string, clipId: string): Clip | undefined {
  return project.tracks.find((t) => t.id === trackId)?.clips.find((c) => c.id === clipId);
}

function moveClipAcrossTracks(
  project: Project,
  srcTrackId: string,
  destTrackId: string,
  clipId: string,
  newStartUs: number,
): Project {
  let moved: Clip | undefined;
  const withoutSrc = project.tracks.map((t) => {
    if (t.id !== srcTrackId) return t;
    const clip = t.clips.find((c) => c.id === clipId);
    if (clip) moved = { ...clip, start_us: newStartUs };
    return { ...t, clips: t.clips.filter((c) => c.id !== clipId) };
  });
  if (!moved) return project;
  const movedClip = moved;
  return {
    ...project,
    tracks: withoutSrc.map((t) => (t.id === destTrackId ? { ...t, clips: [...t.clips, movedClip] } : t)),
  };
}

function scrubTo(x: number, store: ReturnType<typeof useEditorStore.getState>, altKey: boolean) {
  const project = selectActiveView(store);
  if (!project) return;
  const us = snapTime(
    usFromCanvasX(x, store.pxPerSec, store.scrollX),
    project,
    store.playhead,
    store.pxPerSec,
    store.snapEnabled && !altKey,
  );
  store.scrubAt(us);
  if (store.snapEnabled && !altKey) {
    const raw = usFromCanvasX(x, store.pxPerSec, store.scrollX);
    store.setSnapGuide(us !== raw ? us : null);
  } else {
    store.setSnapGuide(null);
  }
}

export function useTimelineInteractions(canvasRef: RefObject<HTMLCanvasElement | null>) {
  const dragStateRef = useRef<DragState | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    const onWheel = (ev: WheelEvent) => {
      const store = useEditorStore.getState();
      if (!store.project) return;
      ev.preventDefault();

      // Ctrl/Meta + wheel → zoom (playhead-anchored when possible).
      if (ev.ctrlKey || ev.metaKey) {
        const rect = canvas.getBoundingClientRect();
        const x = ev.clientX - rect.left;
        const beforeUs = usFromCanvasX(x, store.pxPerSec, store.scrollX);
        const nextPx = store.pxPerSec + (ev.deltaY > 0 ? -12 : 12);
        store.setZoom(nextPx);
        const after = useEditorStore.getState();
        // Keep the time under the cursor fixed.
        const newScrollX = (beforeUs / 1_000_000) * after.pxPerSec - (x - TRACK_LABEL_W - 8);
        after.setScroll(newScrollX, after.scrollY);
        return;
      }

      // Shift + wheel or dominant deltaX → horizontal pan; else vertical.
      const horiz = ev.shiftKey || Math.abs(ev.deltaX) > Math.abs(ev.deltaY);
      if (horiz) {
        store.panBy(ev.deltaX !== 0 ? ev.deltaX : ev.deltaY, 0);
      } else {
        store.panBy(0, ev.deltaY);
      }
    };

    const onMouseDown = (ev: MouseEvent) => {
      const store = useEditorStore.getState();
      const project = selectActiveView(store);
      if (!project) return;
      const rect = canvas.getBoundingClientRect();
      const x = ev.clientX - rect.left;
      const y = ev.clientY - rect.top;

      // Middle mouse → pan.
      if (ev.button === 1) {
        ev.preventDefault();
        dragStateRef.current = {
          mode: "pan",
          startClientX: ev.clientX,
          startClientY: ev.clientY,
          origScrollX: store.scrollX,
          origScrollY: store.scrollY,
        };
        store.setDragging(true);
        canvas.style.cursor = "grabbing";
        return;
      }

      if (ev.button !== 0) return;

      // A click on a marker flag in the ruler jumps the playhead to it.
      if (y < RULER_H && (project.markers?.length ?? 0) > 0) {
        const raw = usFromCanvasX(x, store.pxPerSec, store.scrollX);
        const hitMarker = nearestMarker(project.markers, raw, (MARKER_HIT_PX / store.pxPerSec) * 1_000_000);
        if (hitMarker && Math.abs(clipLeft(hitMarker.time_us, store.pxPerSec, store.scrollX) - x) <= MARKER_HIT_PX) {
          void store.seekTo(hitMarker.time_us);
          return;
        }
      }

      const junctionHit = hitTestTransitionJunction(
        project,
        x,
        y,
        rect.height,
        store.pxPerSec,
        store.scrollX,
        store.scrollY,
      );
      if (junctionHit && store.toolMode === "select" && !ev.shiftKey) {
        store.select({ trackId: junctionHit.trackId, clipId: junctionHit.clipId });
        store.setLeftTab("transitions");
        return;
      }

      const hit = hitTestClip(
        project,
        x,
        y,
        rect.height,
        store.pxPerSec,
        store.scrollX,
        store.scrollY,
      );

      if (hit) {
        const hitTrack = project.tracks.find((t) => t.id === hit.trackId);
        const locked = hitTrack?.locked ?? false;
        const hitSel = { trackId: hit.trackId, clipId: hit.clip.id };

        if (locked) {
          if (ev.shiftKey) store.toggleSelect(hitSel);
          else store.select(hitSel);
          store.toast("Track is locked — unlock it to edit with the mouse", "info");
          return;
        }

        if (store.toolMode === "razor") {
          const at = snapTime(
            usFromCanvasX(x, store.pxPerSec, store.scrollX),
            project,
            store.playhead,
            store.pxPerSec,
            store.snapEnabled && !ev.altKey,
          );
          void store.dispatch(splitClip(hit.trackId, hit.clip.id, at));
          return;
        }

        // C3: Shift+click toggles multi-select (no drag).
        if (ev.shiftKey && store.toolMode === "select") {
          store.toggleSelect(hitSel);
          return;
        }

        const inMulti =
          store.selections.length > 1 &&
          store.selections.some((s) => s.trackId === hitSel.trackId && s.clipId === hitSel.clipId);
        if (!inMulti) {
          store.select(hitSel);
        }

        const edge = hit.edge;
        if (
          store.toolMode === "select" &&
          hit.clip.type === "adjustment" &&
          (edge === "left" || edge === "right")
        ) {
          dragStateRef.current = {
            mode: "trim-adjustment",
            edge,
            trackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origPos: hit.clip.start_us,
            origDur: hit.clip.duration_us,
          };
        } else if (store.toolMode === "select" && edge === "left" && isTimed(hit.clip)) {
          dragStateRef.current = {
            mode: "trim-left-caption",
            trackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origPos: hit.clip.start_us,
            origDur: hit.clip.duration_us,
          };
        } else if (
          store.toolMode === "select" &&
          edge === "left" &&
          (hit.clip.type === "video" || hit.clip.type === "audio")
        ) {
          dragStateRef.current = {
            mode: "trim-left",
            trackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origIn: hit.clip.source_in_us,
            origOut: hit.clip.source_out_us,
            origPos: hit.clip.start_us,
          };
        } else if (store.toolMode === "select" && edge === "right" && hit.clip.type !== "adjustment") {
          dragStateRef.current = {
            mode: "trim-right",
            trackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origOut: isTimed(hit.clip) ? hit.clip.duration_us : hit.clip.source_out_us,
          };
        } else if (store.toolMode === "select") {
          const fresh = useEditorStore.getState();
          let group: { trackId: string; clipId: string; origPos: number }[] | null = null;
          if (fresh.selections.length > 1) {
            group = [];
            for (const s of fresh.selections) {
              const c = findClip(project, s.trackId, s.clipId);
              if (c) group.push({ trackId: s.trackId, clipId: s.clipId, origPos: c.start_us });
            }
            if (group.length < 2) group = null;
          }
          dragStateRef.current = {
            mode: "move",
            origTrackId: hit.trackId,
            currentTrackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origPos: hit.clip.start_us,
            group,
          };
        }
        if (dragStateRef.current) store.setDragging(true);
        return;
      }

      if (store.toolMode === "razor") return;

      // Ruler or empty lane → scrub (click + drag).
      dragStateRef.current = { mode: "scrub", lastScrubMs: 0 };
      store.setDragging(true);
      store.select(null);
      scrubTo(x, store, ev.altKey);
      canvas.style.cursor = "ew-resize";
    };

    const onMouseMove = (ev: MouseEvent) => {
      const drag = dragStateRef.current;
      const store = useEditorStore.getState();
      const project = selectActiveView(store);
      if (!drag || !project) {
        // Hover cursor: playhead vicinity / trim handles.
        if (!project) return;
        const rect = canvas.getBoundingClientRect();
        const x = ev.clientX - rect.left;
        const y = ev.clientY - rect.top;
        if (y < RULER_H) {
          canvas.style.cursor = "ew-resize";
          return;
        }
        const hit = hitTestClip(
          project,
          x,
          y,
          rect.height,
          store.pxPerSec,
          store.scrollX,
          store.scrollY,
        );
        if (hit && (hit.edge === "left" || hit.edge === "right")) canvas.style.cursor = "ew-resize";
        else if (hit) canvas.style.cursor = "grab";
        else canvas.style.cursor = "default";
        return;
      }

      const rect = canvas.getBoundingClientRect();
      const x = ev.clientX - rect.left;
      const y = ev.clientY - rect.top;

      if (drag.mode === "pan") {
        const dx = drag.startClientX - ev.clientX;
        const dy = drag.startClientY - ev.clientY;
        store.setScroll(drag.origScrollX + dx, drag.origScrollY + dy);
        return;
      }

      if (drag.mode === "scrub") {
        const now = performance.now();
        // Throttle scrubAudio IPC ~60Hz; playhead updates every move.
        scrubTo(x, store, ev.altKey);
        // Edge auto-scroll while scrubbing.
        const edge = 40;
        if (x > rect.width - edge) store.panBy(8, 0);
        else if (x < TRACK_LABEL_W + edge) store.panBy(-8, 0);
        drag.lastScrubMs = now;
        return;
      }

      const delta = pxToUs(x - drag.startX, store.pxPerSec);
      let nextProject: Project | null = null;

      if (drag.mode === "move") {
        const clip = findClip(project, drag.currentTrackId, drag.clipId);
        if (!clip) return;
        const rawPos = Math.max(0, drag.origPos + delta);

        // Where the clip is heading: another lane of the same kind, or the one it is on.
        // Plain clip-drag never auto-creates a track (only media-panel drops do, via
        // `dropMediaOnTimeline`) - `dropTargetAtY` returning `trackIndex: null` (drag
        // above the top lane or below the bottom lane) correctly falls through to
        // `destTrack` being `undefined` below.
        const grouped = !!drag.group && drag.group.length > 1;
        const destTrackIndex = grouped ? null : dropTargetAtY(project, rect.height, y, store.scrollY).trackIndex;
        const destTrack = destTrackIndex != null ? project.tracks[destTrackIndex] : undefined;
        const srcTrack = project.tracks.find((t) => t.id === drag.currentTrackId);
        const canCrossMove =
          !grouped &&
          destTrack &&
          srcTrack &&
          destTrack.id !== srcTrack.id &&
          destTrack.kind === srcTrack.kind &&
          !destTrack.locked;
        const landingTrack = canCrossMove ? destTrack : srcTrack;
        // On a magnetic track core decides where the clip lands (insert by centre, then
        // ripple), so the clip just follows the pointer and an indicator shows the slot.
        const magnetic = landingTrack?.magnetic === true;

        const pos = magnetic
          ? frameSnap(project, rawPos)
          : snapTime(
              rawPos,
              project,
              store.playhead,
              store.pxPerSec,
              store.snapEnabled && !ev.altKey,
              clip.id,
            );
        if (magnetic && landingTrack) {
          store.setSnapGuide(magneticInsertBoundaryUs(landingTrack, clip.id, pos, clipDurationUs(clip)));
        } else {
          store.setSnapGuide(pos !== rawPos ? pos : null);
        }
        const appliedDelta = pos - drag.origPos;

        // C3 group move: same delta for every selected clip; no cross-track.
        if (grouped && drag.group) {
          // Clamp so none go negative.
          let minOrig = Infinity;
          for (const g of drag.group) minOrig = Math.min(minOrig, g.origPos);
          const clampedDelta = appliedDelta < -minOrig ? -minOrig : appliedDelta;
          let next = project;
          for (const g of drag.group) {
            next = withClip(next, g.trackId, g.clipId, (c) => ({
              ...c,
              start_us: Math.max(0, g.origPos + clampedDelta),
            }));
          }
          nextProject = next;
        } else if (canCrossMove && destTrack) {
          nextProject = moveClipAcrossTracks(project, drag.currentTrackId, destTrack.id, clip.id, pos);
          dragStateRef.current = { ...drag, currentTrackId: destTrack.id };
        } else {
          nextProject = withClip(project, drag.currentTrackId, clip.id, (c) => ({
            ...c,
            start_us: pos,
          }));
        }

        // Edge auto-scroll while dragging clips.
        if (x > rect.width - 40) store.panBy(10, 0);
        else if (x < 100) store.panBy(-10, 0);
      } else if (drag.mode === "trim-adjustment") {
        const clip = findClip(project, drag.trackId, drag.clipId);
        if (!clip || clip.type !== "adjustment") return;
        const origEdge = drag.edge === "left" ? drag.origPos : drag.origPos + drag.origDur;
        let edgeUs = Math.max(0, origEdge + delta);
        if (store.snapEnabled && !ev.altKey) {
          const snapped = snapTime(edgeUs, project, store.playhead, store.pxPerSec, true, clip.id);
          store.setSnapGuide(snapped !== edgeUs ? snapped : null);
          edgeUs = snapped;
        } else {
          edgeUs = frameSnap(project, edgeUs);
        }
        const next = trimAdjustment(drag.edge, drag.origPos, drag.origDur, edgeUs);
        nextProject = withClip(project, drag.trackId, drag.clipId, (c) =>
          c.type === "adjustment"
            ? { ...c, start_us: next.position, duration_us: next.duration }
            : c,
        );
      } else {
        const clip = findClip(project, drag.trackId, drag.clipId);
        if (!clip || clip.type === "adjustment") return;
        const magneticTrim = project.tracks.find((t) => t.id === drag.trackId)?.magnetic === true;

        if (drag.mode === "trim-right") {
          const raw = isTimed(clip)
            ? Math.max(MIN_CAPTION_US, drag.origOut + delta)
            : Math.max(clip.source_in_us + MIN_TRIM_US, drag.origOut + delta);
          // Caption length is measured to a frame-snapped END, so start + duration stays on a frame.
          let next = isTimed(clip) ? frameSnap(project, clip.start_us + raw) - clip.start_us : frameSnap(project, raw);
          if (store.snapEnabled && !ev.altKey) {
            const absEnd = isTimed(clip) ? clip.start_us + raw : clip.start_us + (raw - clip.source_in_us);
            const snappedAbs = snapTime(
              absEnd,
              project,
              store.playhead,
              store.pxPerSec,
              true,
              clip.id,
            );
            store.setSnapGuide(snappedAbs !== absEnd ? snappedAbs : null);
            if (isTimed(clip)) {
              next = Math.max(MIN_CAPTION_US, snappedAbs - clip.start_us);
            } else {
              next = Math.max(
                clip.source_in_us + MIN_TRIM_US,
                snappedAbs - clip.start_us + clip.source_in_us,
              );
            }
          }
          nextProject = withClip(project, drag.trackId, drag.clipId, (c) =>
            isTimed(c) ? { ...c, duration_us: next } : c.type === "adjustment" ? c : { ...c, source_out_us: next },
          );
        } else if (drag.mode === "trim-left" && !isTimed(clip)) {
          let newIn = frameSnap(
            project,
            Math.min(drag.origOut - MIN_TRIM_US, Math.max(0, drag.origIn + delta)),
          );
          const inDelta = newIn - drag.origIn;
          let newPos = drag.origPos + inDelta;
          if (store.snapEnabled && !ev.altKey) {
            const snappedPos = snapTime(
              newPos,
              project,
              store.playhead,
              store.pxPerSec,
              true,
              clip.id,
            );
            store.setSnapGuide(snappedPos !== newPos ? snappedPos : null);
            const posDelta = snappedPos - drag.origPos;
            newIn = drag.origIn + posDelta;
            newPos = snappedPos;
            if (newIn > drag.origOut - MIN_TRIM_US) {
              newIn = drag.origOut - MIN_TRIM_US;
              newPos = drag.origPos + (newIn - drag.origIn);
            }
            if (newIn < 0) {
              newIn = 0;
              newPos = drag.origPos + (newIn - drag.origIn);
            }
          }
          const finalIn = newIn;
          // On a magnetic track the clip's start is where the track packs it; a left trim
          // only shortens it and core pulls the later clips in.
          const finalPos = magneticTrim ? drag.origPos : newPos;
          nextProject = withClip(project, drag.trackId, drag.clipId, (c) =>
            isTimed(c) || c.type === "adjustment" ? c : { ...c, source_in_us: finalIn, start_us: finalPos },
          );
        } else if (drag.mode === "trim-left-caption" && isTimed(clip)) {
          const newPos = snapTime(
            Math.max(0, drag.origPos + delta),
            project,
            store.playhead,
            store.pxPerSec,
            store.snapEnabled && !ev.altKey,
            clip.id,
          );
          store.setSnapGuide(
            store.snapEnabled && !ev.altKey && newPos !== drag.origPos + delta
              ? newPos
              : null,
          );
          const posDelta = newPos - drag.origPos;
          nextProject = withClip(project, drag.trackId, drag.clipId, (c) =>
            isTimed(c)
              ? {
                  ...c,
                  start_us: magneticTrim ? drag.origPos : newPos,
                  duration_us: Math.max(MIN_CAPTION_US, drag.origDur - posDelta),
                }
              : c,
          );
        }
      }

      if (nextProject && store.project) {
        useEditorStore.setState({
          project: replaceActiveTracks(store.project, store.activeSequenceId, nextProject.tracks),
        });
      }
    };

    const onMouseUp = (ev: MouseEvent) => {
      const drag = dragStateRef.current;
      dragStateRef.current = null;
      const store = useEditorStore.getState();
      store.setSnapGuide(null);
      store.setDragging(false);
      canvas.style.cursor = "default";
      const project = selectActiveView(store);
      if (!drag || !project) return;

      if (drag.mode === "pan") return;

      if (drag.mode === "scrub") {
        // Pin the paused frame with a real seek (scrubAudio is for live feedback).
        void store.seekTo(store.playhead);
        return;
      }

      if (drag.mode === "move") {
        // C3: group commit — same delta for all selected clips (all-or-nothing batch).
        if (drag.group && drag.group.length > 1) {
          const primary = findClip(project, drag.currentTrackId, drag.clipId);
          if (!primary) return;
          const appliedDelta = primary.start_us - drag.origPos;
          let minOrig = Infinity;
          for (const g of drag.group) minOrig = Math.min(minOrig, g.origPos);
          const clampedDelta = appliedDelta < -minOrig ? -minOrig : appliedDelta;
          const commands = drag.group.map((g) =>
            moveClip(g.trackId, g.clipId, Math.max(0, g.origPos + clampedDelta)),
          );
          void store.dispatchBatch(commands);
          return;
        }

        const clip = findClip(project, drag.currentTrackId, drag.clipId);
        if (!clip) return;
        const landing = project.tracks.find((t) => t.id === drag.currentTrackId);
        // Magnetic landing track: send the raw (frame-snapped) drop position; core picks the slot.
        const pos =
          landing?.magnetic === true
            ? frameSnap(project, clip.start_us)
            : snapTime(
                clip.start_us,
                project,
                store.playhead,
                store.pxPerSec,
                store.snapEnabled && !ev.altKey,
                clip.id,
              );
        const newTrackId = drag.currentTrackId !== drag.origTrackId ? drag.currentTrackId : null;
        void store.dispatch(moveClip(drag.origTrackId, clip.id, pos, newTrackId));
        return;
      }

      if (drag.mode === "trim-adjustment") {
        const adj = findClip(project, drag.trackId, drag.clipId);
        if (!adj || adj.type !== "adjustment") return;
        if (
          adj.start_us !== drag.origPos ||
          adj.duration_us !== drag.origDur
        ) {
          void store.dispatch(
            setAdjustmentLayer(drag.trackId, adj.id, {
              startUs: adj.start_us,
              durationUs: adj.duration_us,
            }),
          );
        }
        return;
      }

      const clip = findClip(project, drag.trackId, drag.clipId);
      if (!clip || clip.type === "adjustment") return;

      const magnetic = project.tracks.find((t) => t.id === drag.trackId)?.magnetic === true;
      if (drag.mode === "trim-right") {
        if (isTimed(clip)) {
          void store.dispatch(
            timingCommand(drag.trackId, clip, { durationUs: Math.max(MIN_CAPTION_US, clip.duration_us) }),
          );
        } else {
          void store.dispatch(trimClip(drag.trackId, clip.id, null, clip.source_out_us));
        }
      } else if (drag.mode === "trim-left-caption" && isTimed(clip)) {
        void store.dispatch(
          timingCommand(drag.trackId, clip, {
            // A magnetic track keeps the clip where it packs; only the length changes.
            ...(magnetic ? {} : { startUs: clip.start_us }),
            durationUs: Math.max(MIN_CAPTION_US, clip.duration_us),
          }),
        );
      } else if (drag.mode === "trim-left" && !isTimed(clip)) {
        const inDelta = clip.source_in_us - drag.origIn;
        if (inDelta !== 0) {
          void store.dispatchBatch(
            magnetic
              ? [trimClip(drag.trackId, clip.id, clip.source_in_us, null)]
              : [
                  trimClip(drag.trackId, clip.id, clip.source_in_us, null),
                  moveClip(drag.trackId, clip.id, clip.start_us),
                ],
          );
        }
      }
    };

    canvas.addEventListener("wheel", onWheel, { passive: false });
    canvas.addEventListener("mousedown", onMouseDown);
    window.addEventListener("mousemove", onMouseMove);
    window.addEventListener("mouseup", onMouseUp);
    return () => {
      canvas.removeEventListener("wheel", onWheel);
      canvas.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("mousemove", onMouseMove);
      window.removeEventListener("mouseup", onMouseUp);
    };
  }, [canvasRef]);
}

export async function splitSelectedAtPlayhead(): Promise<void> {
  const store = useEditorStore.getState();
  if (!store.selection) {
    store.toast("Select a clip first, then split at the playhead", "info");
    return;
  }
  const track = activeTracks(store.project, store.activeSequenceId).find(
    (t) => t.id === store.selection!.trackId,
  );
  if (track?.locked) {
    store.toast("Track is locked — unlock it to edit", "info");
    return;
  }
  const at = store.project ? frameSnap(store.project, store.playhead) : store.playhead;
  await store.dispatch(splitClip(store.selection.trackId, store.selection.clipId, at));
}

export async function deleteSelected(ripple: boolean): Promise<void> {
  const store = useEditorStore.getState();
  if (store.selections.length === 0) return;
  for (const sel of store.selections) {
    const track = activeTracks(store.project, store.activeSequenceId).find((t) => t.id === sel.trackId);
    if (track?.locked) {
      store.toast("Track is locked — unlock it to edit", "info");
      return;
    }
  }
  const commands = store.selections.map((sel) => deleteClip(sel.trackId, sel.clipId, ripple));
  if (commands.length === 1) await store.dispatch(commands[0]!);
  else await store.dispatchBatch(commands);
  store.select(null);
}
