import { readFileSync } from "node:fs";
import { join } from "node:path";
import fc from "fast-check";
import { describe, expect, it } from "vitest";
import "./basic-commands";
import type { Command } from "./commands";
import { parseProject, serialiseProject } from "./migrate";
import type { Project } from "./schema";
import { ProjectStore } from "./store";
import { ViewStore } from "./view-state";

const fixture = readFileSync(
  join(import.meta.dirname, "..", "fixtures", "holiday-edit.project.json"),
  "utf8",
);

function load(): Project {
  const result = parseProject(fixture);
  if (!result.ok) throw new Error(JSON.stringify(result));
  return result.project;
}

const MAIN = "s0a1b2c3d4e5f6g7";
const OTHER = "s1h2j3k4m5n6p7q8";
const CLIP = "c0a1b2c3d4e5f6g7";
const CLIP_2 = "c2r3s4t5v6w7x8y9";
const MARKER = "k0a1b2c3d4e5f6g7";

/** The document without its revision, as written to disk. */
const content = (project: Project) => serialiseProject({ ...project, revision: 0 });

const opacity = (store: ProjectStore, clip = CLIP) => {
  for (const sequence of store.project.sequences)
    for (const track of sequence.tracks)
      for (const c of track.clips)
        if (c.id === clip)
          return (c.components as { type: string; opacity?: number }[]).find(
            (x) => x.type === "transform",
          )?.opacity;
  return undefined;
};

const setOpacity = (value: number, clip = CLIP): Command => ({
  type: "component.set",
  payload: { clip, component: "transform", field: "opacity", value },
});

// --- the fuzz test: random commands, all undone, give back the original document ---

const idChars = "0123456789abcdefghjkmnpqrstvwxyz";
const markerId = fc.constantFrom(MARKER, ...[..."abcdefgh"].map((c) => `k${c}${"0".repeat(14)}`));
const commandArb: fc.Arbitrary<Command> = fc.oneof(
  fc.string({ minLength: 1, maxLength: 12, unit: fc.constantFrom(...idChars) }).map((name) => ({
    type: "project.rename",
    payload: { name },
  })),
  fc
    .record({
      sequence: fc.constantFrom(MAIN, OTHER),
      id: markerId,
      t: fc.nat(10_000_000_000),
      name: fc.string({ maxLength: 4 }),
    })
    .map((payload) => ({
      type: "marker.add",
      payload,
    })),
  fc
    .record({ sequence: fc.constantFrom(MAIN, OTHER), id: markerId, t: fc.nat(10_000_000_000) })
    .map((payload) => ({
      type: "marker.move",
      payload,
    })),
  fc.record({ sequence: fc.constantFrom(MAIN, OTHER), id: markerId }).map((payload) => ({
    type: "marker.remove",
    payload,
  })),
  fc
    .tuple(fc.double({ min: 0, max: 1.2, noNaN: true }), fc.constantFrom(CLIP, CLIP_2))
    .map(([v, clip]) => setOpacity(v, clip)),
);
type Step =
  | { kind: "command"; command: Command }
  | { kind: "undo" }
  | { kind: "redo" }
  | { kind: "transaction"; commands: Command[] };
const stepArb: fc.Arbitrary<Step> = fc.oneof(
  { weight: 6, arbitrary: commandArb.map((command) => ({ kind: "command" as const, command })) },
  { weight: 2, arbitrary: fc.constant({ kind: "undo" as const }) },
  { weight: 1, arbitrary: fc.constant({ kind: "redo" as const }) },
  {
    weight: 1,
    arbitrary: fc
      .array(commandArb, { maxLength: 5 })
      .map((commands) => ({ kind: "transaction" as const, commands })),
  },
);

describe("ProjectStore", () => {
  it("undoing every random command gives back the original document", () => {
    fc.assert(
      fc.property(fc.array(stepArb, { maxLength: 60 }), (steps) => {
        const original = load();
        const store = new ProjectStore(original);
        let revision = store.revision;
        for (const step of steps) {
          if (step.kind === "command") store.dispatch(step.command);
          else if (step.kind === "undo") store.undo();
          else if (step.kind === "redo") store.redo();
          else
            store.transaction("batch", () => void step.commands.forEach((c) => store.dispatch(c)));
          expect(store.revision).toBeGreaterThanOrEqual(revision); // never goes back
          revision = store.revision;
        }
        const edited = content(store.project);
        let undone = 0;
        while (store.undo().ok) undone++;
        expect(content(store.project)).toBe(content(original));
        for (let i = 0; i < undone; i++) expect(store.redo().ok).toBe(true);
        expect(content(store.project)).toBe(edited);
      }),
      { numRuns: 1000 },
    );
  });

  it("makes exactly one undo entry for a 200-step slider gesture", () => {
    const store = new ProjectStore(load());
    const start = store.revision;
    const seen: number[] = [];
    store.subscribe(() => seen.push(opacity(store) ?? -1));
    const gesture = store.beginGesture("opacity-slider", "component.set");
    for (let i = 0; i < 200; i++) {
      const result = gesture.preview({
        clip: CLIP,
        component: "transform",
        field: "opacity",
        value: i / 199,
      });
      expect(result.ok).toBe(true);
    }
    expect(seen).toHaveLength(200);
    expect(store.revision).toBe(start); // previews do not count as changes
    expect(store.canUndo).toBe(false);
    const committed = gesture.commit({
      clip: CLIP,
      component: "transform",
      field: "opacity",
      value: 0.5,
    });
    expect(committed).toMatchObject({
      ok: true,
      revision: start + 1,
      changes: { added: [], removed: [], changed: [CLIP] },
    });
    expect(store.undoLabels).toEqual(["Change opacity"]);
    expect(opacity(store)).toBe(0.5);
    store.undo();
    expect(opacity(store)).toBe(1);
    expect(store.canUndo).toBe(false);
  });

  it("cancels a gesture back to where it began, and blocks dispatch while one runs", () => {
    const store = new ProjectStore(load());
    const before = store.project;
    const gesture = store.beginGesture("drag", "component.set");
    gesture.preview({ clip: CLIP, component: "transform", field: "opacity", value: 0.2 });
    expect(store.dispatch(setOpacity(0.3)).ok).toBe(false);
    expect(store.dispatch(setOpacity(0.3))).toMatchObject({ error: { code: "gesture-active" } });
    expect(store.undo()).toMatchObject({ ok: false, reason: "gesture-active" });
    gesture.cancel();
    expect(store.project).toBe(before);
    expect(
      gesture.commit({ clip: CLIP, component: "transform", field: "opacity", value: 0.2 }).ok,
    ).toBe(false);
    expect(store.dispatch(setOpacity(0.3)).ok).toBe(true);
  });

  it("fails a stale expectedRevision with a conflict and changes nothing", () => {
    const store = new ProjectStore(load());
    const stale = store.revision;
    store.dispatch({ type: "project.rename", payload: { name: "First" } });
    const before = store.project;
    const result = store.dispatch(
      { type: "project.rename", payload: { name: "Second" } },
      { expectedRevision: stale },
    );
    expect(result).toMatchObject({ ok: false, revision: stale + 1, error: { code: "conflict" } });
    expect(result.ok || result.error.hint).toMatch(/rebase/);
    expect(store.project).toBe(before);
    expect(store.undoLabels).toEqual(["Rename project"]);
    expect(
      store.dispatch(
        { type: "project.rename", payload: { name: "Second" } },
        { expectedRevision: stale + 1 },
      ).ok,
    ).toBe(true);
  });

  it("dryRun reports the same changes as the real run and leaves the revision alone", () => {
    const store = new ProjectStore(load());
    const before = store.project;
    const command: Command = {
      type: "marker.add",
      payload: { sequence: MAIN, id: "kzzzzzzzzzzzzzzz", t: 0 },
    };
    const dry = store.dispatch(command, { dryRun: true });
    expect(store.project).toBe(before);
    expect(store.revision).toBe(before.revision);
    expect(store.canUndo).toBe(false);
    const real = store.dispatch(command);
    expect(dry).toMatchObject({ ok: true, dryRun: true, revision: before.revision });
    expect(real).toMatchObject({ ok: true, dryRun: false, revision: before.revision + 1 });
    if (!dry.ok || !real.ok) throw new Error("expected success");
    expect(dry.changes).toEqual(real.changes);
    expect(real.changes).toEqual({ added: ["kzzzzzzzzzzzzzzz"], removed: [], changed: [MAIN] });
  });

  it("returns the first result when a requestId is retried", () => {
    const store = new ProjectStore(load());
    const first = store.dispatch(
      { type: "project.rename", payload: { name: "Once" } },
      { requestId: "r1" },
    );
    const again = store.dispatch(
      { type: "project.rename", payload: { name: "Twice" } },
      { requestId: "r1" },
    );
    expect(again).toEqual(first);
    expect(store.project.name).toBe("Once");
    expect(store.undoLabels).toHaveLength(1);
  });

  it("returns typed errors and leaves the project untouched", () => {
    const store = new ProjectStore(load());
    const before = store.project;
    expect(store.dispatch({ type: "nope", payload: {} })).toMatchObject({
      error: { code: "unknown-command" },
    });
    expect(store.dispatch({ type: "project.rename", payload: { name: "" } })).toMatchObject({
      error: { code: "invalid-payload", path: "name" },
    });
    expect(
      store.dispatch({
        type: "marker.remove",
        payload: { sequence: MAIN, id: "kaaaaaaaaaaaaaaa" },
      }),
    ).toMatchObject({
      error: { code: "not-found", path: "id" },
    });
    // Opacity 2 passes the payload schema but breaks the project: refused as a whole.
    expect(store.dispatch(setOpacity(2))).toMatchObject({
      error: {
        code: "invalid-result",
        path: "sequences[0].tracks[0].clips[0].components[0].opacity",
      },
    });
    expect(store.project).toBe(before);
    expect(store.canUndo).toBe(false);
  });

  it("groups a transaction into one undo step and rolls it back on failure", () => {
    const store = new ProjectStore(load());
    const original = content(store.project);
    store.transaction("Fade both", () => {
      store.dispatch(setOpacity(0.25));
      store.dispatch(setOpacity(0.5, CLIP_2));
    });
    expect(store.undoLabels).toEqual(["Fade both"]);
    expect([opacity(store), opacity(store, CLIP_2)]).toEqual([0.25, 0.5]);
    store.undo();
    expect(content(store.project)).toBe(original);

    const kept = store.transaction("Abandoned", () => {
      store.dispatch(setOpacity(0.1));
      return false;
    });
    expect(kept).toBe(false);
    expect(content(store.project)).toBe(original);
    expect(store.canUndo).toBe(false);
    expect(store.canRedo).toBe(true); // the abandoned transaction left the redo stack alone
  });

  it("reports only the entity that changed, not its parents", () => {
    const store = new ProjectStore(load());
    const result = store.dispatch(setOpacity(0.4));
    expect(result).toMatchObject({
      ok: true,
      changes: { added: [], removed: [], changed: [CLIP] },
    });
    const removed = store.dispatch({
      type: "marker.remove",
      payload: { sequence: MAIN, id: MARKER },
    });
    expect(removed).toMatchObject({
      ok: true,
      changes: { added: [], removed: [MARKER], changed: [MAIN] },
    });
    // Undo reports what it restored; the revision bump is not an edit to the project.
    expect(store.undo()).toMatchObject({
      ok: true,
      changes: { added: [MARKER], removed: [], changed: [MAIN] },
    });
  });

  it("calls selector subscribers only when their slice changes", () => {
    const store = new ProjectStore(load());
    const names: string[] = [];
    const markerCounts: number[] = [];
    store.select(
      (p) => p.name,
      (name) => names.push(name),
    );
    store.select(
      (p) => p.sequences[0]!.markers,
      (markers) => markerCounts.push(markers.length),
    );
    store.dispatch({ type: "project.rename", payload: { name: "Renamed" } });
    store.dispatch(setOpacity(0.3));
    store.dispatch({ type: "marker.remove", payload: { sequence: MAIN, id: MARKER } });
    expect(names).toEqual(["Renamed"]);
    expect(markerCounts).toEqual([0]);
  });
});

describe("ViewStore", () => {
  it("keeps view state outside the project and its undo", () => {
    const view = new ViewStore({ sequence: MAIN });
    const playheads: number[] = [];
    view.select(
      (s) => s.playhead,
      (t) => playheads.push(t),
    );
    view.set({ playhead: 705_600_000 });
    view.set({ playhead: 705_600_000 }); // same value: no notification
    view.set({ selection: [CLIP] });
    expect(playheads).toEqual([705_600_000]);
    expect(view.get()).toMatchObject({ sequence: MAIN, playhead: 705_600_000, selection: [CLIP] });
  });
});

describe("read-only and replace", () => {
  it("refuses edits, undo and gestures while read-only", () => {
    const store = new ProjectStore(load());
    store.dispatch({ type: "project.rename", payload: { name: "Before" } });
    store.readOnly = true;
    const before = store.project;
    expect(store.dispatch({ type: "project.rename", payload: { name: "No" } })).toMatchObject({
      ok: false,
      error: { code: "read-only" },
    });
    expect(store.undo()).toMatchObject({ ok: false, reason: "read-only" });
    expect(() => store.beginGesture("g", "component.set")).toThrow(/read-only/);
    expect(store.project).toBe(before);
    store.readOnly = false;
    expect(store.undo().ok).toBe(true);
  });

  it("replaces the whole project and drops the history", () => {
    const store = new ProjectStore(load());
    store.dispatch({ type: "project.rename", payload: { name: "Edited" } }, { requestId: "r" });
    let calls = 0;
    store.subscribe(() => calls++);
    const fresh = { ...load(), name: "From disk", revision: 999 };
    store.replace(fresh);
    expect(store.project).toBe(fresh);
    expect([store.canUndo, store.canRedo, calls]).toEqual([false, false, 1]);
    // The request id was forgotten with the old document, so it applies again.
    expect(
      store.dispatch({ type: "project.rename", payload: { name: "Again" } }, { requestId: "r" }).ok,
    ).toBe(true);
    expect(store.project.name).toBe("Again");
  });
});
