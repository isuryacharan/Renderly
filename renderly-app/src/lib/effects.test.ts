import { describe, expect, it } from "vitest";
import {
  addEffectChange,
  easingMenuValue,
  easingPickCommand,
  findEntry,
  hasParamTrack,
  hoverStack,
  initialParams,
  isKeyframable,
  moveEffectChange,
  paramValue,
  previewStack,
  refBase,
  replaceInstanceChange,
  stackItems,
  stackWithParam,
  targetSlot,
  transitionEasingFromMenu,
  transitionEasingMenuValue,
  withKeyAt,
} from "./effects";
import { setClipEffects } from "./commands";
import type { CatalogueEntry } from "./ipc";
import type { AdjustmentClip, EffectInstance, Keyframe, MediaClip } from "./types";
import { effectStack, effectTrackProperty, parseEffectProperty } from "./types";
import { videoClip } from "../test/fixtures";

const fx = (id: string, ref: string, params: EffectInstance["params"] = {}): EffectInstance => ({
  id,
  ref,
  enabled: true,
  params,
});

const entry = (
  ref: string,
  slot: CatalogueEntry["slot"],
  params: CatalogueEntry["params"] = [],
  kind: CatalogueEntry["kind"] = "effect",
): CatalogueEntry => ({ ref, kind, label: ref, source: "builtin", state: "ok", slot, params });

const blur = entry("builtin:blur@1", "effects", [
  { key: "radius", type: "float", min: 0, max: 64, default: 0, initial: 8 },
]);

function stacked(): MediaClip {
  return videoClip("c", 0, 0, 4, {
    filter: fx("f1", "builtin:lut_filter@1"),
    key: fx("k1", "builtin:chroma_key@1"),
    adjust: fx("a1", "builtin:color_adjust@1", { exposure: 1 }),
    effects: [fx("e1", "builtin:blur@1", { radius: 3 }), fx("e2", "builtin:glitch@1")],
  });
}

describe("catalogue lookup", () => {
  const cat = [blur, entry("builtin:blur@2", "effects")];
  it("matches the exact ref, else the same item at another version", () => {
    expect(findEntry(cat, "builtin:blur@2")?.ref).toBe("builtin:blur@2");
    expect(findEntry(cat, "builtin:blur@1")?.ref).toBe("builtin:blur@1");
    expect(findEntry([blur], "builtin:blur@7")?.ref).toBe("builtin:blur@1");
    expect(findEntry(cat, "pack:x.y/z@1")).toBeUndefined();
    expect(refBase("pack:a.b/c@12")).toBe("pack:a.b/c");
  });
});

describe("new instances use the engine's `initial` values", () => {
  it("initialParams copies each param's initial (blur starts at 8, not a UI default)", () => {
    expect(initialParams(blur)).toEqual({ radius: 8 });
  });

  it("vector initials are copied, not shared", () => {
    const e = entry("pack:p/v@1", "effects", [
      { key: "tint", type: "color", default: [0, 0, 0, 1], initial: [1, 0, 0, 1] },
    ]);
    const a = initialParams(e);
    (a.tint as number[])[0] = 0.5;
    expect(initialParams(e).tint).toEqual([1, 0, 0, 1]);
  });

  it("an instance missing a stored key shows the engine's `default` for it", () => {
    const spec = blur.params[0];
    expect(paramValue(fx("i", "builtin:blur@1", {}), spec)).toBe(0);
    expect(paramValue(fx("i", "builtin:blur@1", { radius: 5 }), spec)).toBe(5);
  });

  it("only numeric, colour and vec2 params that the engine allows can be keyframed", () => {
    expect(isKeyframable({ type: "float" })).toBe(true);
    expect(isKeyframable({ type: "int" })).toBe(true);
    expect(isKeyframable({ type: "color" })).toBe(true);
    expect(isKeyframable({ type: "vec2" })).toBe(true);
    expect(isKeyframable({ type: "bool" })).toBe(false);
    expect(isKeyframable({ type: "choice" })).toBe(false);
    expect(isKeyframable({ type: "lut" })).toBe(false);
    expect(isKeyframable({ type: "float", animatable: false })).toBe(false);
  });
});

describe("slots", () => {
  it("lists the stack in render order: filter, key, adjust, then effects[]", () => {
    const clip = stacked();
    expect(stackItems(clip).map((i) => `${i.slot}:${i.instance.id}`)).toEqual([
      "filter:f1",
      "key:k1",
      "adjust:a1",
      "effects:e1",
      "effects:e2",
    ]);
    expect(effectStack(clip).map((e) => e.id)).toEqual(["f1", "k1", "a1", "e1", "e2"]);
  });

  it("an adjustment layer has no key slot", () => {
    const adj: AdjustmentClip = {
      type: "adjustment",
      id: "adj",
      start_us: 0,
      duration_us: 1,
      enabled: true,
      filter: fx("f", "builtin:lut_filter@1"),
      effects: [fx("e", "builtin:blur@1")],
    };
    expect(stackItems(adj).map((i) => i.slot)).toEqual(["filter", "effects"]);
    expect(targetSlot(entry("builtin:chroma_key@1", "key"), adj)).toBe("effects");
    expect(targetSlot(entry("builtin:chroma_key@1", "key"), stacked())).toBe("key");
  });

  it("a catalogue slot is where an item goes by default", () => {
    const clip = stacked();
    expect(targetSlot(entry("a", "filter"), clip)).toBe("filter");
    expect(targetSlot(entry("a", "adjust"), clip)).toBe("adjust");
    expect(targetSlot(entry("a", "effects"), clip)).toBe("effects");
  });

  it("adding to a slot sends only that slot (it replaces the occupant); nothing else is touched", () => {
    const clip = stacked();
    const change = addEffectChange(clip, "filter", { ref: "builtin:lut_filter@1" });
    expect(change).toEqual({ filter: { ref: "builtin:lut_filter@1" } });
    const cmd = setClipEffects("t", "c", change);
    expect(cmd).toEqual({
      command: "SetClipEffects",
      track_id: "t",
      clip_id: "c",
      filter: { ref: "builtin:lut_filter@1" },
    });
    expect(cmd).not.toHaveProperty("effects");
  });

  it("adding to effects[] appends and keeps existing instances by id with their params", () => {
    const clip = stacked();
    const change = addEffectChange(clip, "effects", { ref: "builtin:blur@1" });
    expect(change.effects).toEqual([
      { id: "e1", ref: "builtin:blur@1", enabled: true, params: { radius: 3 } },
      { id: "e2", ref: "builtin:glitch@1", enabled: true, params: {} },
      { ref: "builtin:blur@1" },
    ]);
    // A new spec carries no params: the engine writes the effect's initial values.
    expect(change.effects?.[2]).not.toHaveProperty("params");
  });

  it("removing a slot item clears the slot (null); removing from effects[] drops just that entry", () => {
    const clip = stacked();
    const items = stackItems(clip);
    expect(replaceInstanceChange(clip, items[0], null)).toEqual({ filter: null });
    expect(replaceInstanceChange(clip, items[2], null)).toEqual({ adjust: null });
    const dropped = replaceInstanceChange(clip, items[3], null);
    expect(dropped.effects?.map((e) => e.id)).toEqual(["e2"]);
    expect(dropped).not.toHaveProperty("filter");
  });

  it("editing an instance keeps its id, so its keyframes survive", () => {
    const clip = stacked();
    const item = stackItems(clip)[3];
    const edited = replaceInstanceChange(clip, item, {
      ...item.instance,
      params: { radius: 9 },
    });
    expect(edited.effects?.[0]).toEqual({
      id: "e1",
      ref: "builtin:blur@1",
      enabled: true,
      params: { radius: 9 },
    });
    const toggled = replaceInstanceChange(clip, stackItems(clip)[0], {
      ...clip.filter!,
      enabled: false,
    });
    expect(toggled.filter).toMatchObject({ id: "f1", enabled: false });
  });

  it("reordering moves within effects[] only and refuses to go past the ends", () => {
    const clip = stacked();
    expect(moveEffectChange(clip, 0, -1)).toBeNull();
    expect(moveEffectChange(clip, 1, 1)).toBeNull();
    expect(moveEffectChange(clip, 1, -1)?.effects?.map((e) => e.id)).toEqual(["e2", "e1"]);
    expect(moveEffectChange(clip, 0, 1)?.effects?.map((e) => e.id)).toEqual(["e2", "e1"]);
  });
});

describe("hover preview stack", () => {
  it("is the whole stack in render order as {id, ref, params}", () => {
    const stack = previewStack(stacked());
    expect(stack.map((e) => e.id)).toEqual(["f1", "k1", "a1", "e1", "e2"]);
    expect(stack[2]).toEqual({ id: "a1", ref: "builtin:color_adjust@1", enabled: true, params: { exposure: 1 } });
    expect(JSON.stringify(stack)).not.toContain("effect_id");
  });

  it("appends one temporary instance with the item's initial params", () => {
    const stack = hoverStack(stacked(), blur, "tmp");
    expect(stack).toHaveLength(6);
    expect(stack[5]).toEqual({ id: "tmp", ref: "builtin:blur@1", enabled: true, params: { radius: 8 } });
  });

  it("a slider drag changes one param of one instance in the stack", () => {
    const stack = stackWithParam(stacked(), "e1", "radius", 20);
    expect(stack.find((e) => e.id === "e1")?.params).toEqual({ radius: 20 });
    expect(stack.find((e) => e.id === "a1")?.params).toEqual({ exposure: 1 });
  });
});

describe("keyframes on effect params", () => {
  it("builds and parses effect:<id>/<param> properties", () => {
    const id = "11111111-1111-4111-8111-111111111111";
    const prop = effectTrackProperty(id, "radius");
    expect(prop).toBe(`effect:${id}/radius`);
    expect(parseEffectProperty(prop)).toEqual([id, "radius"]);
    expect(parseEffectProperty("opacity")).toBeNull();
    expect(parseEffectProperty("effect:nope")).toBeNull();
  });

  it("withKeyAt inserts a linear key, replaces the value of an existing one and keeps order", () => {
    const prop = effectTrackProperty("e1", "radius");
    let tracks = withKeyAt(undefined, prop, 1_000_000, 4);
    expect(tracks).toEqual([{ property: prop, keys: [{ time_us: 1_000_000, value: 4, easing: "linear" }] }]);
    tracks = withKeyAt(tracks, prop, 0, 1);
    expect(tracks[0].keys.map((k) => k.time_us)).toEqual([0, 1_000_000]);
    tracks = withKeyAt(
      [{ property: prop, keys: [{ time_us: 0, value: 1, easing: "hold" }] }],
      prop,
      0,
      9,
    );
    expect(tracks[0].keys).toEqual([{ time_us: 0, value: 9, easing: "hold" }]);
  });

  it("vector params take array keys (colour and vec2)", () => {
    const prop = effectTrackProperty("e1", "tint");
    const tracks = withKeyAt([], prop, 0, [1, 0, 0, 1]);
    expect(tracks[0].keys[0].value).toEqual([1, 0, 0, 1]);
  });

  it("other tracks are left alone", () => {
    const other = { property: "opacity" as const, keys: [{ time_us: 0, value: 1 }] };
    const tracks = withKeyAt([other], effectTrackProperty("e1", "radius"), 0, 2);
    expect(tracks).toHaveLength(2);
    expect(tracks[0]).toBe(other);
  });

  it("hasParamTrack sees only non-empty tracks for that instance and param", () => {
    const prop = effectTrackProperty("e1", "radius");
    expect(hasParamTrack({ keyframes: [{ property: prop, keys: [{ time_us: 0, value: 1 }] }] }, "e1", "radius")).toBe(true);
    expect(hasParamTrack({ keyframes: [{ property: prop, keys: [] }] }, "e1", "radius")).toBe(false);
    expect(hasParamTrack({ keyframes: [] }, "e1", "radius")).toBe(false);
    expect(hasParamTrack({}, "e1", "radius")).toBe(false);
  });
});

describe("keyframe easing menu", () => {
  const a = (extra: Partial<Keyframe> = {}): Keyframe => ({ time_us: 0, value: 0, easing: "linear", ...extra });
  const b = (extra: Partial<Keyframe> = {}): Keyframe => ({ time_us: 1_000_000, value: 10, easing: "linear", ...extra });

  it("reads the stored easing back", () => {
    expect(easingMenuValue(a({ easing: "linear" }), b())).toBe("linear");
    expect(easingMenuValue(a({ easing: "hold" }), b())).toBe("hold");
    expect(easingMenuValue(a({ easing: "auto" }), b())).toBe("auto");
    expect(easingMenuValue(a({ easing: "ease_in_out" }), b())).toBe("ease_in_out");
    expect(easingMenuValue(a({}), undefined)).toBe("linear");
  });

  it("recognises a preset core wrote as bezier handles, else custom", () => {
    // ease_out = (0, 0, 0.58, 1): handle_right = (0, 0); next.handle_left = ((0.58-1)*span, 0)
    const out = easingMenuValue(
      a({ easing: "bezier", handle_right: [0, 0] }),
      b({ handle_left: [-420_000, 0] }),
    );
    expect(out).toBe("ease_out");
    // ease_in = (0.42, 0, 1, 1) on a 10-unit change: handle_right = (420000, 0)
    expect(
      easingMenuValue(a({ easing: "bezier", handle_right: [420_000, 0] }), b({ handle_left: [0, 0] })),
    ).toBe("ease_in");
    expect(
      easingMenuValue(
        a({ easing: "bezier", handle_right: [420_000, 0] }),
        b({ handle_left: [-420_000, 0] }),
      ),
    ).toBe("ease_in_out");
    expect(
      easingMenuValue(
        a({ easing: "bezier", handle_right: [100_000, 5] }),
        b({ handle_left: [-100_000, -5] }),
      ),
    ).toBe("custom");
    expect(easingMenuValue(a({ easing: "bezier" }), b())).toBe("custom");
  });

  it("a pick becomes SetKeyframeEasing for the key at that time", () => {
    const keys = [a(), b({ time_us: 500_000 }), b({ time_us: 1_000_000 })];
    expect(easingPickCommand("t", "c", "opacity", keys, 1, "ease_in_out")).toEqual({
      command: "SetKeyframeEasing",
      track_id: "t",
      clip_id: "c",
      property: "opacity",
      time: 500_000,
      easing: "ease_in_out",
    });
    const prop = effectTrackProperty("e1", "radius");
    expect(easingPickCommand("t", "c", prop, keys, 0, "hold")).toMatchObject({
      property: prop,
      time: 0,
      easing: "hold",
    });
  });

  it("every menu entry maps to a name core accepts, and the last key has no span to ease", () => {
    const keys = [a(), b()];
    for (const pick of ["linear", "hold", "auto", "ease_in", "ease_out", "ease_in_out"] as const) {
      expect(easingPickCommand("t", "c", "opacity", keys, 0, pick)?.easing).toBe(pick);
    }
    expect(easingPickCommand("t", "c", "opacity", keys, 1, "linear")).toBeNull();
    expect(easingPickCommand("t", "c", "opacity", keys, 5, "linear")).toBeNull();
  });
});

describe("transition easing menu", () => {
  it("default is null, presets are sent by name, bezier points read back as their preset", () => {
    expect(transitionEasingMenuValue(null)).toBe("default");
    expect(transitionEasingMenuValue(undefined)).toBe("default");
    expect(transitionEasingMenuValue("linear")).toBe("linear");
    expect(transitionEasingMenuValue({ bezier: [0.42, 0, 0.58, 1] })).toBe("ease_in_out");
    expect(transitionEasingMenuValue({ bezier: [0, 0, 0.58, 1] })).toBe("ease_out");
    expect(transitionEasingMenuValue({ bezier: [0.1, 0.2, 0.3, 0.4] })).toBe("custom");
    expect(transitionEasingFromMenu("default")).toBeNull();
    expect(transitionEasingFromMenu("linear")).toBe("linear");
    expect(transitionEasingFromMenu("ease_in")).toBe("ease_in");
  });
});
