// Agent chat: wire types and the pure transcript reducer (docs/agent-chat-design.md §4.2).
// `reduceAgentEvent` turns backend `agent:event` payloads into transcript items. It has no
// React, Tauri or store dependency, so it is covered by Vitest (agentChat.test.ts).

export type AgentId = "claude-code" | "codex";

export interface AgentInfo {
  id: AgentId;
  name: string;
  installed: boolean;
  version: string | null;
  signIn: "signed_in" | "signed_out" | "unknown";
  signInCommand: string | null;
  installHint: string;
  note: string | null;
}

export interface PermissionOption {
  id: "allow_once" | "allow_always" | "reject_once" | string;
  label: string;
}

/** Mirror of the Rust `AgentEvent` enum (src-tauri/src/agent_chat/events.rs). */
export type AgentEvent =
  | { type: "session_started"; agent_id: string }
  | { type: "text_delta"; text: string }
  | { type: "tool_started"; id: string; name: string; input: unknown }
  | { type: "tool_finished"; id: string; output_summary: string; is_error: boolean }
  | {
      type: "permission_request";
      request_id: string;
      tool: string;
      input: unknown;
      options: PermissionOption[];
    }
  | { type: "turn_finished"; stop_reason: string }
  | { type: "error"; message: string }
  | { type: "session_ended"; reason: string };

/** The `agent:event` payload: the event plus the session it belongs to. */
export type AgentEnvelope = { session_id: string } & AgentEvent;

/** Context chips sent with a prompt (Rust `ChatContext`, camelCase). */
export interface ChatContext {
  playheadSecs?: number;
  selectedClipIds?: string[];
  sequenceId?: string;
}

export type TranscriptItem =
  | { kind: "user"; id: string; text: string; context: ChatContext | null }
  | { kind: "agent"; id: string; text: string; streaming: boolean }
  | {
      kind: "tool";
      id: string;
      name: string;
      input: unknown;
      status: "running" | "done" | "error";
      output: string;
    }
  | {
      kind: "permission";
      id: string;
      tool: string;
      input: unknown;
      options: PermissionOption[];
      /** Option the user chose, or "cancelled" when the session ended first. */
      answer: string | null;
    }
  | { kind: "notice"; id: string; tone: "info" | "error"; text: string }
  | { kind: "turn"; id: string; stopReason: string };

export type SessionStatus = "idle" | "starting" | "ready" | "running" | "ended";

export interface AgentChatState {
  sessionId: string | null;
  agentId: AgentId | null;
  status: SessionStatus;
  items: TranscriptItem[];
  /** Last error text shown in the header (cleared by the next turn). */
  error: string | null;
  /** Per-turn sequence for stable item ids. */
  seq: number;
}

export const EMPTY_AGENT_CHAT: AgentChatState = {
  sessionId: null,
  agentId: null,
  status: "idle",
  items: [],
  error: null,
  seq: 0,
};

const nextId = (s: AgentChatState, prefix: string): [string, AgentChatState] => {
  const seq = s.seq + 1;
  return [`${prefix}-${seq}`, { ...s, seq }];
};

/** Record a prompt the user just sent and mark the session busy. */
export function reduceUserSent(
  s: AgentChatState,
  text: string,
  context: ChatContext | null,
): AgentChatState {
  const [id, next] = nextId(s, "user");
  return {
    ...next,
    status: "running",
    error: null,
    items: [...next.items.map(closeStreaming), { kind: "user", id, text, context }],
  };
}

function closeStreaming(item: TranscriptItem): TranscriptItem {
  return item.kind === "agent" && item.streaming ? { ...item, streaming: false } : item;
}

/** Mark a pending permission prompt as answered (the panel calls this after the IPC returns). */
export function reducePermissionAnswered(
  s: AgentChatState,
  requestId: string,
  optionId: string,
): AgentChatState {
  return {
    ...s,
    items: s.items.map((item) =>
      item.kind === "permission" && item.id === requestId ? { ...item, answer: optionId } : item,
    ),
  };
}

/** Apply one backend event. Events for another session are ignored. */
export function reduceAgentEvent(s: AgentChatState, env: AgentEnvelope): AgentChatState {
  if (s.sessionId !== null && env.session_id !== s.sessionId) return s;
  const base: AgentChatState = s.sessionId === null ? { ...s, sessionId: env.session_id } : s;
  switch (env.type) {
    case "session_started":
      return { ...base, agentId: env.agent_id as AgentId, status: "ready", error: null };

    case "text_delta": {
      const last = base.items[base.items.length - 1];
      if (last && last.kind === "agent" && last.streaming) {
        const items = base.items.slice(0, -1).concat({ ...last, text: last.text + env.text });
        return { ...base, items };
      }
      const [id, next] = nextId(base, "agent");
      return {
        ...next,
        items: [...next.items, { kind: "agent", id, text: env.text, streaming: true }],
      };
    }

    case "tool_started":
      return {
        ...base,
        status: "running",
        items: [
          ...base.items.map(closeStreaming),
          {
            kind: "tool",
            id: env.id,
            name: env.name,
            input: env.input,
            status: "running",
            output: "",
          },
        ],
      };

    case "tool_finished":
      return {
        ...base,
        items: base.items.map((item) =>
          item.kind === "tool" && item.id === env.id
            ? {
                ...item,
                status: env.is_error ? "error" : "done",
                output: env.output_summary,
              }
            : item,
        ),
      };

    case "permission_request": {
      const [, next] = nextId(base, "perm");
      return {
        ...next,
        items: [
          ...next.items.map(closeStreaming),
          {
            kind: "permission",
            id: env.request_id,
            tool: env.tool,
            input: env.input,
            options: env.options,
            answer: null,
          },
        ],
      };
    }

    case "turn_finished": {
      const items = base.items.map(closeStreaming);
      const isNormal = env.stop_reason === "end_turn" || env.stop_reason === "completed";
      const [id, next] = nextId({ ...base, items }, "turn");
      const withTurn: TranscriptItem[] = isNormal
        ? next.items
        : [...next.items, { kind: "turn", id, stopReason: env.stop_reason }];
      return {
        ...next,
        status: base.status === "ended" ? "ended" : "ready",
        items: withTurn,
      };
    }

    case "error": {
      const [id, next] = nextId(base, "err");
      return {
        ...next,
        error: env.message,
        items: [
          ...next.items.map(closeStreaming),
          { kind: "notice", id, tone: "error", text: env.message },
        ],
      };
    }

    case "session_ended": {
      const [id, next] = nextId(base, "end");
      return {
        ...next,
        status: "ended",
        items: [
          ...next.items.map((item) =>
            item.kind === "permission" && item.answer === null
              ? { ...item, answer: "cancelled" }
              : closeStreaming(item),
          ),
          { kind: "notice", id, tone: "info", text: `Session ended (${env.reason}).` },
        ],
      };
    }
  }
}

/** Whether a tool name is a Renderly edit that changed the project (shows an edit card). */
export function isRenderlyEdit(name: string): boolean {
  return name === "mcp__renderly__apply_command" || name === "mcp__renderly__apply_commands";
}

/** Human-readable label for a command name: "SetClipOpacity" -> "Set clip opacity". */
export function humanizeCommand(name: string): string {
  const spaced = name.replace(/([a-z0-9])([A-Z])/g, "$1 $2").replace(/([A-Z])([A-Z][a-z])/g, "$1 $2");
  const lower = spaced.toLowerCase();
  return lower.charAt(0).toUpperCase() + lower.slice(1);
}

/**
 * One line describing an edit's input for the card title. Reads the first command of
 * `apply_command` / `apply_commands`; falls back to the raw tool name.
 */
export function describeEdit(name: string, input: unknown): { title: string; count: number } {
  const obj = (input ?? {}) as Record<string, unknown>;
  const commands: Record<string, unknown>[] = [];
  if (name.endsWith("apply_commands") && Array.isArray(obj.commands)) {
    for (const c of obj.commands) if (c && typeof c === "object") commands.push(c as Record<string, unknown>);
  } else if (obj.command && typeof obj.command === "object") {
    commands.push(obj.command as Record<string, unknown>);
  }
  if (commands.length === 0) return { title: name, count: 0 };
  const first = commands[0]!;
  const kind = typeof first.command === "string" ? humanizeCommand(first.command) : "Edit";
  const clip = typeof first.clip_id === "string" ? ` · clip ${first.clip_id.slice(0, 8)}` : "";
  const more = commands.length > 1 ? ` (+${commands.length - 1} more)` : "";
  return { title: `${kind}${clip}${more}`, count: commands.length };
}

/** The most recent successful edit card: the only one whose Undo is offered, because Undo
 * reverts the latest project change, not an arbitrary one. */
export function lastSuccessfulEditId(items: TranscriptItem[]): string | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i]!;
    if (item.kind === "tool" && isRenderlyEdit(item.name)) {
      return item.status === "done" ? item.id : null;
    }
  }
  return null;
}
