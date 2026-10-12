import { beforeEach, describe, expect, it, vi } from "vitest";
import { captionClip, project as makeProject, track, videoClip } from "../test/fixtures";
import type { Project } from "../lib/types";

// The store talks to the Rust backend only through lib/ipc, which in turn wraps
// @tauri-apps/api/*. Mocking ipc wholesale keeps the Tauri modules out of the test graph.
const ipc = vi.hoisted(() => ({
  applyCommand: vi.fn(),
  applyCommands: vi.fn(),
  undo: vi.fn(),
  redo: vi.fn(),
  refreshFrame: vi.fn(),
  getProject: vi.fn(),
  setEditorSelection: vi.fn(),
  setActiveSequence: vi.fn(),
  seek: vi.fn(),
  scrubAudio: vi.fn(),
  pause: vi.fn(),
  play: vi.fn(),
  assetUrl: vi.fn((p: string) => `asset://${p}`),
  onProjectChanged: vi.fn(),
  onPlaybackTick: vi.fn(() => () => {}),
  onPlaybackState: vi.fn(() => () => {}),
  onPlaybackError: vi.fn(() => () => {}),
  onThumbnailsReady: vi.fn(() => () => {}),
  onWaveformReady: vi.fn(() => () => {}),
  onPlaybackPerf: vi.fn(() => () => {}),
  onBridgePlayhead: vi.fn(() => () => {}),
}));
vi.mock("../lib/ipc", () => ipc);

import { connectStoreToBackendEvents, useEditorStore } from "./editorStore";

const initial = useEditorStore.getState();
const store = () => useEditorStore.getState();
const lastToast = () => store().toasts[store().toasts.length - 1];

// A: video [0,2]   B: video [3,5] on v1;  C: audio [1,3] on a1;  K: caption [2,3] on c1
const A = videoClip("A", 0, 0, 2);
const B = videoClip("B", 3, 1, 3);
const C = { ...videoClip("C", 1, 0, 2), type: "audio" as const };
const K = captionClip("K", 2, 1);

function sample(): Project {
  return makeProject([
    track("v1", "video", [A, B]),
    track("a1", "audio", [C]),
    track("c1", "caption", [K]),
  ]);
}

function load(p: Project = sample(), extra: Record<string, unknown> = {}) {
  useEditorStore.setState({ project: p, projectPath: "/tmp/p.renderly", clientRevision: 1, ...extra });
}

const ok = (patch: unknown[] = [], revision = 2) => ({ revision, patch, outcome: "Applied", outcomes: [] });

beforeEach(() => {
  vi.clearAllMocks();
  useEditorStore.setState(initial, true);
  ipc.refreshFrame.mockResolvedValue(undefined);
  ipc.setEditorSelection.mockResolvedValue(undefined);
  ipc.setActiveSequence.mockResolvedValue(undefined);
  ipc.scrubAudio.mockResolvedValue(undefined);
  ipc.seek.mockResolvedValue(undefined);
  ipc.pause.mockResolvedValue(7);
  ipc.play.mockResolvedValue(undefined);
  ipc.getProject.mockResolvedValue({ project: sample(), revision: 99 });
  // toast() schedules removal through window.setTimeout; no DOM needed otherwise.
  vi.stubGlobal("window", { setTimeout: () => 0 });
  vi.stubGlobal("document", { getElementById: () => null });
});

describe("selection", () => {
  const sel = (trackId: string, clipId: string) => ({ trackId, clipId });

  it("select sets primary + list and mirrors to the backend", () => {
    store().select(sel("v1", "A"));
    expect(store().selection).toEqual(sel("v1", "A"));
    expect(store().selections).toEqual([sel("v1", "A")]);
    expect(ipc.setEditorSelection).toHaveBeenLastCalledWith({ primary: sel("v1", "A"), all: [sel("v1", "A")] });
    store().select(null);
    expect(store().selections).toEqual([]);
    expect(ipc.setEditorSelection).toHaveBeenLastCalledWith(null);
  });

  it("toggleSelect adds same-kind clips and removes on a second toggle", () => {
    load();
    store().select(sel("v1", "A"));
    store().toggleSelect(sel("v1", "B"));
    expect(store().selections).toEqual([sel("v1", "A"), sel("v1", "B")]);
    store().toggleSelect(sel("v1", "B"));
    expect(store().selections).toEqual([sel("v1", "A")]);
  });

  it("toggleSelect replaces the selection when the new clip is on a different track kind", () => {
    load();
    store().select(sel("v1", "A"));
    store().toggleSelect(sel("a1", "C"));
    expect(store().selections).toEqual([sel("a1", "C")]);
    expect(store().selection).toEqual(sel("a1", "C"));
  });

  it("toggling off the primary promotes the next clip; toggling off the last clears it", () => {
    load();
    store().setSelections([sel("v1", "A"), sel("v1", "B")]);
    store().toggleSelect(sel("v1", "A"));
    expect(store().selection).toEqual(sel("v1", "B"));
    store().toggleSelect(sel("v1", "B"));
    expect(store().selection).toBeNull();
    expect(ipc.setEditorSelection).toHaveBeenLastCalledWith(null);
  });

  it("setSelections uses the first entry as primary", () => {
    store().setSelections([sel("v1", "B"), sel("v1", "A")]);
    expect(store().selection).toEqual(sel("v1", "B"));
    store().setSelections([]);
    expect(store().selection).toBeNull();
  });

  it("openContextMenu on an unselected clip selects only it", () => {
    load();
    store().setSelections([sel("v1", "A"), sel("v1", "B")]);
    store().openContextMenu(10, 20, "a1", "C", 1.5);
    expect(store().selections).toEqual([sel("a1", "C")]);
    expect(store().contextMenu).toEqual({ x: 10, y: 20, trackId: "a1", clipId: "C", atSecs: 1.5 });
  });

  it("openContextMenu on a member of a multi-selection keeps the group and promotes it to primary", () => {
    load();
    store().setSelections([sel("v1", "A"), sel("v1", "B")]);
    store().openContextMenu(0, 0, "v1", "B", 0);
    expect(store().selections).toEqual([sel("v1", "B"), sel("v1", "A")]);
    expect(store().selection).toEqual(sel("v1", "B"));
  });

  it("pruneStaleSelection drops clips that no longer exist and re-elects the primary", () => {
    load();
    store().setSelections([sel("v1", "A"), sel("v1", "B"), sel("v1", "GONE"), sel("nope", "A")]);
    store().pruneStaleSelection();
    expect(store().selections).toEqual([sel("v1", "A"), sel("v1", "B")]);
    store().setSelections([sel("v1", "GONE"), sel("v1", "B")]);
    store().pruneStaleSelection();
    expect(store().selection).toEqual(sel("v1", "B"));
    store().setSelections([sel("v1", "GONE")]);
    store().pruneStaleSelection();
    expect(store().selection).toBeNull();
  });
});

describe("playhead & transport", () => {
  it("setPlayheadLocal clamps below zero", () => {
    store().setPlayheadLocal(-3);
    expect(store().playhead).toBe(0);
    store().setPlayheadLocal(4.25);
    expect(store().playhead).toBe(4.25);
  });

  it("seekTo clamps, updates the playhead optimistically and tells the backend", async () => {
    await store().seekTo(-1);
    expect(store().playhead).toBe(0);
    expect(ipc.seek).toHaveBeenLastCalledWith(0);
    await store().seekTo(6.5);
    expect(store().playhead).toBe(6.5);
    expect(ipc.seek).toHaveBeenLastCalledWith(6.5);
  });

  it("seekTo surfaces backend errors as a toast without losing the playhead", async () => {
    ipc.seek.mockRejectedValueOnce("seek failed: boom");
    await store().seekTo(2);
    expect(store().playhead).toBe(2);
    expect(lastToast()).toMatchObject({ kind: "error", message: "seek failed: boom" });
  });

  it("scrubAt clamps and scrubs audio", () => {
    store().scrubAt(-9);
    expect(store().playhead).toBe(0);
    store().scrubAt(1.2);
    expect(ipc.scrubAudio).toHaveBeenLastCalledWith(1.2);
  });

  it("playback tick/state events overwrite the playhead; stopping clears the perf sample", () => {
    const sample = { fps: 60 } as never;
    store().onPlaybackPerf(sample);
    store().onPlaybackState({ playing: true, time_secs: 1 });
    expect(store().perfSample).toBe(sample);
    store().onPlaybackTick({ time_secs: 2, playing: true });
    expect(store().playhead).toBe(2);
    store().onPlaybackState({ playing: false, time_secs: 3 });
    expect(store()).toMatchObject({ playing: false, playhead: 3, perfSample: null });
  });

  it("startPlayback needs a project, and rolls back `playing` if the backend refuses", async () => {
    await store().startPlayback();
    expect(ipc.play).not.toHaveBeenCalled();
    load();
    ipc.play.mockRejectedValueOnce("no audio device");
    await store().startPlayback();
    expect(store().playing).toBe(false);
    expect(lastToast()?.kind).toBe("error");
  });

  it("stopPlayback adopts the time the backend reports", async () => {
    useEditorStore.setState({ playing: true });
    store().stopPlayback();
    expect(store().playing).toBe(false);
    await Promise.resolve();
    await Promise.resolve();
    expect(store().playhead).toBe(7);
  });
});

describe("zoom / scroll", () => {
  it("setZoom clamps to 20..400 px/s", () => {
    store().setZoom(5);
    expect(store().pxPerSec).toBe(20);
    store().setZoom(9999);
    expect(store().pxPerSec).toBe(400);
    store().setZoom(123);
    expect(store().pxPerSec).toBe(123);
  });

  it("setScroll clamps into [0, max] using the canvas size", () => {
    // 10s clip at 80px/s: content = 88+8+800+200 = 1096; viewport 800 -> maxX 296
    load(makeProject([track("v", "video", [videoClip("X", 0, 0, 10)])]));
    vi.stubGlobal("document", { getElementById: () => ({ clientWidth: 800, clientHeight: 300 }) });
    store().setScroll(10_000, 10_000);
    expect(store().scrollX).toBe(296);
    expect(store().scrollY).toBe(0);
    store().setScroll(-50);
    expect(store().scrollX).toBe(0);
  });

  it("setScroll without a project pins scroll at zero", () => {
    store().setScroll(500, 500);
    expect(store()).toMatchObject({ scrollX: 0, scrollY: 0 });
  });

  it("panBy is relative to the current scroll", () => {
    load(makeProject([track("v", "video", [videoClip("X", 0, 0, 10)])]));
    vi.stubGlobal("document", { getElementById: () => ({ clientWidth: 800, clientHeight: 300 }) });
    store().setScroll(100, 0);
    store().panBy(50, 0);
    expect(store().scrollX).toBe(150);
  });

  it("ensurePlayheadVisible scrolls right when the playhead is off-screen", () => {
    load(makeProject([track("v", "video", [videoClip("X", 0, 0, 30)])]));
    vi.stubGlobal("document", { getElementById: () => ({ clientWidth: 800, clientHeight: 300 }) });
    useEditorStore.setState({ playhead: 20 }); // x = 96 + 1600 = 1696 > 800 - 64
    store().ensurePlayheadVisible();
    expect(store().scrollX).toBe(1696 - (800 - 64));
  });
});

describe("applying backend JSON patches (dispatch)", () => {
  it("applies a replace patch and records the new revision", async () => {
    load();
    ipc.applyCommand.mockResolvedValue(
      ok([{ op: "replace", path: "/tracks/0/clips/0/position_secs", value: 10 }], 5),
    );
    expect(await store().dispatch({ command: "MoveClip" })).toBe(true);
    expect(store().project!.tracks[0].clips[0].position_secs).toBe(10);
    expect(store().clientRevision).toBe(5);
    expect(ipc.getProject).not.toHaveBeenCalled();
  });

  it("never mutates the previous project object (React/zustand identity contract)", async () => {
    load();
    const before = store().project!;
    const snapshot = JSON.stringify(before);
    ipc.applyCommand.mockResolvedValue(ok([{ op: "replace", path: "/name", value: "renamed" }]));
    await store().dispatch({});
    expect(store().project).not.toBe(before);
    expect(store().project!.name).toBe("renamed");
    expect(JSON.stringify(before)).toBe(snapshot);
  });

  it("supports add / remove / move / copy / test ops", async () => {
    load();
    ipc.applyCommand.mockResolvedValue(
      ok([
        { op: "add", path: "/tracks/1/clips/-", value: { ...C, id: "C2", position_secs: 9 } },
        { op: "remove", path: "/tracks/0/clips/1" },
        { op: "copy", from: "/name", path: "/id" },
        { op: "move", from: "/tracks/2/name", path: "/tracks/2/name2" },
        { op: "test", path: "/name", value: "test" },
      ]),
    );
    await store().dispatch({});
    const p = store().project!;
    expect(p.tracks[1].clips.map((c) => c.id)).toEqual(["C", "C2"]);
    expect(p.tracks[0].clips.map((c) => c.id)).toEqual(["A"]);
    expect(p.id).toBe("test");
  });

  it("a patch that throws (stale path) leaves the project alone and falls back to a full refetch", async () => {
    load();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    ipc.applyCommand.mockResolvedValue(ok([{ op: "replace", path: "/tracks/9/name", value: "x" }], 4));
    expect(await store().dispatch({})).toBe(true);
    expect(ipc.getProject).toHaveBeenCalledTimes(1);
    expect(store().clientRevision).toBe(99);
    warn.mockRestore();
  });

  it("is a no-op toast when no project is open", async () => {
    expect(await store().dispatch({ command: "X" })).toBe(false);
    expect(ipc.applyCommand).not.toHaveBeenCalled();
    expect(lastToast()).toMatchObject({ kind: "error" });
  });

  it("a rejected command toasts (unless quiet) and resyncs from the backend", async () => {
    load();
    ipc.applyCommand.mockRejectedValue("clip overlaps another clip");
    expect(await store().dispatch({})).toBe(false);
    expect(lastToast()?.message).toBe("clip overlaps another clip");
    expect(ipc.getProject).toHaveBeenCalledTimes(1);

    ipc.applyCommand.mockRejectedValue("nope");
    const toastsBefore = store().toasts.length;
    expect(await store().dispatch({}, true)).toBe(false);
    expect(store().toasts).toHaveLength(toastsBefore);
  });

  it("prefixes opaque errors but not self-describing ones", async () => {
    load();
    ipc.applyCommand.mockRejectedValueOnce(new Error("kaput"));
    await store().dispatch({});
    expect(lastToast()?.message).toBe("Edit failed: kaput");
  });

  it("passes the command and a mutation id to the backend, then refreshes the frame at the playhead", async () => {
    load(sample(), { playhead: 1.5 });
    ipc.applyCommand.mockResolvedValue(ok());
    await store().dispatch({ command: "SplitClip" });
    expect(ipc.applyCommand).toHaveBeenCalledWith({ command: "SplitClip" }, expect.any(String));
    expect(ipc.refreshFrame).toHaveBeenCalledWith(1.5);
  });

  it("a failing refreshFrame never fails the edit", async () => {
    load();
    ipc.applyCommand.mockResolvedValue(ok());
    ipc.refreshFrame.mockRejectedValue(new Error("no gpu"));
    expect(await store().dispatch({})).toBe(true);
  });

  it("dispatchBatch applies one patch for the whole batch", async () => {
    load();
    ipc.applyCommands.mockResolvedValue(
      ok(
        [
          { op: "replace", path: "/tracks/0/clips/0/position_secs", value: 1 },
          { op: "replace", path: "/tracks/0/clips/1/position_secs", value: 8 },
        ],
        3,
      ),
    );
    await store().dispatchBatch([{ a: 1 }, { b: 2 }]);
    expect(ipc.applyCommands).toHaveBeenCalledWith([{ a: 1 }, { b: 2 }], expect.any(String));
    expect(store().project!.tracks[0].clips.map((c) => c.position_secs)).toEqual([1, 8]);
  });

  it("refetchProject is skipped with no project path", async () => {
    await store().refetchProject();
    expect(ipc.getProject).not.toHaveBeenCalled();
  });
});

describe("undo / redo bookkeeping", () => {
  it("undo applies the patch, updates can-flags and prunes selections that vanished", async () => {
    load();
    store().setSelections([
      { trackId: "v1", clipId: "A" },
      { trackId: "v1", clipId: "B" },
    ]);
    ipc.undo.mockResolvedValue({
      can_undo: false,
      can_redo: true,
      revision: 7,
      patch: [{ op: "remove", path: "/tracks/0/clips/1" }],
    });
    await store().undo();
    expect(store()).toMatchObject({ canUndo: false, canRedo: true, clientRevision: 7 });
    expect(store().project!.tracks[0].clips.map((c) => c.id)).toEqual(["A"]);
    expect(store().selections).toEqual([{ trackId: "v1", clipId: "A" }]);
  });

  it("redo with an empty patch only advances the revision", async () => {
    load();
    const before = store().project;
    ipc.redo.mockResolvedValue({ can_undo: true, can_redo: false, revision: 8, patch: [] });
    await store().redo();
    expect(store().project).toBe(before);
    expect(store()).toMatchObject({ canUndo: true, canRedo: false, clientRevision: 8 });
  });

  it("a bad undo patch falls back to refetch", async () => {
    load();
    vi.spyOn(console, "warn").mockImplementation(() => {});
    ipc.undo.mockResolvedValue({
      can_undo: true,
      can_redo: true,
      revision: 3,
      patch: [{ op: "remove", path: "/tracks/5" }],
    });
    await store().undo();
    expect(ipc.getProject).toHaveBeenCalledTimes(1);
  });

  it("undo failure toasts with an 'Undo failed' prefix for opaque errors", async () => {
    load();
    ipc.undo.mockRejectedValue(new Error("empty stack"));
    await store().undo();
    expect(lastToast()?.message).toBe("Undo failed: empty stack");
  });
});

describe("project:changed listener (self-echo suppression)", () => {
  type Handler = (p: { mutation_id?: string; revision: number; can_undo: boolean; can_redo: boolean }) => void;
  let fire: Handler;

  beforeEach(() => {
    ipc.onProjectChanged.mockImplementation((cb: Handler) => {
      fire = cb;
      return () => {};
    });
    connectStoreToBackendEvents();
    load(sample(), { clientRevision: 5 });
  });

  it("always syncs undo/redo flags and marks saved", () => {
    fire({ revision: 5, can_undo: true, can_redo: false });
    expect(store()).toMatchObject({ canUndo: true, canRedo: false, saveStatus: "saved" });
  });

  it("refetches on an external edit (unknown mutation id, new revision)", () => {
    fire({ mutation_id: "someone-else", revision: 6, can_undo: true, can_redo: false });
    expect(ipc.getProject).toHaveBeenCalledTimes(1);
  });

  it("does not refetch when already at that revision", () => {
    fire({ revision: 5, can_undo: false, can_redo: false });
    expect(ipc.getProject).not.toHaveBeenCalled();
  });

  it("does not refetch while a timeline drag is in progress", () => {
    store().setDragging(true);
    fire({ mutation_id: "x", revision: 9, can_undo: true, can_redo: false });
    expect(ipc.getProject).not.toHaveBeenCalled();
  });

  it("ignores the echo of this tab's own in-flight mutation but not a different one", async () => {
    let release!: (v: unknown) => void;
    ipc.applyCommand.mockImplementation(() => new Promise((r) => (release = r)));
    const pending = store().dispatch({});
    const ownId = ipc.applyCommand.mock.calls[0][1] as string;

    fire({ mutation_id: ownId, revision: 6, can_undo: true, can_redo: false });
    expect(ipc.getProject).not.toHaveBeenCalled();
    fire({ mutation_id: "other", revision: 7, can_undo: true, can_redo: false });
    expect(ipc.getProject).toHaveBeenCalledTimes(1);

    release(ok([], 6));
    await pending;

    // After completion the id is forgotten: a late duplicate with a new revision refetches.
    ipc.getProject.mockClear();
    fire({ mutation_id: ownId, revision: 10, can_undo: true, can_redo: false });
    expect(ipc.getProject).toHaveBeenCalledTimes(1);
  });
});

describe("clipboard / duplicate", () => {
  beforeEach(() => {
    ipc.applyCommand.mockResolvedValue(ok());
    ipc.applyCommands.mockResolvedValue(ok());
  });

  it("copySelection stores clips with their track kind; no-ops on an empty selection", () => {
    load();
    store().copySelection();
    expect(store().clipboard).toBeNull();
    store().setSelections([{ trackId: "v1", clipId: "A" }, { trackId: "a1", clipId: "C" }]);
    store().copySelection();
    expect(store().clipboard!.items.map((i) => [i.clip.id, i.trackKind])).toEqual([["A", "video"], ["C", "audio"]]);
    expect(lastToast()?.message).toBe("Copied 2 clips");
  });

  it("duplicateSelection places a media clip's copy right after the original (speed-aware)", async () => {
    const fast = videoClip("F", 4, 0, 6, { speed: 2 }); // 3s long
    load(makeProject([track("v1", "video", [fast])]));
    store().setSelections([{ trackId: "v1", clipId: "F" }]);
    await store().duplicateSelection();
    expect(ipc.applyCommand).toHaveBeenCalledWith(
      { command: "AddClip", track_id: "v1", media_id: "m-F", position_secs: 7, source_in_secs: 0, source_out_secs: 6 },
      expect.any(String),
    );
  });

  it("duplicating a caption emits AddCaption with its text/style", async () => {
    load();
    store().setSelections([{ trackId: "c1", clipId: "K" }]);
    await store().duplicateSelection();
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({
      command: "AddCaption",
      track_id: "c1",
      text: "hi",
      position_secs: 3,
      duration_secs: 1,
      style_id: "tiktok-minimal",
    });
  });

  it("duplicating several clips goes through one atomic batch", async () => {
    load();
    store().setSelections([{ trackId: "v1", clipId: "A" }, { trackId: "v1", clipId: "B" }]);
    await store().duplicateSelection();
    expect(ipc.applyCommand).not.toHaveBeenCalled();
    expect(ipc.applyCommands.mock.calls[0][0]).toHaveLength(2);
  });

  it("pasteAtPlayhead keeps relative spacing and anchors the earliest clip at the playhead", async () => {
    load(sample(), { playhead: 10 });
    store().setSelections([{ trackId: "v1", clipId: "A" }, { trackId: "v1", clipId: "B" }]);
    store().copySelection();
    await store().pasteAtPlayhead();
    const cmds = ipc.applyCommands.mock.calls[0][0] as { position_secs: number; track_id: string }[];
    expect(cmds.map((c) => c.position_secs)).toEqual([10, 13]); // A at 0, B at 3 -> offsets preserved
    expect(cmds.every((c) => c.track_id === "v1")).toBe(true);
  });

  it("paste prefers the selected track when it is the same kind, else the first track of that kind", async () => {
    const p = makeProject([track("v1", "video", [A]), track("v2", "video"), track("a1", "audio", [C])]);
    load(p);
    store().setSelections([{ trackId: "v1", clipId: "A" }]);
    store().copySelection();
    store().select({ trackId: "v2", clipId: "none" });
    await store().pasteAtPlayhead();
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ track_id: "v2" });
    store().select({ trackId: "a1", clipId: "C" }); // wrong kind -> falls back to first video track
    await store().pasteAtPlayhead();
    expect(ipc.applyCommand.mock.calls[1][0]).toMatchObject({ track_id: "v1" });
  });

  it("paste aborts with a toast when no compatible track exists", async () => {
    load(makeProject([track("v1", "video", [A])]));
    useEditorStore.setState({ clipboard: { items: [{ clip: C, trackKind: "audio" }] } });
    await store().pasteAtPlayhead();
    expect(ipc.applyCommand).not.toHaveBeenCalled();
    expect(lastToast()?.message).toBe("No audio track to paste onto");
  });
});

describe("placeMediaOnTimeline / dropMediaOnTimeline", () => {
  beforeEach(() => {
    ipc.applyCommand.mockResolvedValue(ok());
    ipc.applyCommands.mockResolvedValue(ok());
  });

  const withMedia = (tracks = sample().tracks): Project => ({
    ...makeProject(tracks),
    media: [{ id: "M", path: "C:\\clips\\x.mp4", kind: "video", duration_secs: 4, width: 1, height: 1, fps: 30 }],
  });

  it("places at the playhead when the slot is free", async () => {
    load(withMedia([track("v1", "video", [A])]), { playhead: 5 });
    await store().placeMediaOnTimeline("M", "video", true);
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({
      command: "AddClip",
      track_id: "v1",
      media_id: "M",
      position_secs: 5,
      source_in_secs: 0,
      source_out_secs: 4,
    });
  });

  it("falls back to appending after the last clip when the playhead slot overlaps", async () => {
    load(withMedia([track("v1", "video", [A, B])]), { playhead: 1 });
    await store().placeMediaOnTimeline("M", "video", true);
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ position_secs: 5 }); // B ends at 5
  });

  it("an exactly-adjacent slot is not treated as an overlap", async () => {
    load(withMedia([track("v1", "video", [A])]), { playhead: 2 }); // A ends at 2
    await store().placeMediaOnTimeline("M", "video", true);
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ position_secs: 2 });
  });

  it("uses a 5s default for media with unknown duration (images)", async () => {
    const p = withMedia([track("v1", "video")]);
    p.media[0] = { ...p.media[0], kind: "image", duration_secs: null };
    load(p);
    await store().placeMediaOnTimeline("M", "image", true);
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ source_out_secs: 5 });
  });

  it("creates a track of the right kind when none exists", async () => {
    load(withMedia([]));
    // AddTrack's patch inserts the track so ensureTrack can find it.
    ipc.applyCommand.mockImplementationOnce(async () =>
      ok([{ op: "add", path: "/tracks/-", value: track("new", "video") }]),
    );
    await store().placeMediaOnTimeline("M", "video", true);
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({ command: "AddTrack", kind: "video", name: "Video 1" });
    expect(ipc.applyCommand.mock.calls[1][0]).toMatchObject({ command: "AddClip", track_id: "new" });
  });

  const ghost = (over = {}) => ({
    mediaId: "M",
    kind: "video" as const,
    durationSecs: 4,
    positionSecs: 2.5,
    trackIndex: 0 as number | null,
    row: 0,
    valid: true,
    ...over,
  });

  it("dropping on an existing lane issues a single AddClip and clears the ghost", async () => {
    load(withMedia());
    store().setDragGhost(ghost());
    await store().dropMediaOnTimeline();
    expect(store().dragGhost).toBeNull();
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({
      command: "AddClip",
      track_id: "v1",
      media_id: "M",
      position_secs: 2.5,
      source_in_secs: 0,
      source_out_secs: 4,
    });
  });

  it("dropping outside any lane batches AddTrack + AddClip sharing a client-generated track id", async () => {
    load(withMedia());
    store().setDragGhost(ghost({ trackIndex: null, row: -1 }));
    await store().dropMediaOnTimeline();
    const [cmds] = ipc.applyCommands.mock.calls[0] as [Record<string, unknown>[]];
    expect(cmds).toHaveLength(2);
    expect(cmds[0]).toMatchObject({ command: "AddTrack", kind: "video", name: "Video 4" });
    expect(cmds[0].id).toEqual(expect.any(String));
    expect(cmds[1]).toMatchObject({ command: "AddClip", track_id: cmds[0].id });
  });

  it("audio drops create an audio track", async () => {
    load(withMedia());
    store().setDragGhost(ghost({ kind: "audio", trackIndex: null, row: 3 }));
    await store().dropMediaOnTimeline();
    expect((ipc.applyCommands.mock.calls[0][0] as { kind?: string }[])[0].kind).toBe("audio");
  });

  it("an invalid ghost or empty project does nothing (but still clears the ghost)", async () => {
    load(withMedia());
    store().setDragGhost(ghost({ valid: false }));
    await store().dropMediaOnTimeline();
    expect(store().dragGhost).toBeNull();
    expect(ipc.applyCommand).not.toHaveBeenCalled();
    expect(ipc.applyCommands).not.toHaveBeenCalled();
  });
});

describe("misc store state", () => {
  it("toast appends and dismissToast removes by id", () => {
    store().toast("a");
    store().toast("b", "success");
    const [first, second] = store().toasts;
    expect(second).toMatchObject({ message: "b", kind: "success" });
    expect(first.kind).toBe("info");
    store().dismissToast(first.id);
    expect(store().toasts.map((t) => t.message)).toEqual(["b"]);
  });

  it("togglePerfHud / bumpThemeEpoch", () => {
    store().togglePerfHud();
    expect(store().perfHudVisible).toBe(true);
    store().bumpThemeEpoch();
    store().bumpThemeEpoch();
    expect(store().themeEpoch).toBe(2);
  });

  it("onWaveformReady stores peaks per media id without clobbering thumbnails", () => {
    useEditorStore.setState({ mediaAssets: { m1: { thumbnails: { stripUrl: "s" } as never } } });
    store().onWaveformReady({ media_id: "m1", peaks: [0.1, 0.9], bucket_secs: 0.5 } as never);
    expect(store().mediaAssets.m1.waveform).toEqual({ peaks: [0.1, 0.9], bucketSecs: 0.5 });
    expect(store().mediaAssets.m1.thumbnails?.stripUrl).toBe("s");
  });
});

// ---- Schema v7: nested sequences (active container, nest/unnest, adjustment layers) ----

describe("nested sequences (schema v7)", () => {
  // Root: v1 [A 0..2], a1 [C]. Sequence "Intro" (s1): iv1 [IA 0..3], ia1 audio [IC 0..3].
  // Sequence "Titles" (s2): tv1 [TA 0..1]. Intro contains a clip placing Titles (media m-s2).
  function seqProject(): Project {
    const p = makeProject([track("v1", "video", [A]), track("a1", "audio", [C])]);
    p.media = [
      { id: "m-s1", path: "", kind: "sequence", sequence_id: "s1", duration_secs: null, width: null, height: null, fps: null },
      { id: "m-s2", path: "", kind: "sequence", sequence_id: "s2", duration_secs: null, width: null, height: null, fps: null },
    ];
    p.sequences = [
      {
        id: "s1",
        name: "Intro",
        tracks: [
          track("iv1", "video", [videoClip("IA", 0, 0, 3), videoClip("IT", 3, 0, 1, { media_id: "m-s2" })]),
          track("ia1", "audio", [{ ...videoClip("IC", 0, 0, 3), type: "audio" as const }]),
        ],
      },
      { id: "s2", name: "Titles", tracks: [track("tv1", "video", [videoClip("TA", 0, 0, 1)])] },
    ];
    return p;
  }

  beforeEach(() => {
    ipc.applyCommand.mockResolvedValue(ok());
    ipc.applyCommands.mockResolvedValue(ok());
  });

  it("opens a sequence: active id + path set, selection and playhead reset, backend and seek informed", async () => {
    load(seqProject(), { playhead: 5, selection: { trackId: "v1", clipId: "A" }, selections: [{ trackId: "v1", clipId: "A" }] });
    await store().openSequence("s1");
    expect(store().activeSequenceId).toBe("s1");
    expect(store().sequencePath).toEqual(["s1"]);
    expect(store().selection).toBeNull();
    expect(store().playhead).toBe(0);
    expect(ipc.setActiveSequence).toHaveBeenLastCalledWith("s1");
    expect(ipc.seek).toHaveBeenLastCalledWith(0);
  });

  it("ignores unknown sequences and stops playback when switching container", async () => {
    load(seqProject(), { playing: true });
    await store().openSequence("nope");
    expect(store().activeSequenceId).toBeNull();
    expect(ipc.setActiveSequence).not.toHaveBeenCalled();
    await store().openSequence("s1");
    expect(store().playing).toBe(false);
    expect(ipc.pause).toHaveBeenCalled();
  });

  it("supports nesting depth: open child, close to parent, then to root", async () => {
    load(seqProject());
    await store().openSequence("s1");
    await store().openSequence("s2");
    expect(store().sequencePath).toEqual(["s1", "s2"]);
    expect(store().activeSequenceId).toBe("s2");
    await store().closeSequence();
    expect(store().activeSequenceId).toBe("s1");
    expect(ipc.setActiveSequence).toHaveBeenLastCalledWith("s1");
    await store().closeSequence();
    expect(store().activeSequenceId).toBeNull();
    expect(ipc.setActiveSequence).toHaveBeenLastCalledWith(null);
    // Closing at the root is a no-op.
    ipc.setActiveSequence.mockClear();
    await store().closeSequence();
    expect(ipc.setActiveSequence).not.toHaveBeenCalled();
  });

  it("opening from the media panel (fromRoot) starts a fresh path instead of nesting under the open sequence", async () => {
    load(seqProject());
    await store().openSequence("s1");
    await store().openSequence("s2", { fromRoot: true });
    expect(store().sequencePath).toEqual(["s2"]);
    expect(store().activeSequenceId).toBe("s2");
  });

  it("breadcrumb navigation jumps to an ancestor or the root", async () => {
    load(seqProject());
    await store().openSequence("s1");
    await store().openSequence("s2");
    await store().navigateToSequence("s1");
    expect(store().sequencePath).toEqual(["s1"]);
    await store().openSequence("s2");
    await store().navigateToSequence(null);
    expect(store().sequencePath).toEqual([]);
    expect(store().activeSequenceId).toBeNull();
  });

  it("falls back to the root when the open sequence disappears (delete / undo / external edit)", async () => {
    load(seqProject());
    await store().openSequence("s1");
    store().select({ trackId: "iv1", clipId: "IA" });
    const without = seqProject();
    without.sequences = [];
    useEditorStore.setState({ project: without });
    expect(store().activeSequenceId).toBeNull();
    expect(store().sequencePath).toEqual([]);
    expect(store().selection).toBeNull();
    expect(ipc.setActiveSequence).toHaveBeenLastCalledWith(null);
  });

  it("falls back to the nearest surviving ancestor when only the inner sequence disappears", async () => {
    load(seqProject());
    await store().openSequence("s1");
    await store().openSequence("s2");
    const p = seqProject();
    p.sequences = p.sequences!.filter((s) => s.id !== "s2");
    useEditorStore.setState({ project: p });
    expect(store().sequencePath).toEqual(["s1"]);
    expect(store().activeSequenceId).toBe("s1");
  });

  it("keeps the open sequence across an undo that leaves it in place, and drops to root when undo removes it", async () => {
    load(seqProject());
    await store().openSequence("s2");
    ipc.undo.mockResolvedValueOnce({ can_undo: true, can_redo: true, revision: 3, patch: [{ op: "replace", path: "/name", value: "renamed" }] });
    await store().undo();
    expect(store().activeSequenceId).toBe("s2");
    expect(store().project!.name).toBe("renamed");
    ipc.undo.mockResolvedValueOnce({ can_undo: false, can_redo: true, revision: 4, patch: [{ op: "remove", path: "/sequences/1" }] });
    await store().undo();
    expect(store().activeSequenceId).toBeNull();
    expect(store().project!.sequences).toHaveLength(1);
  });

  it("creates tracks inside the open sequence (ensureTrack and drop-to-new-track pass sequence_id)", async () => {
    load(seqProject());
    await store().openSequence("s2");
    // No audio track in Titles: ensureTrack must target the sequence, not the root's a1.
    ipc.applyCommand.mockImplementationOnce(async () => ({
      revision: 2,
      patch: [{ op: "add", path: "/sequences/1/tracks/-", value: track("new-a", "audio") }],
      outcome: "Applied",
    }));
    const t = await store().ensureTrack("audio", "Audio 1");
    expect(t.id).toBe("new-a");
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ command: "AddTrack", kind: "audio", sequence_id: "s2" });

    ipc.applyCommands.mockClear();
    store().setDragGhost({ mediaId: "m-s2", kind: "video", durationSecs: 2, positionSecs: 1, trackIndex: null, row: -1, valid: true });
    await store().dropMediaOnTimeline();
    const batch = ipc.applyCommands.mock.calls[0][0];
    expect(batch[0]).toMatchObject({ command: "AddTrack", kind: "video", sequence_id: "s2" });
    expect(batch[1]).toMatchObject({ command: "AddClip", media_id: "m-s2", position_secs: 1 });
  });

  it("drops resolve track indices against the open container", async () => {
    load(seqProject());
    await store().openSequence("s1");
    // Index 1 of Intro is its audio lane `ia1`; of the root it would be `a1`... use index 0 -> iv1.
    store().setDragGhost({ mediaId: "m-s2", kind: "sequence", durationSecs: 1, positionSecs: 4, trackIndex: 0, row: 0, valid: true });
    await store().dropMediaOnTimeline();
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ command: "AddClip", track_id: "iv1" });
  });

  it("a sequence dropped below every lane creates an audio track; empty sequences are refused", async () => {
    load(seqProject());
    store().setDragGhost({ mediaId: "m-s1", kind: "sequence", durationSecs: 3, positionSecs: 0, trackIndex: null, row: 2, valid: true });
    await store().dropMediaOnTimeline();
    expect(ipc.applyCommands.mock.calls[0][0][0]).toMatchObject({ command: "AddTrack", kind: "audio" });
    ipc.applyCommands.mockClear();
    store().setDragGhost({ mediaId: "m-s1", kind: "sequence", durationSecs: 0, positionSecs: 0, trackIndex: null, row: -1, valid: true });
    await store().dropMediaOnTimeline();
    expect(ipc.applyCommands).not.toHaveBeenCalled();
    expect(lastToast()?.kind).toBe("error");
  });

  it("copy/paste and duplicate act on the open container's tracks", async () => {
    load(seqProject());
    await store().openSequence("s1");
    store().setSelections([{ trackId: "iv1", clipId: "IA" }]);
    store().copySelection();
    expect(store().clipboard!.items[0]!.clip.id).toBe("IA");
    store().setPlayheadLocal(5);
    await store().pasteAtPlayhead();
    // Pasted onto Intro's video lane, not the root's v1.
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ command: "AddClip", track_id: "iv1", position_secs: 5 });
  });

  it("nestSelection dispatches NestClips with the selected clips", async () => {
    load();
    store().setSelections([{ trackId: "v1", clipId: "A" }, { trackId: "a1", clipId: "C" }]);
    await store().nestSelection();
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({
      command: "NestClips",
      clips: [{ track_id: "v1", clip_id: "A" }, { track_id: "a1", clip_id: "C" }],
      name: "Nested 1",
    });
    expect(store().selections).toEqual([]);
    expect(lastToast()?.kind).toBe("success");
  });

  it("nestSelection refuses locked tracks without dispatching", async () => {
    const p = sample();
    p.tracks[0] = { ...p.tracks[0]!, locked: true };
    load(p);
    store().setSelections([{ trackId: "v1", clipId: "A" }]);
    await store().nestSelection();
    expect(ipc.applyCommand).not.toHaveBeenCalled();
  });

  it("unnestClip dispatches UnnestClip and surfaces the engine's refusal as an error toast", async () => {
    load(seqProject());
    store().select({ trackId: "v1", clipId: "A" });
    ipc.applyCommand.mockRejectedValueOnce("cannot unnest: the clip is trimmed");
    await store().unnestClip();
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({ command: "UnnestClip", track_id: "v1", clip_id: "A" });
    expect(lastToast()).toMatchObject({ kind: "error" });
    expect(lastToast()!.message).toContain("cannot unnest: the clip is trimmed");
    // Selection stays when the command failed.
    expect(store().selection).toEqual({ trackId: "v1", clipId: "A" });
  });

  it("addAdjustmentLayerAtPlayhead adds a 5s layer at the playhead on the selected video track and selects it", async () => {
    load();
    store().select({ trackId: "v1", clipId: "A" });
    store().setPlayheadLocal(6);
    ipc.applyCommand.mockImplementationOnce(async () => ({
      revision: 2,
      patch: [{ op: "add", path: "/tracks/0/clips/-", value: { type: "adjustment", id: "ADJ", position_secs: 6, duration_secs: 5, enabled: true } }],
      outcome: "Applied",
    }));
    await store().addAdjustmentLayerAtPlayhead();
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({
      command: "AddAdjustmentLayer",
      track_id: "v1",
      position_secs: 6,
      duration_secs: 5,
    });
    expect(store().selection).toEqual({ trackId: "v1", clipId: "ADJ" });
  });

  it("addAdjustmentLayerAtPlayhead surfaces the overlap error and creates a track in an empty sequence", async () => {
    load(seqProject());
    store().setPlayheadLocal(1);
    ipc.applyCommand.mockRejectedValueOnce("clip range [1, 6) overlaps an existing clip on track v1");
    await store().addAdjustmentLayerAtPlayhead("v1");
    expect(lastToast()!.message).toContain("overlaps");

    const p = seqProject();
    p.sequences![1]!.tracks = [];
    load(p);
    await store().openSequence("s2");
    await store().addAdjustmentLayerAtPlayhead(undefined, 2);
    const batch = ipc.applyCommands.mock.calls[0][0];
    expect(batch[0]).toMatchObject({ command: "AddTrack", kind: "video", sequence_id: "s2" });
    expect(batch[1]).toMatchObject({ command: "AddAdjustmentLayer", track_id: batch[0].id, duration_secs: 2 });
  });
});
