// The project store: the current project, its revision, undo and redo, transactions, gestures,
// and the automation contract on dispatch. All edits arrive as commands.
//
// - `revision` increases with every change (commands, undo, redo, committed gestures) and never
//   goes back. It lives in the document, but is not part of undo.
// - Undo records are Immer patches, not snapshots.
// - A transaction groups several commands into one undo step.
// - A gesture (a slider or drag) previews many values against the state it started from and
//   becomes one undo step when it commits.

import { applyPatches, enablePatches, produce, produceWithPatches, type Patch } from "immer";
import { diffEntities, type Changes } from "./changes";
import { CommandError, failure, getCommand, type Command, type Failure } from "./commands";
import { formatPath, projectSchema, type Project } from "./schema";

enablePatches();

export interface DispatchOptions {
  /** Fail with a conflict unless the project is at this revision. */
  expectedRevision?: number;
  /** Validate and report the changes without committing anything. */
  dryRun?: boolean;
  /** Retrying with the same id returns the first result instead of applying again. */
  requestId?: string;
}

export type DispatchResult =
  | { ok: true; revision: number; changes: Changes; dryRun: boolean; requestId?: string }
  | { ok: false; revision: number; error: Failure; requestId?: string };

export interface HistoryEntry {
  label: string;
  patches: Patch[];
  inverse: Patch[];
}

export type UndoResult =
  | { ok: true; revision: number; label: string; changes: Changes }
  | {
      ok: false;
      revision: number;
      reason: "nothing-to-undo" | "nothing-to-redo" | "gesture-active" | "read-only";
    };

export interface StoreOptions {
  /** Validate the whole project after each command (rejects a command that breaks it). */
  validate?: boolean;
  /** Undo steps kept. */
  historyLimit?: number;
}

export interface Gesture {
  readonly key: string;
  /** Shows the command's result without recording it. Each preview starts from the same state. */
  preview(payload: unknown): DispatchResult;
  /** Applies the command for real: one undo step. */
  commit(payload: unknown): DispatchResult;
  /** Puts the project back as it was when the gesture began. */
  cancel(): void;
}

type Listener = () => void;

const REQUEST_MEMORY = 256;

/** Revision is bookkeeping, not an edit: undo patches never touch it. */
const withoutRevision = (patches: Patch[]) =>
  patches.filter((patch) => patch.path[0] !== "revision");

export class ProjectStore {
  private current: Project;
  private readonly undoStack: HistoryEntry[] = [];
  private readonly redoStack: HistoryEntry[] = [];
  private readonly listeners = new Set<Listener>();
  private readonly results = new Map<string, DispatchResult>();
  private transactionEntry: HistoryEntry | null = null;
  private gesture: { key: string; base: Project } | null = null;
  private readonly validate: boolean;
  private readonly historyLimit: number;

  constructor(project: Project, options: StoreOptions = {}) {
    this.current = project;
    this.validate = options.validate ?? true;
    this.historyLimit = options.historyLimit ?? 500;
  }

  /** Refuses every edit while true: set when another tab is the project's writer. */
  readOnly = false;

  get project(): Project {
    return this.current;
  }

  get revision(): number {
    return this.current.revision;
  }

  /**
   * The project without a running gesture's preview: what saving should write. The same as
   * `project` when no gesture is running.
   */
  get committed(): Project {
    return this.gesture ? this.gesture.base : this.current;
  }

  get canUndo(): boolean {
    return this.undoStack.length > 0;
  }

  get canRedo(): boolean {
    return this.redoStack.length > 0;
  }

  /** Undo labels, most recent first. */
  get undoLabels(): string[] {
    return this.undoStack.map((entry) => entry.label).reverse();
  }

  /**
   * Swaps in a whole project (a newer version from disk, a restored backup). Undo history and
   * remembered request ids are dropped: they describe a document that is gone.
   */
  replace(project: Project): void {
    if (this.gesture) throw new Error(`Gesture "${this.gesture.key}" is still active`);
    this.current = project;
    this.undoStack.length = 0;
    this.redoStack.length = 0;
    this.results.clear();
    this.notify();
  }

  /** Calls `listener` after every change. Returns an unsubscribe function. */
  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /**
   * Calls `listener` when the selected value changes (by `equals`, default `Object.is`). Thanks to
   * structural sharing, selecting an object is cheap and changes only when that object changed.
   */
  select<T>(
    selector: (project: Project) => T,
    listener: (value: T, previous: T) => void,
    equals = Object.is,
  ): () => void {
    let last = selector(this.current);
    return this.subscribe(() => {
      const next = selector(this.current);
      if (!equals(next, last)) {
        const previous = last;
        last = next;
        listener(next, previous);
      }
    });
  }

  dispatch(command: Command, options: DispatchOptions = {}): DispatchResult {
    const { requestId } = options;
    if (requestId !== undefined) {
      const earlier = this.results.get(requestId);
      if (earlier) return earlier;
    }
    const result = this.run(command, options);
    if (requestId !== undefined && !(result.ok && result.dryRun))
      this.remember(requestId, { ...result, requestId });
    return requestId === undefined ? result : { ...result, requestId };
  }

  /**
   * Runs `body` as one undo step: every command it dispatches is grouped. If `body` throws or
   * returns false, everything it did is rolled back.
   */
  transaction(label: string, body: () => unknown): boolean {
    if (this.transactionEntry) return body() !== false; // nested: join the outer transaction
    const entry: HistoryEntry = { label, patches: [], inverse: [] };
    this.transactionEntry = entry;
    let keep = false;
    try {
      keep = body() !== false;
    } finally {
      this.transactionEntry = null;
      if (!keep && entry.patches.length > 0) {
        this.commitState(applyPatches(this.current, entry.inverse));
      } else if (entry.patches.length > 0) {
        this.pushUndo(entry);
      }
    }
    return keep;
  }

  undo(): UndoResult {
    if (this.readOnly) return { ok: false, revision: this.revision, reason: "read-only" };
    if (this.gesture) return { ok: false, revision: this.revision, reason: "gesture-active" };
    const entry = this.undoStack.pop();
    if (!entry) return { ok: false, revision: this.revision, reason: "nothing-to-undo" };
    const before = this.current;
    const next = applyPatches(before, entry.inverse);
    this.commitState(next);
    this.redoStack.push(entry);
    return {
      ok: true,
      revision: this.revision,
      label: entry.label,
      changes: diffEntities(before, next),
    };
  }

  redo(): UndoResult {
    if (this.readOnly) return { ok: false, revision: this.revision, reason: "read-only" };
    if (this.gesture) return { ok: false, revision: this.revision, reason: "gesture-active" };
    const entry = this.redoStack.pop();
    if (!entry) return { ok: false, revision: this.revision, reason: "nothing-to-redo" };
    const before = this.current;
    const next = applyPatches(before, entry.patches);
    this.commitState(next);
    this.undoStack.push(entry);
    return {
      ok: true,
      revision: this.revision,
      label: entry.label,
      changes: diffEntities(before, next),
    };
  }

  /**
   * Starts a gesture for commands of `type` (a slider drag, a clip drag). Only one gesture runs at
   * a time, and ordinary dispatches wait until it ends. The caller throttles previews (~30 ms).
   */
  beginGesture(key: string, type: string): Gesture {
    if (this.readOnly) throw new Error("The project is open read-only");
    if (this.gesture) throw new Error(`Gesture "${this.gesture.key}" is still active`);
    const base = this.current;
    this.gesture = { key, base };
    let open = true;
    const end = () => {
      open = false;
      this.gesture = null;
    };
    const closed = (): DispatchResult => ({
      ok: false,
      revision: this.revision,
      error: failure("rejected", `Gesture "${key}" has ended`),
    });
    return {
      key,
      preview: (payload) => {
        if (!open) return closed();
        const attempt = this.compute({ type, payload }, base, false);
        if (!attempt.ok) return { ok: false, revision: this.revision, error: attempt.error };
        this.current = { ...attempt.next, revision: base.revision };
        this.notify();
        return {
          ok: true,
          revision: this.revision,
          changes: diffEntities(base, attempt.next),
          dryRun: true,
        };
      },
      commit: (payload) => {
        if (!open) return closed();
        const attempt = this.compute({ type, payload }, base, this.validate);
        if (!attempt.ok) {
          this.current = base;
          end();
          this.notify();
          return { ok: false, revision: this.revision, error: attempt.error };
        }
        end();
        this.current = base;
        this.pushUndo({ label: attempt.label, patches: attempt.patches, inverse: attempt.inverse });
        this.commitState(attempt.next);
        return {
          ok: true,
          revision: this.revision,
          changes: diffEntities(base, attempt.next),
          dryRun: false,
        };
      },
      cancel: () => {
        if (!open) return;
        end();
        this.current = base;
        this.notify();
      },
    };
  }

  private run(command: Command, options: DispatchOptions): DispatchResult {
    const fail = (error: Failure): DispatchResult => ({
      ok: false,
      revision: this.revision,
      error,
    });
    if (this.readOnly) return fail(failure("read-only", "The project is open read-only"));
    if (this.gesture)
      return fail(failure("gesture-active", `Gesture "${this.gesture.key}" is in progress`));
    if (options.expectedRevision !== undefined && options.expectedRevision !== this.revision) {
      return fail(
        failure(
          "conflict",
          `Expected revision ${options.expectedRevision}, but the project is at ${this.revision}`,
        ),
      );
    }
    const attempt = this.compute(command, this.current, this.validate);
    if (!attempt.ok) return fail(attempt.error);
    const changes = diffEntities(this.current, attempt.next);
    if (options.dryRun) return { ok: true, revision: this.revision, changes, dryRun: true };
    // A command that changed nothing (a trim already at its limit) is not an undo step.
    if (attempt.next === this.current)
      return { ok: true, revision: this.revision, changes, dryRun: false };

    const entry: HistoryEntry = {
      label: attempt.label,
      patches: attempt.patches,
      inverse: attempt.inverse,
    };
    if (this.transactionEntry) {
      this.transactionEntry.patches.push(...entry.patches);
      this.transactionEntry.inverse.unshift(...entry.inverse);
    } else {
      this.pushUndo(entry);
    }
    this.commitState(attempt.next);
    return { ok: true, revision: this.revision, changes, dryRun: false };
  }

  /** Runs a command against `base` without touching the store. */
  private compute(command: Command, base: Project, validate: boolean) {
    type Attempt =
      | { ok: true; next: Project; patches: Patch[]; inverse: Patch[]; label: string }
      | { ok: false; error: Failure };
    const definition = getCommand(command.type);
    if (!definition)
      return {
        ok: false,
        error: failure("unknown-command", `No command "${command.type}"`),
      } as Attempt;
    const parsed = definition.payload.safeParse(command.payload);
    if (!parsed.success) {
      const issue = parsed.error.issues[0];
      return {
        ok: false,
        error: failure(
          "invalid-payload",
          issue?.message ?? "Invalid payload",
          formatPath(issue?.path ?? []),
        ),
      } as Attempt;
    }
    try {
      const [next, patches, inverse] = produceWithPatches(base, (draft) => {
        definition.apply(draft, parsed.data);
      });
      if (validate && next !== base) {
        const check = projectSchema().safeParse(next);
        if (!check.success) {
          const issue = check.error.issues[0];
          return {
            ok: false,
            error: failure(
              "invalid-result",
              issue?.message ?? "Invalid project",
              formatPath(issue?.path ?? []),
            ),
          } as Attempt;
        }
      }
      return {
        ok: true,
        next,
        patches: withoutRevision(patches),
        inverse: withoutRevision(inverse),
        label: definition.label(parsed.data),
      } as Attempt;
    } catch (error) {
      if (error instanceof CommandError)
        return { ok: false, error: failure(error.code, error.message, error.path) } as Attempt;
      throw error;
    }
  }

  /** Makes `next` current with the next revision and tells subscribers. */
  private commitState(next: Project): void {
    const revision = this.current.revision + 1;
    this.current = produce(next, (draft) => {
      draft.revision = revision;
    });
    this.notify();
  }

  private pushUndo(entry: HistoryEntry): void {
    this.undoStack.push(entry);
    if (this.undoStack.length > this.historyLimit) this.undoStack.shift();
    this.redoStack.length = 0;
  }

  private remember(requestId: string, result: DispatchResult): void {
    this.results.set(requestId, result);
    if (this.results.size > REQUEST_MEMORY) {
      const oldest = this.results.keys().next().value;
      if (oldest !== undefined) this.results.delete(oldest);
    }
  }

  private notify(): void {
    for (const listener of this.listeners) listener();
  }
}
