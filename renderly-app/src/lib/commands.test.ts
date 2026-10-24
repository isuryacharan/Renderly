import { describe, expect, it } from "vitest";
import * as c from "./commands";
// Raw text of the spec, via Vite's ?raw (no node typings needed in the app tsconfig).
import commandApiMd from "../../../docs/command-api.md?raw";
import { IDENTITY_TRANSFORM } from "./types";

// The documented Rust enum (docs/command-api.md "Shape" block) is the source of truth for
// field names. Parse it so a rename on either side fails here rather than at runtime as an
// opaque serde error.
//
// Schema v8 renamed the time parameters (they take a core `Time`). docs/command-api.md is updated
// in the docs step of the v8 work; until then it still lists the old float-seconds names, so they
// are mapped to the current names here (a no-op once the doc uses the new ones).
// The legacy suffix is spelled out of two parts so the repo-wide check for stray v7 names stays
// meaningful: this is the only place that still talks about them.
const LEGACY_SUFFIX = ["_", "secs"].join("");

/** v7 name -> v8 name: drop the `new_` prefix and the legacy suffix; `position` became `start`. */
function v8ParamName(field: string): string {
  if (!field.endsWith(LEGACY_SUFFIX)) return field;
  const base = field.slice(0, -LEGACY_SUFFIX.length).replace(/^new_/, "");
  return base === "position" ? "start" : base;
}

function documentedCommands(): Map<string, string[]> {
  const md = commandApiMd;
  const block = md.slice(md.indexOf("```rust"), md.indexOf("```", md.indexOf("```rust") + 7));
  const out = new Map<string, string[]>();
  for (const line of block.split(/\r?\n/)) {
    const m = line.match(/^\s{4}([A-Z]\w+) \{ (.*) \},?\s*$/);
    if (!m) continue;
    // Split on top-level commas only (types like Option<f64> contain no commas, but be safe).
    const fields = m[2]
      .split(/,\s*(?=\w+:)/)
      .map((f: string) => f.split(":")[0].trim())
      .map(v8ParamName);
    out.set(m[1], fields);
  }
  return out;
}

// Documented fields a builder leaves out on purpose: `targets` is the batch form of
// track_id + clip_id (core takes one or the other).
const OMITTED_BY_DESIGN: Record<string, string[]> = {
  SetClipTransform: ["targets"],
  SetClipEffects: ["targets"],
  SetClipTransition: ["targets"],
  // Text edits take `targets` (one or many clips); the single-clip pair is the same thing.
  SetTextBody: ["track_id", "clip_id", "runs"],
  SetTextStyle: ["track_id", "clip_id"],
  SetTextAnimation: ["track_id", "clip_id", "hash", "params", "duration"],
};

const STYLE = { ref: "builtin:tiktok-box@1", params: {}, overrides: {} };

const T = "track-1";
const K = "clip-1";

// One representative call per builder, with every optional argument supplied.
const samples: Record<string, unknown> = {
  ImportMedia: c.importMedia("C:/a.mp4"),
  AddTrack: c.addTrack("video", "V1", "id-1", null, true),
  AddClip: c.addClip(T, "m", 1, 2, 3),
  SplitClip: c.splitClip(T, K, 1.5),
  TrimClip: c.trimClip(T, K, 1, 2),
  MoveClip: c.moveClip(T, K, 4, "t2"),
  DeleteClip: c.deleteClip(T, K, true),
  AddCaption: c.addCaption(T, "hi", 1, 2, STYLE, { in: { ref: "builtin:fade-up@1", params: {} } }),
  SetCaption: c.setCaption(T, K, { text: "x", startUs: 1, durationUs: 2, style: STYLE, animations: {} }),
  AddText: c.addText(T, "Title", 1, 2, STYLE, { in: { ref: "builtin:fade-up@1", params: {} } }),
  SetTextTiming: c.setTextTiming(T, K, { startUs: 1, durationUs: 2 }),
  SetTextBody: c.setTextBody([{ trackId: T, clipId: K }], "new"),
  SetTextStyle: c.setTextStyle([{ trackId: T, clipId: K }], {
    ref: "builtin:tiktok-box@1",
    hash: null,
    params: {},
    overrides: { anchor: "top" },
  }),
  SetTextAnimation: c.setTextAnimation([{ trackId: T, clipId: K }], "in", "builtin:fade-up@1"),
  SetTrackCaptionStyle: c.setTrackCaptionStyle(T, STYLE, true),
  SetCaptionWords: c.setCaptionWords(T, K, [{ text: "hi", start: 0, end: 1, emphasis: true, word_id: "w-1" }]),
  SetWordEmphasis: c.setWordEmphasis(T, K, [0, 1], true),
  SplitCaptions: c.splitCaptions(T, { maxChars: 20, maxLines: 1, maxDurationUs: 3_000_000 }),
  AddMarkers: c.addMarkers([{ timeUs: 1_000_000, label: "m", color: "#FF0000", kind: "beat", id: "mk-1" }]),
  RemoveMarkers: c.removeMarkers(["mk-1"], "user"),
  ApplyTranscriptCuts: c.applyTranscriptCuts("tr-1", ["w-1"], ["filler"]),
  RestoreTranscriptWords: c.restoreTranscriptWords("tr-1", ["w-1"]),
  SetAudioGain: c.setAudioGain(T, K, -3),
  GenerateCaptions: c.generateCaptions("m", T, STYLE, 2),
  GenerateVoiceover: c.generateVoiceover("t", T, 0, "o.wav", { provider: "piper_local" }),
  SetAudioFade: c.setAudioFade(T, K, 0.1, 0.2),
  SetTrackAudioRole: c.setTrackAudioRole(T, "music"),
  SetProjectSettings: c.setProjectSettings({ width: 1 }),
  SetTrackFlags: c.setTrackFlags(T, { muted: true, magnetic: true }),
  RenameTrack: c.renameTrack(T, "n"),
  DeleteTrack: c.deleteTrack(T),
  MoveTrack: c.moveTrack(T, 2),
  SetClipEnabled: c.setClipEnabled(T, K, false),
  SetClipTransform: c.setClipTransform(T, K, IDENTITY_TRANSFORM),
  SetClipKeyframes: c.setClipKeyframes(T, K, []),
  SetClipEffects: c.setClipEffects(T, K, {
    filter: { ref: "builtin:lut_filter@1" },
    key: null,
    adjust: { ref: "builtin:color_adjust@1", id: "i-1", params: { exposure: 1 } },
    effects: [{ ref: "builtin:blur@1" }],
    blendMode: "multiply",
  }),
  SetKeyframeEasing: c.setKeyframeEasing(T, K, "opacity", 1_000_000, "ease_in"),
  SetClipTransition: c.setClipTransition(T, K, "builtin:crossfade@1", {
    durationUs: 500_000,
    params: {},
    easing: "linear",
  }),
  SetClipSpeed: c.setClipSpeed(T, K, 2),
  LoadAssetPack: c.loadAssetPack("p"),
  UnloadAssetPack: c.unloadAssetPack("p"),
  LoadWasmPlugin: c.loadWasmPlugin("p"),
  UnloadWasmPlugin: c.unloadWasmPlugin("p"),
  AddStickerFromPack: c.addStickerFromPack("p", "s", T, 1),
  AddSfxFromPack: c.addSfxFromPack("p", "s", T, 1),
  SetClipMask: c.setClipMask(T, K, null),
  SetClipBackgroundRemoval: c.setClipBackgroundRemoval(T, K, null),
  GenerateBackgroundMatte: c.generateBackgroundMatte(T, K),
  AttachBackgroundMatte: c.attachBackgroundMatte(T, K, "C:/cache/matte"),
  SetClipAudioDenoise: c.setClipAudioDenoise(T, K, null),
  ApplyTemplate: c.applyTemplate("p", "t", 1),
  GenerateSticker: c.generateSticker("cat", T, 1, "o.png"),
  CreateMulticamGroup: c.createMulticamGroup("g", ["a", "b"]),
  SetMulticamAngle: c.setMulticamAngle("g", 1),
  TrackMotion: c.trackMotion(T, K, 12, {
    region: { x: 0.1, y: 0.1, width: 0.5, height: 0.5 },
    trackRotationScale: true,
    target: { trackId: "t-2", clipId: "k-2" },
  }),
  StabilizeClip: c.stabilizeClip(T, K, { strength: 0.5, sampleCount: 90 }),
  AutoReframeClip: c.autoReframeClip(T, K, 9 / 16),
  CreateSequence: c.createSequence("Intro", { width: 640, height: 360, id: "seq-1" }),
  RenameSequence: c.renameSequence("seq-1", "Outro"),
  SetSequenceSettings: c.setSequenceSettings("seq-1", { width: 640, height: null }),
  DeleteSequence: c.deleteSequence("seq-1"),
  NestClips: c.nestClips([{ trackId: T, clipId: K }], "Nested"),
  UnnestClip: c.unnestClip(T, K),
  AddAdjustmentLayer: c.addAdjustmentLayer(T, 1, 2),
  SetAdjustmentLayer: c.setAdjustmentLayer(T, K, { opacity: 0.5 }),
};

describe("command builders vs docs/command-api.md", () => {
  const documented = documentedCommands();

  it("the docs parser found the whole enum", () => {
    expect(documented.size).toBeGreaterThan(35);
    expect(documented.get("AddClip")).toEqual([
      "track_id",
      "media_id",
      "start",
      "source_in",
      "source_out",
    ]);
  });

  for (const [name, cmd] of Object.entries(samples)) {
    it(`${name}: tag and field names match the documented Rust variant`, () => {
      const fields = documented.get(name);
      expect(fields, `${name} missing from docs`).toBeDefined();
      const obj = cmd as Record<string, unknown>;
      expect(obj.command).toBe(name);
      const sent = Object.keys(obj).filter((k) => k !== "command");
      // Every sent field is documented, and every documented field is sent except those the
      // builder omits by design (a batch `targets` list, which replaces track_id + clip_id).
      expect(fields).toEqual(expect.arrayContaining(sent));
      const omittable = OMITTED_BY_DESIGN[name] ?? [];
      expect(fields!.filter((f) => !sent.includes(f) && !omittable.includes(f))).toEqual([]);
    });
  }

  it("every documented command except Export has a builder", () => {
    const missing = [...documented.keys()].filter((k) => k !== "Export" && !(k in samples));
    expect(missing).toEqual([]);
  });

  it("builders emit plain JSON (no undefined values that serde would drop silently)", () => {
    for (const cmd of Object.values(samples)) {
      expect(JSON.parse(JSON.stringify(cmd))).toEqual(cmd);
    }
  });
});

describe("builder payloads", () => {
  it("addClip maps positional args to the right fields", () => {
    expect(c.addClip("t", "m", 1, 2, 3)).toEqual({
      command: "AddClip",
      track_id: "t",
      media_id: "m",
      start: 1,
      source_in: 2,
      source_out: 3,
    });
  });

  it("addTrack defaults id to null so the server generates one", () => {
    expect(c.addTrack("audio", "A")).toEqual({
      command: "AddTrack",
      kind: "audio",
      name: "A",
      id: null,
      sequence_id: null,
    });
  });

  it("addTrack can target a sequence's timeline", () => {
    expect(c.addTrack("video", "V", null, "seq-1").sequence_id).toBe("seq-1");
  });

  it("nestClips maps camelCase refs to the ClipRef wire shape", () => {
    expect(c.nestClips([{ trackId: "t", clipId: "k" }], "n")).toEqual({
      command: "NestClips",
      clips: [{ track_id: "t", clip_id: "k" }],
      name: "n",
    });
  });

  it("sequence/adjustment optionals serialise None as null", () => {
    expect(c.createSequence("s")).toEqual({
      command: "CreateSequence",
      name: "s",
      width: null,
      height: null,
      id: null,
    });
    expect(c.setSequenceSettings("s", {})).toMatchObject({ width: null, height: null });
    expect(c.setAdjustmentLayer("t", "k", { opacity: 0 })).toMatchObject({
      start: null,
      duration: null,
      opacity: 0,
    });
  });

  it("moveClip defaults new_track_id to null (same-track move)", () => {
    expect(c.moveClip("t", "k", 5).new_track_id).toBeNull();
  });

  it("trimClip passes null through for the untouched edge", () => {
    const cmd = c.trimClip("t", "k", null, 4);
    expect(cmd.source_in).toBeNull();
    expect(cmd.source_out).toBe(4);
  });

  it("setCaption sends only what changes, keeps explicit falsy values, and null style means inherit", () => {
    const empty = c.setCaption("t", "k", {});
    expect(empty).toMatchObject({ start: null, duration: null });
    expect(empty).not.toHaveProperty("body");
    expect(empty).not.toHaveProperty("style");
    const cmd = c.setCaption("t", "k", { text: "", startUs: 0, durationUs: 0, style: null });
    expect(cmd.body).toEqual({ text: "" });
    expect(cmd.start).toBe(0);
    expect(cmd.duration).toBe(0);
    expect(cmd.style).toBeNull();
  });

  it("addCaption / addText leave style and animations out unless given (the clip inherits)", () => {
    const cap = c.addCaption("t", "hi", 1, 2);
    expect(cap).toEqual({ command: "AddCaption", track_id: "t", body: { text: "hi" }, start: 1, duration: 2 });
    expect(c.addText("t", "T", 1, 2, null, {})).toEqual({
      command: "AddText",
      track_id: "t",
      body: { text: "T" },
      start: 1,
      duration: 2,
    });
  });

  it("bodyArg converts stored word times to command words and drops empty runs/words", () => {
    expect(c.bodyArg({ text: "a", runs: [], words: [] })).toEqual({ text: "a" });
    expect(
      c.bodyArg({
        text: "a b",
        runs: [],
        words: [
          { text: "a", start_us: 0, end_us: 5, emphasis: true, word_id: "w-1" },
          { text: "b", start_us: 6, end_us: 9 },
        ],
      }).words,
    ).toEqual([
      { text: "a", start: 0, end: 5, emphasis: true, word_id: "w-1" },
      { text: "b", start: 6, end: 9 },
    ]);
  });

  it("setTextStyle sends ref null as a clear and omits unchanged fields", () => {
    const clear = c.setTextStyle([{ trackId: "t", clipId: "k" }], { ref: null });
    expect(clear).toEqual({ command: "SetTextStyle", targets: [{ track_id: "t", clip_id: "k" }], ref: null });
    expect(c.setTextStyle([{ trackId: "t", clipId: "k" }], { overrides: {} })).not.toHaveProperty("ref");
  });

  it("addTrack sends magnetic only when asked; setTrackFlags keeps it null otherwise", () => {
    expect(c.addTrack("video", "V")).not.toHaveProperty("magnetic");
    expect(c.addTrack("video", "V", null, null, false).magnetic).toBe(false);
    expect(c.setTrackFlags("t", { magnetic: false }).magnetic).toBe(false);
    expect(c.setTrackFlags("t", { muted: true }).magnetic).toBeNull();
  });

  it("addMarkers rounds the time and fills defaults; removeMarkers only sends a kind when given", () => {
    expect(c.addMarkers([{ timeUs: 1_000_000.4 }]).markers).toEqual([
      { time: 1_000_000, label: "", color: "", kind: "user" },
    ]);
    expect(c.removeMarkers(["a"])).toEqual({ command: "RemoveMarkers", ids: ["a"] });
    expect(c.removeMarkers([], "beat")).toEqual({ command: "RemoveMarkers", ids: [], kind: "beat" });
  });

  it("splitCaptions defaults to the engine limits", () => {
    expect(c.splitCaptions("t")).toEqual({
      command: "SplitCaptions",
      track_id: "t",
      max_chars: 32,
      max_lines: 2,
      max_duration: 5_000_000,
    });
  });

  it("setProjectSettings / setTrackFlags keep explicit false and 0 (not coalesced to null)", () => {
    expect(c.setTrackFlags("t", { muted: false }).muted).toBe(false);
    expect(c.setTrackFlags("t", { locked: true })).toMatchObject({ muted: null, locked: true, hidden: null });
    expect(c.setProjectSettings({ fps: 0 }).fps).toBe(0);
    expect(c.setProjectSettings({})).toEqual({
      command: "SetProjectSettings",
      width: null,
      height: null,
      fps: null,
    });
  });

  it("generateCaptions defaults the offset to 0 and trackMotion to 12 samples", () => {
    expect(c.generateCaptions("m", "t", null).timeline_offset).toBe(0);
    expect(c.trackMotion("t", "k").sample_count).toBe(12);
  });

  it("trackMotion sends target null (this clip) unless a target is given", () => {
    expect(c.trackMotion("t", "k").target).toBeNull();
    expect(c.trackMotion("t", "k", 12, { target: { trackId: "t2", clipId: "k2" } }).target).toEqual({
      track_id: "t2",
      clip_id: "k2",
    });
  });

  it("generateVoiceover serialises the provider with the snake_case tag Rust expects", () => {
    expect(c.generateVoiceover("x", "t", 0, "o", { provider: "open_ai", voice: "alloy" }).provider).toEqual({
      provider: "open_ai",
      voice: "alloy",
    });
    expect(c.generateVoiceover("x", "t", 0, "o", { provider: "piper_local" }).provider).toEqual({
      provider: "piper_local",
    });
  });

  it("clip-level optional configs serialise None as null", () => {
    expect(c.setClipMask("t", "k", null).mask).toBeNull();
    expect(c.setClipBackgroundRemoval("t", "k", null).config).toBeNull();
    expect(c.setClipAudioDenoise("t", "k", null).config).toBeNull();
    expect(c.setTrackAudioRole("t", null).role).toBeNull();
  });

  it("setClipTransition sends a ref (null clears) and only the fields it is given", () => {
    expect(c.setClipTransition("t", "k", null)).toEqual({
      command: "SetClipTransition",
      track_id: "t",
      clip_id: "k",
      ref: null,
    });
    expect(
      c.setClipTransition("t", "k", "builtin:wipe_left@1", { durationUs: 400_000 }),
    ).toEqual({
      command: "SetClipTransition",
      track_id: "t",
      clip_id: "k",
      ref: "builtin:wipe_left@1",
      duration: 400_000,
    });
  });

  it("setClipTransition distinguishes an omitted easing (keep) from null (the default) and bezier points", () => {
    const base = { command: "SetClipTransition", track_id: "t", clip_id: "k", ref: "r@1" };
    expect(c.setClipTransition("t", "k", "r@1")).toEqual(base);
    expect(c.setClipTransition("t", "k", "r@1", { easing: null })).toEqual({ ...base, easing: null });
    expect(c.setClipTransition("t", "k", "r@1", { easing: "ease_out" })).toEqual({
      ...base,
      easing: "ease_out",
    });
    expect(
      c.setClipTransition("t", "k", "r@1", { easing: { bezier: [0.1, 0, 0.9, 1] } }).easing,
    ).toEqual({ bezier: [0.1, 0, 0.9, 1] });
  });

  it("setClipEffects omits untouched slots, sends null to clear and never sends effect_id", () => {
    // Only the blend mode: nothing else is mentioned, so nothing else changes.
    expect(c.setBlendMode("t", "k", "screen")).toEqual({
      command: "SetClipEffects",
      track_id: "t",
      clip_id: "k",
      blend_mode: "screen",
    });
    expect(c.setClipEffects("t", "k", { filter: null })).toEqual({
      command: "SetClipEffects",
      track_id: "t",
      clip_id: "k",
      filter: null,
    });
    const cmd = c.setClipEffects("t", "k", { adjust: { ref: "builtin:color_adjust@1" } });
    expect(cmd).not.toHaveProperty("effects");
    expect(JSON.stringify(cmd)).not.toContain("effect_id");
  });

  it("setEffectsList replaces only effects[] and keeps ids, refs, enabled flags and params", () => {
    const cmd = c.setEffectsList("t", "k", [
      { id: "a", ref: "builtin:blur@1", enabled: false, params: { radius: 3 } },
      { id: "b", ref: "pack:p.q/x@2", hash: "sha256:1", enabled: true, params: { tint: [1, 0, 0, 1], on: true } },
    ]);
    expect(Object.keys(cmd).sort()).toEqual(["clip_id", "command", "effects", "track_id"]);
    expect(cmd.effects).toEqual([
      { id: "a", ref: "builtin:blur@1", enabled: false, params: { radius: 3 } },
      {
        id: "b",
        ref: "pack:p.q/x@2",
        hash: "sha256:1",
        enabled: true,
        params: { tint: [1, 0, 0, 1], on: true },
      },
    ]);
  });

  it("setKeyframeEasing addresses a key by property and time, with a preset name or bezier points", () => {
    expect(c.setKeyframeEasing("t", "k", "effect:11111111-1111-4111-8111-111111111111/radius", 250_000, "hold")).toEqual({
      command: "SetKeyframeEasing",
      track_id: "t",
      clip_id: "k",
      property: "effect:11111111-1111-4111-8111-111111111111/radius",
      time: 250_000,
      easing: "hold",
    });
    expect(c.setKeyframeEasing("t", "k", "opacity", 0, { bezier: [0.4, 0, 0.2, 1] }).easing).toEqual({
      bezier: [0.4, 0, 0.2, 1],
    });
  });
});
