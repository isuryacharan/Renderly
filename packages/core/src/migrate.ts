// Loading a stored project: check the format, refuse newer versions, upgrade older ones step by
// step, then validate. Never half-loads: the result is a valid current project or a list of
// problems, each with a path and a reason.

import { CURRENT_VERSION, formatPath, projectSchema, type Project } from "./schema";

export interface Problem {
  /** Where in the document, e.g. `sequences[0].tracks[1].clips[2].start`; empty for the root. */
  path: string;
  message: string;
}

export type MigrateResult =
  | { ok: true; project: Project }
  | { ok: false; reason: "invalid"; problems: Problem[] }
  /** Made by a newer version of the app: refused, never half-loaded. */
  | { ok: false; reason: "newer"; version: number };

/**
 * One pure function per stored version: upgrades a version-N document to version N + 1.
 * Each must be small and tested. They never change their input.
 */
type Upgrade = (document: Record<string, unknown>) => Record<string, unknown>;

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value);

/** Applies `change` to every component of every clip, returning a new document. */
function mapComponents(
  document: Record<string, unknown>,
  change: (component: Record<string, unknown>) => Record<string, unknown>,
): Record<string, unknown> {
  const list = (value: unknown) => (Array.isArray(value) ? value : []);
  return {
    ...document,
    sequences: list(document["sequences"]).map((sequence: unknown) =>
      !isObject(sequence)
        ? sequence
        : {
            ...sequence,
            tracks: list(sequence["tracks"]).map((track: unknown) =>
              !isObject(track)
                ? track
                : {
                    ...track,
                    clips: list(track["clips"]).map((clip: unknown) =>
                      !isObject(clip)
                        ? clip
                        : {
                            ...clip,
                            components: list(clip["components"]).map((c: unknown) =>
                              isObject(c) ? change(c) : c,
                            ),
                          },
                    ),
                  },
            ),
          },
    ),
  };
}

const pair = (value: unknown) => (typeof value === "number" ? [value, value] : value);

/** 1 → 2: the transform's `scale` becomes per axis, [s, s], and so do its scale keyframes. */
export function upgrade1to2(document: Record<string, unknown>): Record<string, unknown> {
  return mapComponents(document, (component) => {
    if (component["type"] !== "transform") return component;
    const next: Record<string, unknown> = { ...component };
    if ("scale" in component) next["scale"] = pair(component["scale"]);
    const keys = component["keys"];
    if (isObject(keys) && Array.isArray(keys["scale"]))
      next["keys"] = {
        ...keys,
        scale: keys["scale"].map((key: unknown) =>
          isObject(key) ? { ...key, v: pair(key["v"]) } : key,
        ),
      };
    return next;
  });
}

/**
 * 2 → 3: every transform says how it fits (`fit`, "fit" as before), and every sequence has a
 * background (black, as before). Blend and crop components and speed 0 are new, not changed.
 */
export function upgrade2to3(document: Record<string, unknown>): Record<string, unknown> {
  const next = mapComponents(document, (component) =>
    component["type"] === "transform" && !("fit" in component)
      ? { ...component, fit: "fit" }
      : component,
  );
  const sequences = next["sequences"];
  return {
    ...next,
    sequences: Array.isArray(sequences)
      ? sequences.map((sequence: unknown) =>
          isObject(sequence) && !("background" in sequence)
            ? { ...sequence, background: { kind: "color", color: "#000000", blur: 0.5 } }
            : sequence,
        )
      : sequences,
  };
}

const UPGRADES: Record<number, Upgrade> = { 1: upgrade1to2, 2: upgrade2to3 };

export function migrate(raw: unknown): MigrateResult {
  const invalid = (path: string, message: string): MigrateResult => ({
    ok: false,
    reason: "invalid",
    problems: [{ path, message }],
  });
  if (!isObject(raw)) return invalid("", "must be a JSON object");
  if (raw["format"] !== "renderly") return invalid("format", 'must be "renderly"');
  const version = raw["version"];
  if (typeof version !== "number" || !Number.isInteger(version) || version < 1) {
    return invalid("version", "must be a whole number from 1");
  }
  if (version > CURRENT_VERSION) return { ok: false, reason: "newer", version };

  let document = raw;
  for (let v = version; v < CURRENT_VERSION; v++) {
    const upgrade = UPGRADES[v];
    if (!upgrade) return invalid("version", `has no upgrade from version ${v}`);
    document = { ...upgrade(document), version: v + 1 };
  }

  const result = projectSchema().safeParse(document);
  if (!result.success) {
    return {
      ok: false,
      reason: "invalid",
      problems: result.error.issues.map((issue) => ({
        path: formatPath(issue.path),
        message: issue.message,
      })),
    };
  }
  return { ok: true, project: result.data };
}

/** Parses project.json text. JSON syntax errors come back as a problem at the root. */
export function parseProject(text: string): MigrateResult {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch (error) {
    return {
      ok: false,
      reason: "invalid",
      problems: [
        {
          path: "",
          message: `is not valid JSON: ${error instanceof Error ? error.message : String(error)}`,
        },
      ],
    };
  }
  return migrate(raw);
}

/**
 * Project → project.json text: two-space indentation, fields in schema order, a final newline.
 * A document that came out of `parseProject` serialises to the same bytes it was read from, as
 * long as it was written this way and every default was present.
 */
export function serialiseProject(project: Project): string {
  return `${JSON.stringify(project, null, 2)}\n`;
}
