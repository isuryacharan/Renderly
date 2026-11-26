// The inspector: follows the timeline selection. Nothing selected shows the project's settings;
// selected picture clips show their transform. With several clips selected, a value they share is
// shown, and one they do not is "mixed" (an en dash); an edit applies to all of them in one step.
// A drag (slider or label) is one store gesture: one undo step, Escape cancels.

import {
  componentDefaults,
  valueAt,
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
  IconButton,
  PropertyRow,
  PropertySection,
  useToast,
  type EasePreset,
  type KeyframeState,
} from "@renderly/ui/react";
import { useRef, useState, useSyncExternalStore } from "react";
import { useActions } from "../actions/react";

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
}

const DEFAULTS = (): Transform => componentDefaults("transform") as unknown as Transform;

/** The transform's fields that keyframes can animate. */
type Animatable = "position" | "scale" | "rotation" | "anchor" | "opacity";
const ANIMATABLE: Animatable[] = ["position", "scale", "rotation", "anchor", "opacity"];

function transformOf(clip: Clip): Transform {
  const found = clip.components.find((c) => (c as { type: string }).type === "transform");
  return { ...DEFAULTS(), ...(found as Partial<Transform> | undefined) };
}

/** The transform as shown at clip time `t`: animated fields at their value there. */
function transformAtTime(transform: Transform, t: number): Transform {
  if (!transform.keys) return transform;
  const shown = { ...transform } as Record<string, unknown>;
  for (const field of ANIMATABLE) {
    const keys = transform.keys[field];
    if (keys && keys.length > 0) shown[field] = valueAt(keys, t) as KeyValue;
  }
  return shown as unknown as Transform;
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
    return <ProjectProperties store={store} project={project} readOnly={readOnly} />;
  return (
    <div className="inspector">
      <p className="inspector__subject text-small">
        {clips.length === 1 ? clipName(project, clips[0]) : `${clips.length} clips`}
      </p>
      <TransformSection store={store} sequence={sequence} clips={clips} readOnly={readOnly} />
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
  const actions = useActions();
  const view = actions.ctx.view;
  const gesture = useRef<Gesture | null>(null);
  // Values follow the playhead, so an animated clip shows what is on screen.
  const playhead = useSyncExternalStore(
    (listener) => view.select((s) => s.playhead, listener),
    () => view.get().playhead,
  );
  const ids = clips.map((c) => c.id);
  const stored = clips.map(transformOf);
  const all = stored.map((t, i) => transformAtTime(t, playhead - (clips[i]?.start ?? 0)));
  const pick = <T,>(get: (t: Transform) => T) => shared(all.map(get));

  /** Field changes as one command for every selected clip. */
  const payload = (field: keyof Transform, value: unknown, index?: number) => ({
    clips: ids,
    component: "transform",
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
  const handlers = (field: keyof Transform, index?: number, both = false) => {
    const value = (v: number) => (both ? [v, v] : v);
    return {
      onBegin: () => {
        if (actions.ctx.view.get().playing) actions.run("playback.pause");
        try {
          gesture.current = store.beginGesture(`inspector:${field}`, "component.setMany");
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
  const keyframe = (fields: Animatable[]): KeyframeState => {
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
            payload: { clips: inside.map((c) => c.id), component: "transform", fields, time },
          }),
        );
      },
      onPrevious: earlier.length ? () => go(Math.max(...earlier)) : undefined,
      onNext: later.length ? () => go(Math.min(...later)) : undefined,
      onEase: (preset: EasePreset) =>
        report(
          store.dispatch({
            type: "keyframe.ease",
            payload: { clips: ids, component: "transform", fields, time: playhead, preset },
          }),
        ),
    };
  };

  const setNow = (field: keyof Transform, value: unknown) =>
    report(store.dispatch({ type: "component.setMany", payload: payload(field, value) }));

  const d = DEFAULTS();
  const uniform = pick((t) => t.uniform) ?? false;
  const scaleX = pick((t) => t.scale[0]);
  const scaleY = pick((t) => t.scale[1]);
  const flipX = pick((t) => t.flipX);
  const flipY = pick((t) => t.flipY);
  const isDefault = all.every((t) => JSON.stringify(t) === JSON.stringify(d));

  return (
    <PropertySection
      title="Transform"
      keyframe={keyframe(ANIMATABLE)}
      resetDisabled={readOnly || isDefault}
      onReset={() =>
        report(
          store.dispatch({
            type: "component.reset",
            payload: { clips: ids, component: "transform" },
          }),
        )
      }
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
                store.dispatch({ type: "component.setMany", payload: payload("uniform", true) }),
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

/** The project's own properties. The name is changed through the rename command. */
function ProjectProperties({
  store,
  project,
  readOnly,
}: {
  store: ProjectStore;
  project: Project;
  readOnly: boolean;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  const commit = () => {
    const name = draft?.trim();
    setDraft(null);
    if (name && name !== project.name)
      store.dispatch({ type: "project.rename", payload: { name } });
  };
  const main = project.sequences[0];
  return (
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
      {main && (
        <>
          <span className="r-label">Sequence</span>
          <span className="r-tabular editor__fact">
            {main.width} × {main.height} · {main.fps[0] / main.fps[1]} fps
          </span>
        </>
      )}
    </div>
  );
}
