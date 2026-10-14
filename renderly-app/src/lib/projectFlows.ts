// Higher-level flows that combine native dialogs (ipc.ts) with store actions. Shared by
// WelcomeScreen and TopBar so "New/Open/Import" behave identically everywhere.

import * as ipc from "./ipc";
import { useEditorStore } from "../store/editorStore";

function isMediaPath(path: string): boolean {
  return /\.(mp4|mov|mkv|webm|avi|png|jpe?g|webp|gif|bmp|mp3|wav|m4a|aac|flac|ogg)$/i.test(path);
}

export async function importFromPath(path: string): Promise<void> {
  const store = useEditorStore.getState();
  if (!isMediaPath(path)) {
    store.toast("That file type is not supported yet", "error");
    return;
  }
  if (!store.project) {
    const ready = await store.quickStart();
    if (!ready) return;
  }
  await useEditorStore.getState().importMediaSmart(path);
}

export async function pickAndImportMedia(): Promise<void> {
  const store = useEditorStore.getState();
  if (!store.project) {
    await quickStartWithImport();
    return;
  }
  const path = await ipc.pickMediaFile();
  if (!path) return;
  await store.importMediaSmart(path);
}

export async function quickStartWithImport(): Promise<void> {
  const store = useEditorStore.getState();
  try {
    const ready = await store.quickStart();
    if (!ready) return;
    const mediaPath = await ipc.pickMediaFile();
    if (!mediaPath) {
      store.toast("Project ready — import media when you're set", "info");
      return;
    }
    await store.importMediaSmart(mediaPath);
  } catch (e) {
    store.toast(`Quick start failed: ${e instanceof Error ? e.message : String(e)}`, "error");
  }
}

export async function createNewProjectFlow(): Promise<void> {
  // G2: CapCut-style — auto-create in the default projects dir, no save dialog.
  const store = useEditorStore.getState();
  try {
    const ready = await store.quickStart();
    if (!ready) return;
    store.toast("New project created", "success");
  } catch (e) {
    store.toast(`New project failed: ${e instanceof Error ? e.message : String(e)}`, "error");
  }
}

export async function openExistingProjectFlow(): Promise<void> {
  const store = useEditorStore.getState();
  const path = await ipc.pickProjectFileToOpen();
  if (!path) return;
  await store.loadProjectFromPath(path);
  store.toast("Project opened", "success");
}

export async function importOtioFlow(): Promise<void> {
  const store = useEditorStore.getState();
  try {
    const path = await ipc.pickOtioToOpen();
    if (!path) return;
    const result = await ipc.importOtio(path);
    await store.loadProjectFromPath(result.projectPath);
    const n = result.warnings.length;
    store.toast(
      n === 0 ? "Timeline imported" : `Timeline imported with ${n} warning${n === 1 ? "" : "s"}`,
      n === 0 ? "success" : "info",
    );
    if (n > 0) console.warn("OTIO import warnings", result.warnings);
  } catch (e) {
    store.toast(`Import failed: ${e instanceof Error ? e.message : String(e)}`, "error");
  }
}

export async function exportOtioFlow(): Promise<void> {
  const store = useEditorStore.getState();
  if (!store.project) return;
  try {
    const path = await ipc.pickOtioSavePath(`${store.project.name}.otio`);
    if (!path) return;
    await ipc.exportOtio(path);
    store.toast("Timeline exported (OpenTimelineIO)", "success");
  } catch (e) {
    store.toast(`Export failed: ${e instanceof Error ? e.message : String(e)}`, "error");
  }
}
