// The first commands: project name, markers, and component fields. Timeline edits (insert,
// ripple, trim, ...) arrive in S1.3 on the same registry.

import type { Draft } from "immer";
import { z } from "zod";
import { CommandError, defineCommand } from "./commands";
import { componentDefaults, componentTypes } from "./components";
import { keyAt } from "./keyframe-commands";
import type { Key } from "./keyframes";
import { Flicks, Id, type Project } from "./schema";

function findSequence(project: Draft<Project>, id: string) {
  const sequence = project.sequences.find((s) => s.id === id);
  if (!sequence) throw new CommandError("not-found", `No sequence ${id}`, "sequence");
  return sequence;
}

function findClip(project: Draft<Project>, id: string) {
  for (const sequence of project.sequences) {
    for (const track of sequence.tracks) {
      const clip = track.clips.find((c) => c.id === id);
      if (clip) return clip;
    }
  }
  throw new CommandError("not-found", `No clip ${id}`, "clip");
}

export const SetProjectName = defineCommand({
  type: "project.rename",
  payload: z.strictObject({ name: z.string().trim().min(1) }),
  label: () => "Rename project",
  apply: (project, { name }) => {
    project.name = name;
  },
});

export const AddMarker = defineCommand({
  type: "marker.add",
  payload: z.strictObject({
    sequence: Id,
    id: Id,
    t: Flicks,
    name: z.string().default(""),
    color: z.string().default("accent"),
  }),
  label: () => "Add marker",
  apply: (project, { sequence, id, t, name, color }) => {
    const markers = findSequence(project, sequence).markers;
    // Markers stay sorted by time; equal times keep insertion order.
    const at = markers.findIndex((m) => m.t > t);
    markers.splice(at < 0 ? markers.length : at, 0, { id, t, name, color });
  },
});

export const MoveMarker = defineCommand({
  type: "marker.move",
  payload: z.strictObject({ sequence: Id, id: Id, t: Flicks }),
  label: () => "Move marker",
  apply: (project, { sequence, id, t }) => {
    const markers = findSequence(project, sequence).markers;
    const marker = markers.find((m) => m.id === id);
    if (!marker) throw new CommandError("not-found", `No marker ${id}`, "id");
    markers.splice(markers.indexOf(marker), 1);
    marker.t = t;
    const at = markers.findIndex((m) => m.t > t);
    markers.splice(at < 0 ? markers.length : at, 0, marker);
  },
});

export const RemoveMarker = defineCommand({
  type: "marker.remove",
  payload: z.strictObject({ sequence: Id, id: Id }),
  label: () => "Remove marker",
  apply: (project, { sequence, id }) => {
    const markers = findSequence(project, sequence).markers;
    const index = markers.findIndex((m) => m.id === id);
    if (index < 0) throw new CommandError("not-found", `No marker ${id}`, "id");
    markers.splice(index, 1);
  },
});

const FieldValue = z.union([
  z.number(),
  z.boolean(),
  z.string(),
  z.array(z.number()).min(1).max(4),
]);

/** The component of `type` on a clip, added with its defaults when the clip has none. */
function componentOf(
  clip: Draft<Project>["sequences"][number]["tracks"][number]["clips"][number],
  type: string,
) {
  const components = clip.components as Record<string, unknown>[];
  let target = components.find((c) => c["type"] === type);
  if (!target) {
    target = componentDefaults(type);
    components.push(target);
  }
  return target;
}

/**
 * Sets one field of a component on several clips in one step (the inspector and the viewer's
 * handles). Clips without the component get it, with its defaults. `value` goes to every clip, or
 * `values` gives one per clip, in order. With `index`, only that element of a vector field
 * changes (position X, scale Y). Values are checked against the component's schema when the store
 * validates the result.
 */
export const SetComponentFieldMany = defineCommand({
  type: "component.setMany",
  payload: z
    .strictObject({
      clips: z.array(Id).min(1),
      component: z.string().min(1),
      field: z
        .string()
        .min(1)
        .refine((f) => f !== "type" && f !== "keys", "cannot set `type` or `keys`"),
      index: z.int().min(0).max(3).optional(),
      value: FieldValue.optional(),
      values: z.array(FieldValue).optional(),
      /**
       * Timeline time of the edit. Where the field is animated, the value goes into the key at
       * that time (added without changing the motion, if there is none); elsewhere it is the value.
       */
      time: Flicks.optional(),
    })
    .refine((p) => (p.value === undefined) !== (p.values === undefined), {
      message: "needs exactly one of `value` or `values`",
    })
    .refine((p) => p.values === undefined || p.values.length === p.clips.length, {
      message: "`values` needs one value per clip",
      path: ["values"],
    }),
  label: ({ field, clips }) =>
    `Change ${field}${clips.length > 1 ? ` on ${clips.length} clips` : ""}`,
  apply: (project, { clips, component, field, index, value, values, time }) => {
    if (!componentTypes().includes(component))
      throw new CommandError("not-found", `No component type ${component}`, "component");
    if (!(field in componentDefaults(component)))
      throw new CommandError(
        "rejected",
        `The ${component} component has no field ${field}`,
        "field",
      );
    clips.forEach((id, i) => {
      const clip = findClip(project, id);
      const target = componentOf(clip, component) as Record<string, unknown> & {
        keys?: Record<string, Key[]>;
      };
      const next = values ? values[i] : value;
      // An animated field: edit the key at the time (a plain value would be ignored).
      let holder: Record<string, unknown> = target;
      let slot = field;
      if (time !== undefined && (target.keys?.[field]?.length ?? 0) > 0) {
        const local = time - clip.start;
        if (local < 0 || local > clip.duration)
          throw new CommandError(
            "rejected",
            `${field} is animated: put the playhead on the clip`,
            "time",
          );
        holder = keyAt(target, field, local) as unknown as Record<string, unknown>;
        slot = "v";
      }
      if (index === undefined) {
        holder[slot] = next;
        return;
      }
      const current = holder[slot];
      if (!Array.isArray(current) || typeof next !== "number" || index >= current.length)
        throw new CommandError("rejected", `${field} has no element ${index}`, "index");
      current[index] = next;
    });
  },
});

/** Puts a component back to its defaults on several clips (keyframes go too). */
export const ResetComponent = defineCommand({
  type: "component.reset",
  payload: z.strictObject({ clips: z.array(Id).min(1), component: z.string().min(1) }),
  label: ({ component }) => `Reset ${component}`,
  apply: (project, { clips, component }) => {
    if (!componentTypes().includes(component))
      throw new CommandError("not-found", `No component type ${component}`, "component");
    for (const id of clips) {
      const components = findClip(project, id).components as Record<string, unknown>[];
      const at = components.findIndex((c) => c["type"] === component);
      if (at >= 0) components[at] = componentDefaults(component);
    }
  },
});

/**
 * Sets one field of a clip's component, e.g. the transform's opacity. The value is checked against
 * the component's schema when the store validates the result.
 */
export const SetComponentField = defineCommand({
  type: "component.set",
  payload: z.strictObject({
    clip: Id,
    component: z.string().min(1),
    field: z
      .string()
      .min(1)
      .refine((f) => f !== "type" && f !== "keys", "cannot set `type` or `keys`"),
    value: z.union([z.number(), z.boolean(), z.string(), z.array(z.number()).min(1).max(4)]),
  }),
  label: ({ field }) => `Change ${field}`,
  apply: (project, { clip, component, field, value }) => {
    const components = findClip(project, clip).components as Record<string, unknown>[];
    const target = components.find((c) => c["type"] === component);
    if (!target)
      throw new CommandError(
        "not-found",
        `Clip ${clip} has no ${component} component`,
        "component",
      );
    if (!(field in target))
      throw new CommandError(
        "rejected",
        `The ${component} component has no field ${field}`,
        "field",
      );
    target[field] = value;
  },
});
