import { newProject, ProjectStore, ViewStore } from "@renderly/core";
import { describe, expect, it } from "vitest";
import { EDITOR_ACTIONS } from "./editor-actions";
import { Actions, eventKey, formatKey, type Action, type ActionContext } from "./registry";

/** A keydown as the registry reads it (no DOM in unit tests). */
function key(
  code: string,
  keyName: string,
  mods: { ctrl?: boolean; meta?: boolean; alt?: boolean; shift?: boolean; repeat?: boolean } = {},
): KeyboardEvent {
  let prevented = false;
  return {
    code,
    key: keyName,
    ctrlKey: mods.ctrl ?? false,
    metaKey: mods.meta ?? false,
    altKey: mods.alt ?? false,
    shiftKey: mods.shift ?? false,
    repeat: mods.repeat ?? false,
    target: null,
    get defaultPrevented() {
      return prevented;
    },
    preventDefault() {
      prevented = true;
    },
  } as unknown as KeyboardEvent;
}

function context(): ActionContext {
  const store = new ProjectStore(newProject("Test"));
  return {
    store,
    view: new ViewStore(),
    readOnly: false,
    timeline: null,
    link: null,
    bin: null,
    viewer: null,
    showShortcuts: () => {},
    navigateHome: () => {},
  };
}

describe("eventKey", () => {
  it("names keys by their physical place for letters, digits and punctuation", () => {
    expect(eventKey(key("KeyS", "s"))).toBe("S");
    expect(eventKey(key("KeyZ", "z", { ctrl: true, shift: true }))).toBe("Mod+Shift+Z");
    // Shift+, types "<", but the shortcut is still Shift+,
    expect(eventKey(key("Comma", "<", { shift: true }))).toBe("Shift+,");
    expect(eventKey(key("Space", " "))).toBe("Space");
    expect(eventKey(key("ArrowLeft", "ArrowLeft", { alt: true }))).toBe("Alt+ArrowLeft");
    expect(eventKey(key("BracketLeft", "["))).toBe("[");
  });

  it("formats keys for people", () => {
    expect(formatKey("Mod+Shift+Z")).toBe("Ctrl Shift Z");
    expect(formatKey("ArrowLeft")).toBe("←");
  });
});

describe("Actions", () => {
  const log: string[] = [];
  const actions = (): Actions => {
    log.length = 0;
    const a = new Actions(context());
    const list: Action[] = [
      {
        id: "a.global",
        label: "Global",
        group: "Edit",
        keys: ["S"],
        run: () => log.push("global"),
      },
      {
        id: "a.off",
        label: "Off",
        group: "Edit",
        keys: ["X"],
        enabled: () => false,
        run: () => log.push("off"),
      },
      {
        id: "a.hold",
        label: "Hold",
        group: "Edit",
        keys: ["H"],
        repeat: true,
        run: () => log.push("hold"),
      },
    ];
    a.register(list);
    return a;
  };

  it("runs the action bound to a key and prevents the browser's default", () => {
    const a = actions();
    const event = key("KeyS", "s");
    expect(a.handleKey(event)).toBe(true);
    expect(event.defaultPrevented).toBe(true);
    expect(log).toEqual(["global"]);
    expect(a.handleKey(key("KeyQ", "q"))).toBe(false);
  });

  it("does not run disabled actions, nor repeat a held key unless the action allows it", () => {
    const a = actions();
    a.handleKey(key("KeyX", "x"));
    a.handleKey(key("KeyS", "s", { repeat: true }));
    a.handleKey(key("KeyH", "h", { repeat: true }));
    expect(log).toEqual(["hold"]);
  });

  it("refuses an id registered twice", () => {
    const a = actions();
    expect(() =>
      a.register([{ id: "a.global", label: "", group: "Edit", run: () => {} }]),
    ).toThrow();
  });
});

describe("the editor's actions", () => {
  it("have unique ids and no global key bound twice", () => {
    const a = new Actions(context());
    a.register(EDITOR_ACTIONS); // throws on a duplicate id
    const seen = new Map<string, string>();
    for (const action of EDITOR_ACTIONS) {
      if (action.scope) continue;
      for (const k of action.keys ?? []) {
        expect(seen.get(k), `${k} is bound to ${seen.get(k)} and ${action.id}`).toBeUndefined();
        seen.set(k, action.id);
      }
    }
  });

  it("enable undo only when there is something to undo and the project is writable", () => {
    const ctx = context();
    const a = new Actions(ctx);
    a.register(EDITOR_ACTIONS);
    expect(a.isEnabled("edit.undo")).toBe(false);
    ctx.store.dispatch({ type: "project.rename", payload: { name: "Renamed" } });
    expect(a.isEnabled("edit.undo")).toBe(true);
    a.attach({ readOnly: true });
    expect(a.isEnabled("edit.undo")).toBe(false);
    a.attach({ readOnly: false });
    expect(a.run("edit.undo")).toBe(true);
    expect(ctx.store.project.name).toBe("Test");
  });
});
