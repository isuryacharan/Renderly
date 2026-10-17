import { useEditorStore } from "../../store/editorStore";
import { setClipEffects } from "../../lib/commands";
import type { AdjustmentClip, MediaClip, Track } from "../../lib/types";
import { EffectList } from "../leftpanel/EffectsPanel";

/// Shared "Effects · N" disclosure with the editable effect stack: used by the media-clip
/// inspector and the adjustment-layer inspector (both take `SetClipEffects`). Effects are added
/// from the left-panel Effects tab, which offers the right catalog for the selected clip kind.
export function EffectStackSection({
  track,
  clip,
}: {
  track: Track;
  clip: MediaClip | AdjustmentClip;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const effects = clip.effects ?? [];
  if (effects.length === 0) return null;
  return (
    <details className="insp-disclosure" open>
      <summary>Effects · {effects.length}</summary>
      <div className="inspector-section insp-disclosure-body">
        <EffectList
          track={track}
          clip={clip}
          effects={effects}
          locked={track.locked}
          onCommit={async (next, opts) => {
            await dispatch(setClipEffects(track.id, clip.id, next), false, opts);
          }}
        />
      </div>
    </details>
  );
}
