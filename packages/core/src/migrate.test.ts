import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { createId } from "./ids";
import { migrate, parseProject, serialiseProject, type MigrateResult } from "./migrate";

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
    expect(migrate(changed("version", setTo(2)))).toEqual({
      ok: false,
      reason: "newer",
      version: 2,
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
