import { describe, expect, it } from "vitest";
import * as c from "./commands";
// Raw text of the spec, via Vite's ?raw (no node typings needed in the app tsconfig).
import commandApiMd from "../../../docs/command-api.md?raw";
import { IDENTITY_TRANSFORM } from "./types";

// The documented Rust enum (docs/command-api.md "Shape" block) is the source of truth for
// field names. Parse it so a rename on either side fails here rather than at runtime as an
// opaque serde error.
function documentedCommands(): Map<string, string[]> {
  const md = commandApiMd;
  const block = md.slice(md.indexOf("```rust"), md.indexOf("```", md.indexOf("```rust") + 7));
  const out = new Map<string, string[]>();
  for (const line of block.split(/\r?\n/)) {
    const m = line.match(/^\s{4}([A-Z]\w+) \{ (.*) \},?\s*$/);
    if (!m) continue;
    // Split on top-level commas only (types like Option<f64> contain no commas, but be safe).
    const fields = m[2].split(/,\s*(?=\w+:)/).map((f: string) => f.split(":")[0].trim());
    out.set(m[1], fields);
  }
  return out;
}

const T = "track-1";
const K = "clip-1";

// One representative call per builder, with every optional argument supplied.
const samples: Record<string, unknown> = {
  ImportMedia: c.importMedia("C:/a.mp4"),
  AddTrack: c.addTrack("video", "V1", "id-1"),
  AddClip: c.addClip(T, "m", 1, 2, 3),
  SplitClip: c.splitClip(T, K, 1.5),
  TrimClip: c.trimClip(T, K, 1, 2),
  MoveClip: c.moveClip(T, K, 4, "t2"),
  DeleteClip: c.deleteClip(T, K, true),
  AddCaption: c.addCaption(T, "hi", 1, 2, "tiktok-box"),
  SetCaption: c.setCaption(T, K, { text: "x" }),
  SetAudioGain: c.setAudioGain(T, K, -3),
  GenerateCaptions: c.generateCaptions("m", T, "s", 2),
  GenerateVoiceover: c.generateVoiceover("t", T, 0, "o.wav", { provider: "piper_local" }),
  SetAudioFade: c.setAudioFade(T, K, 0.1, 0.2),
  SetTrackAudioRole: c.setTrackAudioRole(T, "music"),
  SetProjectSettings: c.setProjectSettings({ width: 1 }),
  SetTrackFlags: c.setTrackFlags(T, { muted: true }),
  RenameTrack: c.renameTrack(T, "n"),
  DeleteTrack: c.deleteTrack(T),
  MoveTrack: c.moveTrack(T, 2),
  SetClipEnabled: c.setClipEnabled(T, K, false),
  SetClipTransform: c.setClipTransform(T, K, IDENTITY_TRANSFORM),
  SetClipKeyframes: c.setClipKeyframes(T, K, []),
  SetClipEffects: c.setClipEffects(T, K, []),
  SetClipTransition: c.setClipTransition(T, K, null),
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
  SetClipAudioDenoise: c.setClipAudioDenoise(T, K, null),
  ApplyTemplate: c.applyTemplate("p", "t", 1),
  GenerateSticker: c.generateSticker("cat", T, 1, "o.png"),
  CreateMulticamGroup: c.createMulticamGroup("g", ["a", "b"]),
  SetMulticamAngle: c.setMulticamAngle("g", 1),
  TrackMotion: c.trackMotion(T, K, 12, { region: { x: 0.1, y: 0.1, width: 0.5, height: 0.5 }, trackRotationScale: true }),
  StabilizeClip: c.stabilizeClip(T, K, { strength: 0.5, sampleCount: 90 }),
  AutoReframeClip: c.autoReframeClip(T, K, 9 / 16),
};

describe("command builders vs docs/command-api.md", () => {
  const documented = documentedCommands();

  it("the docs parser found the whole enum", () => {
    expect(documented.size).toBeGreaterThan(35);
    expect(documented.get("AddClip")).toEqual([
      "track_id",
      "media_id",
      "position_secs",
      "source_in_secs",
      "source_out_secs",
    ]);
  });

  for (const [name, cmd] of Object.entries(samples)) {
    it(`${name}: tag and field names match the documented Rust variant`, () => {
      const fields = documented.get(name);
      expect(fields, `${name} missing from docs`).toBeDefined();
      const obj = cmd as Record<string, unknown>;
      expect(obj.command).toBe(name);
      expect(Object.keys(obj).filter((k) => k !== "command").sort()).toEqual([...fields!].sort());
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
      position_secs: 1,
      source_in_secs: 2,
      source_out_secs: 3,
    });
  });

  it("addTrack defaults id to null so the server generates one", () => {
    expect(c.addTrack("audio", "A")).toEqual({ command: "AddTrack", kind: "audio", name: "A", id: null });
  });

  it("moveClip defaults new_track_id to null (same-track move)", () => {
    expect(c.moveClip("t", "k", 5).new_track_id).toBeNull();
  });

  it("trimClip passes null through for the untouched edge", () => {
    const cmd = c.trimClip("t", "k", null, 4);
    expect(cmd.new_source_in_secs).toBeNull();
    expect(cmd.new_source_out_secs).toBe(4);
  });

  it("setCaption turns omitted fields into null but preserves explicit falsy values", () => {
    expect(c.setCaption("t", "k", {})).toMatchObject({
      text: null,
      position_secs: null,
      duration_secs: null,
      style_id: null,
    });
    const cmd = c.setCaption("t", "k", { text: "", positionSecs: 0, durationSecs: 0 });
    expect(cmd.text).toBe("");
    expect(cmd.position_secs).toBe(0);
    expect(cmd.duration_secs).toBe(0);
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
    expect(c.generateCaptions("m", "t", "s").timeline_offset_secs).toBe(0);
    expect(c.trackMotion("t", "k").sample_count).toBe(12);
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
    expect(c.setClipTransition("t", "k", null).transition).toBeNull();
    expect(c.setTrackAudioRole("t", null).role).toBeNull();
  });

  it("setClipTransition keeps the transition object intact", () => {
    const tr = { kind: "wipe_left", duration_secs: 0.4 } as const;
    expect(c.setClipTransition("t", "k", tr).transition).toEqual(tr);
  });
});
