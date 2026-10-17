import { useEffect, useState } from "react";
import { Aperture } from "lucide-react";
import { setClipEffects } from "../../lib/commands";
import * as ipc from "../../lib/ipc";
import type { ExtensionCatalog } from "../../lib/ipc";
import type { EffectInstance, MediaClip } from "../../lib/types";
import { activeTracks } from "../../lib/container";
import { useEditorStore, type DispatchOptions } from "../../store/editorStore";
import { EffectList } from "./EffectsPanel";

const BUILTIN_LUTS: { id: string; label: string }[] = [
  { id: "builtin:lut_contrast", label: "Contrast" },
  { id: "builtin:lut_warm", label: "Warm" },
];

function newId(): string {
  return crypto.randomUUID();
}

export function FiltersPanel() {
  const project = useEditorStore((s) => s.project);
  const selection = useEditorStore((s) => s.selection);
  const dispatch = useEditorStore((s) => s.dispatch);
  const [catalog, setCatalog] = useState<ExtensionCatalog | null>(null);

  useEffect(() => {
    if (!project) {
      setCatalog(null);
      return;
    }
    void ipc.listExtensions().then(setCatalog).catch(() => setCatalog(null));
  }, [project, project?.asset_pack_paths?.join("|")]);

  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const track = activeTracks(project, activeSequenceId).find((t) => t.id === selection?.trackId);
  const clip = track?.clips.find((c) => c.id === selection?.clipId);
  const mediaClip =
    clip && clip.type !== "caption" ? (clip as MediaClip) : null;
  const locked = track?.locked ?? true;

  const packLuts =
    catalog?.packs.flatMap((p) =>
      p.luts.map((l) => ({
        id: `pack:${p.id}:lut:${l.id}`,
        label: `${p.name}: ${l.label}`,
      })),
    ) ?? [];

  if (!mediaClip || !track || track.kind === "caption") {
    return (
      <div className="panel-body">
        <h3>Filters</h3>
        <p className="empty-hint">Select a video clip to apply color filters.</p>
      </div>
    );
  }

  if (track.kind === "audio") {
    return (
      <div className="panel-body">
        <h3>Filters</h3>
        <p className="empty-hint">Filters apply to video clips. Use Effects for audio plugins.</p>
      </div>
    );
  }

  const effects = mediaClip.effects ?? [];

  async function commit(next: EffectInstance[], opts?: DispatchOptions) {
    await dispatch(setClipEffects(track!.id, mediaClip!.id, next), false, opts);
  }

  function addFilter(effectId: string) {
    void commit([
      ...effects,
      {
        id: newId(),
        effect_id: effectId,
        enabled: true,
        params: { intensity: 1 },
      },
    ]);
  }

  return (
    <div className="panel-body effects-panel">
      <h3>Filters</h3>
      <p className="empty-hint">Builtin and pack LUTs via SetClipEffects.</p>
      <div className="effects-catalog">
        {BUILTIN_LUTS.map((e) => (
          <button key={e.id} type="button" disabled={locked} onClick={() => addFilter(e.id)}>
            <Aperture size={14} strokeWidth={1.75} />
            {e.label}
          </button>
        ))}
        {packLuts.map((e) => (
          <button key={e.id} type="button" disabled={locked} onClick={() => addFilter(e.id)}>
            <Aperture size={14} strokeWidth={1.75} />
            {e.label}
          </button>
        ))}
      </div>
      <EffectList
        track={track}
        clip={mediaClip}
        effects={effects}
        locked={locked}
        onCommit={commit}
      />
    </div>
  );
}
