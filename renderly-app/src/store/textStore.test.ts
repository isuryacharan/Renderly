import { beforeEach, describe, expect, it, vi } from "vitest";
import { captionClip, project as makeProject, textClip, track, videoClip } from "../test/fixtures";
import type { Project, Track } from "../lib/types";

// Same wholesale ipc mock as editorStore.test.ts: the store only talks to the backend through it.
const ipc = vi.hoisted(() => ({
  applyCommand: vi.fn(),
  applyCommands: vi.fn(),
  undo: vi.fn(),
  redo: vi.fn(),
  setPreviewOverride: vi.fn(),
  clearPreviewOverride: vi.fn(),
  endGesture: vi.fn(),
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

import { useEditorStore } from "./editorStore";

const initial = useEditorStore.getState();
const store = () => useEditorStore.getState();
const lastToast = () => store().toasts[store().toasts.length - 1];
const ok = (patch: unknown[] = [], revision = 2) => ({ revision, patch, outcome: "Applied", outcomes: [] });

function load(p: Project, extra: Record<string, unknown> = {}) {
  useEditorStore.setState({ project: p, projectPath: "/tmp/p.renderly", clientRevision: 1, ...extra });
}

const withFlags = (t: Track, flags: Partial<Track>): Track => ({ ...t, ...flags });

beforeEach(() => {
  vi.clearAllMocks();
  useEditorStore.setState(initial, true);
  ipc.refreshFrame.mockResolvedValue(undefined);
  ipc.setEditorSelection.mockResolvedValue(undefined);
  ipc.setActiveSequence.mockResolvedValue(undefined);
  ipc.getProject.mockResolvedValue({ project: makeProject([]), revision: 99 });
  ipc.setPreviewOverride.mockResolvedValue(undefined);
  ipc.clearPreviewOverride.mockResolvedValue(undefined);
  ipc.applyCommand.mockResolvedValue(ok());
  ipc.applyCommands.mockResolvedValue(ok());
  vi.stubGlobal("window", { setTimeout: () => 0 });
  vi.stubGlobal("document", { getElementById: () => null });
});

describe("addTitleAtPlayhead", () => {
  it("adds a 3 s title at the playhead on the first free non-magnetic video track and selects it", async () => {
    const main = withFlags(track("v1", "video", [videoClip("A", 0, 0, 10)]), { magnetic: true });
    const overlay = track("v2", "video", []);
    load(makeProject([main, overlay]));
    store().setPlayheadLocal(2_000_000);
    ipc.applyCommand.mockImplementationOnce(async () => ({
      revision: 2,
      patch: [{ op: "add", path: "/tracks/1/clips/-", value: textClip("T", 2, 3, "Hello") }],
      outcome: "Applied",
    }));
    await store().addTitleAtPlayhead("Hello");
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({
      command: "AddText",
      track_id: "v2",
      body: { text: "Hello" },
      start: 2_000_000,
      duration: 3_000_000,
      style: { ref: "builtin:tiktok-minimal@1", params: {}, overrides: { anchor: "center" } },
    });
    expect(store().selection).toEqual({ trackId: "v2", clipId: "T" });
  });

  it("never puts a title on the magnetic main track: a busy overlay lane makes a Titles track", async () => {
    const main = withFlags(track("v1", "video", [videoClip("A", 0, 0, 10)]), { magnetic: true });
    const overlay = track("v2", "video", [textClip("X", 0, 10)]);
    load(makeProject([main, overlay]));
    store().setPlayheadLocal(1_000_000);
    await store().addTitleAtPlayhead("Again", { styleRef: "builtin:tiktok-box@1", durationUs: 2_000_000 });
    const batch = ipc.applyCommands.mock.calls[0][0];
    expect(batch).toHaveLength(2);
    expect(batch[0]).toMatchObject({ command: "AddTrack", kind: "video", name: "Titles", magnetic: false });
    expect(batch[1]).toMatchObject({
      command: "AddText",
      track_id: batch[0].id,
      duration: 2_000_000,
      style: { ref: "builtin:tiktok-box@1" },
    });
  });

  it("creates the (magnetic) main track first when the container has no video track", async () => {
    load(makeProject([track("a1", "audio", [])]));
    await store().addTitleAtPlayhead();
    const batch = ipc.applyCommands.mock.calls[0][0];
    expect(batch.map((c: { command: string }) => c.command)).toEqual(["AddTrack", "AddTrack", "AddText"]);
    // The first AddTrack leaves `magnetic` out so core makes it the magnetic main track.
    expect(batch[0]).not.toHaveProperty("magnetic");
    expect(batch[1]).toMatchObject({ name: "Titles", magnetic: false });
    expect(batch[2]).toMatchObject({ body: { text: "Title" } });
  });
});

describe("markers", () => {
  it("addMarkerAtPlayhead sends AddMarkers at the frame-snapped playhead", async () => {
    load(makeProject([track("v1", "video", [])]));
    store().setPlayheadLocal(1_000_000);
    await store().addMarkerAtPlayhead();
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({
      command: "AddMarkers",
      markers: [{ time: 1_000_000, label: "", color: "", kind: "user" }],
    });
  });

  it("does not stack two markers on the same time, and refuses inside a nested sequence", async () => {
    const p = makeProject([track("v1", "video", [])]);
    p.markers = [{ id: "m1", time_us: 1_000_000, label: "", color: "", kind: "user" }];
    load(p);
    store().setPlayheadLocal(1_000_000);
    await store().addMarkerAtPlayhead();
    expect(ipc.applyCommand).not.toHaveBeenCalled();
    expect(lastToast()!.message).toContain("already a marker");

    p.sequences = [{ id: "s1", name: "S", tracks: [] }];
    load(p, { activeSequenceId: "s1", sequencePath: ["s1"] });
    store().setPlayheadLocal(2_000_000);
    await store().addMarkerAtPlayhead();
    expect(ipc.applyCommand).not.toHaveBeenCalled();
    expect(lastToast()!.message).toContain("main timeline");
  });
});

describe("toggleMagnet", () => {
  it("flips magnetic on the selected video track, else the first video track", async () => {
    const v1 = withFlags(track("v1", "video", [videoClip("A", 0, 0, 2)]), { magnetic: true });
    const v2 = track("v2", "video", [videoClip("B", 0, 0, 2)]);
    load(makeProject([v1, v2]));
    await store().toggleMagnet();
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({ command: "SetTrackFlags", track_id: "v1", magnetic: false });
    store().select({ trackId: "v2", clipId: "B" });
    await store().toggleMagnet();
    expect(ipc.applyCommand.mock.calls[1][0]).toMatchObject({ command: "SetTrackFlags", track_id: "v2", magnetic: true });
    await store().toggleMagnet("v1");
    expect(ipc.applyCommand.mock.calls[2][0]).toMatchObject({ track_id: "v1", magnetic: false });
  });

  it("does nothing without a video track", async () => {
    load(makeProject([track("a1", "audio", [])]));
    await store().toggleMagnet();
    expect(ipc.applyCommand).not.toHaveBeenCalled();
    expect(lastToast()!.message).toContain("video track");
  });
});

describe("duplicating text", () => {
  it("a title is duplicated as AddText with its style, right after the original", async () => {
    const v = track("v2", "video", [textClip("T", 1, 2, "Hi")]);
    load(makeProject([v]));
    store().setSelections([{ trackId: "v2", clipId: "T" }]);
    await store().duplicateSelection();
    expect(ipc.applyCommand.mock.calls[0][0]).toEqual({
      command: "AddText",
      track_id: "v2",
      body: { text: "Hi" },
      start: 3_000_000,
      duration: 2_000_000,
      style: { ref: "builtin:tiktok-minimal@1", params: {}, overrides: { anchor: "center" } },
    });
  });

  it("a speech caption keeps its words (and its own style)", async () => {
    const cap = captionClip("K", 0, 2, "so um");
    cap.body.words = [{ text: "so", start_us: 0, end_us: 400_000, word_id: "w-1" }];
    cap.style = { ref: "builtin:tiktok-box@1", params: {}, overrides: {} };
    load(makeProject([track("c1", "caption", [cap])]));
    store().setSelections([{ trackId: "c1", clipId: "K" }]);
    await store().duplicateSelection();
    expect(ipc.applyCommand.mock.calls[0][0]).toMatchObject({
      command: "AddCaption",
      body: { text: "so um", words: [{ text: "so", start: 0, end: 400_000, word_id: "w-1" }] },
      style: { ref: "builtin:tiktok-box@1" },
    });
  });
});

describe("hover preview", () => {
  it("a transition preview applies to a selected title too", () => {
    load(makeProject([track("v2", "video", [textClip("T", 0, 3)])]), {
      selections: [{ trackId: "v2", clipId: "T" }],
      selection: { trackId: "v2", clipId: "T" },
    });
    store().previewTransitionOnSelection({ ref: "builtin:crossfade@1", params: [] }, 500_000);
    expect(store().previewOverrides.T).toMatchObject({ transition: { ref: "builtin:crossfade@1", duration_us: 500_000 } });
  });
});
