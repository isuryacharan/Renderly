import { SlidersHorizontal } from "lucide-react";
import { useCatalogue } from "../../lib/useCatalogue";
import { useSelectedClip } from "../../lib/useSelectedClip";
import { EffectStack } from "../inspector/EffectStack";
import { EffectGallery } from "./EffectGallery";

/// Adjust tab: colour adjustment (the clip's adjust slot) with generic parameter rows.
export function AdjustPanel() {
  const catalogue = useCatalogue();
  const selected = useSelectedClip();

  if (!selected || selected.track.kind !== "video") {
    return (
      <div className="panel-body">
        <h3>Adjust</h3>
        <p className="empty-hint">Select a video clip to adjust its colour.</p>
      </div>
    );
  }
  const { track, clip } = selected;
  const items = catalogue
    .filter((e) => e.slot === "adjust" && e.state === "ok")
    .map((e) => ({ ref: e.ref, label: e.label, entry: e }));

  return (
    <div className="panel-body effects-panel">
      <h3>Adjust</h3>
      <p className="empty-hint">Colour adjustment (one per clip).</p>
      {!clip.adjust && (
        <EffectGallery
          items={items}
          track={track}
          clip={clip}
          icon={<SlidersHorizontal size={14} strokeWidth={1.75} />}
        />
      )}
      <EffectStack track={track} clip={clip} slots={["adjust"]} emptyHint="" />
    </div>
  );
}
