import { beforeEach, describe, expect, it, vi } from "vitest";
import { project as makeProject, track } from "../test/fixtures";

// Flows combine native dialogs (ipc.ts) with store actions; mock ipc wholesale so no Tauri
// module enters the test graph.
const ipc = vi.hoisted(() => ({
  pickOtioToOpen: vi.fn(),
  pickOtioSavePath: vi.fn(),
  importOtio: vi.fn(),
  exportOtio: vi.fn(),
  openProject: vi.fn(),
  getProject: vi.fn(),
  setEditorSelection: vi.fn(),
}));
vi.mock("./ipc", () => ipc);

import { ACTIONS } from "./actions";
import { exportOtioFlow, importOtioFlow } from "./projectFlows";
import { useEditorStore } from "../store/editorStore";

const store = () => useEditorStore.getState();
const lastToast = () => store().toasts[store().toasts.length - 1];
const initial = useEditorStore.getState();

beforeEach(() => {
  vi.clearAllMocks();
  // toast() schedules removal through window.setTimeout; no DOM needed otherwise.
  vi.stubGlobal("window", { setTimeout: () => 0 });
  useEditorStore.setState(initial, true);
  useEditorStore.setState({ project: makeProject([track("v1", "video", [])]) });
});

describe("OpenTimelineIO flows", () => {
  it("registers import (always enabled) and export (needs a project) actions", () => {
    expect(ACTIONS["project.import-otio"].isEnabled(store())).toBe(true);
    expect(ACTIONS["project.export-otio"].isEnabled(store())).toBe(true);
    useEditorStore.setState({ project: null });
    expect(ACTIONS["project.export-otio"].isEnabled(store())).toBe(false);
    expect(ACTIONS["project.import-otio"].isEnabled(store())).toBe(true);
  });

  it("export asks for a path, writes the file and toasts", async () => {
    ipc.pickOtioSavePath.mockResolvedValue("C:/out/edit.otio");
    ipc.exportOtio.mockResolvedValue(undefined);
    await exportOtioFlow();
    expect(ipc.pickOtioSavePath).toHaveBeenCalledWith(expect.stringMatching(/\.otio$/));
    expect(ipc.exportOtio).toHaveBeenCalledWith("C:/out/edit.otio");
    expect(lastToast().kind).toBe("success");
  });

  it("a cancelled export dialog does nothing", async () => {
    ipc.pickOtioSavePath.mockResolvedValue(null);
    await exportOtioFlow();
    expect(ipc.exportOtio).not.toHaveBeenCalled();
  });

  it("import creates a project from the file, opens it and reports warnings", async () => {
    ipc.pickOtioToOpen.mockResolvedValue("C:/in/cut.otio");
    ipc.importOtio.mockResolvedValue({
      projectPath: "C:/Documents/Renderly/cut.renderly.json",
      warnings: [{ kind: "missing_media", path: "tracks", message: "x" }],
    });
    ipc.openProject.mockResolvedValue(undefined);
    ipc.getProject.mockResolvedValue({ project: makeProject([]), revision: 1 });
    await importOtioFlow();
    expect(ipc.importOtio).toHaveBeenCalledWith("C:/in/cut.otio");
    expect(ipc.openProject).toHaveBeenCalledWith("C:/Documents/Renderly/cut.renderly.json");
    expect(lastToast().message).toContain("1 warning");
  });

  it("import failures surface as an error toast", async () => {
    ipc.pickOtioToOpen.mockResolvedValue("C:/in/bad.otio");
    ipc.importOtio.mockRejectedValue("not an OpenTimelineIO timeline");
    await importOtioFlow();
    expect(ipc.openProject).not.toHaveBeenCalled();
    expect(lastToast().kind).toBe("error");
    expect(lastToast().message).toContain("not an OpenTimelineIO timeline");
  });
});
