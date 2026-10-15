/// Central actions registry (improvement-plan C1).
///
/// Keyboard shortcuts, toolbar buttons, context menus, and (later) a command palette
/// all resolve through `invokeAction` / `ACTIONS` instead of duplicating enablement and
/// run logic. Keep `run` thin — call into `editorStore` / `projectFlows` / timeline helpers.

import { useEditorStore, type EditorStore } from "../store/editorStore";
import {
  createNewProjectFlow,
  exportOtioFlow,
  importOtioFlow,
  openExistingProjectFlow,
} from "./projectFlows";
import { deleteSelected, splitSelectedAtPlayhead } from "../timeline/interactions";
import { activeTracks, containerDurationSecs, isNestedClip } from "./container";
import { cycleTheme } from "./theme";
import { resetTimelineThemeCache } from "../timeline/theme";
import { useAgentChatStore } from "../store/agentChatStore";

export interface ActionContext {
  /** Optional UI callbacks that aren't pure store state (e.g. open export dialog). */
  openExport?: () => void;
}

export interface EditorAction {
  id: string;
  label: string;
  /** Human-readable chord for tooltips (e.g. "Ctrl+S"). */
  shortcut?: string;
  isEnabled: (state: EditorStore) => boolean;
  run: (state: EditorStore, ctx?: ActionContext) => void | Promise<void>;
  /**
   * Return true if this keydown should invoke the action. Checked after input/dialog
   * guards in `handleGlobalKeyDown`. `mod` is Ctrl (Win/Linux) or Meta (macOS).
   */
  matchKey?: (e: KeyboardEvent, mod: boolean) => boolean;
}

const hasProject = (s: EditorStore) => !!s.project;
const hasSelection = (s: EditorStore) => s.selections.length > 0;

export const ACTIONS: Record<string, EditorAction> = {
  "project.new": {
    id: "project.new",
    label: "New project",
    shortcut: "Ctrl+N",
    isEnabled: () => true,
    run: () => void createNewProjectFlow(),
    matchKey: (e, mod) => mod && (e.key === "n" || e.key === "N") && !e.shiftKey,
  },
  "project.open": {
    id: "project.open",
    label: "Open project",
    isEnabled: () => true,
    run: () => void openExistingProjectFlow(),
  },
  "project.save": {
    id: "project.save",
    label: "Save",
    shortcut: "Ctrl+S",
    isEnabled: (s) => !!s.projectPath,
    run: (s) => void s.saveProject(),
    matchKey: (e, mod) => mod && (e.key === "s" || e.key === "S"),
  },
  "project.import-otio": {
    id: "project.import-otio",
    label: "Import timeline (OTIO)",
    isEnabled: () => true,
    run: () => void importOtioFlow(),
  },
  "project.export-otio": {
    id: "project.export-otio",
    label: "Export timeline (OTIO)",
    isEnabled: hasProject,
    run: () => void exportOtioFlow(),
  },
  "project.close": {
    id: "project.close",
    label: "All projects",
    isEnabled: hasProject,
    run: (s) => void s.closeProject(),
  },
  "project.export": {
    id: "project.export",
    label: "Export",
    isEnabled: hasProject,
    run: (_s, ctx) => ctx?.openExport?.(),
  },
  "edit.undo": {
    id: "edit.undo",
    label: "Undo",
    shortcut: "Ctrl+Z",
    isEnabled: (s) => s.canUndo,
    run: (s) => void s.undo(),
    matchKey: (e, mod) => mod && (e.key === "z" || e.key === "Z") && !e.shiftKey,
  },
  "edit.redo": {
    id: "edit.redo",
    label: "Redo",
    shortcut: "Ctrl+Y",
    isEnabled: (s) => s.canRedo,
    run: (s) => void s.redo(),
    matchKey: (e, mod) =>
      (mod && (e.key === "y" || e.key === "Y")) ||
      (mod && e.shiftKey && (e.key === "z" || e.key === "Z")),
  },
  "edit.copy": {
    id: "edit.copy",
    label: "Copy",
    shortcut: "Ctrl+C",
    isEnabled: hasSelection,
    run: (s) => s.copySelection(),
    matchKey: (e, mod) => mod && (e.key === "c" || e.key === "C"),
  },
  "edit.paste": {
    id: "edit.paste",
    label: "Paste",
    shortcut: "Ctrl+V",
    isEnabled: (s) => !!s.clipboard && !!s.project,
    run: (s) => void s.pasteAtPlayhead(),
    matchKey: (e, mod) => mod && (e.key === "v" || e.key === "V"),
  },
  "edit.duplicate": {
    id: "edit.duplicate",
    label: "Duplicate",
    shortcut: "Ctrl+D",
    isEnabled: hasSelection,
    run: (s) => void s.duplicateSelection(),
    matchKey: (e, mod) => mod && (e.key === "d" || e.key === "D"),
  },
  "edit.delete": {
    id: "edit.delete",
    label: "Delete",
    shortcut: "Delete",
    isEnabled: hasSelection,
    run: () => void deleteSelected(false),
    matchKey: (e, mod) => !mod && (e.key === "Delete" || e.key === "Backspace") && !e.shiftKey,
  },
  "edit.delete-ripple": {
    id: "edit.delete-ripple",
    label: "Ripple delete",
    shortcut: "Shift+Delete",
    isEnabled: hasSelection,
    run: () => void deleteSelected(true),
    matchKey: (e, mod) => !mod && (e.key === "Delete" || e.key === "Backspace") && e.shiftKey,
  },
  "edit.nest": {
    id: "edit.nest",
    label: "Nest clips",
    shortcut: "Ctrl+G",
    isEnabled: hasSelection,
    run: (s) => void s.nestSelection(),
    matchKey: (e, mod) => mod && !e.shiftKey && (e.key === "g" || e.key === "G"),
  },
  "edit.unnest": {
    id: "edit.unnest",
    label: "Unnest clip",
    shortcut: "Ctrl+Shift+G",
    isEnabled: (s) => {
      if (!s.project || !s.selection) return false;
      const clip = activeTracks(s.project, s.activeSequenceId)
        .find((t) => t.id === s.selection!.trackId)
        ?.clips.find((c) => c.id === s.selection!.clipId);
      return !!clip && isNestedClip(s.project, clip);
    },
    run: (s) => void s.unnestClip(),
    matchKey: (e, mod) => mod && e.shiftKey && (e.key === "g" || e.key === "G"),
  },
  "timeline.add-adjustment": {
    id: "timeline.add-adjustment",
    label: "Add adjustment layer",
    isEnabled: hasProject,
    run: (s) => void s.addAdjustmentLayerAtPlayhead(),
  },
  "timeline.exit-sequence": {
    id: "timeline.exit-sequence",
    label: "Close sequence",
    shortcut: "Alt+↑",
    isEnabled: (s) => s.sequencePath.length > 0,
    run: (s) => void s.closeSequence(),
    matchKey: (e, mod) => !mod && e.altKey && e.key === "ArrowUp",
  },
  "edit.split": {
    id: "edit.split",
    label: "Split at playhead",
    shortcut: "S",
    isEnabled: hasSelection,
    run: () => void splitSelectedAtPlayhead(),
    matchKey: (e, mod) => !mod && (e.key === "s" || e.key === "S"),
  },
  "playback.toggle": {
    id: "playback.toggle",
    label: "Play / Pause",
    shortcut: "Space",
    isEnabled: hasProject,
    run: (s) => {
      if (s.playing) s.stopPlayback();
      else void s.startPlayback();
    },
    matchKey: (e, mod) => !mod && e.code === "Space",
  },
  "playback.home": {
    id: "playback.home",
    label: "Go to start",
    shortcut: "Home",
    isEnabled: hasProject,
    run: (s) => void s.seekTo(0),
    matchKey: (e, mod) => !mod && e.key === "Home",
  },
  "playback.end": {
    id: "playback.end",
    label: "Go to end",
    shortcut: "End",
    isEnabled: hasProject,
    run: (s) => {
      if (s.project) void s.seekTo(Math.max(containerDurationSecs(s.project, s.activeSequenceId), 1));
    },
    matchKey: (e, mod) => !mod && e.key === "End",
  },
  "playback.step-back": {
    id: "playback.step-back",
    label: "Step back",
    shortcut: "←",
    isEnabled: hasProject,
    run: (s) => {
      const fps = s.project?.settings.fps ?? 30;
      void s.seekTo(s.playhead - 1 / fps);
    },
    matchKey: (e, mod) => !mod && e.key === "ArrowLeft" && !e.shiftKey,
  },
  "playback.step-forward": {
    id: "playback.step-forward",
    label: "Step forward",
    shortcut: "→",
    isEnabled: hasProject,
    run: (s) => {
      const fps = s.project?.settings.fps ?? 30;
      void s.seekTo(s.playhead + 1 / fps);
    },
    matchKey: (e, mod) => !mod && e.key === "ArrowRight" && !e.shiftKey,
  },
  "playback.jump-back": {
    id: "playback.jump-back",
    label: "Jump back 1s",
    shortcut: "Shift+←",
    isEnabled: hasProject,
    run: (s) => void s.seekTo(s.playhead - 1),
    matchKey: (e, mod) => !mod && e.key === "ArrowLeft" && e.shiftKey,
  },
  "playback.jump-forward": {
    id: "playback.jump-forward",
    label: "Jump forward 1s",
    shortcut: "Shift+→",
    isEnabled: hasProject,
    run: (s) => void s.seekTo(s.playhead + 1),
    matchKey: (e, mod) => !mod && e.key === "ArrowRight" && e.shiftKey,
  },
  "tool.select": {
    id: "tool.select",
    label: "Select tool",
    shortcut: "V",
    isEnabled: () => true,
    run: (s) => s.setTool("select"),
    matchKey: (e, mod) => !mod && (e.key === "v" || e.key === "V"),
  },
  "tool.razor": {
    id: "tool.razor",
    label: "Razor tool",
    shortcut: "C",
    isEnabled: () => true,
    run: (s) => s.setTool("razor"),
    matchKey: (e, mod) => !mod && (e.key === "c" || e.key === "C"),
  },
  "tool.mask": {
    id: "tool.mask",
    label: "Mask tool",
    shortcut: "M",
    isEnabled: () => true,
    run: (s) => s.setTool("mask"),
    matchKey: (e, mod) => !mod && (e.key === "m" || e.key === "M"),
  },
  "timeline.zoom-in": {
    id: "timeline.zoom-in",
    label: "Zoom in",
    shortcut: "+",
    isEnabled: hasProject,
    run: (s) => s.setZoom(s.pxPerSec + 20),
    matchKey: (e, mod) => !mod && (e.key === "+" || e.key === "="),
  },
  "timeline.zoom-out": {
    id: "timeline.zoom-out",
    label: "Zoom out",
    shortcut: "-",
    isEnabled: hasProject,
    run: (s) => s.setZoom(s.pxPerSec - 20),
    matchKey: (e, mod) => !mod && (e.key === "-" || e.key === "_"),
  },
  "view.toggle-perf-hud": {
    id: "view.toggle-perf-hud",
    label: "Toggle perf HUD",
    shortcut: "Ctrl+Shift+P",
    isEnabled: () => true,
    run: (s) => s.togglePerfHud(),
    matchKey: (e, mod) => mod && e.shiftKey && (e.key === "p" || e.key === "P"),
  },
  /** Dev flag for light theme (D1) — cycles dark → light → system. */
  "view.cycle-theme": {
    id: "view.cycle-theme",
    label: "Cycle theme",
    shortcut: "Ctrl+Shift+L",
    isEnabled: () => true,
    run: () => {
      cycleTheme();
      resetTimelineThemeCache();
      useEditorStore.getState().bumpThemeEpoch();
    },
    matchKey: (e, mod) => mod && e.shiftKey && (e.key === "l" || e.key === "L"),
  },
  /** Opens the Agent tab in the right column and focuses its prompt box. */
  "view.focus-agent": {
    id: "view.focus-agent",
    label: "Ask the agent",
    shortcut: "Ctrl+L",
    isEnabled: () => true,
    run: () => useAgentChatStore.getState().requestPromptFocus(),
    matchKey: (e, mod) => mod && !e.shiftKey && (e.key === "l" || e.key === "L"),
  },
};

export function getAction(id: string): EditorAction | undefined {
  return ACTIONS[id];
}

export function invokeAction(id: string, ctx?: ActionContext): boolean {
  const action = ACTIONS[id];
  if (!action) return false;
  const state = useEditorStore.getState();
  if (!action.isEnabled(state)) return false;
  void action.run(state, ctx);
  return true;
}

/** List actions currently enabled — foundation for a future command palette. */
export function listEnabledActions(): EditorAction[] {
  const state = useEditorStore.getState();
  return Object.values(ACTIONS).filter((a) => a.isEnabled(state));
}

/**
 * Global keydown router. Returns true if an action handled the event (caller should
 * typically `preventDefault` — we do that here when matching).
 */
export function handleGlobalKeyDown(e: KeyboardEvent, ctx?: ActionContext): boolean {
  const target = e.target;
  if (
    target instanceof HTMLInputElement ||
    target instanceof HTMLTextAreaElement ||
    target instanceof HTMLSelectElement
  ) {
    return false;
  }
  // A `<dialog>` opened via `showModal()` traps focus natively; don't steal its keys.
  if (target instanceof Element && target.closest("dialog[open]")) return false;

  const mod = e.ctrlKey || e.metaKey;
  const state = useEditorStore.getState();

  // Prefer mod chords first so Ctrl+V doesn't also match tool.select's bare "v".
  const ordered = Object.values(ACTIONS).sort((a, b) => {
    const am = a.shortcut?.includes("Ctrl") || a.shortcut?.includes("Cmd") ? 0 : 1;
    const bm = b.shortcut?.includes("Ctrl") || b.shortcut?.includes("Cmd") ? 0 : 1;
    return am - bm;
  });

  for (const action of ordered) {
    if (!action.matchKey?.(e, mod)) continue;
    if (!action.isEnabled(state)) {
      // Matched chord but disabled — still consume so e.g. bare letters don't leak.
      if (mod || e.code === "Space" || e.key === "Delete" || e.key === "Backspace") {
        e.preventDefault();
      }
      return true;
    }
    e.preventDefault();
    void action.run(state, ctx);
    return true;
  }
  return false;
}
