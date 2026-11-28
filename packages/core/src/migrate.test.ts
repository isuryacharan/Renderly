import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { createId } from "./ids";
import {
  migrate,
  parseProject,
  serialiseProject,
  upgrade1to2,
  upgrade2to3,
  type MigrateResult,
} from "./migrate";
import { newProject } from "./new-project";

const fixture = readFileSync(
  join(import.meta.dirname, "..", "fixtures", "holiday-edit.project.json"),
  "utf8",
);

function problems(result: MigrateResult) {
  if (result.ok || result.reason !== "invalid")
    throw new Error(`expected invalid, got ${JSON.stringify(result)}`);
  return result.problems;
}

type Json = string | number | boolean | null | Json[] | { [key: string]: Json };

/** The fixture, parsed, with `edit` applied to the value at `path` ("a.0.b"). */
function changed(
  path: string,
  edit: (parent: Json[] | Record<string, Json>, key: string) => void,
): unknown {
  const document = JSON.parse(fixture) as Json;
  const keys = path.split(".");
  const last = keys.pop() as string;
  let node = document;
  for (const key of keys) node = (node as Record<string, Json>)[key] as Json;
  edit(node as Record<string, Json>, last);
  return document;
}
const setTo = (value: Json) => (parent: Json[] | Record<string, Json>, key: string) => {
  (parent as Record<string, Json>)[key] = value;
};
describe("fixture project", () => {
  it("round-trips through parse and serialise byte for byte", () => {
    const result = parseProject(fixture);
    if (!result.ok) throw new Error(JSON.stringify(result));
    expect(serialiseProject(result.project)).toBe(fixture);
  });

  it("keeps every time an exact integer", () => {
    const result = parseProject(fixture);
    if (!result.ok) throw new Error("fixture invalid");
    const clip = result.project.sequences[0]?.tracks[0]?.clips[0];
    expect(clip?.duration).toBe(2_116_800_000); // 3 s
    expect(Number.isInteger(clip?.in)).toBe(true);
  });
});

describe("upgrades", () => {
  const v1 = readFileSync(
    join(import.meta.dirname, "..", "fixtures", "holiday-edit.v1.project.json"),
    "utf8",
  );

  const v2 = readFileSync(
    join(import.meta.dirname, "..", "fixtures", "holiday-edit.v2.project.json"),
    "utf8",
  );

  it("upgrade version 1 and version 2 projects to exactly the current fixture", () => {
    for (const old of [v1, v2]) {
      const result = parseProject(old);
      if (!result.ok) throw new Error(JSON.stringify(result));
      expect(serialiseProject(result.project)).toBe(fixture);
    }
  });

  it("2 → 3 adds the transform's fit and the sequence background, without touching the input", () => {
    const document = JSON.parse(v2) as Record<string, unknown>;
    const before = JSON.stringify(document);
    const upgraded = upgrade2to3(document) as {
      sequences: {
        background: unknown;
        tracks: { clips: { components: Record<string, unknown>[] }[] }[];
      }[];
    };
    expect(upgraded.sequences[0]?.background).toEqual({
      kind: "color",
      color: "#000000",
      blur: 0.5,
    });
    expect(upgraded.sequences[0]?.tracks[0]?.clips[0]?.components[0]?.["fit"]).toBe("fit");
    expect(JSON.stringify(document)).toBe(before);
  });

  it("round-trips the version 3 additions: blend, crop, fill, a background and a freeze frame", () => {
    const document = JSON.parse(fixture) as {
      sequences: {
        background: unknown;
        tracks: { clips: { speed: number; components: Record<string, unknown>[] }[] }[];
      }[];
    };
    const sequence = document.sequences[0];
    const clip = sequence?.tracks[0]?.clips[0];
    if (!sequence || !clip) throw new Error("fixture has no clip");
    sequence.background = { kind: "blur", color: "#1a2b3c", blur: 0.8 };
    clip.speed = 0;
    (clip.components[0] as Record<string, unknown>)["fit"] = "fill";
    clip.components.push(
      { type: "blend", mode: "softLight" },
      { type: "crop", left: 0.1, top: 0, right: 0.2, bottom: 0.05 },
    );
    const text = `${JSON.stringify(document, null, 2)}
`;
    const result = parseProject(text);
    if (!result.ok) throw new Error(JSON.stringify(result));
    expect(serialiseProject(result.project)).toBe(text);
    const bad = JSON.parse(text) as typeof document;
    (bad.sequences[0] as { background: unknown }).background = { kind: "color", color: "red" };
    expect(problems(migrate(bad))[0]?.path).toBe("sequences[0].background.color");
  });

  it("1 → 2 makes the transform's scale and its keyframes per axis, without touching the input", () => {
    const document = JSON.parse(v1) as Record<string, unknown>;
    const before = JSON.stringify(document);
    const clip = (d: Record<string, unknown>) =>
      (d as { sequences: { tracks: { clips: { components: Record<string, unknown>[] }[] }[] }[] })
        .sequences[0]?.tracks[0]?.clips[0]?.components[0] ?? {};
    const transform = clip(document);
    const ownKeys = transform["keys"] as Record<string, unknown>;
    transform["keys"] = { ...ownKeys, scale: [{ t: 0, v: 2, ease: "auto" }] };
    const withScaleKeys = JSON.stringify(document);
    const upgraded = upgrade1to2(document);
    expect(clip(upgraded)["scale"]).toEqual([1.1, 1.1]);
    expect((clip(upgraded)["keys"] as { scale: { v: unknown }[] }).scale[0]?.v).toEqual([2, 2]);
    // Other keys survive, and the input is untouched.
    expect((clip(upgraded)["keys"] as Record<string, unknown>)["position"]).toEqual(
      ownKeys["position"],
    );
    expect(JSON.stringify(document)).toBe(withScaleKeys);
    transform["keys"] = ownKeys;
    expect(JSON.stringify(document)).toBe(before);
  });
});

describe("defaults", () => {
  it("fills every default, and the result then round-trips", () => {
    const minimal = {
      format: "renderly",
      version: 1,
      id: createId(),
      name: "New project",
      revision: 0,
      sequences: [{ id: createId(), name: "Main", width: 1920, height: 1080, fps: [25, 1] }],
    };
    const result = migrate(minimal);
    if (!result.ok) throw new Error(JSON.stringify(result));
    expect(result.project.packs).toEqual([]);
    expect(result.project.media).toEqual([]);
    expect(result.project.sequences[0]?.sampleRate).toBe(48_000);
    expect(result.project.sequences[0]?.tracks).toEqual([]);
    const text = serialiseProject(result.project);
    const again = parseProject(text);
    expect(again.ok && serialiseProject(again.project)).toBe(text);
  });
});

describe("invalid documents fail with a path and a reason", () => {
  it("refuses anything that is not a Renderly project", () => {
    expect(problems(migrate([]))[0]).toEqual({ path: "", message: "must be a JSON object" });
    expect(problems(migrate({ format: "other" }))[0]?.path).toBe("format");
    expect(problems(parseProject("{ nope"))[0]?.message).toMatch(/^is not valid JSON/);
  });

  it("refuses a project from a newer version instead of half-loading it", () => {
    expect(migrate(changed("version", setTo(4)))).toEqual({
      ok: false,
      reason: "newer",
      version: 4,
    });
  });

  const clip0 = "sequences.0.tracks.0.clips.0";
  it.each([
    [
      "a fractional time",
      changed(`${clip0}.start`, setTo(1.5)),
      "sequences[0].tracks[0].clips[0].start",
    ],
    [
      "a zero duration",
      changed(`${clip0}.duration`, setTo(0)),
      "sequences[0].tracks[0].clips[0].duration",
    ],
    ["an unsupported frame rate", changed("sequences.0.fps", setTo([29, 1])), "sequences[0].fps"],
    [
      "an unknown field",
      changed("sequences.0.tracks.0.colour", setTo("red")),
      "sequences[0].tracks[0]",
    ],
    [
      "an unknown component type",
      changed("sequences.0.tracks.0.clips.1.components.0", setTo({ type: "sparkles" })),
      "sequences[0].tracks[0].clips[1].components[0].type",
    ],
    [
      "a bad keyframe",
      changed(`${clip0}.components.0.keys.position.0.ease`, setTo("bouncy")),
      "sequences[0].tracks[0].clips[0].components[0].keys.position[0].ease",
    ],
    [
      "a dangling media reference",
      changed(`${clip0}.media`, setTo("zzzzzzzzzzzzzzzz")),
      "sequences[0].tracks[0].clips[0].media",
    ],
    [
      "a duplicate id",
      changed("sequences.0.tracks.1", (tracks, key) => {
        const list = tracks as Record<string, Json>;
        (list[key] as Record<string, Json>)["id"] = (list["0"] as Record<string, Json>)[
          "id"
        ] as Json;
      }),
      "sequences[0].tracks[1].id",
    ],
    [
      "a clip with two sources",
      changed(`${clip0}.sequence`, setTo("s1h2j3k4m5n6p7q8")),
      "sequences[0].tracks[0].clips[0]",
    ],
    ["a malformed id", changed("media.0.id", setTo("Not An Id")), "media[0].id"],
  ] as const)("%s", (_name, document, path) => {
    const found = problems(migrate(document));
    const match = found.find((p) => p.path === path);
    expect(match, JSON.stringify(found)).toBeDefined();
    expect(match?.message.length).toBeGreaterThan(0);
  });
});

describe("newProject", () => {
  it("makes a valid empty project that round-trips", () => {
    const project = newProject("Fresh");
    expect(project).toMatchObject({ name: "Fresh", revision: 0, media: [], packs: [] });
    expect(project.sequences).toHaveLength(1);
    expect(project.sequences[0]).toMatchObject({
      width: 1920,
      height: 1080,
      fps: [30, 1],
      tracks: [],
    });
    const again = parseProject(serialiseProject(project));
    expect(again.ok && again.project).toEqual(project);
  });
});
