import type { ReactNode } from "react";
import { setClipEffects } from "../../lib/commands";
import { addEffectChange, targetSlot } from "../../lib/effects";
import type { CatalogueEntry } from "../../lib/ipc";
import type { EffectClip, Track } from "../../lib/types";
import { useEditorStore } from "../../store/editorStore";

/** What the gallery can offer: a catalogue entry, or a `wasm:` plugin (no params, no hover). */
export interface GalleryItem {
  ref: string;
  label: string;
  entry?: CatalogueEntry;
}

/// A grid of apply-by-click buttons for catalogue items. Hover (or keyboard focus) previews the
/// item on the selected clips through a preview override - no command, no undo record - using
/// the item's `initial` params. A click is one `SetClipEffects` that puts it in its default slot
/// (a filter / key / adjust slot is replaced; `effects[]` gets it appended).
export function EffectGallery({
  items,
  track,
  clip,
  icon,
}: {
  items: GalleryItem[];
  track: Track;
  clip: EffectClip;
  icon: ReactNode;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const previewEffectOnSelection = useEditorStore((s) => s.previewEffectOnSelection);
  const clearEffectPreview = useEditorStore((s) => s.clearEffectPreview);
  const locked = track.locked;

  const apply = async (item: GalleryItem) => {
    // A click while hovering must not leave the hover instance stacked on the committed one.
    clearEffectPreview();
    const slot = item.entry ? targetSlot(item.entry, clip) : "effects";
    await dispatch(setClipEffects(track.id, clip.id, addEffectChange(clip, slot, { ref: item.ref })));
  };

  return (
    <div className="effects-catalog">
      {items.map((item) => {
        const entry = item.entry;
        const show = entry ? () => previewEffectOnSelection(entry) : undefined;
        return (
          <button
            key={item.ref}
            type="button"
            disabled={locked}
            onMouseEnter={show}
            onMouseLeave={clearEffectPreview}
            onFocus={show}
            onBlur={clearEffectPreview}
            onClick={() => void apply(item)}
          >
            {icon}
            {item.label}
          </button>
        );
      })}
    </div>
  );
}
