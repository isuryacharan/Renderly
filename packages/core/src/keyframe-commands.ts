// Keyframe commands: add (several fields of several clips at one time, as one step), remove,
// move and ease. Times in payloads are timeline flicks; keys are stored in clip time. Adding a key
// never changes the motion (keyframes.ts `insertKey`): it only marks the value already there.

import type { Draft } from "immer";
import { z } from "zod";
import { CommandError, defineCommand } from "./commands";
import { componentDefaults, componentTypes } from "./components";
import { applyEase, insertKey, type Key, type KeyValue } from "./keyframes";
import { Flicks, Id, type Clip, type Project } from "./schema";

type DClip = Draft<Clip>;
type Component = Record<string, unknown> & { keys?: Record<string, Key[]> };

function findClip(project: Draft<Project>, id: string): DClip {
  for (const sequence of project.sequences)
    for (const track of sequence.tracks) {
      const clip = track.clips.find((c) => c.id === id);
      if (clip) {
        if (track.locked)
          throw new CommandError("rejected", `Track ${track.id} is locked`, "clips");
        return clip;
      }
    }
  throw new CommandError("not-found", `No clip ${id}`, "clips");
}

/** The component of `type` on a clip, added with its defaults when the clip has none. */
export function componentOn(clip: DClip, type: string): Component {
  if (!componentTypes().includes(type))
    throw new CommandError("not-found", `No component type ${type}`, "component");
  const components = clip.components as Component[];
  let found = components.find((c) => c["type"] === type);
  if (!found) {
    found = componentDefaults(type) as Component;
    components.push(found);
  }
  return found;
}

/** Only numbers and lists of numbers can be animated. */
function animatable(component: Component, field: string): KeyValue {
  const value = component[field];
  const numeric =
    typeof value === "number" ||
    (Array.isArray(value) && value.every((v) => typeof v === "number"));
  if (!numeric) throw new CommandError("rejected", `${field} cannot be animated`, "fields");
  return JSON.parse(JSON.stringify(value)) as KeyValue;
}

/** The clip time of timeline time `t`, which must fall inside the clip. */
function clipTime(clip: DClip, t: number): number {
  const local = t - clip.start;
  if (local < 0 || local > clip.duration)
    throw new CommandError("rejected", "The time is outside the clip", "time");
  return local;
}

/** Puts a key at clip time `t` (keeping the motion), and returns it. */
export function keyAt(component: Component, field: string, t: number): Key {
  const keys = (component.keys ??= {});
  const list = keys[field];
  if (!list || list.length === 0) {
    const key: Key = { t, v: animatable(component, field), ease: "auto" };
    keys[field] = [key];
    return key;
  }
  const next = insertKey(list, t);
  keys[field] = next;
  return next.find((k) => k.t === t) as Key;
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export const AddKeyframes = defineCommand({
  type: "keyframe.add",
  payload: z.strictObject({
    clips: z.array(Id).min(1),
    component: z.string().min(1),
    /** Fields keyed together (a section's group key passes all of them). */
    fields: z.array(z.string().min(1)).min(1),
    /** Timeline time. */
    time: Flicks,
  }),
  label: ({ fields }) => (fields.length === 1 ? `Add ${fields[0]} keyframe` : "Add keyframes"),
  apply: (project, { clips, component, fields, time }) => {
    for (const id of clips) {
      const clip = findClip(project, id);
      const target = componentOn(clip, component);
      const t = clipTime(clip, time);
      for (const field of fields) {
        if (!(field in target))
          throw new CommandError(
            "rejected",
            `The ${component} component has no field ${field}`,
            "fields",
          );
        keyAt(target, field, t);
      }
    }
  },
});

/** The keys of a field, which must exist. */
function keysOf(project: Draft<Project>, clipId: string, component: string, field: string) {
  const clip = findClip(project, clipId);
  const target = (clip.components as Component[]).find((c) => c["type"] === component);
  const list = target?.keys?.[field];
  if (!target || !list || list.length === 0)
    throw new CommandError("not-found", `${field} has no keyframes`, "field");
  return { clip, target, list };
}

export const RemoveKeyframes = defineCommand({
  type: "keyframe.remove",
  payload: z.strictObject({
    clips: z.array(Id).min(1),
    component: z.string().min(1),
    fields: z.array(z.string().min(1)).min(1),
    time: Flicks,
  }),
  label: ({ fields }) =>
    fields.length === 1 ? `Remove ${fields[0]} keyframe` : "Remove keyframes",
  apply: (project, { clips, component, fields, time }) => {
    let removed = 0;
    for (const id of clips)
      for (const field of fields) {
        const clip = findClip(project, id);
        const target = (clip.components as Component[]).find((c) => c["type"] === component);
        const list = target?.keys?.[field];
        if (!target || !list) continue;
        const t = time - clip.start;
        const at = list.findIndex((k) => k.t === t);
        if (at < 0) continue;
        const [key] = list.splice(at, 1);
        removed++;
        // The last key goes: the property keeps its value, so nothing jumps.
        if (list.length === 0 && key) {
          target[field] = key.v;
          delete target.keys?.[field];
          if (target.keys && Object.keys(target.keys).length === 0) delete target.keys;
        }
      }
    if (removed === 0) throw new CommandError("not-found", "No keyframe at that time", "time");
  },
});

export const MoveKeyframe = defineCommand({
  type: "keyframe.move",
  payload: z.strictObject({
    clip: Id,
    component: z.string().min(1),
    fields: z.array(z.string().min(1)).min(1),
    /** Timeline times. */
    from: Flicks,
    to: Flicks,
  }),
  label: () => "Move keyframe",
  apply: (project, { clip: clipId, component, fields, from, to }) => {
    for (const field of fields) {
      const { clip, list } = keysOf(project, clipId, component, field);
      const source = from - clip.start;
      const target = clipTime(clip, to);
      const key = list.find((k) => k.t === source);
      if (!key) throw new CommandError("not-found", "No keyframe at that time", "from");
      if (target !== source && list.some((k) => k.t === target))
        throw new CommandError("rejected", "There is already a keyframe there", "to");
      key.t = target;
      list.sort((a, b) => a.t - b.t);
    }
  },
});

export const EaseKeyframe = defineCommand({
  type: "keyframe.ease",
  payload: z.strictObject({
    clips: z.array(Id).min(1),
    component: z.string().min(1),
    fields: z.array(z.string().min(1)).min(1),
    time: Flicks,
    preset: z.enum(["linear", "ease-in", "ease-out", "ease-in-out", "auto", "hold"]),
  }),
  label: ({ clips }) => `Change ${plural(clips.length, "keyframe")}' easing`,
  apply: (project, { clips, component, fields, time, preset }) => {
    let changed = 0;
    for (const id of clips)
      for (const field of fields) {
        const clip = findClip(project, id);
        const target = (clip.components as Component[]).find((c) => c["type"] === component);
        const list = target?.keys?.[field];
        if (!target?.keys || !list) continue;
        const at = list.findIndex((k) => k.t === time - clip.start);
        if (at < 0) continue;
        target.keys[field] = applyEase(list, at, preset);
        changed++;
      }
    if (changed === 0) throw new CommandError("not-found", "No keyframe at that time", "time");
  },
});
