import { Sparkles } from "lucide-react";
import { useCatalogue, usePlugins } from "../../lib/useCatalogue";
import { useSelectedClip } from "../../lib/useSelectedClip";
import { EffectStack } from "../inspector/EffectStack";
import { EffectGallery, type GalleryItem } from "./EffectGallery";

/// Effects tab: the catalogue's effects (builtin and pack) plus WASM plugins, applied to the
/// selected clip. Filters and the colour adjustment have their own tabs; keying effects such as
/// chroma key are offered here and land in the clip's key slot.
export function EffectsPanel() {
  const catalogue = useCatalogue();
  const plugins = usePlugins();
  const selected = useSelectedClip();

  if (!selected) {
    return (
      <div className="panel-body">
        <h3>Effects</h3>
        <p className="empty-hint">Select a video or audio clip to add effects.</p>
      </div>
    );
  }
  const { track, clip } = selected;
  const isAudio = track.kind === "audio";

  const wasm: GalleryItem[] = plugins
    .filter((p) => (isAudio ? p.has_audio : p.has_frame))
    // WASM frame plugins run on CPU pixels of a decoded source frame; an adjustment layer's
    // input exists only on the GPU, so the engine rejects them there - don't offer them.
    .filter(() => clip.type !== "adjustment" && clip.type !== "text")
    .map((p) => ({ ref: `wasm:${p.id}`, label: p.name }));
  const builtin: GalleryItem[] = isAudio
    ? []
    : catalogue
        .filter((e) => e.kind === "effect" && e.state === "ok" && e.slot !== "adjust")
        .map((e) => ({ ref: e.ref, label: e.label, entry: e }));

  return (
    <div className="panel-body effects-panel">
      <h3>Effects</h3>
      <p className="empty-hint">
        {isAudio
          ? "Audio WASM plugins applied on mix/export."
          : "GPU, pack and WASM frame effects. Hover to preview."}
      </p>
      <EffectGallery
        items={[...builtin, ...wasm]}
        track={track}
        clip={clip}
        icon={<Sparkles size={14} strokeWidth={1.75} />}
      />
      <EffectStack track={track} clip={clip} slots={["key", "effects"]} />
    </div>
  );
}
