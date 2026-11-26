// Every editor action and its default keys. Buttons and menus show these labels and shortcuts;
// the shortcuts list (?) shows this whole table.

import {
  cutPoints,
  evaluateSequence,
  flicksPerFrame,
  FLICKS_PER_SECOND,
  sequenceDuration,
} from "@renderly/core";
import type { Action, ActionContext } from "./registry";

const writable = (ctx: ActionContext) => !ctx.readOnly;
const sequenceOf = (ctx: ActionContext) => {
  const id = ctx.view.get().sequence;
  return ctx.store.project.sequences.find((s) => s.id === id) ?? ctx.store.project.sequences[0];
};
const hasClips = (ctx: ActionContext) =>
  sequenceOf(ctx)?.tracks.some((track) => track.clips.length > 0) ?? false;
const editSelection = (ctx: ActionContext) =>
  writable(ctx) && (ctx.timeline?.canEditSelection ?? false);
const playable = (ctx: ActionContext) => ctx.link !== null && hasClips(ctx);

/** Moves the playhead (pausing first), by frames or to a time. */
/**
 * Moving the playhead needs only the timeline, not the engine: these keys work while the engine is
 * still loading (on a slow machine that takes a while), and pause playback first when it plays.
 */
const canMove = (ctx: ActionContext) => ctx.timeline !== null;
function seekTo(ctx: ActionContext, t: number) {
  void (ctx.link?.pause() ?? Promise.resolve()).then(() => ctx.timeline?.movePlayhead(t));
}
function seekBy(ctx: ActionContext, frames: number) {
  const sequence = sequenceOf(ctx);
  if (!sequence) return;
  const frame = flicksPerFrame(sequence.fps);
  void (ctx.link?.pause() ?? Promise.resolve()).then(() => {
    const at = Math.round(ctx.view.get().playhead / frame) * frame;
    ctx.timeline?.movePlayhead(at + frames * frame);
  });
}
/** The previous or next cut on any drawing layer, from the project itself. */
function jump(ctx: ActionContext, direction: -1 | 1) {
  const sequence = sequenceOf(ctx);
  if (!sequence) return;
  const graph = evaluateSequence(ctx.store.project, sequence.id);
  const points = [...cutPoints(graph), graph.duration];
  const t = ctx.view.get().playhead;
  const target =
    direction > 0 ? points.find((p) => p > t) : [...points].reverse().find((p) => p < t);
  if (target !== undefined) seekTo(ctx, target);
}

export const EDITOR_ACTIONS: Action[] = [
  // --- project ---
  {
    id: "app.home",
    label: "Home",
    group: "Project",
    icon: "home",
    run: (ctx) => ctx.navigateHome(),
  },
  {
    id: "help.shortcuts",
    label: "Keyboard shortcuts",
    group: "Help",
    icon: "info",
    keys: ["Shift+/", "F1"],
    run: (ctx) => ctx.showShortcuts(),
  },

  // --- edit ---
  {
    id: "edit.undo",
    label: "Undo",
    group: "Edit",
    icon: "undo",
    keys: ["Mod+Z"],
    repeat: true,
    enabled: (ctx) => writable(ctx) && ctx.store.canUndo,
    run: (ctx) => ctx.store.undo(),
  },
  {
    id: "edit.redo",
    label: "Redo",
    group: "Edit",
    icon: "redo",
    keys: ["Mod+Shift+Z", "Mod+Y"],
    repeat: true,
    enabled: (ctx) => writable(ctx) && ctx.store.canRedo,
    run: (ctx) => ctx.store.redo(),
  },

  // --- playback ---
  {
    id: "playback.toggle",
    label: "Play or pause",
    group: "Playback",
    keys: ["Space"],
    enabled: playable,
    run: (ctx) => ctx.link?.toggle(),
  },
  {
    id: "playback.play",
    label: "Play",
    group: "Playback",
    icon: "play",
    keys: ["L"],
    enabled: playable,
    run: (ctx) => {
      if (!ctx.view.get().playing) ctx.link?.play();
    },
  },
  {
    id: "playback.pause",
    label: "Pause",
    group: "Playback",
    icon: "pause",
    keys: ["K"],
    enabled: (ctx) => ctx.link !== null,
    run: (ctx) => ctx.link?.pause(),
  },
  {
    id: "playback.back",
    label: "Back one second (paused; reverse play is not supported yet)",
    group: "Playback",
    keys: ["J"],
    repeat: true,
    enabled: playable,
    run: (ctx) => {
      const sequence = sequenceOf(ctx);
      if (sequence) seekBy(ctx, -Math.round(FLICKS_PER_SECOND / flicksPerFrame(sequence.fps)));
    },
  },
  {
    id: "playback.start",
    label: "Go to the start",
    group: "Playback",
    icon: "to-start",
    keys: ["Home"],
    enabled: canMove,
    run: (ctx) => seekTo(ctx, 0),
  },
  {
    id: "playback.end",
    label: "Go to the end",
    group: "Playback",
    icon: "to-end",
    keys: ["End"],
    enabled: canMove,
    run: (ctx) => {
      const sequence = sequenceOf(ctx);
      if (sequence) seekTo(ctx, sequenceDuration(sequence));
    },
  },
  {
    id: "playback.frameBack",
    label: "Step back one frame",
    group: "Playback",
    icon: "frame-back",
    keys: ["ArrowLeft"],
    repeat: true,
    enabled: canMove,
    run: (ctx) => seekBy(ctx, -1),
  },
  {
    id: "playback.frameForward",
    label: "Step forward one frame",
    group: "Playback",
    icon: "frame-forward",
    keys: ["ArrowRight"],
    repeat: true,
    enabled: canMove,
    run: (ctx) => seekBy(ctx, 1),
  },
  {
    id: "playback.tenBack",
    label: "Step back ten frames",
    group: "Playback",
    keys: ["Shift+ArrowLeft"],
    repeat: true,
    enabled: canMove,
    run: (ctx) => seekBy(ctx, -10),
  },
  {
    id: "playback.tenForward",
    label: "Step forward ten frames",
    group: "Playback",
    keys: ["Shift+ArrowRight"],
    repeat: true,
    enabled: canMove,
    run: (ctx) => seekBy(ctx, 10),
  },
  {
    id: "playback.previousCut",
    label: "Go to the previous cut",
    group: "Playback",
    icon: "previous-cut",
    keys: ["ArrowUp"],
    repeat: true,
    enabled: canMove,
    run: (ctx) => jump(ctx, -1),
  },
  {
    id: "playback.nextCut",
    label: "Go to the next cut",
    group: "Playback",
    icon: "next-cut",
    keys: ["ArrowDown"],
    repeat: true,
    enabled: canMove,
    run: (ctx) => jump(ctx, 1),
  },
  {
    id: "playback.loop",
    label: "Loop the selection, or the whole sequence",
    group: "Playback",
    icon: "loop",
    keys: ["Shift+L"],
    enabled: (ctx) => ctx.link !== null,
    pressed: (ctx) => ctx.view.get().loop,
    run: (ctx) => ctx.view.set({ loop: !ctx.view.get().loop }),
  },

  // --- viewer ---
  ...(
    [
      ["viewer.fit", "Fit the picture to the viewer", null, ["Shift+F"]],
      ["viewer.zoom25", "Show the picture at 25%", 0.25, []],
      ["viewer.zoom50", "Show the picture at 50%", 0.5, []],
      ["viewer.zoom100", "Show the picture at 100%", 1, []],
      ["viewer.zoom200", "Show the picture at 200%", 2, []],
    ] as const
  ).map(([id, label, zoom, keys]): Action => ({
    id,
    label,
    group: "Viewer",
    keys,
    enabled: (ctx) => ctx.viewer !== null,
    pressed: (ctx) => ctx.viewer?.zoom === zoom,
    run: (ctx) => ctx.viewer?.setZoom(zoom),
  })),

  {
    id: "viewer.safeArea",
    label: "Show the safe areas",
    group: "Viewer",
    icon: "safe-area",
    keys: ["'"],
    enabled: (ctx) => ctx.viewer !== null,
    pressed: (ctx) => ctx.viewer?.safeArea ?? false,
    run: (ctx) => ctx.viewer?.setOverlay({ safeArea: !ctx.viewer.safeArea }),
  },
  {
    id: "viewer.thirds",
    label: "Show a rule-of-thirds grid",
    group: "Viewer",
    icon: "thirds",
    keys: ["Shift+'"],
    enabled: (ctx) => ctx.viewer !== null,
    pressed: (ctx) => ctx.viewer?.thirds ?? false,
    run: (ctx) => ctx.viewer?.setOverlay({ thirds: !ctx.viewer.thirds }),
  },

  // --- timeline ---
  {
    id: "timeline.selectTool",
    label: "Selection tool",
    group: "Timeline",
    icon: "select-tool",
    keys: ["V"],
    pressed: (ctx) => ctx.timeline?.tool === "select",
    run: (ctx) => ctx.timeline?.setTool("select"),
  },
  {
    id: "timeline.bladeTool",
    label: "Blade tool",
    group: "Timeline",
    icon: "blade-tool",
    keys: ["B"],
    pressed: (ctx) => ctx.timeline?.tool === "blade",
    run: (ctx) => ctx.timeline?.setTool("blade"),
  },
  {
    id: "timeline.split",
    label: "Split at the playhead",
    group: "Timeline",
    icon: "split",
    keys: ["S"],
    enabled: (ctx) => writable(ctx) && hasClips(ctx),
    run: (ctx) => ctx.timeline?.splitAtPlayhead(),
  },
  {
    id: "timeline.delete",
    label: "Delete the selected clips",
    group: "Timeline",
    icon: "delete",
    keys: ["Delete", "Backspace"],
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.deleteSelected(false),
  },
  {
    id: "timeline.rippleDelete",
    label: "Ripple delete: delete and close the gap",
    group: "Timeline",
    keys: ["Shift+Delete", "Shift+Backspace"],
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.deleteSelected(true),
  },
  {
    id: "timeline.selectAll",
    label: "Select all clips",
    group: "Timeline",
    keys: ["Mod+A"],
    enabled: hasClips,
    run: (ctx) => ctx.timeline?.selectAll(),
  },
  {
    id: "timeline.selectAtPlayhead",
    label: "Select the clips under the playhead",
    group: "Timeline",
    keys: ["D"],
    enabled: hasClips,
    run: (ctx) => ctx.timeline?.selectAtPlayhead(),
  },
  {
    id: "timeline.deselect",
    label: "Clear the selection",
    group: "Timeline",
    keys: ["Escape"],
    enabled: (ctx) => ctx.timeline?.hasSelection ?? false,
    run: (ctx) => ctx.timeline?.deselect(),
  },
  {
    id: "timeline.nudgeBack",
    label: "Move the selected clips one frame earlier",
    group: "Timeline",
    keys: [","],
    repeat: true,
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.nudge(-1),
  },
  {
    id: "timeline.nudgeForward",
    label: "Move the selected clips one frame later",
    group: "Timeline",
    keys: ["."],
    repeat: true,
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.nudge(1),
  },
  {
    id: "timeline.nudgeBackTen",
    label: "Move the selected clips ten frames earlier",
    group: "Timeline",
    keys: ["Shift+,"],
    repeat: true,
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.nudge(-10),
  },
  {
    id: "timeline.nudgeForwardTen",
    label: "Move the selected clips ten frames later",
    group: "Timeline",
    keys: ["Shift+."],
    repeat: true,
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.nudge(10),
  },
  {
    id: "timeline.trackUp",
    label: "Move the selected clips to the track above",
    group: "Timeline",
    keys: ["Alt+ArrowUp"],
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.moveToTrack(1),
  },
  {
    id: "timeline.trackDown",
    label: "Move the selected clips to the track below",
    group: "Timeline",
    keys: ["Alt+ArrowDown"],
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.moveToTrack(-1),
  },
  {
    id: "timeline.trimStart",
    label: "Trim the start of the selected clips to the playhead",
    group: "Timeline",
    keys: ["["],
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.trimToPlayhead("start"),
  },
  {
    id: "timeline.trimEnd",
    label: "Trim the end of the selected clips to the playhead",
    group: "Timeline",
    keys: ["]"],
    enabled: editSelection,
    run: (ctx) => ctx.timeline?.trimToPlayhead("end"),
  },
  {
    id: "timeline.snapping",
    label: "Snapping",
    group: "Timeline",
    icon: "magnetic",
    keys: ["N"],
    pressed: (ctx) => ctx.timeline?.snapping ?? true,
    run: (ctx) => ctx.timeline?.setSnapping(!ctx.timeline.snapping),
  },
  {
    id: "timeline.zoomIn",
    label: "Zoom in",
    group: "Timeline",
    icon: "zoom-in",
    keys: ["=", "Shift+="],
    repeat: true,
    run: (ctx) => ctx.timeline?.zoomBy(1 / 1.5),
  },
  {
    id: "timeline.zoomOut",
    label: "Zoom out",
    group: "Timeline",
    icon: "zoom-out",
    keys: ["-"],
    repeat: true,
    run: (ctx) => ctx.timeline?.zoomBy(1.5),
  },
  {
    id: "timeline.fit",
    label: "Zoom to fit",
    group: "Timeline",
    icon: "zoom-fit",
    keys: ["Shift+Z", "\\"],
    run: (ctx) => ctx.timeline?.fit(),
  },
  {
    id: "timeline.addVideoTrack",
    label: "Add a video track",
    group: "Timeline",
    icon: "video",
    enabled: writable,
    run: (ctx) => ctx.timeline?.addTrack("video"),
  },
  {
    id: "timeline.addAudioTrack",
    label: "Add an audio track",
    group: "Timeline",
    icon: "audio",
    enabled: writable,
    run: (ctx) => ctx.timeline?.addTrack("audio"),
  },

  // --- media ---
  {
    id: "media.import",
    label: "Import",
    group: "Media",
    icon: "import",
    keys: ["Mod+I"],
    enabled: (ctx) => writable(ctx) && ctx.bin !== null,
    run: (ctx) => ctx.bin?.importFiles(),
  },
  {
    id: "media.importFolder",
    label: "Import a folder",
    group: "Media",
    icon: "folder",
    enabled: (ctx) => writable(ctx) && (ctx.bin?.canImportFolder ?? false),
    run: (ctx) => ctx.bin?.importFolder(),
  },
  {
    id: "media.append",
    label: "Add to the timeline at the playhead",
    group: "Media",
    icon: "add",
    keys: ["Enter", "E"],
    scope: "[data-media-id]",
    enabled: writable,
    run: (ctx, args) => {
      const media = mediaArg(ctx, args);
      if (media) ctx.timeline?.appendMedia(media);
    },
  },
  {
    id: "media.showGrid",
    label: "Grid",
    group: "Media",
    icon: "grid",
    enabled: (ctx) => ctx.bin !== null,
    pressed: (ctx) => ctx.bin?.view === "grid",
    run: (ctx) => ctx.bin?.setView("grid"),
  },
  {
    id: "media.showList",
    label: "List",
    group: "Media",
    icon: "list",
    enabled: (ctx) => ctx.bin !== null,
    pressed: (ctx) => ctx.bin?.view === "list",
    run: (ctx) => ctx.bin?.setView("list"),
  },
  {
    id: "media.relink",
    label: "Relink…",
    group: "Media",
    icon: "relink",
    enabled: writable,
    run: (ctx, args) => {
      const media = mediaArg(ctx, args);
      if (media) ctx.bin?.relink(media);
    },
  },
  {
    id: "media.reconnect",
    label: "Reconnect",
    group: "Media",
    icon: "reconnect",
    run: (ctx) => ctx.bin?.reconnect(),
  },
  {
    id: "media.remove",
    label: "Remove from project",
    group: "Media",
    icon: "delete",
    keys: ["Delete", "Backspace"],
    scope: "[data-media-id]",
    enabled: writable,
    run: (ctx, args) => {
      const media = mediaArg(ctx, args);
      if (media) ctx.bin?.remove(media);
    },
  },
];

/** The media item an action is for: given, or the focused bin item. */
function mediaArg(ctx: ActionContext, args?: Record<string, unknown>): string | null {
  return (args?.["media"] as string | undefined) ?? ctx.bin?.focused() ?? null;
}
