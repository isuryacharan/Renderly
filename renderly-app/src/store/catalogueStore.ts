// The engine's effect catalogue (what can be applied, with parameter specs) and the status of
// the open project's effects (changed / missing / disabled). Both come from the Tauri backend;
// components read them through `useCatalogue` / `useEffectStatus` instead of calling IPC.

import { create } from "zustand";
import * as ipc from "../lib/ipc";
import type { CatalogueEntry, EffectStatusEntry, LoadedPluginInfo } from "../lib/ipc";

interface CatalogueState {
  entries: CatalogueEntry[];
  plugins: LoadedPluginInfo[];
  loaded: boolean;
  /** Bumps whenever the installed set of items actually changed (preview refetches pack files). */
  epoch: number;
  status: EffectStatusEntry[];
  refresh(): Promise<void>;
  refreshStatus(): Promise<void>;
  clear(): void;
}

let signature = "";
let inflight: Promise<void> | null = null;

export const useCatalogueStore = create<CatalogueState>((set, get) => ({
  entries: [],
  plugins: [],
  loaded: false,
  epoch: 0,
  status: [],

  async refresh() {
    if (inflight) return inflight;
    inflight = (async () => {
      try {
        const cat = await ipc.listExtensions();
        const entries = cat.effects ?? [];
        const sig = JSON.stringify([entries, cat.plugins]);
        if (sig !== signature || !get().loaded) {
          const changed = sig !== signature;
          signature = sig;
          set((s) => ({
            entries,
            plugins: cat.plugins ?? [],
            loaded: true,
            epoch: changed && s.loaded ? s.epoch + 1 : s.epoch,
          }));
        }
      } catch {
        // No session (or the backend is not up yet): keep what we have.
      } finally {
        inflight = null;
      }
    })();
    return inflight;
  },

  async refreshStatus() {
    try {
      const status = await ipc.getEffectStatus();
      const prev = get().status;
      if (JSON.stringify(prev) !== JSON.stringify(status)) set({ status });
    } catch {
      if (get().status.length > 0) set({ status: [] });
    }
  },

  clear() {
    signature = "";
    set({ entries: [], plugins: [], loaded: false, status: [] });
  },
}));
