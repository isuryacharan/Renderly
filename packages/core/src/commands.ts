// Commands: every change to a project is a typed command with a validated payload. Commands are
// plain data ({ type, payload }), so they can be logged, replayed, sent from another tab or issued
// by an agent. A command's `apply` edits an Immer draft; the store records the patches.
//
// Commands must be deterministic: everything they need, including the ids of things they create,
// is in the payload. Use `createId()` when building the command, not inside `apply`.

import type { Draft } from "immer";
import type { z } from "zod";
import type { Project } from "./schema";

export interface Command<T extends string = string, P = unknown> {
  type: T;
  payload: P;
}

export interface CommandDefinition<P> {
  type: string;
  payload: z.ZodType<P>;
  /** A short description for the undo menu, e.g. "Move 3 markers". */
  label: (payload: P) => string;
  /** Edits the draft. Throws `CommandError` when the payload does not fit the project. */
  apply: (project: Draft<Project>, payload: P) => void;
}

export type FailureCode =
  /** No command of this type is registered. */
  | "unknown-command"
  /** The payload does not match the command's schema. */
  | "invalid-payload"
  /** The payload names something that is not in the project. */
  | "not-found"
  /** The command does not apply in the project's current state. */
  | "rejected"
  /** The command would leave the project invalid; nothing changed. */
  | "invalid-result"
  /** `expectedRevision` did not match: the project moved on. */
  | "conflict"
  /** A gesture is in progress; commit or cancel it first. */
  | "gesture-active"
  /** The project is open read-only here (another tab is editing it). */
  | "read-only";

export interface Failure {
  code: FailureCode;
  message: string;
  /** Where the problem is, when there is a place: in the payload or in the project. */
  path?: string;
  /** What the caller can do next. */
  hint: string;
}

const HINTS: Record<FailureCode, string> = {
  "unknown-command": "Check the command type against the registered commands.",
  "invalid-payload": "Fix the payload at `path` and send the command again.",
  "not-found": "Read the project again; the target may have been removed.",
  rejected: "Read the project again and adjust the command to its current state.",
  "invalid-result": "This is a bug in the command; report it with the payload.",
  conflict: "Read the project again, rebase your change on the new revision, and retry.",
  "gesture-active": "Wait for the gesture to commit or cancel, then retry.",
  "read-only": "Another tab is editing this project. Take over editing first.",
};

export function failure(code: FailureCode, message: string, path?: string): Failure {
  return { code, message, ...(path === undefined ? {} : { path }), hint: HINTS[code] };
}

/** Thrown by `apply` to reject a command. The store turns it into a typed failure. */
export class CommandError extends Error {
  constructor(
    readonly code: "not-found" | "rejected",
    message: string,
    readonly path?: string,
  ) {
    super(message);
  }
}

const registry = new Map<string, CommandDefinition<unknown>>();

export function defineCommand<P>(definition: CommandDefinition<P>): CommandDefinition<P> {
  if (registry.has(definition.type))
    throw new Error(`Command "${definition.type}" is already registered`);
  registry.set(definition.type, definition as CommandDefinition<unknown>);
  return definition;
}

export function getCommand(type: string): CommandDefinition<unknown> | undefined {
  return registry.get(type);
}

export function commandTypes(): string[] {
  return [...registry.keys()];
}
