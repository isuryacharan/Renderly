// Timeline mouse interaction state machine: hit-test → drag → commit-on-mouseup. Drag
// mutates the store's `project` immutably for live visual feedback (optimistic local
// mutation), then issues exactly one command on mouseup — see docs/architecture.md.

import { useEffect, useRef, type RefObject } from "react";
import { deleteClip, moveClip, setCaption, splitClip, trimClip } from "../lib/commands";
import type { Clip, Project } from "../lib/types";
import { useEditorStore } from "../store/editorStore";
import {
  dropTargetAtY,
  hitTestClip,
  hitTestTransitionJunction,
  RULER_H,
  TRACK_LABEL_W,
  secsFromCanvasX,
} from "./layout";
import { snapTime } from "./snap";

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
  | { mode: "trim-right"; trackId: string; clipId: string; startX: number; origOut: number };

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
  newPositionSecs: number,
): Project {
  let moved: Clip | undefined;
  const withoutSrc = project.tracks.map((t) => {
    if (t.id !== srcTrackId) return t;
    const clip = t.clips.find((c) => c.id === clipId);
    if (clip) moved = { ...clip, position_secs: newPositionSecs };
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
  const project = store.project;
  if (!project) return;
  const secs = snapTime(
    secsFromCanvasX(x, store.pxPerSec, store.scrollX),
    project,
    store.playhead,
    store.pxPerSec,
    store.snapEnabled && !altKey,
  );
  store.scrubAt(secs);
  if (store.snapEnabled && !altKey) {
    const raw = secsFromCanvasX(x, store.pxPerSec, store.scrollX);
    store.setSnapGuide(Math.abs(secs - raw) > 1e-6 ? secs : null);
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
        const beforeSecs = secsFromCanvasX(x, store.pxPerSec, store.scrollX);
        const nextPx = store.pxPerSec + (ev.deltaY > 0 ? -12 : 12);
        store.setZoom(nextPx);
        const after = useEditorStore.getState();
        // Keep the time under the cursor fixed.
        const newScrollX = beforeSecs * after.pxPerSec - (x - TRACK_LABEL_W - 8);
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
      const project = store.project;
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
            secsFromCanvasX(x, store.pxPerSec, store.scrollX),
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

        // Adjustment layers have no source range to trim (TrimClip is rejected for them):
        // their edges behave like the body (move only).
        const edge = hit.clip.type === "adjustment" ? "body" : hit.edge;
        if (store.toolMode === "select" && edge === "left" && hit.clip.type === "caption") {
          dragStateRef.current = {
            mode: "trim-left-caption",
            trackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origPos: hit.clip.position_secs,
            origDur: hit.clip.duration_secs,
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
            origIn: hit.clip.source_in_secs,
            origOut: hit.clip.source_out_secs,
            origPos: hit.clip.position_secs,
          };
        } else if (store.toolMode === "select" && edge === "right" && hit.clip.type !== "adjustment") {
          dragStateRef.current = {
            mode: "trim-right",
            trackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origOut: hit.clip.type === "caption" ? hit.clip.duration_secs : hit.clip.source_out_secs,
          };
        } else if (store.toolMode === "select") {
          const fresh = useEditorStore.getState();
          let group: { trackId: string; clipId: string; origPos: number }[] | null = null;
          if (fresh.selections.length > 1) {
            group = [];
            for (const s of fresh.selections) {
              const c = findClip(project, s.trackId, s.clipId);
              if (c) group.push({ trackId: s.trackId, clipId: s.clipId, origPos: c.position_secs });
            }
            if (group.length < 2) group = null;
          }
          dragStateRef.current = {
            mode: "move",
            origTrackId: hit.trackId,
            currentTrackId: hit.trackId,
            clipId: hit.clip.id,
            startX: x,
            origPos: hit.clip.position_secs,
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
      const project = store.project;
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
        if (hit && hit.clip.type !== "adjustment" && (hit.edge === "left" || hit.edge === "right"))
          canvas.style.cursor = "ew-resize";
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

      const delta = (x - drag.startX) / store.pxPerSec;
      let nextProject: Project | null = null;

      if (drag.mode === "move") {
        const clip = findClip(project, drag.currentTrackId, drag.clipId);
        if (!clip) return;
        const rawPos = Math.max(0, drag.origPos + delta);
        const pos = snapTime(
          rawPos,
          project,
          store.playhead,
          store.pxPerSec,
          store.snapEnabled && !ev.altKey,
          clip.id,
        );
        store.setSnapGuide(Math.abs(pos - rawPos) > 1e-6 ? pos : null);
        const appliedDelta = pos - drag.origPos;

        // C3 group move: same delta for every selected clip; no cross-track.
        if (drag.group && drag.group.length > 1) {
          // Clamp so none go negative.
          let minOrig = Infinity;
          for (const g of drag.group) minOrig = Math.min(minOrig, g.origPos);
          const clampedDelta = appliedDelta < -minOrig ? -minOrig : appliedDelta;
          let next = project;
          for (const g of drag.group) {
            next = withClip(next, g.trackId, g.clipId, (c) => ({
              ...c,
              position_secs: Math.max(0, g.origPos + clampedDelta),
            }));
          }
          nextProject = next;
        } else {
          // Plain clip-drag never auto-creates a track (only media-panel drops do, via
          // `dropMediaOnTimeline`) — `dropTargetAtY` returning `trackIndex: null` (drag
          // above the top lane or below the bottom lane) correctly falls through to
          // `destTrack` being `undefined` below, same as it did before this used the
          // CapCut display-order mapping.
          const destTrackIndex = dropTargetAtY(project, rect.height, y, store.scrollY).trackIndex;
          const destTrack = destTrackIndex != null ? project.tracks[destTrackIndex] : undefined;
          const srcTrack = project.tracks.find((t) => t.id === drag.currentTrackId);
          const canCrossMove =
            destTrack &&
            srcTrack &&
            destTrack.id !== srcTrack.id &&
            destTrack.kind === srcTrack.kind &&
            !destTrack.locked;

          if (canCrossMove) {
            nextProject = moveClipAcrossTracks(project, drag.currentTrackId, destTrack.id, clip.id, pos);
            dragStateRef.current = { ...drag, currentTrackId: destTrack.id };
          } else {
            nextProject = withClip(project, drag.currentTrackId, clip.id, (c) => ({
              ...c,
              position_secs: pos,
            }));
          }
        }

        // Edge auto-scroll while dragging clips.
        if (x > rect.width - 40) store.panBy(10, 0);
        else if (x < 100) store.panBy(-10, 0);
      } else {
        const clip = findClip(project, drag.trackId, drag.clipId);
        if (!clip || clip.type === "adjustment") return;

        if (drag.mode === "trim-right") {
          const raw =
            clip.type === "caption"
              ? Math.max(0.1, drag.origOut + delta)
              : Math.max(clip.source_in_secs + 0.05, drag.origOut + delta);
          let next = raw;
          if (store.snapEnabled && !ev.altKey) {
            const absEnd =
              clip.type === "caption"
                ? clip.position_secs + raw
                : clip.position_secs + (raw - clip.source_in_secs);
            const snappedAbs = snapTime(
              absEnd,
              project,
              store.playhead,
              store.pxPerSec,
              true,
              clip.id,
            );
            store.setSnapGuide(Math.abs(snappedAbs - absEnd) > 1e-6 ? snappedAbs : null);
            if (clip.type === "caption") {
              next = Math.max(0.1, snappedAbs - clip.position_secs);
            } else {
              next = Math.max(
                clip.source_in_secs + 0.05,
                snappedAbs - clip.position_secs + clip.source_in_secs,
              );
            }
          }
          nextProject = withClip(project, drag.trackId, drag.clipId, (c) =>
            c.type === "caption"
              ? { ...c, duration_secs: next }
              : { ...c, source_out_secs: next },
          );
        } else if (drag.mode === "trim-left" && clip.type !== "caption") {
          let newIn = Math.min(drag.origOut - 0.05, Math.max(0, drag.origIn + delta));
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
            store.setSnapGuide(Math.abs(snappedPos - newPos) > 1e-6 ? snappedPos : null);
            const posDelta = snappedPos - drag.origPos;
            newIn = drag.origIn + posDelta;
            newPos = snappedPos;
            if (newIn > drag.origOut - 0.05) {
              newIn = drag.origOut - 0.05;
              newPos = drag.origPos + (newIn - drag.origIn);
            }
            if (newIn < 0) {
              newIn = 0;
              newPos = drag.origPos + (newIn - drag.origIn);
            }
          }
          const finalIn = newIn;
          const finalPos = newPos;
          nextProject = withClip(project, drag.trackId, drag.clipId, (c) =>
            c.type === "caption"
              ? c
              : { ...c, source_in_secs: finalIn, position_secs: finalPos },
          );
        } else if (drag.mode === "trim-left-caption" && clip.type === "caption") {
          const newPos = snapTime(
            Math.max(0, drag.origPos + delta),
            project,
            store.playhead,
            store.pxPerSec,
            store.snapEnabled && !ev.altKey,
            clip.id,
          );
          store.setSnapGuide(
            store.snapEnabled && !ev.altKey && Math.abs(newPos - (drag.origPos + delta)) > 1e-6
              ? newPos
              : null,
          );
          const posDelta = newPos - drag.origPos;
          nextProject = withClip(project, drag.trackId, drag.clipId, (c) =>
            c.type === "caption"
              ? { ...c, position_secs: newPos, duration_secs: Math.max(0.1, drag.origDur - posDelta) }
              : c,
          );
        }
      }

      if (nextProject) useEditorStore.setState({ project: nextProject });
    };

    const onMouseUp = (ev: MouseEvent) => {
      const drag = dragStateRef.current;
      dragStateRef.current = null;
      const store = useEditorStore.getState();
      store.setSnapGuide(null);
      store.setDragging(false);
      canvas.style.cursor = "default";
      const project = store.project;
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
          const appliedDelta = primary.position_secs - drag.origPos;
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
        const pos = snapTime(
          clip.position_secs,
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

      const clip = findClip(project, drag.trackId, drag.clipId);
      if (!clip || clip.type === "adjustment") return;

      if (drag.mode === "trim-right") {
        if (clip.type === "caption") {
          void store.dispatch(
            setCaption(drag.trackId, clip.id, { durationSecs: Math.max(0.1, clip.duration_secs) }),
          );
        } else {
          void store.dispatch(trimClip(drag.trackId, clip.id, null, clip.source_out_secs));
        }
      } else if (drag.mode === "trim-left-caption" && clip.type === "caption") {
        void store.dispatch(
          setCaption(drag.trackId, clip.id, {
            positionSecs: clip.position_secs,
            durationSecs: Math.max(0.1, clip.duration_secs),
          }),
        );
      } else if (drag.mode === "trim-left" && clip.type !== "caption") {
        const inDelta = clip.source_in_secs - drag.origIn;
        if (Math.abs(inDelta) > 1e-6) {
          void store.dispatchBatch([
            trimClip(drag.trackId, clip.id, clip.source_in_secs, null),
            moveClip(drag.trackId, clip.id, clip.position_secs),
          ]);
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
  const track = store.project?.tracks.find((t) => t.id === store.selection!.trackId);
  if (track?.locked) {
    store.toast("Track is locked — unlock it to edit", "info");
    return;
  }
  await store.dispatch(splitClip(store.selection.trackId, store.selection.clipId, store.playhead));
}

export async function deleteSelected(ripple: boolean): Promise<void> {
  const store = useEditorStore.getState();
  if (store.selections.length === 0) return;
  for (const sel of store.selections) {
    const track = store.project?.tracks.find((t) => t.id === sel.trackId);
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
