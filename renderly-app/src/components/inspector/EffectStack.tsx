import { ArrowDown, ArrowUp, Trash2 } from "lucide-react";
import { setClipEffects, setClipKeyframes } from "../../lib/commands";
import {
  effectLabel,
  findEntry,
  hasParamTrack,
  isKeyframable,
  moveEffectChange,
  paramValue,
  replaceInstanceChange,
  stackItems,
  stackWithParam,
  withKeyAt,
  type StackItem,
  type StackSlot,
} from "../../lib/effects";
import type { CatalogueEntry } from "../../lib/ipc";
import type { EffectClip, EffectParamValue, Track } from "../../lib/types";
import { effectTrackProperty } from "../../lib/types";
import { useCatalogue, useEffectStatus, usePlugins } from "../../lib/useCatalogue";
import { useCatalogueStore } from "../../store/catalogueStore";
import { useEditorStore } from "../../store/editorStore";
import { ParamRow } from "./ParamRow";

const SLOT_LABEL: Record<StackSlot, string> = {
  filter: "Filter",
  key: "Key",
  adjust: "Adjust",
  effects: "Effect",
};

/** Text of the badge for a non-`ok` effect, or null. */
export function statusBadge(status: string | undefined, label: string): string | null {
  switch (status) {
    case "missing":
      return `Effect missing: ${label}`;
    case "changed":
      return "Effect changed since it was applied";
    case "disabled":
      return `Effect unavailable in this build: ${label}`;
    default:
      return null;
  }
}

/**
 * The clip's effect stack: filter / key / adjust slots, then the `effects[]` list, each with its
 * generic parameter rows (driven by the catalogue's param specs). Edits are `SetClipEffects`;
 * slider drags show a render-only preview override and commit one command on release.
 * `slots` limits which slots are listed (the left-panel tabs show only their own).
 */
export function EffectStack({
  track,
  clip,
  slots,
  emptyHint = "No effects on this clip.",
}: {
  track: Track;
  clip: EffectClip;
  slots?: StackSlot[];
  emptyHint?: string;
}) {
  const catalogue = useCatalogue();
  const plugins = usePlugins();
  const statuses = useEffectStatus(clip);
  const all = stackItems(clip);
  const items = slots ? all.filter((i) => slots.includes(i.slot)) : all;
  if (items.length === 0) return emptyHint ? <p className="empty-hint">{emptyHint}</p> : null;
  const effectsCount = clip.effects?.length ?? 0;
  return (
    <ul className="effect-list">
      {items.map((item) => (
        <EffectInstanceEditor
          key={item.instance.id}
          track={track}
          clip={clip}
          item={item}
          entry={findEntry(catalogue, item.instance.ref)}
          catalogue={catalogue}
          pluginName={
            item.instance.ref.startsWith("wasm:")
              ? plugins.find((p) => `wasm:${p.id}` === item.instance.ref)?.name
              : undefined
          }
          status={statuses.get(item.instance.id)}
          canMoveUp={item.slot === "effects" && (item.index ?? 0) > 0}
          canMoveDown={item.slot === "effects" && (item.index ?? 0) < effectsCount - 1}
        />
      ))}
    </ul>
  );
}

function EffectInstanceEditor({
  track,
  clip,
  item,
  entry,
  catalogue,
  pluginName,
  status,
  canMoveUp,
  canMoveDown,
}: {
  track: Track;
  clip: EffectClip;
  item: StackItem;
  entry: CatalogueEntry | undefined;
  catalogue: CatalogueEntry[];
  /** Name of the `wasm:` plugin the instance refers to (plugins are not in the catalogue). */
  pluginName: string | undefined;
  status: string | undefined;
  canMoveUp: boolean;
  canMoveDown: boolean;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const playhead = useEditorStore((s) => s.playhead);
  const setPreviewOverride = useEditorStore((s) => s.setPreviewOverride);
  const clearPreviewOverride = useEditorStore((s) => s.clearPreviewOverride);
  const endGesture = useEditorStore((s) => s.endGesture);
  // A drag in progress shows through the render-only override; the stored value is unchanged
  // until release, so the control must read the live one or it would snap back mid-drag.
  const live = useEditorStore((s) => s.previewOverrides[clip.id]?.effects);
  const fx = item.instance;
  const locked = track.locked;
  const label = pluginName ?? effectLabel(catalogue, fx.ref);
  const loaded = useCatalogueStore((s) => s.loaded);
  // The backend's status wins; before it answers, a ref the loaded catalogue does not know is missing.
  const implied = loaded && !entry && !fx.ref.startsWith("wasm:") ? "missing" : undefined;
  const badge = statusBadge(status ?? implied, label);
  const mergeKey = `effects:${clip.id}:${fx.id}`;

  const previewParam = (key: string, v: EffectParamValue) =>
    setPreviewOverride(clip.id, { effects: stackWithParam(clip, fx.id, key, v) });

  const commitParam = async (key: string, v: EffectParamValue) => {
    if (JSON.stringify(fx.params[key]) === JSON.stringify(v)) {
      // A click without movement: nothing to commit, so no undo step.
      clearPreviewOverride(clip.id);
      endGesture();
      return;
    }
    const next = { ...fx, params: { ...fx.params, [key]: v } };
    await dispatch(setClipEffects(track.id, clip.id, replaceInstanceChange(clip, item, next)), false, {
      mergeKey,
    });
    clearPreviewOverride(clip.id);
    endGesture();
  };

  const keyframeParam = async (key: string, v: EffectParamValue) => {
    if (typeof v === "boolean" || typeof v === "string") return;
    const local = Math.max(0, playhead - clip.start_us);
    await dispatch(
      setClipKeyframes(
        track.id,
        clip.id,
        withKeyAt(clip.keyframes, effectTrackProperty(fx.id, key), local, v),
      ),
    );
  };

  return (
    <li className={!fx.enabled ? "disabled" : ""} data-slot={item.slot} data-instance={fx.id}>
      <div className="effect-list-header">
        <label className="toggle-row">
          <input
            type="checkbox"
            checked={fx.enabled}
            disabled={locked}
            aria-label={`Enable ${label}`}
            onChange={(e) =>
              void dispatch(
                setClipEffects(
                  track.id,
                  clip.id,
                  replaceInstanceChange(clip, item, { ...fx, enabled: e.target.checked }),
                ),
              )
            }
          />
          <span>{label}</span>
        </label>
        <span className="effect-slot-tag">{SLOT_LABEL[item.slot]}</span>
        {item.slot === "effects" && (
          <>
            <button
              type="button"
              className="btn-ghost"
              disabled={locked || !canMoveUp}
              title="Move up"
              aria-label={`Move ${label} up`}
              onClick={() => {
                const change = moveEffectChange(clip, item.index ?? 0, -1);
                if (change) void dispatch(setClipEffects(track.id, clip.id, change));
              }}
            >
              <ArrowUp size={14} strokeWidth={1.75} />
            </button>
            <button
              type="button"
              className="btn-ghost"
              disabled={locked || !canMoveDown}
              title="Move down"
              aria-label={`Move ${label} down`}
              onClick={() => {
                const change = moveEffectChange(clip, item.index ?? 0, 1);
                if (change) void dispatch(setClipEffects(track.id, clip.id, change));
              }}
            >
              <ArrowDown size={14} strokeWidth={1.75} />
            </button>
          </>
        )}
        <button
          type="button"
          className="btn-ghost"
          disabled={locked}
          title="Remove"
          aria-label={`Remove ${label}`}
          onClick={() =>
            void dispatch(setClipEffects(track.id, clip.id, replaceInstanceChange(clip, item, null)))
          }
        >
          <Trash2 size={14} strokeWidth={1.75} />
        </button>
      </div>
      {badge && (
        <p className={`effect-badge ${status ?? implied ?? "missing"}`} role="status">
          {badge}
        </p>
      )}
      {entry?.params.map((spec) => (
        <ParamRow
          key={spec.key}
          spec={spec}
          value={live?.find((e) => e.id === fx.id)?.params[spec.key] ?? paramValue(fx, spec)}
          disabled={locked || !fx.enabled}
          animated={hasParamTrack(clip, fx.id, spec.key)}
          canKeyframe={isKeyframable(spec)}
          lutChoices={catalogue.filter((c) => c.kind === "lut")}
          onPreview={(v) => previewParam(spec.key, v)}
          onCommit={(v) => void commitParam(spec.key, v)}
          onKeyframe={() => void keyframeParam(spec.key, paramValue(fx, spec))}
        />
      ))}
      {!entry && Object.keys(fx.params).length > 0 && (
        <p className="empty-hint">Its settings are kept and apply again once the effect is installed.</p>
      )}
    </li>
  );
}
