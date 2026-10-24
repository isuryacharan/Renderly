import { stackItems } from "../../lib/effects";
import type { EffectClip, Track } from "../../lib/types";
import { EffectStack } from "./EffectStack";

/// Shared "Effects · N" disclosure with the editable effect stack (filter / key / adjust slots,
/// then the effects list): used by the media-clip inspector and the adjustment-layer inspector.
/// Effects are added from the left-panel Effects / Filters / Adjust tabs, which offer the right
/// catalogue items for the selected clip kind.
export function EffectStackSection({
  track,
  clip,
}: {
  track: Track;
  clip: EffectClip;
}) {
  const count = stackItems(clip).length;
  if (count === 0) return null;
  return (
    <details className="insp-disclosure" open>
      <summary>Effects · {count}</summary>
      <div className="inspector-section insp-disclosure-body">
        <EffectStack track={track} clip={clip} />
      </div>
    </details>
  );
}
