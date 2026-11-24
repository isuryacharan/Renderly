// Components: the typed features attached to a clip (transform, crop, effect, ...). Adding a
// feature adds a component type here; the clip type never grows, and old clips are untouched.
//
// Each component type registers a Zod schema for its own fields. Every component may also carry
// keyframes under `keys`: per property, a list of { t, v, ease } with `t` in flicks relative to
// the clip start.

import { z } from "zod";

export const Ease = z.enum(["linear", "auto", "bezier", "hold"]);

/** A value at a time. Bézier handles are relative to the key, in flicks and value units. */
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
 * Where a visual clip sits in the frame. `position` moves the clip's centre by a fraction of the
 * sequence size (x right, y down); `scale` 1 fits the clip to the frame; `rotation` is degrees
 * clockwise; `opacity` is 0 to 1.
 */
registerComponent("transform", {
  position: z.tuple([z.number(), z.number()]).default([0, 0]),
  scale: z.number().positive().default(1),
  rotation: z.number().default(0),
  opacity: z.number().min(0).max(1).default(1),
});
