import { useEffect, useState } from "react";
import { useEditorStore } from "../../store/editorStore";
import { setClipMask } from "../../lib/commands";
import { defaultMask, switchMaskShape } from "../../lib/maskMath";
import type { ClipMask, EffectClip, Track } from "../../lib/types";

/// Shared "Mask" disclosure: used by the media-clip inspector and the adjustment-layer
/// inspector. Both clip kinds carry `mask` and take `SetClipMask`; the only difference is the
/// coordinate space (a media clip's mask UVs are in its source frame, an adjustment layer's
/// are output-frame - the whole canvas), which `hint` explains to the user.
export function MaskSection({
  track,
  clip,
  hint,
}: {
  track: Track;
  clip: EffectClip;
  hint?: string;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const setTool = useEditorStore((s) => s.setTool);
  const [maskFeather, setMaskFeather] = useState(clip.mask?.feather ?? 0);

  useEffect(() => {
    setMaskFeather(clip.mask?.feather ?? 0);
  }, [clip.id, clip.mask]);

  const m = clip.mask;
  const shapeType =
    m?.kind.type === "rect" || m?.kind.type === "ellipse"
      ? m.kind.type
      : m?.kind.type === "raster" || m?.kind.type === "generated"
        ? m.kind.type
        : null;
  const enabled = m?.enabled ?? true;
  const invert = m?.invert ?? false;
  const locked = track.locked;

  const commitMask = (next: ClipMask | null) => {
    void dispatch(setClipMask(track.id, clip.id, next));
  };

  const patchMask = (patch: Partial<ClipMask> & { kind?: ClipMask["kind"] }) => {
    if (!m) return;
    commitMask({
      enabled: patch.enabled ?? enabled,
      invert: patch.invert ?? invert,
      feather: patch.feather ?? (m.feather ?? 0),
      kind: patch.kind ?? m.kind,
    });
  };

  const commitFeather = (v: number) => {
    if (!m) return;
    setMaskFeather(v);
    commitMask({ ...m, enabled, invert, feather: v });
  };

  return (
    <details className="insp-disclosure" open={!!clip.mask}>
      <summary>Mask</summary>
      <div className="inspector-section insp-disclosure-body">
        <p className="empty-hint">
          {hint ?? "Mask tool (M) · drag on paused preview · Shift=ellipse · Alt=from center"}
        </p>
        <div className="insp-btn-row">
          <button type="button" disabled={locked} onClick={() => setTool("mask")}>
            Mask tool
          </button>
          {shapeType ? <span className="insp-meta muted">· {shapeType}</span> : null}
        </div>
        <div className="insp-btn-row">
          <label className="insp-enable">
            <input
              type="checkbox"
              checked={!!m && enabled}
              disabled={locked || !m}
              onChange={(e) => patchMask({ enabled: e.target.checked })}
            />
            <span>Enable</span>
          </label>
          <label className="insp-enable">
            <input
              type="checkbox"
              checked={!!m && invert}
              disabled={locked || !m}
              onChange={(e) => patchMask({ invert: e.target.checked })}
            />
            <span>Invert</span>
          </label>
        </div>
        <label className="insp-cell">
          <span>Feather · {maskFeather.toFixed(2)}</span>
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={m ? maskFeather : 0}
            disabled={locked || !m}
            onChange={(e) => {
              if (!m) return;
              setMaskFeather(parseFloat(e.target.value));
            }}
            onMouseUp={(e) => commitFeather(parseFloat((e.target as HTMLInputElement).value))}
            onTouchEnd={(e) => commitFeather(parseFloat((e.target as HTMLInputElement).value))}
          />
        </label>
        {(shapeType === "rect" || shapeType === "ellipse") && (
          <div className="insp-btn-row">
            <button
              type="button"
              disabled={locked || shapeType === "rect"}
              onClick={() => {
                if (!m) return;
                const kind = switchMaskShape(m.kind, "rect");
                if (kind) patchMask({ kind });
              }}
            >
              As rect
            </button>
            <button
              type="button"
              disabled={locked || shapeType === "ellipse"}
              onClick={() => {
                if (!m) return;
                const kind = switchMaskShape(m.kind, "ellipse");
                if (kind) patchMask({ kind });
              }}
            >
              As ellipse
            </button>
          </div>
        )}
        <div className="insp-btn-row">
          <button
            type="button"
            disabled={locked}
            onClick={() =>
              commitMask(defaultMask({ type: "rect", x: 0.15, y: 0.15, width: 0.7, height: 0.7 }))
            }
          >
            Rect preset
          </button>
          <button
            type="button"
            disabled={locked}
            onClick={() =>
              commitMask(
                defaultMask({ type: "ellipse", cx: 0.5, cy: 0.5, rx: 0.4, ry: 0.45 }),
              )
            }
          >
            Ellipse preset
          </button>
          <button type="button" disabled={locked || !m} onClick={() => commitMask(null)}>
            Clear
          </button>
        </div>
      </div>
    </details>
  );
}
