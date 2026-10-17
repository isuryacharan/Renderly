import { useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { deleteClip, setAdjustmentLayer, setClipEnabled } from "../../lib/commands";
import { MIN_ADJUSTMENT_SECS } from "../../lib/container";
import type { AdjustmentClip, Track } from "../../lib/types";
import { Tooltip } from "../ui/Tooltip";
import { KeyframeEditor } from "./KeyframeEditor";
import { EffectStackSection } from "./EffectStackSection";
import { MaskSection } from "./MaskSection";

/// Inspector for an adjustment layer (schema v7): timing, opacity (the mix between the
/// unadjusted and adjusted picture), opacity keyframes, the effect stack and a mask. The
/// effect stack and mask sections are the same components the media-clip inspector uses.
export function AdjustmentInspector({ track, clip }: { track: Track; clip: AdjustmentClip }) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const select = useEditorStore((s) => s.select);
  const setLeftTab = useEditorStore((s) => s.setLeftTab);
  const setPreviewOverride = useEditorStore((s) => s.setPreviewOverride);
  const clearPreviewOverride = useEditorStore((s) => s.clearPreviewOverride);
  const endGesture = useEditorStore((s) => s.endGesture);

  const opacityNow = clip.opacity ?? 1;
  const [opacity, setOpacity] = useState(opacityNow);
  const [position, setPosition] = useState(clip.position_secs);
  const [duration, setDuration] = useState(clip.duration_secs);

  useEffect(() => {
    setOpacity(clip.opacity ?? 1);
    setPosition(clip.position_secs);
    setDuration(clip.duration_secs);
  }, [clip.id, clip.opacity, clip.position_secs, clip.duration_secs]);

  const hasOpacityKeys = (clip.keyframes ?? []).some((k) => k.property === "opacity" && k.keys.length > 0);
  // Release commits one undo step; the drag itself is a render-only override (no command).
  const commitOpacity = async (v: number) => {
    if (v !== opacityNow) {
      await dispatch(setAdjustmentLayer(track.id, clip.id, { opacity: v }), false, {
        mergeKey: `opacity:${clip.id}`,
      });
    }
    clearPreviewOverride(clip.id);
    endGesture();
  };

  return (
    <div className="inspector">
      <div className="inspector-section">
        <div className="insp-clip-head">
          <div>
            <h3>Adjustment layer</h3>
            <p className="insp-meta">{track.name}</p>
            <p className="insp-meta muted">Applies its effects to everything beneath it</p>
          </div>
          <label className="insp-enable">
            <input
              type="checkbox"
              checked={clip.enabled}
              disabled={track.locked}
              onChange={(e) => void dispatch(setClipEnabled(track.id, clip.id, e.target.checked))}
            />
            <span>On</span>
          </label>
        </div>
      </div>

      <div className="inspector-section">
        <h3>Timing</h3>
        <div className="insp-grid-2">
          <label className="insp-cell">
            <span>Start</span>
            <input
              type="number"
              step={0.1}
              min={0}
              value={Number.isFinite(position) ? position : 0}
              disabled={track.locked}
              onChange={(e) => setPosition(parseFloat(e.target.value) || 0)}
              onBlur={(e) => {
                const v = Math.max(0, parseFloat(e.target.value) || 0);
                setPosition(v);
                if (v !== clip.position_secs)
                  void dispatch(setAdjustmentLayer(track.id, clip.id, { positionSecs: v }));
              }}
            />
          </label>
          <label className="insp-cell">
            <span>Duration</span>
            <input
              type="number"
              step={0.1}
              min={MIN_ADJUSTMENT_SECS}
              value={Number.isFinite(duration) ? duration : 0}
              disabled={track.locked}
              onChange={(e) => setDuration(parseFloat(e.target.value) || 0)}
              onBlur={(e) => {
                const v = Math.max(MIN_ADJUSTMENT_SECS, parseFloat(e.target.value) || 0);
                setDuration(v);
                if (v !== clip.duration_secs)
                  void dispatch(setAdjustmentLayer(track.id, clip.id, { durationSecs: v }));
              }}
            />
          </label>
        </div>
      </div>

      <div className="inspector-section">
        <h3>Strength</h3>
        <label className="insp-cell">
          <span>Opacity · {Math.round(opacity * 100)}%</span>
          <input
            type="range"
            min={0}
            max={1}
            step={0.01}
            value={opacity}
            disabled={track.locked}
            aria-label="Adjustment opacity"
            onChange={(e) => {
              const v = parseFloat(e.target.value);
              setOpacity(v);
              setPreviewOverride(clip.id, { opacity: v });
            }}
            onMouseUp={(e) => commitOpacity(parseFloat((e.target as HTMLInputElement).value))}
            onTouchEnd={(e) => commitOpacity(parseFloat((e.target as HTMLInputElement).value))}
            onKeyUp={(e) => commitOpacity(parseFloat((e.target as HTMLInputElement).value))}
          />
        </label>
        {hasOpacityKeys && (
          <p className="empty-hint">Opacity keyframes are set - they animate this value over the layer.</p>
        )}
      </div>

      <details className="insp-disclosure">
        <summary>Keyframes</summary>
        <KeyframeEditor track={track} clip={clip} nested />
      </details>

      <EffectStackSection track={track} clip={clip} />
      <div className="inspector-section">
        <div className="insp-btn-row">
          <button type="button" disabled={track.locked} onClick={() => setLeftTab("effects")}>
            {(clip.effects?.length ?? 0) > 0 ? "Add more effects" : "Add effects"}
          </button>
          <button type="button" disabled={track.locked} onClick={() => setLeftTab("filters")}>
            Filters
          </button>
        </div>
        <p className="empty-hint">
          Built-in effects and pack LUTs only - WASM plugin effects cannot run on an adjustment layer.
        </p>
      </div>

      <MaskSection
        track={track}
        clip={clip}
        hint="Mask coordinates cover the whole output frame. Mask tool (M) · drag on the paused preview."
      />

      <div className="inspector-actions">
        <Tooltip content="Delete adjustment layer">
          <button
            type="button"
            className="btn-danger"
            disabled={track.locked}
            aria-label="Delete adjustment layer"
            onClick={async () => {
              await dispatch(deleteClip(track.id, clip.id, false));
              select(null);
            }}
          >
            <Trash2 size={14} strokeWidth={1.75} className="btn-lucide" />
          </button>
        </Tooltip>
      </div>
    </div>
  );
}
