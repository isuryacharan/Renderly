// A project open in a tab. One tab per project is the writer (a Web Lock named after the project
// id); other tabs are read-only followers that reload after each of the writer's saves
// (BroadcastChannel). A follower can take over: it asks the writer to flush, then steals the lock,
// and the old writer becomes a follower. When the writer closes, a waiting follower becomes the
// writer by itself. The writer flushes on tab hide and page hide.
//
// A write started while the page unloads may not finish (a reload inside the debounce window), so
// on hide the writer also keeps a copy of unsaved edits in localStorage, synchronously. The next
// writer to open the project adopts that copy if the disk is still at the revision the edits were
// based on, and drops it otherwise. Copies too big for localStorage (~5 MB) rely on the flush.

import { parseProject, ProjectStore, serialiseProject, type MigrateResult } from "@renderly/core";
import { Autosave, type AutosaveOptions, type SaveEvent } from "./autosave";
import { readBackup } from "./backups";
import type { Dir } from "./fs";
import { loadFrom, PROJECT_FILE } from "./projects";

// FileSystemObserver (Chromium 133+) reports outside changes as they happen. Elsewhere autosave
// still checks before every write.
interface ChangeRecord {
  relativePathComponents?: string[];
}
interface Observer {
  observe(handle: Dir): Promise<void>;
  disconnect(): void;
}
type ObserverClass = new (callback: (records: ChangeRecord[]) => void) => Observer;

export type Role = "writer" | "follower" | "closed";

export type SessionEvent =
  | { type: "role"; role: Role }
  | { type: "save"; save: SaveEvent }
  /** A follower loaded the writer's latest save. */
  | { type: "reloaded"; revision: number }
  /** The writer adopted edits a previous page kept when it unloaded before saving them. */
  | { type: "recovered"; revision: number };

type Message =
  | { type: "saved"; revision: number; text: string }
  | { type: "flush"; id: string }
  | { type: "flushed"; id: string };

export class ProjectLoadError extends Error {
  constructor(readonly result: Extract<MigrateResult, { ok: false }>) {
    super(
      result.reason === "newer"
        ? `The project is from a newer app (version ${result.version})`
        : "The project is not valid",
    );
  }
}

export interface SessionOptions {
  autosave?: AutosaveOptions;
  /** How long a take-over waits for the writer to flush before stealing anyway. */
  takeOverWait?: number;
  /**
   * Start a save when the tab is hidden or the page unloads (default true). Turned off only by
   * tests, to check that the localStorage copy alone keeps the edits.
   */
  flushOnHide?: boolean;
}

interface UnsavedCopy {
  /** The saved revision the edits were made on top of, and project.json's stamp then. */
  base: number;
  stamp: number;
  text: string;
}

const storage = (): Storage | null => (typeof localStorage === "undefined" ? null : localStorage);

const isAbort = (error: unknown) => error instanceof DOMException && error.name === "AbortError";

export class ProjectSession {
  role: Role = "follower";
  autosave: Autosave | null = null;
  /** The revision adopted from a kept unsaved copy when this tab became the writer, if any. */
  recovered: number | null = null;
  private release: (() => void) | null = null;
  private waiting: AbortController | null = null;
  private observer: Observer | null = null;
  private readonly channel: BroadcastChannel;
  private readonly lockName: string;
  private readonly unsavedKey: string;
  private readonly listeners = new Set<(event: SessionEvent) => void>();
  private readonly onHide = () => {
    if (document.visibilityState === "hidden") this.onPageHide();
  };
  private readonly onPageHide = () => {
    this.keepUnsaved();
    if (this.options.flushOnHide !== false) void this.autosave?.flush();
  };

  private constructor(
    readonly dir: Dir,
    readonly store: ProjectStore,
    private disk: { text: string; stamp: number },
    private readonly options: SessionOptions,
  ) {
    this.lockName = `renderly:project:${store.project.id}`;
    this.unsavedKey = `renderly:unsaved:${store.project.id}`;
    this.channel = new BroadcastChannel(this.lockName);
    this.channel.onmessage = (event: MessageEvent<Message>) => void this.receive(event.data);
    store.readOnly = true;
  }

  /** Opens the project in `dir`: as the writer if no other tab is, otherwise as a follower. */
  static async open(dir: Dir, options: SessionOptions = {}): Promise<ProjectSession> {
    const loaded = await loadFrom(dir);
    if (!loaded.result.ok) throw new ProjectLoadError(loaded.result);
    const session = new ProjectSession(
      dir,
      new ProjectStore(loaded.result.project),
      loaded,
      options,
    );
    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", session.onHide);
      addEventListener("pagehide", session.onPageHide);
    }
    if (!(await session.acquire("try"))) {
      session.setRole("follower");
      void session.acquire("wait");
    }
    return session;
  }

  on(listener: (event: SessionEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** Becomes the writer: the current writer saves first, then hands over. */
  async takeOver(): Promise<void> {
    if (this.role !== "follower") return;
    const id = crypto.randomUUID();
    const flushed = new Promise<void>((resolve) => {
      const listener = (event: MessageEvent<Message>) => {
        if (event.data.type === "flushed" && event.data.id === id) {
          this.channel.removeEventListener("message", listener);
          resolve();
        }
      };
      this.channel.addEventListener("message", listener);
      setTimeout(() => {
        this.channel.removeEventListener("message", listener);
        resolve();
      }, this.options.takeOverWait ?? 2000);
    });
    this.channel.postMessage({ type: "flush", id } satisfies Message);
    await flushed;
    this.waiting?.abort();
    this.waiting = null;
    await this.acquire("steal");
  }

  /** Restores a backup as the current version (writer only). The undo history is dropped. */
  async restore(name: string): Promise<void> {
    if (this.role !== "writer" || !this.autosave) throw new Error("Only the writer can restore");
    const result = parseProject(await readBackup(this.dir, name));
    if (!result.ok) throw new ProjectLoadError(result);
    this.store.replace({ ...result.project, revision: this.store.revision + 1 });
    await this.autosave.flush();
  }

  /** Saves, lets a waiting tab become the writer, and stops. */
  async close(): Promise<void> {
    if (this.role === "closed") return;
    if (this.autosave) {
      await this.autosave.flush();
      this.autosave.stop();
      this.autosave = null;
    }
    this.unwatch();
    this.waiting?.abort();
    this.release?.();
    this.release = null;
    this.channel.close();
    if (typeof document !== "undefined") {
      document.removeEventListener("visibilitychange", this.onHide);
      removeEventListener("pagehide", this.onPageHide);
    }
    this.store.readOnly = true;
    this.setRole("closed");
  }

  /**
   * Requests the writer lock. "try" gives up at once if another tab holds it, "wait" queues until
   * it is free, "steal" takes it from the holder. Resolves true once this tab is the writer.
   */
  private acquire(how: "try" | "wait" | "steal"): Promise<boolean> {
    return new Promise((resolve) => {
      const options: LockOptions =
        how === "try" ? { ifAvailable: true } : how === "steal" ? { steal: true } : {};
      if (how === "wait") {
        this.waiting = new AbortController();
        options.signal = this.waiting.signal;
      }
      navigator.locks
        .request(this.lockName, options, async (lock) => {
          if (!lock || this.role === "closed") {
            resolve(false);
            return;
          }
          await this.becomeWriter();
          resolve(true);
          await new Promise<void>((done) => (this.release = done));
        })
        .catch((error: unknown) => {
          resolve(false);
          if (!isAbort(error)) throw error;
          // Aborted while waiting (we stole or closed), or the lock was stolen from us.
          if (this.role === "writer") this.lostLock();
        });
    });
  }

  private async becomeWriter(): Promise<void> {
    // The previous writer may have saved since this tab last loaded.
    const loaded = await loadFrom(this.dir);
    if (loaded.text !== this.disk.text && loaded.result.ok) {
      const revision = Math.max(loaded.result.project.revision, this.store.revision);
      this.store.replace({ ...loaded.result.project, revision });
    }
    this.disk = { text: loaded.text, stamp: loaded.stamp };
    const autosave = new Autosave(this.dir, this.store, this.disk, {
      ...this.options.autosave,
      onSaved: (text, revision) => {
        this.disk = { text, stamp: 0 };
        if (!this.autosave?.dirty) storage()?.removeItem(this.unsavedKey);
        this.channel.postMessage({ type: "saved", revision, text } satisfies Message);
        this.options.autosave?.onSaved?.(text, revision);
      },
    });
    autosave.on((save) => this.emit({ type: "save", save }));
    autosave.start();
    this.autosave = autosave;
    if (loaded.result.ok) this.recoverUnsaved(loaded.result.project.revision, loaded.stamp);
    this.watch();
    this.store.readOnly = false;
    this.setRole("writer");
  }

  /** Keeps unsaved edits in localStorage, synchronously: the page may be about to go. */
  private keepUnsaved(): void {
    const autosave = this.autosave;
    if (this.role !== "writer" || !autosave?.dirty) return;
    try {
      const copy: UnsavedCopy = {
        base: autosave.state.savedRevision,
        stamp: autosave.diskStamp,
        text: serialiseProject(this.store.project),
      };
      storage()?.setItem(this.unsavedKey, JSON.stringify(copy));
    } catch {
      // Too big for localStorage: the flush is all there is.
    }
  }

  /** Adopts a kept copy if it was based on exactly what is on disk now; drops it otherwise. */
  private recoverUnsaved(diskRevision: number, diskStamp: number): void {
    const raw = storage()?.getItem(this.unsavedKey);
    if (!raw) return;
    try {
      const copy = JSON.parse(raw) as UnsavedCopy;
      const result = parseProject(copy.text);
      const untouched = copy.base === diskRevision && copy.stamp === diskStamp;
      if (untouched && result.ok && result.project.revision > diskRevision) {
        this.store.replace(result.project); // unsaved now, so autosave writes it
        this.recovered = result.project.revision;
        this.emit({ type: "recovered", revision: result.project.revision });
        return;
      }
    } catch {
      // unreadable: drop it
    }
    storage()?.removeItem(this.unsavedKey);
  }

  private watch(): void {
    const Observer = (globalThis as { FileSystemObserver?: ObserverClass }).FileSystemObserver;
    if (!Observer || this.observer) return;
    this.observer = new Observer((records) => {
      if (records.some((r) => r.relativePathComponents?.[0] === PROJECT_FILE))
        void this.autosave?.checkOutside();
    });
    this.observer.observe(this.dir).catch(() => {
      this.observer = null;
    });
  }

  private unwatch(): void {
    this.observer?.disconnect();
    this.observer = null;
  }

  private lostLock(): void {
    this.unwatch();
    this.autosave?.stop();
    this.autosave = null;
    this.release = null;
    this.store.readOnly = true;
    this.setRole("follower");
    void this.acquire("wait");
  }

  private async receive(message: Message): Promise<void> {
    if (message.type === "flush" && this.role === "writer") {
      await this.autosave?.flush();
      this.channel.postMessage({ type: "flushed", id: message.id } satisfies Message);
    } else if (message.type === "saved" && this.role === "follower") {
      const result = parseProject(message.text);
      if (!result.ok) return;
      this.disk = { text: message.text, stamp: 0 };
      this.store.replace(result.project);
      this.emit({ type: "reloaded", revision: message.revision });
    }
  }

  private setRole(role: Role): void {
    this.role = role;
    this.emit({ type: "role", role });
  }

  private emit(event: SessionEvent): void {
    for (const listener of this.listeners) listener(event);
  }
}
