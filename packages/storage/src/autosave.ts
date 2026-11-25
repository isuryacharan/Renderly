// Autosave: writes project.json after edits.
// - Debounced: `debounce` ms after the last change.
// - Coalesced: never more than one write at a time; changes made during a write are written next.
// - Atomic: each write replaces the file only when complete (see fs.ts).
// - Retried with backoff (1 s, 2 s, 4 s ... 30 s) when a write fails.
// - Careful: before writing, it checks that project.json is still what it last wrote. If
//   something else changed it, it stops with a "conflict" until the user picks a version.
// Flushing on tab hide is the session's job (session.ts).

import { parseProject, serialiseProject, type Problem, type ProjectStore } from "@renderly/core";
import { backUp, DEFAULT_BACKUPS, type BackupPolicy } from "./backups";
import { fileStamp, readText, writeText, type Dir } from "./fs";
import { META_FILE, metaOf, PROJECT_FILE } from "./projects";

export type SaveStatus = "saved" | "unsaved" | "saving" | "error" | "conflict";

export interface SaveEvent {
  status: SaveStatus;
  /** The revision on disk. */
  savedRevision: number;
  /** The revision in memory. */
  revision: number;
  /** For "saved": bytes written and how long the write took. */
  bytes?: number;
  ms?: number;
  /** For "error": what failed and when the next try is. */
  error?: string;
  retryIn?: number;
}

export interface AutosaveOptions {
  debounce?: number;
  /** Backup policy; null turns backups off. */
  backups?: BackupPolicy | null;
  now?: () => number;
  /** Called after every successful write with the text written. */
  onSaved?: (text: string, revision: number) => void;
}

export const backoff = (failures: number): number =>
  Math.min(30_000, 1000 * 2 ** Math.max(0, failures - 1));

export class Autosave {
  private status: SaveStatus = "saved";
  private savedRevision: number;
  private lastText: string;
  private lastStamp: number;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private writing: Promise<void> | null = null;
  private again = false;
  private failures = 0;
  private unsubscribe: (() => void) | null = null;
  private readonly listeners = new Set<(event: SaveEvent) => void>();
  private readonly debounce: number;
  private readonly backups: BackupPolicy | null;
  private readonly now: () => number;

  constructor(
    private readonly dir: Dir,
    private readonly store: ProjectStore,
    disk: { text: string; stamp: number },
    private readonly options: AutosaveOptions = {},
  ) {
    this.savedRevision = store.revision;
    this.lastText = disk.text;
    this.lastStamp = disk.stamp;
    this.debounce = options.debounce ?? 400;
    this.backups = options.backups === undefined ? DEFAULT_BACKUPS : options.backups;
    this.now = options.now ?? Date.now;
  }

  get state(): SaveEvent {
    return {
      status: this.status,
      savedRevision: this.savedRevision,
      revision: this.store.revision,
    };
  }

  /** The modification stamp of project.json as this autosave last wrote or read it. */
  get diskStamp(): number {
    return this.lastStamp;
  }

  get dirty(): boolean {
    return this.store.revision !== this.savedRevision;
  }

  on(listener: (event: SaveEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  start(): void {
    this.unsubscribe ??= this.store.subscribe(() => {
      if (!this.dirty || this.status === "conflict") return;
      if (this.status !== "saving") this.emit("unsaved");
      this.schedule(this.debounce);
    });
  }

  /** Stops listening. Does not write: call `flush()` first to keep unsaved edits. */
  stop(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
    this.clearTimer();
  }

  /** Writes now if anything is unsaved, and waits for any write in progress. */
  async flush(): Promise<void> {
    this.clearTimer();
    await this.run(false);
  }

  /**
   * Checks project.json for a change made by something else (a file watcher calls this). Returns
   * true and enters "conflict" when it changed.
   */
  async checkOutside(): Promise<boolean> {
    if (this.writing) await this.writing.catch(() => undefined);
    if (await this.changedOnDisk()) {
      this.emit("conflict");
      return true;
    }
    return false;
  }

  /**
   * Ends a conflict. "mine" writes the project in memory over the disk version. "disk" loads the
   * disk version into the store (its undo history is dropped); it fails with the problems if the
   * disk version is not a valid project.
   */
  async resolve(
    choice: "mine" | "disk",
  ): Promise<{ ok: true } | { ok: false; problems: Problem[] }> {
    if (choice === "mine") {
      await this.run(true);
      return { ok: true };
    }
    const text = (await readText(this.dir, PROJECT_FILE)) ?? "";
    const result = parseProject(text);
    if (!result.ok)
      return {
        ok: false,
        problems:
          result.reason === "invalid"
            ? result.problems
            : [{ path: "version", message: `is from a newer app (${result.version})` }],
      };
    // The revision never goes back, even when the disk version is older.
    const revision = Math.max(result.project.revision, this.store.revision) + 1;
    this.store.replace({ ...result.project, revision });
    this.lastText = text;
    this.lastStamp = (await fileStamp(this.dir, PROJECT_FILE)) ?? 0;
    this.savedRevision = revision;
    this.emit("saved");
    return { ok: true };
  }

  private schedule(delay: number): void {
    this.clearTimer();
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.run(false);
    }, delay);
  }

  private clearTimer(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  private run(force: boolean): Promise<void> {
    if (this.writing) {
      this.again = true;
      return this.writing;
    }
    this.writing = (async () => {
      let overwrite = force;
      do {
        this.again = false;
        await this.writeOnce(overwrite);
        overwrite = false;
      } while (this.again && this.dirty && this.status !== "conflict" && this.status !== "error");
    })().finally(() => {
      this.writing = null;
    });
    return this.writing;
  }

  private async changedOnDisk(): Promise<boolean> {
    const stamp = await fileStamp(this.dir, PROJECT_FILE);
    if (stamp === this.lastStamp) return false;
    const text = await readText(this.dir, PROJECT_FILE);
    if (text === this.lastText) {
      this.lastStamp = stamp ?? 0;
      return false;
    }
    return true;
  }

  private async writeOnce(overwrite: boolean): Promise<void> {
    if (this.status === "conflict" && !overwrite) return;
    if (!this.dirty && !overwrite) return;
    const started = performance.now();
    try {
      if (!overwrite && (await this.changedOnDisk())) {
        this.emit("conflict");
        return;
      }
      const project = this.store.project;
      const text = serialiseProject(project);
      this.emit("saving");
      const now = this.now();
      this.lastStamp = await writeText(this.dir, PROJECT_FILE, text);
      this.lastText = text;
      await writeText(this.dir, META_FILE, `${JSON.stringify(metaOf(project, now), null, 2)}\n`);
      if (this.backups) await backUp(this.dir, text, project.revision, now, this.backups);
      this.savedRevision = project.revision;
      this.failures = 0;
      this.emit("saved", { bytes: text.length, ms: performance.now() - started });
      this.options.onSaved?.(text, project.revision);
      if (this.dirty) {
        this.emit("unsaved");
        this.schedule(this.debounce);
      }
    } catch (error) {
      this.failures++;
      const retryIn = backoff(this.failures);
      this.emit("error", {
        error: error instanceof Error ? error.message : String(error),
        retryIn,
      });
      this.schedule(retryIn);
    }
  }

  private emit(status: SaveStatus, extra: Partial<SaveEvent> = {}): void {
    this.status = status;
    const event = { ...this.state, ...extra };
    for (const listener of this.listeners) listener(event);
  }
}
