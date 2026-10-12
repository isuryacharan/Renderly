import { SlidersHorizontal } from "lucide-react";
import { setClipEffects } from "../../lib/commands";
import type { EffectInstance, MediaClip } from "../../lib/types";
import { activeTracks } from "../../lib/container";
import { useEditorStore } from "../../store/editorStore";
import { EffectList } from "./EffectsPanel";

const COLOR_ADJUST_ID = "builtin:color_adjust";
const DEFAULTS = { exposure: 0, contrast: 1, saturation: 1 };

function newId(): string {
  return crypto.randomUUID();
}

export function AdjustPanel() {
  const project = useEditorStore((s) => s.project);
  const selection = useEditorStore((s) => s.selection);
  const dispatch = useEditorStore((s) => s.dispatch);

  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const track = activeTracks(project, activeSequenceId).find((t) => t.id === selection?.trackId);
  const clip = track?.clips.find((c) => c.id === selection?.clipId);
  const mediaClip =
    clip && clip.type !== "caption" ? (clip as MediaClip) : null;
  const locked = track?.locked ?? true;

  if (!mediaClip || !track || track.kind !== "video") {
    return (
      <div className="panel-body">
        <h3>Adjust</h3>
        <p className="empty-hint">Select a video clip to adjust exposure, contrast, and saturation.</p>
      </div>
    );
  }

  const effects = mediaClip.effects ?? [];
  const hasAdjust = effects.some((e) => e.effect_id === COLOR_ADJUST_ID);

  async function commit(next: EffectInstance[]) {
    await dispatch(setClipEffects(track!.id, mediaClip!.id, next));
  }

  function ensureAdjust() {
    if (hasAdjust) return;
    void commit([
      ...effects,
      {
        id: newId(),
        effect_id: COLOR_ADJUST_ID,
        enabled: true,
        params: { ...DEFAULTS },
      },
    ]);
  }

  const adjustOnly = effects.filter((e) => e.effect_id === COLOR_ADJUST_ID);

  return (
    <div className="panel-body effects-panel">
      <h3>Adjust</h3>
      <p className="empty-hint">Exposure, contrast, and saturation (builtin:color_adjust).</p>
      {!hasAdjust ? (
        <button type="button" disabled={locked} onClick={ensureAdjust}>
          <SlidersHorizontal size={14} strokeWidth={1.75} />
          Add color adjust
        </button>
      ) : (
        <EffectList
          track={track}
          clip={mediaClip}
          effects={adjustOnly}
          locked={locked}
          onCommit={async (nextAdjust) => {
            const others = effects.filter((e) => e.effect_id !== COLOR_ADJUST_ID);
            await commit([...others, ...nextAdjust]);
          }}
        />
      )}
    </div>
  );
}
