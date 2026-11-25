// The first commands: project name, markers, and component fields. Timeline edits (insert,
// ripple, trim, ...) arrive in S1.3 on the same registry.

import type { Draft } from "immer";
import { z } from "zod";
import { CommandError, defineCommand } from "./commands";
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
