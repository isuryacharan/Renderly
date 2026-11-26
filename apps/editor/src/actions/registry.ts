// The actions registry: one list of everything the editor can do. Toolbar buttons, menu items
// and keyboard shortcuts all read it, so a label, a shortcut or an enabled state is defined once.
//
// An action runs against a context the panels attach themselves to (the timeline, the viewer's
// engine link, the media bin). Shortcuts respect focus: nothing fires while typing in a field,
// inside an open dialog or menu, or (for Space and Enter) on a focused button.

import type { ProjectStore, ViewStore } from "@renderly/core";
import type { IconName } from "@renderly/ui/react";
import type { TimelineView } from "../timeline/view";
import type { EngineLink } from "../viewer/link";

/** What the media bin offers to actions. */
export interface BinApi {
  importFiles(): void;
  importFolder(): void;
  canImportFolder: boolean;
  /** The bin item with keyboard focus, if any. */
  focused(): string | null;
  relink(media: string): void;
  reconnect(): void;
  /** Removes an item, asking first when clips use it. */
  remove(media: string): void;
  view: "grid" | "list";
  setView(view: "grid" | "list"): void;
}

/** What the viewer offers to actions. */
export interface ViewerApi {
  zoom: number | null;
  setZoom(zoom: number | null): void;
  safeArea: boolean;
  thirds: boolean;
  setOverlay(overlay: { safeArea?: boolean; thirds?: boolean }): void;
}

export interface ActionContext {
  store: ProjectStore;
  view: ViewStore;
  readOnly: boolean;
  timeline: TimelineView | null;
  link: EngineLink | null;
  bin: BinApi | null;
  viewer: ViewerApi | null;
  /** Opens the keyboard shortcuts list. */
  showShortcuts: () => void;
  navigateHome: () => void;
}

export interface Action {
  id: string;
  label: string;
  group: "Project" | "Edit" | "Playback" | "Viewer" | "Timeline" | "Media" | "Help";
  icon?: IconName;
  /** Default keys, such as "Mod+Shift+Z", "Space", "Shift+Delete", ",". */
  keys?: readonly string[];
  /** Only when focus is inside an element matching this selector. */
  scope?: string;
  /** A key held down repeats the action. */
  repeat?: boolean;
  enabled?: (ctx: ActionContext) => boolean;
  /** For toggles: whether it is on. */
  pressed?: (ctx: ActionContext) => boolean;
  run: (ctx: ActionContext, args?: Record<string, unknown>) => unknown;
}

const isMac = () => /Mac|iPhone|iPad/.test(navigator.platform);

/** Physical keys whose character changes with Shift or the layout: matched by code. */
const CODE_KEYS: Record<string, string> = {
  Comma: ",",
  Period: ".",
  Slash: "/",
  Backslash: "\\",
  BracketLeft: "[",
  BracketRight: "]",
  Equal: "=",
  Minus: "-",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
};

/** The key an event names, as written in `keys`: "Mod+Shift+Z", "Space", "Shift+,". */
export function eventKey(event: KeyboardEvent): string {
  let key = CODE_KEYS[event.code] ?? event.key;
  if (/^Key[A-Z]$/.test(event.code)) key = event.code.slice(3);
  else if (/^Digit\d$/.test(event.code)) key = event.code.slice(5);
  else if (key === " ") key = "Space";
  else if (key.length === 1) key = key.toUpperCase();
  const mod = isMac() ? event.metaKey : event.ctrlKey;
  const parts = [];
  if (mod) parts.push("Mod");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  parts.push(key);
  return parts.join("+");
}

const KEY_NAMES: Record<string, string> = {
  ArrowLeft: "←",
  ArrowRight: "→",
  ArrowUp: "↑",
  ArrowDown: "↓",
  Delete: "Delete",
  Backspace: "Backspace",
  Escape: "Esc",
};

/** A key written the platform's way: "Ctrl Shift Z" or "⇧⌘Z". */
export function formatKey(key: string): string {
  const parts = key.split("+");
  const main = parts.pop() ?? "";
  const name = KEY_NAMES[main] ?? main;
  if (isMac()) {
    const symbols: Record<string, string> = { Mod: "⌘", Alt: "⌥", Shift: "⇧" };
    const order = ["Alt", "Shift", "Mod"];
    return `${order
      .filter((m) => parts.includes(m))
      .map((m) => symbols[m])
      .join("")}${name}`;
  }
  const words: Record<string, string> = { Mod: "Ctrl", Alt: "Alt", Shift: "Shift" };
  return [...parts.map((m) => words[m] ?? m), name].join(" ");
}

/** The element an event is for; null outside a DOM (tests) or for non-element targets. */
function targetOf(event: KeyboardEvent): Element | null {
  return typeof Element !== "undefined" && event.target instanceof Element ? event.target : null;
}

/** True when a shortcut must not fire for this event (see the file comment). */
export function ignoredTarget(event: KeyboardEvent): boolean {
  const target = targetOf(event);
  if (!target) return false;
  if (target.closest("input, textarea, select, [contenteditable='true'], [contenteditable='']"))
    return true;
  if (target.closest("dialog[open], [role='menu']")) return true;
  if (
    (event.key === " " || event.key === "Enter") &&
    target.closest("button, a[href], summary, [role='button'], [role='menuitem']")
  )
    return true;
  return false;
}

declare global {
  interface Window {
    /** Dev builds only: the editor's actions, for the end-to-end checks. */
    renderlyActions?: Actions;
  }
}

export class Actions {
  private readonly actions = new Map<string, Action>();
  private readonly byKey = new Map<string, Action[]>();
  private readonly listeners = new Set<() => void>();
  private version = 0;

  constructor(private context: ActionContext) {}

  get ctx(): ActionContext {
    return this.context;
  }

  register(list: readonly Action[]): void {
    for (const action of list) {
      if (this.actions.has(action.id)) throw new Error(`Action ${action.id} is registered twice`);
      this.actions.set(action.id, action);
      for (const key of action.keys ?? []) {
        const existing = this.byKey.get(key) ?? [];
        this.byKey.set(key, [...existing, action]);
      }
    }
    this.changed();
  }

  /** Adds or replaces parts of the context (a panel attaching itself) and refreshes buttons. */
  attach(parts: Partial<ActionContext>): void {
    this.context = { ...this.context, ...parts };
    this.changed();
  }

  get(id: string): Action {
    const action = this.actions.get(id);
    if (!action) throw new Error(`No action ${id}`);
    return action;
  }

  list(): Action[] {
    return [...this.actions.values()];
  }

  isEnabled(id: string): boolean {
    const action = this.get(id);
    return action.enabled ? action.enabled(this.context) : true;
  }

  isPressed(id: string): boolean | undefined {
    return this.get(id).pressed?.(this.context);
  }

  /** The first default key, written for this platform; undefined when there is none. */
  shortcut(id: string): string | undefined {
    const key = this.get(id).keys?.[0];
    return key ? formatKey(key) : undefined;
  }

  run(id: string, args?: Record<string, unknown>): boolean {
    if (!this.isEnabled(id)) return false;
    void this.get(id).run(this.context, args);
    this.changed();
    return true;
  }

  /** Runs the action bound to a key event. Returns true when one ran. */
  handleKey(event: KeyboardEvent): boolean {
    if (event.defaultPrevented || ignoredTarget(event)) return false;
    const candidates = this.byKey.get(eventKey(event));
    if (!candidates) return false;
    const target = targetOf(event);
    // A scoped action beats a global one when focus is inside its scope.
    const scoped = candidates.find((a) => a.scope && target?.closest(a.scope));
    const action = scoped ?? candidates.find((a) => !a.scope);
    if (!action) return false;
    if (event.repeat && !action.repeat) {
      event.preventDefault();
      return true;
    }
    event.preventDefault();
    this.run(action.id);
    return true;
  }

  /** Tells buttons to re-read enabled and pressed states. */
  changed(): void {
    this.version++;
    for (const listener of this.listeners) listener();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  getVersion(): number {
    return this.version;
  }
}
