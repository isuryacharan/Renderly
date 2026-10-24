// Pure helpers for effect slots, catalogue-driven parameters, hover-preview stacks and keyframe
// easing display. No store or IPC here, so every rule is unit-testable.

import type { CatalogueEntry, CatalogueParam, PreviewEffect } from "./ipc";
import {
  effectSpec,
  setKeyframeEasing,
  type EffectSpec,
  type EffectsChange,
} from "./commands";
import type {
  EasingChoice,
  EffectClip,
  EffectInstance,
  EffectParams,
  EffectParamValue,
  EffectSlot,
  Keyframe,
  KeyframeProperty,
  KeyframeTrack,
  KeyValue,
  ProgressEasing,
  TransitionEasingArg,
} from "./types";
import { effectStack } from "./types";



/** The ref without its `@version` suffix. */
export function refBase(ref: string): string {
  const at = ref.lastIndexOf("@");
  return at > 0 ? ref.slice(0, at) : ref;
}

/** Catalogue entry for a stored ref: the exact ref, else the same item at another version. */
export function findEntry(
  catalogue: readonly CatalogueEntry[],
  ref: string,
): CatalogueEntry | undefined {
  return (
    catalogue.find((e) => e.ref === ref) ??
    catalogue.find((e) => refBase(e.ref) === refBase(ref))
  );
}

/** Display name for a stored ref when the catalogue does not know it. */
export function refFallbackLabel(ref: string): string {
  const base = refBase(ref);
  const name = base.slice(Math.max(base.lastIndexOf("/"), base.lastIndexOf(":")) + 1);
  return name.replace(/[_-]+/g, " ").replace(/^./, (c) => c.toUpperCase());
}

export function effectLabel(catalogue: readonly CatalogueEntry[], ref: string): string {
  return findEntry(catalogue, ref)?.label ?? refFallbackLabel(ref);
}

/** The values a new instance gets: each param's `initial` (never a hard-coded UI default). */
export function initialParams(entry: Pick<CatalogueEntry, "params">): EffectParams {
  const out: EffectParams = {};
  for (const p of entry.params) out[p.key] = cloneValue(p.initial);
  return out;
}

function cloneValue(v: EffectParamValue): EffectParamValue {
  return Array.isArray(v) ? [...v] : v;
}

/** The value a param shows: the stored one, else the engine's `default` for an absent key. */
export function paramValue(
  instance: Pick<EffectInstance, "params">,
  spec: CatalogueParam,
): EffectParamValue {
  const stored = instance.params[spec.key];
  return stored === undefined ? spec.default : stored;
}

/** Params the user can keyframe: numeric, colour and vec2 (not bool, choice or LUT). */
export function isKeyframable(spec: Pick<CatalogueParam, "type" | "animatable">): boolean {
  if (spec.animatable === false) return false;
  return spec.type === "float" || spec.type === "int" || spec.type === "color" || spec.type === "vec2";
}

// ---- slots -------------------------------------------------------------------------------

export type StackSlot = EffectSlot | "effects";

/** Where an item goes by default on this clip: its catalogue slot, with `key` folded into
 *  `effects` on an adjustment layer (it has no key slot) and `transition` unsupported. */
export function targetSlot(entry: Pick<CatalogueEntry, "slot">, clip: EffectClip): StackSlot {
  const slot = entry.slot;
  if (slot === "transition") return "effects";
  if (slot === "key" && (clip.type === "adjustment" || clip.type === "text")) return "effects";
  return slot;
}

/** A slot occupant with the slot it sits in, in render order. */
export interface StackItem {
  slot: StackSlot;
  instance: EffectInstance;
  /** Index in `effects[]` (only for `slot === "effects"`). */
  index?: number;
}

export function stackItems(clip: EffectClip): StackItem[] {
  const out: StackItem[] = [];
  if (clip.filter) out.push({ slot: "filter", instance: clip.filter });
  if (clip.type !== "adjustment" && clip.type !== "text" && clip.key) out.push({ slot: "key", instance: clip.key });
  if (clip.adjust) out.push({ slot: "adjust", instance: clip.adjust });
  (clip.effects ?? []).forEach((instance, index) =>
    out.push({ slot: "effects", instance, index }),
  );
  return out;
}

/** The change that puts a new instance of `ref` on the clip. A slot item replaces what the slot
 *  held; an `effects[]` item is appended (existing instances keep their ids and params). */
export function addEffectChange(
  clip: EffectClip,
  slot: StackSlot,
  spec: EffectSpec,
): EffectsChange {
  if (slot === "effects") return { effects: [...(clip.effects ?? []).map(effectSpec), spec] };
  return { [slot]: spec } as EffectsChange;
}

/** The change after editing `item` into `next` (or removing it when `next` is null). */
export function replaceInstanceChange(
  clip: EffectClip,
  item: StackItem,
  next: EffectInstance | null,
): EffectsChange {
  if (item.slot === "effects") {
    const list = (clip.effects ?? []).map((fx, i) =>
      i === item.index ? next : fx,
    );
    return { effects: list.filter((fx): fx is EffectInstance => fx !== null).map(effectSpec) };
  }
  return { [item.slot]: next ? effectSpec(next) : null } as EffectsChange;
}

/** Move an `effects[]` entry by `delta` places; null when it cannot move that way. */
export function moveEffectChange(
  clip: EffectClip,
  index: number,
  delta: -1 | 1,
): EffectsChange | null {
  const list = [...(clip.effects ?? [])];
  const to = index + delta;
  if (index < 0 || index >= list.length || to < 0 || to >= list.length) return null;
  const [moved] = list.splice(index, 1);
  list.splice(to, 0, moved);
  return { effects: list.map(effectSpec) };
}

// ---- hover / live preview ------------------------------------------------------------------

function previewOf(fx: EffectInstance): PreviewEffect {
  return { id: fx.id, ref: fx.ref, enabled: fx.enabled, params: fx.params };
}

/** The clip's whole stack in render order, as a preview override carries it. */
export function previewStack(clip: EffectClip): PreviewEffect[] {
  return effectStack(clip).map(previewOf);
}

/** Hover preview of adding `entry`: the current stack, then one temporary instance appended.
 *  (Slot placement is approximated by appending; an override takes the whole stack in order.) */
export function hoverStack(
  clip: EffectClip,
  entry: Pick<CatalogueEntry, "ref" | "params">,
  tempId: string,
): PreviewEffect[] {
  return [
    ...previewStack(clip),
    { id: tempId, ref: entry.ref, enabled: true, params: initialParams(entry) },
  ];
}

/** The stack with one param of one instance changed (a slider drag). */
export function stackWithParam(
  clip: EffectClip,
  instanceId: string,
  key: string,
  value: EffectParamValue,
): PreviewEffect[] {
  return previewStack(clip).map((fx) =>
    fx.id === instanceId ? { ...fx, params: { ...fx.params, [key]: value } } : fx,
  );
}

// ---- keyframe easing -------------------------------------------------------------------------

export type EasingMenuValue = "linear" | "hold" | "auto" | "ease_in" | "ease_out" | "ease_in_out" | "custom";

export const EASING_MENU: { value: Exclude<EasingMenuValue, "custom">; label: string }[] = [
  { value: "linear", label: "Linear" },
  { value: "hold", label: "Hold" },
  { value: "auto", label: "Auto" },
  { value: "ease_in", label: "Ease in" },
  { value: "ease_out", label: "Ease out" },
  { value: "ease_in_out", label: "Ease in-out" },
];

const PRESETS: Record<"ease_in" | "ease_out" | "ease_in_out", [number, number, number, number]> = {
  ease_in: [0.42, 0, 1, 1],
  ease_out: [0, 0, 0.58, 1],
  ease_in_out: [0.42, 0, 0.58, 1],
};

function scalar(v: Keyframe["value"]): number | null {
  return typeof v === "number" ? v : null;
}

/** Which menu entry describes the span after `key`. A `bezier` key reads back as its preset when
 *  its handles are exactly that preset's CSS points (core writes presets as handles), else
 *  "custom". The legacy v7 `ease_in_out` curve shows as Ease in-out. */
export function easingMenuValue(key: Keyframe, next: Keyframe | undefined): EasingMenuValue {
  const easing = key.easing ?? "linear";
  if (easing === "linear" || easing === "hold" || easing === "auto") return easing;
  if (easing === "ease_in_out") return "ease_in_out";
  // bezier: recover the normalised control points from the handles
  if (!next || !key.handle_right || !next.handle_left) return "custom";
  const span = next.time_us - key.time_us;
  if (span <= 0) return "custom";
  let scale = 1;
  const a = scalar(key.value);
  const b = scalar(next.value);
  if (a !== null && b !== null) {
    scale = b - a;
    if (Math.abs(scale) < 1e-9) return "custom";
  }
  const pts = [
    key.handle_right[0] / span,
    key.handle_right[1] / scale,
    1 + next.handle_left[0] / span,
    1 + next.handle_left[1] / scale,
  ];
  for (const [name, p] of Object.entries(PRESETS)) {
    if (p.every((v, i) => Math.abs(v - pts[i]) < 0.01)) return name as EasingMenuValue;
  }
  return "custom";
}

/** The `SetKeyframeEasing` command for picking `pick` on the span after key `index`; null when
 *  there is no such span (a missing key, or the last key: easing describes the span after one). */
export function easingPickCommand(
  trackId: string,
  clipId: string,
  property: KeyframeProperty,
  keys: readonly Keyframe[],
  index: number,
  pick: Exclude<EasingMenuValue, "custom">,
): Record<string, unknown> | null {
  const key = keys[index];
  if (!key || index + 1 >= keys.length) return null;
  const choice: EasingChoice = pick;
  return setKeyframeEasing(trackId, clipId, property, key.time_us, choice);
}

// ---- transition easing -------------------------------------------------------------------------

export type TransitionEasingMenu = "default" | "linear" | "ease_in" | "ease_out" | "ease_in_out" | "custom";

export const TRANSITION_EASING_MENU: { value: Exclude<TransitionEasingMenu, "custom">; label: string }[] = [
  { value: "default", label: "Default" },
  { value: "linear", label: "Linear" },
  { value: "ease_in", label: "Ease in" },
  { value: "ease_out", label: "Ease out" },
  { value: "ease_in_out", label: "Ease in-out" },
];

export function transitionEasingMenuValue(easing: ProgressEasing | null | undefined): TransitionEasingMenu {
  if (easing == null) return "default";
  if (easing === "linear") return "linear";
  const pts = easing.bezier;
  for (const [name, p] of Object.entries(PRESETS)) {
    if (p.every((v, i) => Math.abs(v - pts[i]) < 0.01)) return name as TransitionEasingMenu;
  }
  return "custom";
}

/** The `easing` field for a menu pick: `null` = the transition's declared default; the presets
 *  are sent by name and core stores them as Bezier points. */
export function transitionEasingFromMenu(
  value: Exclude<TransitionEasingMenu, "custom">,
): TransitionEasingArg | null {
  return value === "default" ? null : value;
}

// ---- keyframes on effect parameters ------------------------------------------------------------


/** The tracks with a key of `value` at clip-local `timeUs` on `property`: an existing key at that
 *  time keeps its easing and takes the new value, otherwise a linear key is inserted. */
export function withKeyAt(
  tracks: readonly KeyframeTrack[] | undefined,
  property: KeyframeProperty,
  timeUs: number,
  value: KeyValue,
): KeyframeTrack[] {
  const list = [...(tracks ?? [])];
  const at = list.findIndex((t) => t.property === property);
  const keys = at >= 0 ? [...list[at].keys] : [];
  const existing = keys.findIndex((k) => k.time_us === timeUs);
  if (existing >= 0) keys[existing] = { ...keys[existing], value };
  else keys.push({ time_us: timeUs, value, easing: "linear" });
  keys.sort((a, b) => a.time_us - b.time_us);
  const next: KeyframeTrack = { property, keys };
  if (at >= 0) list[at] = next;
  else list.push(next);
  return list;
}

/** True when the clip has a non-empty keyframe track for this effect parameter. */
export function hasParamTrack(
  clip: Pick<EffectClip, "keyframes">,
  instanceId: string,
  param: string,
): boolean {
  const property = `effect:${instanceId}/${param}`;
  return (clip.keyframes ?? []).some((t) => t.property === property && t.keys.length > 0);
}
