// The inspector: follows the timeline selection. Nothing selected shows the project's settings and
// the sequence's canvas; selected picture clips show their transform, blending and crop. With
// several clips selected, a value they share is shown, and one they do not is "mixed" (an en
// dash); an edit applies to all of them in one step. A drag (slider or label) is one store
// gesture: one undo step, Escape cancels.

import {
  BLEND_MODES,
  componentDefaults,
  valueAt,
  type BlendMode,
  type Clip,
  type Key,
  type KeyValue,
  type Gesture,
  type Project,
  type ProjectStore,
  type Sequence,
  type ViewStore,
} from "@renderly/core";
import {
  Icon,
  IconButton,
  PropertyChoice,
  PropertyRow,
  PropertySection,
  useToast,
  type EasePreset,
  type KeyframeState,
} from "@renderly/ui/react";
import { useRef, useState, useSyncExternalStore } from "react";
import { useActions } from "../actions/react";
import { CanvasSection } from "./CanvasSection";

type Fields = Record<string, unknown> & { keys?: Record<string, Key[]> };

interface Transform {
  keys?: Partial<Record<Animatable, Key[]>>;
  position: [number, number];
  scale: [number, number];
  uniform: boolean;
  rotation: number;
  anchor: [number, number];
  flipX: boolean;
  flipY: boolean;
  opacity: number;
  fit: "fit" | "fill" | "stretch";
}

interface Crop {
  keys?: Partial<Record<CropEdge, Key[]>>;
  left: number;
  top: number;
  right: number;
  bottom: number;
}

/** The transform's fields that keyframes can animate. */
type Animatable = "position" | "scale" | "rotation" | "anchor" | "opacity";
const ANIMATABLE: Animatable[] = ["position", "scale", "rotation", "anchor", "opacity"];
type CropEdge = "left" | "top" | "right" | "bottom";
const CROP_EDGES: CropEdge[] = ["left", "top", "right", "bottom"];

/** A clip's component of `type`, with every field it does not set at its default. */
function componentOf<T>(clip: Clip, type: string): T {
  const found = clip.components.find((c) => (c as { type: string }).type === type);
  return { ...componentDefaults(type), ...(found as object | undefined) } as T;
}

/** A component as shown at clip time `t`: animated fields at their value there. */
function atTime<T extends Fields>(component: T, fields: readonly string[], t: number): T {
  if (!component.keys) return component;
  const shown = { ...component } as Record<string, unknown>;
  for (const field of fields) {
    const keys = component.keys[field];
    if (keys && keys.length > 0) shown[field] = valueAt(keys, t) as KeyValue;
  }
  return shown as T;
}

/** The value all `items` share, or null when they differ. */
function shared<T>(items: readonly T[]): T | null {
  const first = items[0];
  if (first === undefined) return null;
  return items.every((item) => Object.is(item, first)) ? first : null;
}

export function Inspector({
  store,
  project,
  view,
  readOnly,
}: {
  store: ProjectStore;
  project: Project;
  view: ViewStore;
  readOnly: boolean;
}) {
  const selection = useSyncExternalStore(
    (listener) => view.select((s) => s.selection, listener),
    () => view.get().selection,
  );
  const sequence =
    project.sequences.find((s) => s.id === view.get().sequence) ?? project.sequences[0];
  const clips = pictureClips(sequence, selection);
  if (!sequence || clips.length === 0)
    return (
      <ProjectProperties store={store} project={project} sequence={sequence} readOnly={readOnly} />
    );
  return (
    <div className="inspector">
      <p className="inspector__subject text-small">
        {clips.length === 1 ? clipName(project, clips[0]) : `${clips.length} clips`}
      </p>
      <TransformSection store={store} sequence={sequence} clips={clips} readOnly={readOnly} />
      <BlendSection store={store} clips={clips} readOnly={readOnly} />
      <CropSection store={store} clips={clips} readOnly={readOnly} />
    </div>
  );
}

/** Selected clips on picture tracks (the transform applies to them), in timeline order. */
function pictureClips(sequence: Sequence | undefined, selection: readonly string[]): Clip[] {
  const wanted = new Set(selection);
  return (
    sequence?.tracks
      .filter((track) => track.kind !== "audio")
      .flatMap((track) => track.clips)
      .filter((clip) => wanted.has(clip.id)) ?? []
  );
}

function clipName(project: Project, clip: Clip | undefined): string {
  if (!clip) return "";
  if (clip.media !== undefined)
    return project.media.find((m) => m.id === clip.media)?.name ?? "Clip";
  return project.sequences.find((s) => s.id === clip.sequence)?.name ?? "Clip";
}

/**
 * Editing one component on the selected clips: values at the playhead, handlers for property
 * rows (a drag is one gesture), the keyframe controls, and one-step changes.
 */
function useComponentEditor<T extends Fields>(
  store: ProjectStore,
  clips: Clip[],
  component: string,
  animatable: readonly string[],
  readOnly: boolean,
) {
  const toast = useToast();
  const actions = useActions();
  const view = actions.ctx.view;
  const gesture = useRef<Gesture | null>(null);
  // Values follow the playhead, so an animated clip shows what is on screen.
  const playhead = useSyncExternalStore(
    (listener) => view.select((s) => s.playhead, listener),
    () => view.get().playhead,
  );
  const ids = clips.map((c) => c.id);
  const stored = clips.map((clip) => componentOf<T>(clip, component));
  const all = stored.map((c, i) => atTime(c, animatable, playhead - (clips[i]?.start ?? 0)));
  const pick = <V,>(get: (t: T) => V) => shared(all.map(get));

  /** Field changes as one command for every selected clip. */
  const payload = (field: string, value: unknown, index?: number) => ({
    clips: ids,
    component,
    field,
    ...(index === undefined ? {} : { index }),
    value,
    // Animated fields take the value into the key at the playhead.
    time: playhead,
  });
  const report = (result: { ok: boolean; error?: { message: string } }) => {
    if (!result.ok && result.error) toast(result.error.message, { kind: "danger" });
  };
  /** Handlers for one numeric field (or one element of a pair). */
  const handlers = (field: string, index?: number, both = false) => {
    const value = (v: number) => (both ? [v, v] : v);
    return {
      onBegin: () => {
        if (view.get().playing) actions.run("playback.pause");
        try {
          gesture.current = store.beginGesture(
            `inspector:${component}.${field}`,
            "component.setMany",
          );
        } catch (error) {
          toast(error instanceof Error ? error.message : String(error), { kind: "danger" });
        }
      },
      onPreview: (v: number) => {
        gesture.current?.preview(payload(field, value(v), index));
      },
      onCommit: (v: number) => {
        const running = gesture.current;
        gesture.current = null;
        if (running) report(running.commit(payload(field, value(v), index)));
        else
          report(
            store.dispatch({ type: "component.setMany", payload: payload(field, value(v), index) }),
          );
      },
      onCancel: () => {
        gesture.current?.cancel();
        gesture.current = null;
      },
      disabled: readOnly,
    };
  };
  /** The keyframe control of one or more animatable fields (a pair is one field). */
  const keyframe = (fields: readonly string[]): KeyframeState => {
    const local = (clip: Clip) => playhead - clip.start;
    const keyTimes = clips.flatMap((clip, i) =>
      fields.flatMap((field) => (stored[i]?.keys?.[field] ?? []).map((k) => clip.start + k.t)),
    );
    const animated = keyTimes.length > 0;
    const on = clips.some((clip, i) =>
      fields.some((field) => stored[i]?.keys?.[field]?.some((k) => k.t === local(clip))),
    );
    const inside = clips.filter((clip) => local(clip) >= 0 && local(clip) <= clip.duration);
    const earlier = keyTimes.filter((t) => t < playhead);
    const later = keyTimes.filter((t) => t > playhead);
    const go = (t: number) => {
      void actions.ctx.link?.pause();
      view.set({ playhead: t });
    };
    return {
      state: on ? "on" : animated ? "animated" : "none",
      onToggle: () => {
        if (readOnly) return;
        if (inside.length === 0) {
          toast("Put the playhead on the clip to add a keyframe.");
          return;
        }
        if (view.get().playing) actions.run("playback.pause");
        const time = view.get().playhead;
        report(
          store.dispatch({
            type: on ? "keyframe.remove" : "keyframe.add",
            payload: { clips: inside.map((c) => c.id), component, fields, time },
          }),
        );
      },
      onPrevious: earlier.length ? () => go(Math.max(...earlier)) : undefined,
      onNext: later.length ? () => go(Math.min(...later)) : undefined,
      onEase: (preset: EasePreset) =>
        report(
          store.dispatch({
            type: "keyframe.ease",
            payload: { clips: ids, component, fields, time: playhead, preset },
          }),
        ),
    };
  };
  const setNow = (field: string, value: unknown) =>
    report(store.dispatch({ type: "component.setMany", payload: payload(field, value) }));
  const reset = () =>
    report(store.dispatch({ type: "component.reset", payload: { clips: ids, component } }));
  const defaults = componentDefaults(component) as T;
  // Default: no keys, and every field as a new component has it.
  const isDefault = stored.every(
    (c) => !c.keys && JSON.stringify({ ...defaults, ...c }) === JSON.stringify(defaults),
  );
  return { ids, all, pick, handlers, keyframe, setNow, reset, report, defaults, isDefault };
}

function TransformSection({
  store,
  sequence,
  clips,
  readOnly,
}: {
  store: ProjectStore;
  sequence: Sequence;
  clips: Clip[];
  readOnly: boolean;
}) {
  const toast = useToast();
  const {
    ids,
    all,
    pick,
    handlers,
    keyframe,
    setNow,
    reset,
    defaults: d,
    isDefault,
  } = useComponentEditor<Transform & Fields>(store, clips, "transform", ANIMATABLE, readOnly);
  const uniform = pick((t) => t.uniform) ?? false;
  const scaleX = pick((t) => t.scale[0]);
  const scaleY = pick((t) => t.scale[1]);
  const flipX = pick((t) => t.flipX);
  const flipY = pick((t) => t.flipY);
  const fit = pick((t) => t.fit);

  return (
    <PropertySection
      title="Transform"
      keyframe={keyframe(ANIMATABLE)}
      resetDisabled={readOnly || isDefault}
      onReset={reset}
    >
      <PropertyRow
        label="Position X"
        keyframe={keyframe(["position"])}
        value={pick((t) => t.position[0])}
        min={-2}
        max={2}
        step={1 / sequence.width}
        display={sequence.width}
        unit="px"
        defaultValue={d.position[0]}
        {...handlers("position", 0)}
      />
      <PropertyRow
        label="Position Y"
        keyframe={keyframe(["position"])}
        value={pick((t) => t.position[1])}
        min={-2}
        max={2}
        step={1 / sequence.height}
        display={sequence.height}
        unit="px"
        defaultValue={d.position[1]}
        {...handlers("position", 1)}
      />
      {uniform ? (
        <PropertyRow
          label="Scale"
          keyframe={keyframe(["scale"])}
          value={scaleX !== null && scaleX === scaleY ? scaleX : null}
          min={0.01}
          max={4}
          step={0.001}
          display={100}
          precision={1}
          unit="%"
          defaultValue={1}
          {...handlers("scale", undefined, true)}
        />
      ) : (
        <>
          <PropertyRow
            label="Scale X"
            keyframe={keyframe(["scale"])}
            value={scaleX}
            min={0.01}
            max={4}
            step={0.001}
            display={100}
            precision={1}
            unit="%"
            defaultValue={1}
            {...handlers("scale", 0)}
          />
          <PropertyRow
            label="Scale Y"
            keyframe={keyframe(["scale"])}
            value={scaleY}
            min={0.01}
            max={4}
            step={0.001}
            display={100}
            precision={1}
            unit="%"
            defaultValue={1}
            {...handlers("scale", 1)}
          />
        </>
      )}
      <PropertyRow
        label="Rotation"
        keyframe={keyframe(["rotation"])}
        value={pick((t) => t.rotation)}
        min={-360}
        max={360}
        step={0.1}
        precision={1}
        unit="°"
        defaultValue={0}
        {...handlers("rotation")}
      />
      <PropertyRow
        label="Anchor X"
        keyframe={keyframe(["anchor"])}
        value={pick((t) => t.anchor[0])}
        min={-1}
        max={1}
        step={0.001}
        display={100}
        precision={1}
        unit="%"
        defaultValue={0}
        {...handlers("anchor", 0)}
      />
      <PropertyRow
        label="Anchor Y"
        keyframe={keyframe(["anchor"])}
        value={pick((t) => t.anchor[1])}
        min={-1}
        max={1}
        step={0.001}
        display={100}
        precision={1}
        unit="%"
        defaultValue={0}
        {...handlers("anchor", 1)}
      />
      <PropertyRow
        label="Opacity"
        keyframe={keyframe(["opacity"])}
        value={pick((t) => t.opacity)}
        min={0}
        max={1}
        step={0.001}
        display={100}
        unit="%"
        defaultValue={1}
        {...handlers("opacity")}
      />
      <PropertyChoice label="Fit">
        <div className="r-segmented r-segmented--sm" role="group" aria-label="Fit">
          {(
            [
              ["fit", "Fit"],
              ["fill", "Fill"],
              ["stretch", "Stretch"],
            ] as const
          ).map(([value, label]) => (
            <button
              key={value}
              type="button"
              aria-pressed={fit === value}
              disabled={readOnly}
              onClick={() => setNow("fit", value)}
            >
              {label}
            </button>
          ))}
        </div>
      </PropertyChoice>
      <div className="inspector__toggles" role="group" aria-label="Scale and mirroring">
        <IconButton
          icon={uniform ? "linked" : "unlinked"}
          label={uniform ? "Scale X and Y separately" : "Scale X and Y together"}
          size="sm"
          aria-pressed={uniform}
          disabled={readOnly}
          onClick={() => {
            if (uniform) return setNow("uniform", false);
            // Linking makes Y follow X, so each clip keeps one scale: one undo step for both.
            const linked = store.transaction("Link scale", () =>
              [
                store.dispatch({
                  type: "component.setMany",
                  payload: { clips: ids, component: "transform", field: "uniform", value: true },
                }),
                store.dispatch({
                  type: "component.setMany",
                  payload: {
                    clips: ids,
                    component: "transform",
                    field: "scale",
                    values: all.map((t) => [t.scale[0], t.scale[0]]),
                  },
                }),
              ].every((result) => result.ok),
            );
            if (!linked) toast("The scale could not be linked.", { kind: "danger" });
          }}
        />
        <IconButton
          icon="flip-x"
          label="Mirror left to right"
          size="sm"
          aria-pressed={flipX === true}
          disabled={readOnly}
          onClick={() => setNow("flipX", flipX !== true)}
        />
        <IconButton
          icon="flip-y"
          label="Mirror top to bottom"
          size="sm"
          aria-pressed={flipY === true}
          disabled={readOnly}
          onClick={() => setNow("flipY", flipY !== true)}
        />
      </div>
    </PropertySection>
  );
}

/** Names of the blend modes as the menu shows them. */
const BLEND_LABELS: Record<BlendMode, string> = {
  normal: "Normal",
  multiply: "Multiply",
  screen: "Screen",
  overlay: "Overlay",
  softLight: "Soft light",
  add: "Add",
  darken: "Darken",
  lighten: "Lighten",
  difference: "Difference",
  colorDodge: "Colour dodge",
  colorBurn: "Colour burn",
  hue: "Hue",
  saturation: "Saturation",
  color: "Colour",
  luminosity: "Luminosity",
};

function BlendSection({
  store,
  clips,
  readOnly,
}: {
  store: ProjectStore;
  clips: Clip[];
  readOnly: boolean;
}) {
  const { pick, setNow, reset, isDefault } = useComponentEditor<{ mode: BlendMode } & Fields>(
    store,
    clips,
    "blend",
    [],
    readOnly,
  );
  const mode = pick((b) => b.mode);
  return (
    <PropertySection title="Blending" resetDisabled={readOnly || isDefault} onReset={reset}>
      <PropertyChoice label="Mode" id="inspector-blend">
        <div className={`r-field r-field--sm${readOnly ? " r-field--disabled" : ""}`}>
          <select
            id="inspector-blend"
            value={mode ?? ""}
            disabled={readOnly}
            onChange={(event) => setNow("mode", event.target.value)}
          >
            {mode === null && <option value="">Mixed</option>}
            {BLEND_MODES.map((m) => (
              <option key={m} value={m}>
                {BLEND_LABELS[m]}
              </option>
            ))}
          </select>
          <Icon name="chevron-down" />
        </div>
      </PropertyChoice>
    </PropertySection>
  );
}

function CropSection({
  store,
  clips,
  readOnly,
}: {
  store: ProjectStore;
  clips: Clip[];
  readOnly: boolean;
}) {
  const { pick, handlers, keyframe, reset, isDefault } = useComponentEditor<Crop & Fields>(
    store,
    clips,
    "crop",
    CROP_EDGES,
    readOnly,
  );
  const labels: Record<CropEdge, string> = {
    left: "Left",
    top: "Top",
    right: "Right",
    bottom: "Bottom",
  };
  return (
    <PropertySection
      title="Crop"
      keyframe={keyframe(CROP_EDGES)}
      resetDisabled={readOnly || isDefault}
      onReset={reset}
    >
      {CROP_EDGES.map((edge) => (
        <PropertyRow
          key={edge}
          label={labels[edge]}
          keyframe={keyframe([edge])}
          value={pick((c) => c[edge])}
          min={0}
          max={1}
          step={0.001}
          display={100}
          precision={1}
          unit="%"
          defaultValue={0}
          {...handlers(edge)}
        />
      ))}
    </PropertySection>
  );
}

/** The project's own properties and its sequence's canvas. The name goes through the rename command. */
function ProjectProperties({
  store,
  project,
  sequence,
  readOnly,
}: {
  store: ProjectStore;
  project: Project;
  sequence: Sequence | undefined;
  readOnly: boolean;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = () => {
    const name = draft?.trim();
    setDraft(null);
    if (name && name !== project.name)
      store.dispatch({ type: "project.rename", payload: { name } });
  };
  return (
    <div className="inspector">
      <div className="r-form">
        <label className="r-label" htmlFor="project-name">
          Name
        </label>
        <div className={`r-field${readOnly ? " r-field--disabled" : ""}`}>
          <input
            id="project-name"
            value={draft ?? project.name}
            disabled={readOnly}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={commit}
            onKeyDown={(event) => {
              if (event.key === "Enter") commit();
              if (event.key === "Escape") setDraft(null);
            }}
          />
        </div>
      </div>
      {sequence && <CanvasSection store={store} sequence={sequence} readOnly={readOnly} />}
    </div>
  );
}
