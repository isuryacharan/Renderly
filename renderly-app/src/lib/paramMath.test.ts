import { describe, expect, it } from "vitest";
import { colorToHex, formatParam, hexToColor, paramStep, prettyName } from "./paramMath";
import {
  DEFAULT_TRANSITION_US,
  MIN_TRANSITION_US,
  maxTransitionUs,
  transitionDurationFor,
  transitionEntries,
} from "./transitions";
import { track, videoClip } from "../test/fixtures";
import type { CatalogueEntry } from "./ipc";

describe("generic parameter rows", () => {
  it("steps follow the declared range", () => {
    expect(paramStep({ type: "int", min: 0, max: 10 })).toBe(1);
    expect(paramStep({ type: "choice", min: 0, max: 2 })).toBe(1);
    expect(paramStep({ type: "float", min: 0, max: 1 })).toBe(0.005);
    expect(paramStep({ type: "float", min: 0, max: 64 })).toBe(0.2);
    expect(paramStep({ type: "float", min: -5, max: 5 })).toBe(0.05);
    expect(paramStep({ type: "float" })).toBe(0.005);
    expect(paramStep({ type: "float", min: 1, max: 1 })).toBe(0.01);
  });

  it("values print at the step's precision", () => {
    expect(formatParam(8, 1)).toBe("8");
    expect(formatParam(0.5, 0.005)).toBe("0.500");
    expect(formatParam(3.14159, 0.5)).toBe("3.1");
  });

  it("colours round-trip through a hex picker, alpha kept apart", () => {
    expect(colorToHex([1, 0, 0.5, 0.2])).toBe("#ff0080");
    expect(colorToHex([2, -1, 0])).toBe("#ff0000");
    const c = hexToColor("#ff0080", 0.25);
    expect(c[0]).toBe(1);
    expect(c[1]).toBe(0);
    expect(c[2]).toBeCloseTo(128 / 255);
    expect(c[3]).toBe(0.25);
  });

  it("names read well", () => {
    expect(prettyName("soft_light")).toBe("Soft light");
    expect(prettyName("key-r")).toBe("Key r");
  });
});

describe("transition limits", () => {
  const a = videoClip("a", 0, 0, 4);
  const b = videoClip("b", 4, 0, 2);
  const lane = track("v", "video", [a, b]);

  it("the longest transition is half the shorter of the two clips", () => {
    expect(maxTransitionUs(lane, a)).toBe(1_000_000);
    expect(maxTransitionUs(lane, b)).toBe(1_000_000); // no next clip: half its own length
  });

  it("a new transition starts at the default, clamped to what the clips allow, never below the minimum", () => {
    expect(transitionDurationFor(lane, a)).toBe(DEFAULT_TRANSITION_US);
    expect(transitionDurationFor(lane, a, 5_000_000)).toBe(1_000_000);
    const tiny = track("v", "video", [videoClip("x", 0, 0, 0.01), videoClip("y", 0.01, 0, 1)]);
    expect(transitionDurationFor(tiny, tiny.clips[0] as typeof a)).toBe(MIN_TRANSITION_US);
  });

  it("re-applying keeps the clip's current duration", () => {
    const cur = {
      ...a,
      outgoing_transition: { ref: "builtin:crossfade@1", duration_us: 300_000, params: {}, easing: null },
    };
    expect(transitionDurationFor(track("v", "video", [cur, b]), cur)).toBe(300_000);
  });

  it("only usable transition items are offered", () => {
    const e = (ref: string, kind: string, state = "ok"): CatalogueEntry => ({
      ref,
      kind,
      label: ref,
      source: "builtin",
      state,
      slot: kind === "transition" ? "transition" : "effects",
      params: [],
    });
    expect(
      transitionEntries([
        e("builtin:crossfade@1", "transition"),
        e("builtin:blur@1", "effect"),
        e("pack:x/y@1", "transition", "error"),
      ]).map((x) => x.ref),
    ).toEqual(["builtin:crossfade@1"]);
  });
});
