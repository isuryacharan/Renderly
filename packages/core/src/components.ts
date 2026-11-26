// Components: the typed features attached to a clip (transform, crop, effect, ...). Adding a
// feature adds a component type here; the clip type never grows, and old clips are untouched.
//
// Each component type registers a Zod schema for its own fields. Every component may also carry
// keyframes under `keys`: per property, a list of { t, v, ease } with `t` in flicks relative to
// the clip start.

import { z } from "zod";

export const Ease = z.enum(["linear", "auto", "bezier", "hold"]);

/**
 * A value at a time (keyframes.ts). Handles are [time offset in flicks, progress offset] from the
 * key: progress is the share of the span's change, so numbers and pairs ease alike.
 */
export const Keyframe = z.strictObject({
  t: z.int().min(0),
  v: z.union([z.number(), z.array(z.number()).min(1).max(4)]),
  ease: Ease.default("auto"),
  in: z.tuple([z.int(), z.number()]).optional(),
  out: z.tuple([z.int(), z.number()]).optional(),
});

export const Keys = z.record(z.string(), z.array(Keyframe).min(1));

type ComponentShape = Record<string, z.ZodType>;

const registry = new Map<string, z.ZodObject>();
let version = 0;

/**
 * Registers a component type. `fields` must not include `type` or `keys`; those are added. Call
 * at module load, before any document is parsed.
 */
export function registerComponent(type: string, fields: ComponentShape): void {
  if (registry.has(type)) throw new Error(`Component type "${type}" is already registered`);
  if ("type" in fields || "keys" in fields) throw new Error("`type` and `keys` are reserved");
  registry.set(type, z.strictObject({ type: z.literal(type), ...fields, keys: Keys.optional() }));
  version++;
}

export function componentTypes(): string[] {
  return [...registry.keys()];
}

/** A component of `type` with every field at its default (without keyframes). */
export function componentDefaults(type: string): Record<string, unknown> {
  const schema = registry.get(type);
  if (!schema) throw new Error(`No component type "${type}"`);
  return schema.parse({ type }) as Record<string, unknown>;
}

/** Bumped on every registration, so cached schemas know to rebuild. */
export function registryVersion(): number {
  return version;
}

/** One schema accepting any registered component type, chosen by `type`. */
export function componentSchema(): z.ZodType {
  const types = [...registry.values()];
  if (types.length === 0) throw new Error("No component types are registered");
  return z.discriminatedUnion("type", types as [z.ZodObject, ...z.ZodObject[]]);
}

// --- built-in component types ---

/**
 * Where a visual clip sits in the frame (schema version 2).
 * - `position`: where the anchor sits, as a fraction of the sequence size from the frame's centre
 *   (x right, y down).
 * - `scale`: per axis; [1, 1] fits the clip to the frame. `uniform` keeps the two together in the
 *   inspector (it does not change the picture).
 * - `rotation`: degrees clockwise, about the anchor.
 * - `anchor`: the point of the clip that `position` places and that scale and rotation turn about,
 *   as a fraction of the clip's (fitted) picture from its centre.
 * - `flipX`, `flipY`: mirror the picture.
 * - `opacity`: 0 to 1.
 */
registerComponent("transform", {
  position: z.tuple([z.number(), z.number()]).default([0, 0]),
  scale: z.tuple([z.number().positive(), z.number().positive()]).default([1, 1]),
  uniform: z.boolean().default(true),
  rotation: z.number().default(0),
  anchor: z.tuple([z.number(), z.number()]).default([0, 0]),
  flipX: z.boolean().default(false),
  flipY: z.boolean().default(false),
  opacity: z.number().min(0).max(1).default(1),
});
