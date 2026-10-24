import { useEffect, useMemo, useState } from "react";
import { setClipKeyframes } from "../../lib/commands";
import {
  EASING_MENU,
  easingMenuValue,
  easingPickCommand,
  effectLabel,
  isKeyframable,
  paramValue,
  stackItems,
  findEntry,
  type EasingMenuValue,
} from "../../lib/effects";
import type {
  AnimProperty,
  EffectClip,
  Keyframe,
  KeyframeProperty,
  KeyframeTrack,
  KeyValue,
  Track,
} from "../../lib/types";
import { clipDurationUs, effectTrackProperty, parseEffectProperty } from "../../lib/types";
import { prettyName } from "../../lib/paramMath";
import { secsToUs, usToSecs } from "../../lib/time";
import { useCatalogue } from "../../lib/useCatalogue";
import { useEditorStore } from "../../store/editorStore";
import { MenuSelect } from "../ui/MenuSelect";

import { KeyframeGraph } from "./KeyframeGraph";

const PROP_OPTIONS: { value: AnimProperty; label: string; video?: boolean; audio?: boolean }[] = [
  { value: "pos_x", label: "Position X", video: true },
  { value: "pos_y", label: "Position Y", video: true },
  { value: "scale_x", label: "Scale X", video: true },
  { value: "scale_y", label: "Scale Y", video: true },
  { value: "rotation", label: "Rotation", video: true },
  { value: "opacity", label: "Opacity", video: true },
  { value: "fit_x", label: "Pan within frame X", video: true },
  { value: "fit_y", label: "Pan within frame Y", video: true },
  { value: "speed", label: "Speed", video: true, audio: true },
  { value: "volume", label: "Volume (dB)", audio: true, video: true },
];


function animDefault(prop: AnimProperty, clip: EffectClip): number {
  // Adjustment layers animate only `opacity` (plus effect params).
  if (clip.type === "adjustment") return clip.opacity ?? 1;
  const t = clip.transform;
  switch (prop) {
    case "pos_x":
      return t?.x ?? 0;
    case "pos_y":
      return t?.y ?? 0;
    case "scale_x":
      return t?.scale_x ?? 1;
    case "scale_y":
      return t?.scale_y ?? 1;
    case "rotation":
      return t?.rotation_deg ?? 0;
    case "opacity":
      return t?.opacity ?? 1;
    case "fit_x":
      return t?.fit_x ?? 0;
    case "fit_y":
      return t?.fit_y ?? 0;
    case "speed":
      return clip.type === "video" || clip.type === "audio" ? (clip.speed ?? 1) : 1;
    case "volume":
      return clip.type === "video" || clip.type === "audio" ? clip.gain_db : 0;
  }
}

function upsertTrack(
  tracks: KeyframeTrack[],
  property: KeyframeProperty,
  keys: Keyframe[],
): KeyframeTrack[] {
  const next = tracks.filter((tr) => tr.property !== property);
  if (keys.length > 0) {
    next.push({
      property,
      keys: [...keys].sort((a, b) => a.time_us - b.time_us),
    });
  }
  return next;
}

const isVector = (v: KeyValue): v is number[] => Array.isArray(v);
const firstComponent = (v: KeyValue): number => (isVector(v) ? (v[0] ?? 0) : v);

export function KeyframeEditor({
  track,
  clip,
  nested = false,
}: {
  track: Track;
  clip: EffectClip;
  nested?: boolean;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const playhead = useEditorStore((s) => s.playhead);
  const catalogue = useCatalogue();
  // Keyframe times and the clip duration are integer µs, relative to the clip start.
  const duration = clipDurationUs(clip);

  // Transform/audio properties, then one entry per keyframable parameter of the clip's effects
  // (`effect:<instance-id>/<param>`), then any other track the clip already has.
  const props = useMemo(() => {
    const out: { value: KeyframeProperty; label: string }[] = PROP_OPTIONS.filter((p) => {
      if (clip.type === "adjustment") return p.value === "opacity";
      if (clip.type === "audio") return p.audio || p.value === "volume";
      // A title has no source speed, audio or cover-fit window.
      if (clip.type === "text") return p.video && !["speed", "volume", "fit_x", "fit_y"].includes(p.value);
      return p.video;
    }).map((p) => ({ value: p.value, label: p.label }));
    for (const item of stackItems(clip)) {
      const entry = findEntry(catalogue, item.instance.ref);
      const name = effectLabel(catalogue, item.instance.ref);
      for (const spec of entry?.params ?? []) {
        if (!isKeyframable(spec)) continue;
        out.push({
          value: effectTrackProperty(item.instance.id, spec.key),
          label: `${name} · ${prettyName(spec.key)}`,
        });
      }
    }
    for (const tr of clip.keyframes ?? []) {
      if (!out.some((o) => o.value === tr.property)) {
        out.push({ value: tr.property, label: String(tr.property) });
      }
    }
    return out;
  }, [clip, catalogue]);

  const [property, setProperty] = useState<KeyframeProperty>(props[0]?.value ?? "opacity");
  const [selectedIdx, setSelectedIdx] = useState<number | null>(null);

  useEffect(() => {
    if (!props.some((p) => p.value === property)) {
      setProperty(props[0]?.value ?? "opacity");
    }
  }, [props, property]);

  useEffect(() => {
    setSelectedIdx(null);
  }, [clip.id, property]);

  const trackKeys = useMemo(() => {
    const tr = (clip.keyframes ?? []).find((k) => k.property === property);
    return tr?.keys ?? [];
  }, [clip.keyframes, property]);

  /** The value a new key starts with: the property's current static value. */
  function defaultValue(): KeyValue {
    const ref = parseEffectProperty(property);
    if (!ref) return animDefault(property as AnimProperty, clip);
    const item = stackItems(clip).find((i) => i.instance.id === ref[0]);
    const spec = item
      ? findEntry(catalogue, item.instance.ref)?.params.find((p) => p.key === ref[1])
      : undefined;
    const v = item && spec ? paramValue(item.instance, spec) : undefined;
    if (typeof v === "number" || Array.isArray(v)) return v;
    return 0;
  }

  async function commit(keys: Keyframe[]) {
    const keyframes = upsertTrack(clip.keyframes ?? [], property, keys);
    await dispatch(setClipKeyframes(track.id, clip.id, keyframes));
  }

  async function addAtPlayhead() {
    const local = Math.max(0, Math.min(duration, playhead - clip.start_us));
    const keys = [...trackKeys];
    const existing = keys.findIndex((k) => k.time_us === local);
    const value = defaultValue();
    if (existing >= 0) {
      keys[existing] = { ...keys[existing], value };
      setSelectedIdx(existing);
    } else {
      keys.push({ time_us: local, value, easing: "linear" });
      keys.sort((a, b) => a.time_us - b.time_us);
      setSelectedIdx(keys.findIndex((k) => k.time_us === local));
    }
    await commit(keys);
  }

  async function removeSelected() {
    if (selectedIdx == null) return;
    const keys = trackKeys.filter((_, i) => i !== selectedIdx);
    setSelectedIdx(null);
    await commit(keys);
  }

  async function updateKey(idx: number, patch: Partial<Keyframe>) {
    const keys = trackKeys.map((k, i) => (i === idx ? { ...k, ...patch } : k));
    await commit(keys);
  }

  /** Graph drags move a key in time and (for number keys) value; array-valued keys have no
   *  single value axis, so only their time moves there. */
  function updateFromGraph(idx: number, patch: Partial<Keyframe>) {
    const key = trackKeys[idx];
    if (key && isVector(key.value)) {
      const { value: _v, handle_left: _l, handle_right: _r, ...rest } = patch;
      void _v;
      void _l;
      void _r;
      if (Object.keys(rest).length === 0) return;
      void updateKey(idx, rest);
      return;
    }
    void updateKey(idx, patch);
  }

  /** The easing menu: `SetKeyframeEasing` for the span after the key (core writes presets as
   *  Bezier handles). */
  async function setEasing(idx: number, choice: Exclude<EasingMenuValue, "custom">) {
    const cmd = easingPickCommand(track.id, clip.id, property, trackKeys, idx, choice);
    if (cmd) await dispatch(cmd);
  }

  const graphTrack = {
    keys: trackKeys.map((k) => ({ ...k, value: firstComponent(k.value) })),
  };

  return (
    <div className={nested ? "keyframe-editor nested" : "inspector-section keyframe-editor"}>
      {!nested && <h3>Keyframes</h3>}
      <div className="field">
        <label>Property</label>
        <MenuSelect
          value={property}
          disabled={track.locked}
          options={props.map((p) => ({ value: p.value, label: p.label }))}
          onChange={(v) => setProperty(v as KeyframeProperty)}
        />
      </div>

      <div style={{ marginBottom: "1rem" }}>
        <KeyframeGraph
          track={graphTrack}
          duration={duration}
          width={280}
          height={160}
          onUpdateKey={updateFromGraph}
        />
      </div>

      <div className="inspector-actions" style={{ marginBottom: "0.5rem" }}>
        <button type="button" disabled={track.locked} onClick={() => void addAtPlayhead()}>
          Add at playhead
        </button>
        <button
          type="button"
          className="btn-ghost"
          disabled={track.locked || selectedIdx == null}
          onClick={() => void removeSelected()}
        >
          Delete key
        </button>
      </div>

      {trackKeys.length === 0 ? (
        <p className="empty-hint">No keys for this property. Add one at the playhead.</p>
      ) : (
        <ul className="keyframe-list">
          {trackKeys.map((k, i) => {
            const isLast = i === trackKeys.length - 1;
            const menu = easingMenuValue(k, trackKeys[i + 1]);
            return (
              <li key={`${k.time_us}-${i}`} className={selectedIdx === i ? "selected" : ""}>
                <button
                  type="button"
                  className="keyframe-row-select"
                  disabled={track.locked}
                  onClick={() => setSelectedIdx(i)}
                >
                  #{i + 1}
                </button>
                <input
                  type="number"
                  step="0.05"
                  min={0}
                  max={usToSecs(duration)}
                  value={usToSecs(k.time_us)}
                  disabled={track.locked}
                  onChange={(e) => {
                    const timeUs = secsToUs(parseFloat(e.target.value) || 0);
                    void updateKey(i, { time_us: Math.max(0, Math.min(duration, timeUs)) });
                  }}
                  title="Time (s from clip start)"
                />
                {(isVector(k.value) ? k.value : [k.value]).map((component, ci) => (
                  <input
                    key={ci}
                    type="number"
                    step="0.01"
                    value={component}
                    disabled={track.locked}
                    aria-label={isVector(k.value) ? `Value ${ci + 1}` : "Value"}
                    onChange={(e) => {
                      let n = parseFloat(e.target.value);
                      if (Number.isNaN(n)) n = 0;
                      if (property === "opacity") n = Math.max(0, Math.min(1, n));
                      if (property === "speed") n = Math.max(0.25, Math.min(4, n));
                      const value: KeyValue = isVector(k.value)
                        ? k.value.map((c, j) => (j === ci ? n : c))
                        : n;
                      void updateKey(i, { value });
                    }}
                    title="Value"
                  />
                ))}
                <MenuSelect
                  value={menu}
                  disabled={track.locked || isLast}
                  tooltip={isLast ? "Easing applies to the span after a key; this is the last key" : undefined}
                  options={[
                    ...EASING_MENU,
                    ...(menu === "custom" ? [{ value: "custom" as const, label: "Custom" }] : []),
                  ]}
                  onChange={(v) => {
                    if (v !== "custom") void setEasing(i, v as Exclude<EasingMenuValue, "custom">);
                  }}
                />
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
