import { Aperture } from "lucide-react";
import { useCatalogue } from "../../lib/useCatalogue";
import { useSelectedClip } from "../../lib/useSelectedClip";
import { EffectStack } from "../inspector/EffectStack";
import { EffectGallery } from "./EffectGallery";

/// Filters tab: looks and LUTs (builtin and pack). A filter goes in the clip's single filter
/// slot, so applying another replaces it.
export function FiltersPanel() {
  const catalogue = useCatalogue();
  const selected = useSelectedClip();

  if (!selected) {
    return (
      <div className="panel-body">
        <h3>Filters</h3>
        <p className="empty-hint">Select a video clip to apply color filters.</p>
      </div>
    );
  }
  const { track, clip } = selected;
  if (track.kind === "audio") {
    return (
      <div className="panel-body">
        <h3>Filters</h3>
        <p className="empty-hint">Filters apply to video clips. Use Effects for audio plugins.</p>
      </div>
    );
  }
  const items = catalogue
    .filter((e) => (e.kind === "filter" || e.kind === "lut") && e.state === "ok")
    .map((e) => ({ ref: e.ref, label: e.label, entry: e }));

  return (
    <div className="panel-body effects-panel">
      <h3>Filters</h3>
      <p className="empty-hint">One filter per clip; hover to preview.</p>
      <EffectGallery
        items={items}
        track={track}
        clip={clip}
        icon={<Aperture size={14} strokeWidth={1.75} />}
      />
      <EffectStack track={track} clip={clip} slots={["filter"]} emptyHint="No filter on this clip." />
    </div>
  );
}
