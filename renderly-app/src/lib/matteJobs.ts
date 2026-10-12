// Tracks the (single) in-flight AI background-matte bake so the inspector can show progress and a
// Cancel button no matter which clip is selected. The bake itself runs in the Rust backend off the
// project lock (src-tauri/src/ml.rs); this module only mirrors its `matte:progress` /
// `matte:finished` events and starts it by dispatching the ordinary GenerateBackgroundMatte
// command.

import { useSyncExternalStore } from "react";
import * as ipc from "./ipc";
import { generateBackgroundMatte } from "./commands";
import { useEditorStore } from "../store/editorStore";
import type { MatteStatus } from "./ipc";

export interface MatteJob {
  trackId: string;
  clipId: string;
  frame: number;
  total: number;
}

let job: MatteJob | null = null;
const listeners = new Set<() => void>();
let wired = false;
let lastFinished: ipc.MatteFinishedPayload | null = null;

function emit() {
  for (const l of listeners) l();
}

function setJob(next: MatteJob | null) {
  job = next;
  emit();
}

/// Subscribe to backend progress once (idempotent). Safe to call from non-Tauri contexts: the
/// listeners are simply never fired.
function wire() {
  if (wired) return;
  wired = true;
  ipc.onMatteProgress((p) => {
    // Also reflects bakes started elsewhere (a live agent over the bridge).
    setJob({ trackId: p.track_id, clipId: p.clip_id, frame: p.frame, total: p.total });
  });
  ipc.onMatteFinished((p) => {
    lastFinished = p;
    if (job && job.clipId === p.clip_id) setJob(null);
  });
}

export function getMatteJob(): MatteJob | null {
  return job;
}

export function subscribeMatteJob(l: () => void): () => void {
  wire();
  listeners.add(l);
  return () => void listeners.delete(l);
}

/// The running bake (if any). Re-renders on every progress event.
export function useMatteJob(): MatteJob | null {
  return useSyncExternalStore(subscribeMatteJob, getMatteJob, getMatteJob);
}

/// Start generating the matte for a clip. Resolves when finished; failures other than a user
/// cancel are toasted. The backend refuses to start a second bake while one runs.
export async function startMatteJob(trackId: string, clipId: string): Promise<void> {
  wire();
  lastFinished = null;
  setJob({ trackId, clipId, frame: 0, total: 0 });
  const store = useEditorStore.getState();
  const ok = await store.dispatch(generateBackgroundMatte(trackId, clipId), true);
  setJob(null);
  if (!ok) {
    const f = lastFinished as ipc.MatteFinishedPayload | null;
    if (f?.status === "cancelled") {
      store.toast("Background matte cancelled", "info");
    } else {
      store.toast(`Background matte failed: ${f?.message ?? "see log"}`, "error");
    }
  } else {
    store.toast("Background matte ready", "success");
  }
}

export function cancelMatteJob(): Promise<void> {
  return ipc.cancelBackgroundMatte();
}

/// 0..100 for a progress bar (0 while the total is unknown).
export function matteProgressPercent(j: Pick<MatteJob, "frame" | "total"> | null): number {
  if (!j || j.total <= 0) return 0;
  return Math.max(0, Math.min(100, Math.round((j.frame / j.total) * 100)));
}

/// One-line inspector label for the matte state.
export function matteStatusLabel(
  status: MatteStatus | null,
  running: Pick<MatteJob, "frame" | "total"> | null,
): string {
  if (running) {
    return running.total > 0 ? `Generating… ${matteProgressPercent(running)}%` : "Generating…";
  }
  if (!status || status.state === "not_generated") return "Not generated";
  if (status.state === "ready") {
    return status.frames ? `Ready · ${status.frames} frames` : "Ready";
  }
  return `Out of date — ${status.reason}`;
}

/// "≈15 MB" style size for the consent dialog (decimal megabytes, as download sizes are quoted).
export function formatModelSize(bytes: number): string {
  if (bytes >= 1_000_000) return `≈${Math.round(bytes / 1_000_000)} MB`;
  return `≈${Math.max(1, Math.round(bytes / 1_000))} KB`;
}
