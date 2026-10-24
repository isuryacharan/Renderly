import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { setClipTransform } from "../../lib/commands";
import {
  applyHandleDrag,
  cursorForHandle,
  hitTestHandle,
  ndcToPx,
  pxToNdc,
  resolveTransform,
  rotationHandleNdc,
  transformedCorners,
  type DragMods,
  type HandleKind,
} from "../../lib/previewMath";
import type { ClipTransform, MediaClip } from "../../lib/types";
import { clipDurationUs } from "../../lib/types";
import { activeContainerSize, activeTracks } from "../../lib/container";
import { useEditorStore } from "../../store/editorStore";

type ContentBounds = { left: number; top: number; width: number; height: number };

function contentBoundsInHost(host: HTMLElement, aspect: number): ContentBounds | null {
  const rect = host.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return null;
  const hostAspect = rect.width / rect.height;
  let width: number;
  let height: number;
  if (hostAspect > aspect) {
    height = rect.height;
    width = height * aspect;
  } else {
    width = rect.width;
    height = width / aspect;
  }
  return {
    left: (rect.width - width) / 2,
    top: (rect.height - height) / 2,
    width,
    height,
  };
}

function activeVideoSelection(
  playhead: number,
): { trackId: string; clip: MediaClip; locked: boolean } | null {
  const { project, selection, activeSequenceId } = useEditorStore.getState();
  if (!project || !selection) return null;
  const track = activeTracks(project, activeSequenceId).find((t) => t.id === selection.trackId);
  if (!track || track.kind !== "video" || track.hidden) return null;
  const clip = track.clips.find((c) => c.id === selection.clipId);
  if (!clip || clip.type !== "video" || !clip.enabled) return null;
  const end = clip.start_us + clipDurationUs(clip);
  if (playhead < clip.start_us || playhead >= end) return null;
  return { trackId: track.id, clip, locked: track.locked };
}

type DragRef = {
  kind: HandleKind;
  trackId: string;
  clipId: string;
  startTransform: ClipTransform;
  /** The transform as of the last mouse move; committed on release. */
  latest: ClipTransform;
  startNdc: { x: number; y: number };
  moved: boolean;
};

type Guides = { v: boolean; h: boolean };

function readoutText(
  kind: HandleKind,
  t: ClipTransform,
  projW: number,
  projH: number,
): string {
  if (kind === "move") {
    const px = Math.round((t.x * projW) / 2);
    const py = Math.round((-t.y * projH) / 2);
    return `x ${px >= 0 ? "+" : ""}${px}  y ${py >= 0 ? "+" : ""}${py}`;
  }
  if (kind === "rotate") {
    return `${t.rotation_deg.toFixed(1).replace(/\.0$/, "")}°`;
  }
  const sx = Math.round(t.scale_x * 100);
  const sy = Math.round(t.scale_y * 100);
  return sx === sy ? `${sx}%` : `${sx}% × ${sy}%`;
}

export function PreviewHandlesOverlay({
  hostRef,
  aspect,
}: {
  hostRef: React.RefObject<HTMLDivElement | null>;
  aspect: number;
}) {
  const playhead = useEditorStore((s) => s.playhead);
  const selection = useEditorStore((s) => s.selection);
  const playing = useEditorStore((s) => s.playing);
  const project = useEditorStore((s) => s.project);
  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const toolMode = useEditorStore((s) => s.toolMode);
  const dispatch = useEditorStore((s) => s.dispatch);
  const setDragging = useEditorStore((s) => s.setDragging);
  const setPreviewOverride = useEditorStore((s) => s.setPreviewOverride);
  const clearPreviewOverride = useEditorStore((s) => s.clearPreviewOverride);
  const endGesture = useEditorStore((s) => s.endGesture);

  const [bounds, setBounds] = useState<ContentBounds | null>(null);
  const [guides, setGuides] = useState<Guides>({ v: false, h: false });
  const [readout, setReadout] = useState<string | null>(null);
  const [cursor, setCursor] = useState<string>("default");
  const dragRef = useRef<DragRef | null>(null);
  const overlayRef = useRef<HTMLDivElement | null>(null);

  const active = useMemo(() => {
    if (playing || !project || toolMode === "mask") return null;
    return activeVideoSelection(playhead);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- selection/project drive refresh
  }, [playhead, playing, project, selection, toolMode]);

  // The drag's render-only override (see endDrag): the box follows it while dragging.
  const liveTransform = useEditorStore((s) =>
    active ? s.previewOverrides[active.clip.id]?.transform : undefined,
  );
  const transform = resolveTransform(liveTransform ?? active?.clip.transform);
  // The open container's output size (a sequence may have its own).
  const outSize = project ? activeContainerSize(project, activeSequenceId) : null;
  const projW = outSize?.width ?? 1920;
  const projH = outSize?.height ?? 1080;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const sync = () => setBounds(contentBoundsInHost(host, aspect));
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(host);
    return () => ro.disconnect();
  }, [hostRef, aspect, project]);

  const endDrag = useCallback(
    (cancel: boolean) => {
      const drag = dragRef.current;
      if (!drag) return;
      dragRef.current = null;
      setDragging(false);
      setGuides({ v: false, h: false });
      setReadout(null);
      if (cancel || !drag.moved) {
        // Nothing to commit: dropping the override restores the saved transform.
        clearPreviewOverride(drag.clipId);
        return;
      }
      // One command on release, with a merge key so back-to-back handle drags on the same clip
      // coalesce. The override is cleared only once the project holds the committed value, so
      // the box does not flicker back to the pre-drag transform.
      void (async () => {
        await dispatch(setClipTransform(drag.trackId, drag.clipId, drag.latest), false, {
          mergeKey: `transform:${drag.clipId}`,
        });
        clearPreviewOverride(drag.clipId);
        endGesture();
      })();
    },
    [dispatch, setDragging, clearPreviewOverride, endGesture],
  );

  useEffect(() => {
    const onMove = (ev: MouseEvent) => {
      const drag = dragRef.current;
      const overlay = overlayRef.current;
      if (!drag || !overlay) return;
      const rect = overlay.getBoundingClientRect();
      const local = { x: ev.clientX - rect.left, y: ev.clientY - rect.top };
      const curNdc = pxToNdc(local, rect.width, rect.height);
      const mods: DragMods = { shift: ev.shiftKey, alt: ev.altKey };
      const outcome = applyHandleDrag(drag.kind, drag.startTransform, drag.startNdc, curNdc, mods);
      drag.moved = true;
      drag.latest = outcome.transform;
      setGuides({ v: outcome.guideV, h: outcome.guideH });
      setReadout(readoutText(drag.kind, outcome.transform, projW, projH));
      setPreviewOverride(drag.clipId, { transform: outcome.transform });
    };

    const onUp = () => endDrag(false);
    const onKey = (ev: KeyboardEvent) => {
      if (ev.key === "Escape" && dragRef.current) {
        ev.stopPropagation();
        endDrag(true);
      }
    };

    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    window.addEventListener("keydown", onKey, true);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      window.removeEventListener("keydown", onKey, true);
    };
  }, [endDrag, projW, projH, setPreviewOverride]);

  if (!active || !bounds || active.locked || bounds.width < 8) return null;

  const w = bounds.width;
  const h = bounds.height;
  const corners = transformedCorners(transform).map((c) => ndcToPx(c, w, h));
  const centerPx = ndcToPx({ x: transform.x, y: transform.y }, w, h);
  const rot = ndcToPx(rotationHandleNdc(transform), w, h);
  const poly = corners.map((c) => `${c.x},${c.y}`).join(" ");
  // Screen-space rotation (NDC +Y up flips sign when projected to pixels).
  const svgAngle = -transform.rotation_deg;

  const mid = (a: { x: number; y: number }, b: { x: number; y: number }) => ({
    x: (a.x + b.x) / 2,
    y: (a.y + b.y) / 2,
  });
  const edgeHandles: { kind: HandleKind; p: { x: number; y: number }; along: number }[] = [
    { kind: "scale-s", p: mid(corners[0], corners[1]), along: svgAngle },
    { kind: "scale-e", p: mid(corners[1], corners[2]), along: svgAngle + 90 },
    { kind: "scale-n", p: mid(corners[2], corners[3]), along: svgAngle },
    { kind: "scale-w", p: mid(corners[3], corners[0]), along: svgAngle + 90 },
  ];

  const hoverHitTest = (ev: React.MouseEvent) => {
    if (dragRef.current) return;
    const overlay = overlayRef.current;
    if (!overlay) return;
    const rect = overlay.getBoundingClientRect();
    const local = { x: ev.clientX - rect.left, y: ev.clientY - rect.top };
    const kind = hitTestHandle(local, transform, w, h);
    setCursor(kind ? cursorForHandle(kind, local, centerPx) : "default");
  };

  const onMouseDown = (ev: React.MouseEvent) => {
    if (ev.button !== 0 || active.locked) return;
    const overlay = overlayRef.current;
    if (!overlay) return;
    const rect = overlay.getBoundingClientRect();
    const local = { x: ev.clientX - rect.left, y: ev.clientY - rect.top };
    const kind = hitTestHandle(local, transform, w, h);
    if (!kind) return;
    ev.preventDefault();
    ev.stopPropagation();
    setDragging(true);
    setCursor(kind === "rotate" ? "grabbing" : cursorForHandle(kind, local, centerPx));
    dragRef.current = {
      kind,
      trackId: active.trackId,
      clipId: active.clip.id,
      startTransform: { ...transform },
      latest: { ...transform },
      startNdc: pxToNdc(local, w, h),
      moved: false,
    };
  };

  // Anchor the readout above the box's topmost corner.
  const topY = Math.min(...corners.map((c) => c.y));

  return (
    <div
      ref={overlayRef}
      className="preview-handles-overlay"
      style={{
        left: bounds.left,
        top: bounds.top,
        width: bounds.width,
        height: bounds.height,
        cursor,
      }}
      onMouseDown={onMouseDown}
      onMouseMove={hoverHitTest}
      onMouseLeave={() => !dragRef.current && setCursor("default")}
    >
      <svg className="preview-handles-svg" width={w} height={h}>
        {guides.v && <line x1={w / 2} y1={0} x2={w / 2} y2={h} className="preview-guide" />}
        {guides.h && <line x1={0} y1={h / 2} x2={w} y2={h / 2} className="preview-guide" />}
        <polygon points={poly} className="preview-handle-box" />
        <line
          x1={(corners[2].x + corners[3].x) / 2}
          y1={(corners[2].y + corners[3].y) / 2}
          x2={rot.x}
          y2={rot.y}
          className="preview-handle-rot-line"
        />
        {edgeHandles.map((e) => (
          <rect
            key={e.kind}
            x={e.p.x - 8}
            y={e.p.y - 3}
            width={16}
            height={6}
            rx={3}
            className="preview-handle-edge"
            transform={`rotate(${e.along} ${e.p.x} ${e.p.y})`}
          />
        ))}
        {corners.map((c, i) => (
          <rect
            key={i}
            x={c.x - 5}
            y={c.y - 5}
            width={10}
            height={10}
            rx={2.5}
            className="preview-handle-knob"
            transform={`rotate(${svgAngle} ${c.x} ${c.y})`}
          />
        ))}
        <g className="preview-handle-rotate" transform={`translate(${rot.x} ${rot.y})`}>
          <circle r={7} className="preview-handle-knob rotate" />
          <path
            d="M -3 0.5 A 3 3 0 1 1 0.5 3 M 0.5 3 l -1.8 -0.4 M 0.5 3 l 0.5 -1.8"
            className="preview-handle-rotate-glyph"
          />
        </g>
      </svg>
      {readout && (
        <div
          className="preview-readout"
          style={{ left: centerPx.x, top: Math.max(14, topY - 16) }}
        >
          {readout}
        </div>
      )}
    </div>
  );
}
