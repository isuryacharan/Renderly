import { useEffect, useMemo, useRef, useState } from "react";
import { setClipMask } from "../../lib/commands";
import * as ipc from "../../lib/ipc";
import {
  applyMaskHandleDrag,
  boundsToEllipseKind,
  boundsToRectKind,
  coverUv,
  defaultMask,
  hitTestMaskHandle,
  maskCornerUvs,
  normalizeRectUv,
  previewPxToSourceUv,
  shapeBounds,
  sourceUvToPreviewPx,
  type MaskBoundsUv,
  type MaskHandleKind,
} from "../../lib/maskMath";
import type { AdjustmentClip, ClipMask, MediaClip } from "../../lib/types";
import { clipDurationSecs } from "../../lib/types";
import { resolveTransform } from "../../lib/previewMath";
import { activeTracks, findTrackAnywhere, updateClip } from "../../lib/container";
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
): { trackId: string; clip: MediaClip | AdjustmentClip; locked: boolean } | null {
  const { project, selection, activeSequenceId } = useEditorStore.getState();
  if (!project || !selection) return null;
  const track = activeTracks(project, activeSequenceId).find((t) => t.id === selection.trackId);
  if (!track || track.kind !== "video" || track.hidden) return null;
  const clip = track.clips.find((c) => c.id === selection.clipId);
  // Adjustment layers take masks too (their UVs are output-frame: no cover-fit or transform).
  if (!clip || (clip.type !== "video" && clip.type !== "adjustment") || !clip.enabled) return null;
  const end = clip.position_secs + clipDurationSecs(clip);
  if (playhead < clip.position_secs || playhead >= end) return null;
  return { trackId: track.id, clip, locked: track.locked };
}

function patchClipMask(trackId: string, clipId: string, mask: ClipMask | null) {
  const project = useEditorStore.getState().project;
  if (!project) return;
  useEditorStore.setState({
    project: updateClip(project, trackId, clipId, (c) => (c.type !== "caption" ? { ...c, mask } : c)),
  });
}

function isShapeMask(mask: ClipMask | null | undefined): mask is ClipMask {
  if (!mask) return false;
  return mask.kind.type === "rect" || mask.kind.type === "ellipse";
}

type DragRef =
  | {
      mode: "edit";
      kind: MaskHandleKind;
      trackId: string;
      clipId: string;
      startBounds: MaskBoundsUv;
      startUv: { u: number; v: number };
      shape: "rect" | "ellipse";
      base: ClipMask;
    }
  | {
      mode: "create";
      trackId: string;
      clipId: string;
      startUv: { u: number; v: number };
      ellipse: boolean;
      fromCenter: boolean;
      baseFeather: number;
    };

export function PreviewMaskOverlay({
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
  const toolMode = useEditorStore((s) => s.toolMode);
  const dispatch = useEditorStore((s) => s.dispatch);
  const setDragging = useEditorStore((s) => s.setDragging);

  const [bounds, setBounds] = useState<ContentBounds | null>(null);
  const dragRef = useRef<DragRef | null>(null);
  const overlayRef = useRef<HTMLDivElement | null>(null);

  const active = useMemo(() => {
    if (playing || !project || toolMode !== "mask") return null;
    return activeVideoSelection(playhead);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- selection/project drive refresh
  }, [playhead, playing, project, selection, toolMode]);

  const activeMediaClip = active?.clip.type === "video" ? active.clip : null;
  const media = project?.media.find((m) => m.id === activeMediaClip?.media_id);
  const outW = project?.settings.width ?? 1080;
  const outH = project?.settings.height ?? 1920;
  const layerW = media?.width ?? outW;
  const layerH = media?.height ?? outH;
  const cover = useMemo(
    () => coverUv(layerW, layerH, outW, outH),
    [layerW, layerH, outW, outH],
  );
  const transform = resolveTransform(activeMediaClip?.transform);
  const mask = active?.clip.mask ?? null;
  const shapeOk = isShapeMask(mask);
  const maskBounds = shapeOk ? shapeBounds(mask.kind) : null;

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const sync = () => setBounds(contentBoundsInHost(host, aspect));
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(host);
    return () => ro.disconnect();
  }, [hostRef, aspect, project]);

  useEffect(() => {
    const onMove = (ev: MouseEvent) => {
      const drag = dragRef.current;
      const overlay = overlayRef.current;
      if (!drag || !overlay) return;
      const rect = overlay.getBoundingClientRect();
      const local = { x: ev.clientX - rect.left, y: ev.clientY - rect.top };
      const curUv = previewPxToSourceUv(local, cover, transform, rect.width, rect.height);

      if (drag.mode === "create") {
        let box: MaskBoundsUv;
        if (drag.fromCenter) {
          const halfU = Math.abs(curUv.u - drag.startUv.u);
          const halfV = Math.abs(curUv.v - drag.startUv.v);
          box = normalizeRectUv(
            drag.startUv.u - halfU,
            drag.startUv.v - halfV,
            drag.startUv.u + halfU,
            drag.startUv.v + halfV,
          );
        } else {
          box = normalizeRectUv(drag.startUv.u, drag.startUv.v, curUv.u, curUv.v);
        }
        const kind = drag.ellipse ? boundsToEllipseKind(box) : boundsToRectKind(box);
        const next = defaultMask(kind, drag.baseFeather);
        patchClipMask(drag.trackId, drag.clipId, next);
        return;
      }

      const nextBounds = applyMaskHandleDrag(drag.kind, drag.startBounds, drag.startUv, curUv);
      const kind =
        drag.shape === "ellipse" ? boundsToEllipseKind(nextBounds) : boundsToRectKind(nextBounds);
      const next: ClipMask = { ...drag.base, kind };
      patchClipMask(drag.trackId, drag.clipId, next);
    };

    const onUp = async () => {
      const drag = dragRef.current;
      if (!drag) return;
      dragRef.current = null;
      setDragging(false);
      const projectNow = useEditorStore.getState().project;
      const clip = (projectNow ? findTrackAnywhere(projectNow, drag.trackId) : undefined)?.clips.find(
        (c) => c.id === drag.clipId,
      );
      const committed =
        clip && clip.type !== "caption" ? (clip.mask ?? null) : null;
      // Ignore zero-size create drags
      if (
        drag.mode === "create" &&
        committed &&
        (committed.kind.type === "rect" || committed.kind.type === "ellipse")
      ) {
        const b = shapeBounds(committed.kind);
        if (!b || b.width < 0.005 || b.height < 0.005) {
          patchClipMask(drag.trackId, drag.clipId, null);
          await ipc.seek(useEditorStore.getState().playhead).catch(() => {});
          return;
        }
      }
      await dispatch(setClipMask(drag.trackId, drag.clipId, committed));
    };

    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [cover, dispatch, setDragging, transform]);

  if (!active || !bounds || active.locked || bounds.width < 8) return null;

  const w = bounds.width;
  const h = bounds.height;

  const onMouseDown = (ev: React.MouseEvent) => {
    if (ev.button !== 0 || active.locked) return;
    const overlay = overlayRef.current;
    if (!overlay) return;
    const rect = overlay.getBoundingClientRect();
    const local = { x: ev.clientX - rect.left, y: ev.clientY - rect.top };
    const uv = previewPxToSourceUv(local, cover, transform, w, h);

    if (maskBounds && shapeOk) {
      const kind = hitTestMaskHandle(local, maskBounds, cover, transform, w, h);
      if (!kind) return;
      ev.preventDefault();
      ev.stopPropagation();
      setDragging(true);
      dragRef.current = {
        mode: "edit",
        kind,
        trackId: active.trackId,
        clipId: active.clip.id,
        startBounds: { ...maskBounds },
        startUv: uv,
        shape: mask.kind.type === "ellipse" ? "ellipse" : "rect",
        base: {
          enabled: mask.enabled ?? true,
          invert: mask.invert ?? false,
          feather: mask.feather ?? 0,
          kind: mask.kind,
        },
      };
      return;
    }

    // Create new shape mask (Alt = from center; Shift = ellipse)
    if (mask && !shapeOk) return; // raster/generated: inspector-only
    ev.preventDefault();
    ev.stopPropagation();
    setDragging(true);
    dragRef.current = {
      mode: "create",
      trackId: active.trackId,
      clipId: active.clip.id,
      startUv: uv,
      ellipse: ev.shiftKey,
      fromCenter: ev.altKey,
      baseFeather: mask?.feather ?? 0.05,
    };
  };

  let outline: React.ReactNode = null;
  if (maskBounds && shapeOk) {
    const corners = maskCornerUvs(maskBounds).map((uv) =>
      sourceUvToPreviewPx(uv, cover, transform, w, h),
    );
    const poly = corners.map((c) => `${c.x},${c.y}`).join(" ");
    if (mask.kind.type === "ellipse") {
      // Approximate ellipse as transformed bounding-box ellipse via SVG ellipse in UV AABB
      // projected through corner midpoints — draw polygon of many points for accuracy.
      const pts: string[] = [];
      const steps = 48;
      for (let i = 0; i < steps; i++) {
        const a = (i / steps) * Math.PI * 2;
        const u = mask.kind.cx + mask.kind.rx * Math.cos(a);
        const v = mask.kind.cy + mask.kind.ry * Math.sin(a);
        const p = sourceUvToPreviewPx({ u, v }, cover, transform, w, h);
        pts.push(`${p.x},${p.y}`);
      }
      outline = (
        <>
          <polygon points={pts.join(" ")} className="preview-mask-outline" />
          {corners.map((c, i) => (
            <circle key={i} cx={c.x} cy={c.y} r={5} className="preview-mask-knob" />
          ))}
        </>
      );
    } else {
      outline = (
        <>
          <polygon points={poly} className="preview-mask-outline" />
          {corners.map((c, i) => (
            <circle key={i} cx={c.x} cy={c.y} r={5} className="preview-mask-knob" />
          ))}
        </>
      );
    }
  }

  return (
    <div
      ref={overlayRef}
      className="preview-mask-overlay"
      style={{
        left: bounds.left,
        top: bounds.top,
        width: bounds.width,
        height: bounds.height,
        cursor: shapeOk ? "default" : "crosshair",
      }}
      onMouseDown={onMouseDown}
    >
      <svg className="preview-handles-svg" width={w} height={h}>
        {outline}
      </svg>
      {!shapeOk && (
        <div className="preview-mask-hint">
          Drag to draw mask · Shift=ellipse · Alt=from center
        </div>
      )}
    </div>
  );
}
