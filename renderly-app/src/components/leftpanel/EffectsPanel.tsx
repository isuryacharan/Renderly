import { useEffect, useState } from "react";
import { Sparkles, Trash2 } from "lucide-react";
import { setClipEffects } from "../../lib/commands";
import * as ipc from "../../lib/ipc";
import type { ExtensionCatalog } from "../../lib/ipc";
import type { AdjustmentClip, EffectInstance, MediaClip, Track } from "../../lib/types";
import { activeTracks } from "../../lib/container";
import { useEditorStore, type DispatchOptions } from "../../store/editorStore";

export const BUILTIN_VIDEO_EFFECTS: {
  id: string;
  label: string;
  defaults: Record<string, number>;
}[] = [
  {
    id: "builtin:color_adjust",
    label: "Color adjust",
    defaults: { exposure: 0, contrast: 1, saturation: 1 },
  },
  { id: "builtin:blur", label: "Blur", defaults: { radius: 4 } },
  { id: "builtin:lut_contrast", label: "LUT Contrast", defaults: { intensity: 1 } },
  { id: "builtin:lut_warm", label: "LUT Warm", defaults: { intensity: 1 } },
  {
    id: "builtin:glitch",
    label: "Glitch",
    defaults: { intensity: 0.5, slice: 0.5 },
  },
  {
    id: "builtin:chroma_key",
    label: "Chroma key",
    defaults: { key_r: 0, key_g: 1, key_b: 0, tolerance: 0.3, softness: 0.1, spill: 0 },
  },
];

/** @deprecated use BUILTIN_VIDEO_EFFECTS */
export const BUILTIN_EFFECTS = BUILTIN_VIDEO_EFFECTS;

function newId(): string {
  return crypto.randomUUID();
}

export function EffectsPanel() {
  const project = useEditorStore((s) => s.project);
  const selection = useEditorStore((s) => s.selection);
  const dispatch = useEditorStore((s) => s.dispatch);
  const previewEffectOnSelection = useEditorStore((s) => s.previewEffectOnSelection);
  const clearEffectPreview = useEditorStore((s) => s.clearEffectPreview);
  const [catalog, setCatalog] = useState<ExtensionCatalog | null>(null);

  useEffect(() => {
    if (!project) {
      setCatalog(null);
      return;
    }
    void ipc.listExtensions().then(setCatalog).catch(() => setCatalog(null));
  }, [project, project?.wasm_plugin_paths?.join("|"), project?.asset_pack_paths?.join("|")]);

  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const track = activeTracks(project, activeSequenceId).find((t) => t.id === selection?.trackId);
  const clip = track?.clips.find((c) => c.id === selection?.clipId);
  const mediaClip =
    clip && clip.type !== "caption" ? (clip as MediaClip) : null;
  const locked = track?.locked ?? true;
  const isAudio = track?.kind === "audio";

  if (!mediaClip || !track || track.kind === "caption") {
    return (
      <div className="panel-body">
        <h3>Effects</h3>
        <p className="empty-hint">Select a video or audio clip to add effects.</p>
      </div>
    );
  }

  const effects = mediaClip.effects ?? [];
  const packLuts =
    !isAudio
      ? (catalog?.packs.flatMap((p) =>
          p.luts.map((l) => ({
            id: `pack:${p.id}:lut:${l.id}`,
            label: `${p.name}: ${l.label}`,
            defaults: { intensity: 1 },
          })),
        ) ?? [])
      : [];
  const wasmEffects =
    catalog?.plugins
      .filter((p) => (isAudio ? p.has_audio : p.has_frame))
      .map((p) => ({
        id: `wasm:${p.id}`,
        label: p.name,
        defaults: { intensity: 1 },
      })) ?? [];

  // WASM frame plugins run on CPU pixels of a decoded source frame; an adjustment layer's
  // input exists only on the GPU, so the engine rejects them there (schema v7) - don't offer them.
  const isAdjustment = clip?.type === "adjustment";
  const catalogItems = isAudio
    ? wasmEffects
    : [
        ...BUILTIN_VIDEO_EFFECTS,
        ...packLuts,
        ...(isAdjustment ? [] : wasmEffects.filter((p) => p.id.startsWith("wasm:"))),
      ];

  async function commit(next: EffectInstance[], opts?: DispatchOptions) {
    // A click while hovering must not leave the hover instance stacked on the committed one.
    clearEffectPreview();
    await dispatch(setClipEffects(track!.id, mediaClip!.id, next), false, opts);
  }

  return (
    <div className="panel-body effects-panel">
      <h3>Effects</h3>
      <p className="empty-hint">
        {isAudio
          ? "Audio WASM plugins applied on mix/export."
          : "Builtin GPU / pack LUT / WASM frame effects."}
      </p>
      <div className="effects-catalog">
        {catalogItems.map((e) => (
          <button
            key={e.id}
            type="button"
            disabled={locked}
            // Hover preview (docs/architecture.md "Preview overrides"): the selected clip renders
            // the effect while the item is hovered, with no command and no undo record. WASM
            // plugins run only on export, so they are not previewed.
            onMouseEnter={
              e.id.startsWith("wasm:") ? undefined : () => previewEffectOnSelection(e.id, e.defaults)
            }
            onMouseLeave={clearEffectPreview}
            onFocus={
              e.id.startsWith("wasm:") ? undefined : () => previewEffectOnSelection(e.id, e.defaults)
            }
            onBlur={clearEffectPreview}
            onClick={() =>
              void commit([
                ...effects,
                {
                  id: newId(),
                  effect_id: e.id,
                  enabled: true,
                  params: { ...e.defaults },
                },
              ])
            }
          >
            <Sparkles size={14} strokeWidth={1.75} />
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
        labelLookup={[...BUILTIN_VIDEO_EFFECTS, ...packLuts, ...wasmEffects]}
      />
    </div>
  );
}

export function EffectList({
  track,
  clip,
  effects,
  locked,
  onCommit,
  labelLookup = BUILTIN_VIDEO_EFFECTS,
}: {
  track: Track;
  clip: MediaClip | AdjustmentClip;
  effects: EffectInstance[];
  locked: boolean;
  onCommit: (next: EffectInstance[], opts?: DispatchOptions) => Promise<void>;
  labelLookup?: { id: string; label: string }[];
}) {
  void track;
  const liveEffects = useEditorStore((s) => s.previewOverrides[clip.id]?.effects);
  const setPreviewOverride = useEditorStore((s) => s.setPreviewOverride);
  const clearPreviewOverride = useEditorStore((s) => s.clearPreviewOverride);
  const endGesture = useEditorStore((s) => s.endGesture);

  if (effects.length === 0) {
    return <p className="empty-hint">No effects on this clip.</p>;
  }

  // A slider drag shows a render-only override on every tick and commits one command on
  // release, so the drag is one undo step and the history does not flood (see
  // docs/architecture.md "Preview overrides").
  const shownValue = (idx: number, key: string): number =>
    liveEffects?.[idx]?.params[key] ?? effects[idx]?.params[key] ?? 0;
  const previewParam = (idx: number, key: string, v: number) =>
    setPreviewOverride(clip.id, {
      effects: effects.map((x, i) => (i === idx ? { ...x, params: { ...x.params, [key]: v } } : x)),
    });
  const commitParam = async (idx: number, key: string, v: number) => {
    if (effects[idx]?.params[key] === v) {
      // A click without movement: nothing to commit, so no undo step.
      clearPreviewOverride(clip.id);
      endGesture();
      return;
    }
    const next = effects.map((x, i) => (i === idx ? { ...x, params: { ...x.params, [key]: v } } : x));
    // One merge key per effect instance: back-to-back releases on the same slider coalesce.
    await onCommit(next, { mergeKey: `effects:${clip.id}:${effects[idx]?.id ?? idx}` });
    clearPreviewOverride(clip.id);
    endGesture();
  };

  return (
    <ul className="effect-list">
      {effects.map((fx, idx) => {
        const meta = labelLookup.find((b) => b.id === fx.effect_id);
        return (
          <li key={fx.id} className={!fx.enabled ? "disabled" : ""}>
            <div className="effect-list-header">
              <label className="toggle-row">
                <input
                  type="checkbox"
                  checked={fx.enabled}
                  disabled={locked}
                  onChange={(e) => {
                    const next = effects.map((x, i) =>
                      i === idx ? { ...x, enabled: e.target.checked } : x,
                    );
                    void onCommit(next);
                  }}
                />
                <span>{meta?.label ?? fx.effect_id}</span>
              </label>
              <button
                type="button"
                className="btn-ghost"
                disabled={locked}
                title="Remove"
                onClick={() => void onCommit(effects.filter((_, i) => i !== idx))}
              >
                <Trash2 size={14} strokeWidth={1.75} />
              </button>
            </div>
            {Object.keys(fx.params).map((key) => (
              <div className="field" key={key}>
                <label>{key}</label>
                <input
                  type="range"
                  min={key === "radius" ? 0 : key === "exposure" ? -2 : 0}
                  max={key === "radius" ? 32 : key === "exposure" ? 2 : 2}
                  step={0.05}
                  value={shownValue(idx, key)}
                  disabled={locked || !fx.enabled}
                  onChange={(e) => previewParam(idx, key, parseFloat(e.target.value))}
                  onMouseUp={(e) => void commitParam(idx, key, parseFloat(e.currentTarget.value))}
                  onTouchEnd={(e) => void commitParam(idx, key, parseFloat(e.currentTarget.value))}
                  onKeyUp={(e) => void commitParam(idx, key, parseFloat(e.currentTarget.value))}
                />
                <span>{shownValue(idx, key).toFixed(2)}</span>
              </div>
            ))}
          </li>
        );
      })}
    </ul>
  );
}
