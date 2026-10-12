import { useEffect, useMemo, useState } from "react";
import { setClipKeyframes } from "../../lib/commands";
import type {
  AdjustmentClip,
  AnimProperty,
  Easing,
  Keyframe,
  KeyframeTrack,
  MediaClip,
  Track,
} from "../../lib/types";
import { clipDurationSecs } from "../../lib/types";
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
  { value: "speed", label: "Speed", video: true, audio: true },
  { value: "volume", label: "Volume (dB)", audio: true, video: true },
];

const EASING_OPTIONS: { value: Easing; label: string }[] = [
  { value: "linear", label: "Linear" },
  { value: "ease_in", label: "Ease in" },
  { value: "ease_out", label: "Ease out" },
  { value: "ease_in_out", label: "Ease in-out" },
  { value: "bezier", label: "Bezier" },
];

function defaultValue(prop: AnimProperty, clip: MediaClip | AdjustmentClip): number {
  // Adjustment layers animate only `opacity` (the engine rejects other properties on them).
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
    case "speed":
      return clip.speed ?? 1;
    case "volume":
      return clip.gain_db;
  }
}

function upsertTrack(
  tracks: KeyframeTrack[],
  property: AnimProperty,
  keys: Keyframe[],
): KeyframeTrack[] {
  const next = tracks.filter((tr) => tr.property !== property);
  if (keys.length > 0) {
    next.push({
      property,
      keys: [...keys].sort((a, b) => a.time_secs - b.time_secs),
    });
  }
  return next;
}

export function KeyframeEditor({
  track,
  clip,
  nested = false,
}: {
  track: Track;
  clip: MediaClip | AdjustmentClip;
  nested?: boolean;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const playhead = useEditorStore((s) => s.playhead);
  const duration = clipDurationSecs(clip);

  const props = PROP_OPTIONS.filter((p) => {
    if (clip.type === "adjustment") return p.value === "opacity";
    if (clip.type === "audio") return p.audio || p.value === "volume";
    return p.video;
  });

  const [property, setProperty] = useState<AnimProperty>(props[0]?.value ?? "opacity");
  const [selectedIdx, setSelectedIdx] = useState<number | null>(null);

  useEffect(() => {
    if (!props.some((p) => p.value === property)) {
      setProperty(props[0]?.value ?? "opacity");
    }
    setSelectedIdx(null);
  }, [clip.id, property, props]);

  const trackKeys = useMemo(() => {
    const tr = (clip.keyframes ?? []).find((k) => k.property === property);
    return tr?.keys ?? [];
  }, [clip.keyframes, property]);

  async function commit(keys: Keyframe[]) {
    const keyframes = upsertTrack(clip.keyframes ?? [], property, keys);
    await dispatch(setClipKeyframes(track.id, clip.id, keyframes));
  }

  async function addAtPlayhead() {
    const local = Math.max(0, Math.min(duration, playhead - clip.position_secs));
    const keys = [...trackKeys];
    const existing = keys.findIndex((k) => Math.abs(k.time_secs - local) < 1e-3);
    const value = defaultValue(property, clip);
    if (existing >= 0) {
      keys[existing] = { ...keys[existing], value };
      setSelectedIdx(existing);
    } else {
      keys.push({ time_secs: local, value, easing: "linear" });
      keys.sort((a, b) => a.time_secs - b.time_secs);
      setSelectedIdx(keys.findIndex((k) => Math.abs(k.time_secs - local) < 1e-3));
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
    const keys = trackKeys.map((k, i) => {
      if (i === idx) {
        const next = { ...k, ...patch };
        if (next.easing === "bezier" && !next.handle_right && !next.handle_left) {
          // Default handles
          next.handle_right = [0.5, 0];
          next.handle_left = [-0.5, 0];
        }
        return next;
      }
      return k;
    });
    await commit(keys);
  }

  return (
    <div className={nested ? "keyframe-editor nested" : "inspector-section keyframe-editor"}>
      {!nested && <h3>Keyframes</h3>}
      <div className="field">
        <label>Property</label>
        <MenuSelect
          value={property}
          disabled={track.locked}
          options={props.map((p) => ({ value: p.value, label: p.label }))}
          onChange={(v) => setProperty(v as AnimProperty)}
        />
      </div>

      <div style={{ marginBottom: "1rem" }}>
        <KeyframeGraph
          track={{ property, keys: trackKeys }}
          duration={duration}
          width={280}
          height={160}
          onUpdateKey={updateKey}
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
          {trackKeys.map((k, i) => (
            <li key={`${k.time_secs}-${i}`} className={selectedIdx === i ? "selected" : ""}>
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
                max={duration}
                value={k.time_secs}
                disabled={track.locked}
                onChange={(e) => {
                  const time_secs = parseFloat(e.target.value) || 0;
                  void updateKey(i, { time_secs: Math.max(0, Math.min(duration, time_secs)) });
                }}
                title="Time (s from clip start)"
              />
              <input
                type="number"
                step="0.01"
                value={k.value}
                disabled={track.locked}
                onChange={(e) => {
                  let value = parseFloat(e.target.value);
                  if (Number.isNaN(value)) value = 0;
                  if (property === "opacity") value = Math.max(0, Math.min(1, value));
                  if (property === "speed") value = Math.max(0.25, Math.min(4, value));
                  void updateKey(i, { value });
                }}
                title="Value"
              />
              <MenuSelect
                value={k.easing ?? "linear"}
                disabled={track.locked}
                options={EASING_OPTIONS.map((e) => ({ value: e.value, label: e.label }))}
                onChange={(v) => void updateKey(i, { easing: v as Easing })}
              />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
