// What the editor knows about each media item's file right now: being checked, offline (and
// why), or online with the File to read. Worked out when a project opens and never stored. The
// bin fills it in; the timeline reads it for offline clips and thumbnails.

import type { OfflineReason } from "@renderly/storage";
import { useSyncExternalStore } from "react";

export type MediaState =
  | { status: "checking" }
  | { status: "offline"; reason: OfflineReason }
  | { status: "online"; file: File; unsupported?: string };

export class MediaStatus {
  private states = new Map<string, MediaState>();
  private readonly listeners = new Set<() => void>();

  get(id: string): MediaState | undefined {
    return this.states.get(id);
  }

  /** Every state, as an immutable map (a new map after each change). */
  all(): ReadonlyMap<string, MediaState> {
    return this.states;
  }

  set(id: string, state: MediaState): void {
    this.states = new Map(this.states).set(id, state);
    for (const listener of this.listeners) listener();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }
}

export function useMediaStates(status: MediaStatus): ReadonlyMap<string, MediaState> {
  return useSyncExternalStore(
    (listener) => status.subscribe(listener),
    () => status.all(),
  );
}
