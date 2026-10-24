// Agent chat: wire types and the pure transcript reducer (docs/agent-chat-design.md §4.2).
// `reduceAgentEvent` turns backend `agent:event` payloads into transcript items. It has no
// React, Tauri or store dependency, so it is covered by Vitest (agentChat.test.ts).

export type AgentId = "claude-code" | "codex" | "ollama";

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

/**
 * What the panel sends with a prompt beyond the message (Rust `ChatContext`, camelCase). The
 * playhead, timeline outline and open sequence go to the agent automatically in the backend's
 * context block, so only the user's selection is sent here.
 */
export interface ChatContext {
  selectedClipIds?: string[];
}

/**
 * Launch options sent with a session start (Rust `LaunchOptions`, camelCase). `model` and
 * `effort` are null when the user has no choice, so the agent's own default applies. Nothing
 * here is a fixed list: the choices come from the agent (see `AgentModels`).
 */
export interface LaunchOptions {
  model: string | null;
  effort: string | null;
  /** Let the agent use its own built-in tools (each still asks first). */
  nativeTools: boolean;
  /** Ollama only: the server address, or null for `OLLAMA_HOST` / the default. */
  ollamaHost: string | null;
}

export const DEFAULT_LAUNCH_OPTIONS: LaunchOptions = {
  model: null,
  effort: null,
  nativeTools: false,
  ollamaHost: null,
};

/** One model the agent reported (Rust `ModelOption`). */
export interface ModelOption {
  id: string;
  label: string;
  description: string | null;
  /** Effort levels offered for this model. Empty when the picker should not offer effort. */
  efforts: string[];
  defaultEffort: string | null;
  contextLength: number | null;
  vision: boolean;
  thinking: boolean;
}

/** Why a list is or is not available (Rust `ModelStatus`). */
export type ModelStatus =
  | "ready"
  | "not_installed"
  | "not_signed_in"
  | "not_running"
  | "no_models"
  | "error";

/** One agent's model list as fetched at runtime (Rust `AgentModels`). */
export interface AgentModels {
  agentId: AgentId;
  status: ModelStatus;
  models: ModelOption[];
  defaultModel: string | null;
  message: string | null;
}

/**
 * The picker's remembered choices, per agent (a viewer preference, kept in localStorage). A saved
 * model or effort is only used while the agent still offers it.
 */
export interface LaunchPrefs {
  nativeTools: boolean;
  ollamaHost: string | null;
  models: Partial<Record<AgentId, string>>;
  efforts: Partial<Record<AgentId, string>>;
}

export const DEFAULT_LAUNCH_PREFS: LaunchPrefs = {
  nativeTools: false,
  ollamaHost: null,
  models: {},
  efforts: {},
};

/** The model to use for `list`, given the user's saved choice. */
export interface ModelChoice {
  /** The effective model id, or null when no list is known and nothing was saved. */
  model: string | null;
  /** Shown when a saved choice is no longer offered and the default was used instead. */
  notice: string | null;
}

/**
 * Picks the model. A saved choice that the agent still offers wins. Otherwise the agent's own
 * default is used, and a saved choice that vanished gets a notice saying so.
 */
export function chooseModel(
  list: AgentModels | null,
  saved: string | null | undefined,
  agentName: string,
): ModelChoice {
  if (!list || list.status !== "ready" || list.models.length === 0) {
    return { model: saved ?? null, notice: null };
  }
  const offered = (id: string | null | undefined): id is string =>
    !!id && list.models.some((m) => m.id === id);
  if (offered(saved)) return { model: saved, notice: null };
  const fallback =
    (offered(list.defaultModel) ? list.defaultModel : null) ?? list.models[0]?.id ?? null;
  const notice =
    saved && fallback
      ? `"${saved}" is no longer offered by ${agentName}. Using ${fallback}.`
      : null;
  return { model: fallback, notice };
}

/** The model entry for `id`, when the list has it. */
export function modelOption(list: AgentModels | null, id: string | null): ModelOption | null {
  if (!list || !id) return null;
  return list.models.find((m) => m.id === id) ?? null;
}

/**
 * The effort choice for a model: a saved level only when that model offers it, else null (the
 * agent's default). Ollama offers effort only for thinking models, so the picker hides it otherwise.
 */
export function chooseEffort(model: ModelOption | null, saved: string | null | undefined): string | null {
  if (!model || model.efforts.length === 0) return null;
  return saved && model.efforts.includes(saved) ? saved : null;
}

/** The launch options for a session start, from the fetched list and the saved preferences. */
export function resolveLaunchOptions(
  agent: AgentId,
  agentName: string,
  list: AgentModels | null,
  prefs: LaunchPrefs,
): { options: LaunchOptions; notice: string | null } {
  const choice = chooseModel(list, prefs.models[agent], agentName);
  const effort = chooseEffort(modelOption(list, choice.model), prefs.efforts[agent]);
  return {
    options: {
      model: choice.model,
      effort,
      nativeTools: prefs.nativeTools,
      ollamaHost: agent === "ollama" ? prefs.ollamaHost : null,
    },
    notice: choice.notice,
  };
}

/** Parse saved preferences. Anything unreadable falls back to the defaults. */
export function parseLaunchPrefs(raw: string | null): LaunchPrefs {
  if (!raw) return DEFAULT_LAUNCH_PREFS;
  try {
    const parsed = JSON.parse(raw) as Partial<LaunchPrefs>;
    return {
      nativeTools: parsed.nativeTools === true,
      ollamaHost: typeof parsed.ollamaHost === "string" ? parsed.ollamaHost : null,
      models: sanitizeChoices(parsed.models),
      efforts: sanitizeChoices(parsed.efforts),
    };
  } catch {
    return DEFAULT_LAUNCH_PREFS;
  }
}

function sanitizeChoices(value: unknown): Partial<Record<AgentId, string>> {
  const out: Partial<Record<AgentId, string>> = {};
  if (!value || typeof value !== "object") return out;
  for (const [key, v] of Object.entries(value as Record<string, unknown>)) {
    if ((key === "claude-code" || key === "codex" || key === "ollama") && typeof v === "string") {
      out[key] = v;
    }
  }
  return out;
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
      /** What an `edit` call changed, as plain lines, shown as the turn's action list. */
      kind: "actions";
      id: string;
      toolId: string;
      /** One line per command, e.g. "Trimmed 'Gameplay 1' to 0:00.0-0:05.0". */
      lines: string[];
      /** Short ids of anything the edit created. */
      created: string[];
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

    case "tool_finished": {
      const finished = base.items.find((item) => item.kind === "tool" && item.id === env.id);
      const items = base.items.map((item) =>
        item.kind === "tool" && item.id === env.id
          ? {
              ...item,
              status: env.is_error ? ("error" as const) : ("done" as const),
              output: env.output_summary,
            }
          : item,
      );
      // A successful edit becomes plain action lines in the transcript; the raw call stays
      // available under the turn's details.
      if (finished && finished.kind === "tool" && isEditTool(finished.name) && !env.is_error) {
        const { lines, created } = parseEditOutput(env.output_summary);
        const [id, next] = nextId({ ...base, items }, "act");
        return {
          ...next,
          items: [...next.items, { kind: "actions", id, toolId: env.id, lines, created }],
        };
      }
      return { ...base, items };
    }

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

/** The Renderly tools as Claude Code names them (`mcp__<server>__<tool>`, server `renderly`). */
export const EDIT_TOOL = "mcp__renderly__edit";
const PERCEPTION_TOOLS = new Set([
  "mcp__renderly__timeline",
  "mcp__renderly__look",
  "mcp__renderly__assets",
  "mcp__renderly__find_media",
  "mcp__renderly__transcript",
]);

/** The `edit` tool: applies commands to the project as one undo step. */
export function isEditTool(name: string): boolean {
  return name === EDIT_TOOL;
}

/** Read-only tools the agent uses to look (outline, frame, catalog, media search). */
export function isPerceptionTool(name: string): boolean {
  return PERCEPTION_TOOLS.has(name);
}

/**
 * Whether a tool name is a Renderly edit that changed the project. Kept for the older tool
 * names as well, so transcripts from before the `edit` tool still read correctly.
 */
export function isRenderlyEdit(name: string): boolean {
  return (
    isEditTool(name) ||
    name === "mcp__renderly__apply_command" ||
    name === "mcp__renderly__apply_commands"
  );
}

/**
 * Splits an `edit` result into action lines ("- ...") and the "Created: ..." short ids. Anything
 * else in the text is ignored.
 */
export function parseEditOutput(output: string): { lines: string[]; created: string[] } {
  const lines: string[] = [];
  const created: string[] = [];
  for (const raw of output.split("\n")) {
    const line = raw.trim();
    if (line.startsWith("- ")) lines.push(line.slice(2));
    else if (line.startsWith("Created: ")) {
      for (const part of line.slice("Created: ".length).split(";")) {
        const text = part.trim();
        if (text) created.push(text);
      }
    }
  }
  return { lines, created };
}

/**
 * Short ids of the clips an edit touched, from its command inputs. `knownClipIds` are full
 * ids; a short id matches the full id it prefixes.
 */
export function clipIdsTouchedByEdit(input: unknown, knownClipIds: string[]): string[] {
  const obj = (input ?? {}) as Record<string, unknown>;
  const commands = Array.isArray(obj.commands) ? obj.commands : [];
  const out = new Set<string>();
  for (const c of commands) {
    if (!c || typeof c !== "object") continue;
    const clip = (c as Record<string, unknown>).clip_id;
    if (typeof clip !== "string" || clip.length < 8) continue;
    const full = knownClipIds.find((id) => id.toLowerCase().startsWith(clip.toLowerCase()));
    if (full) out.add(full);
  }
  return [...out];
}

/** What the agent is doing right now, for the working indicator. Null when idle. */
export function activityLabel(items: TranscriptItem[]): string | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i]!;
    if (item.kind === "tool" && item.status === "running") {
      if (isEditTool(item.name)) return "Applying the edit…";
      if (item.name === "mcp__renderly__timeline") return "Reading the timeline…";
      if (item.name === "mcp__renderly__look") return "Looking at the frame…";
      if (item.name === "mcp__renderly__find_media") return "Searching for media…";
      if (item.name === "mcp__renderly__assets") return "Checking effects and styles…";
      if (item.name === "mcp__renderly__transcript") return "Reading the transcript…";
      return "Using a tool…";
    }
    if (item.kind === "agent" && item.streaming) return null;
    if (item.kind === "user") return "Working…";
  }
  return null;
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
  if (Array.isArray(obj.commands)) {
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

/**
 * The action list of the current turn, when it is the most recent turn with edits. Undo is
 * offered only there: the whole turn is one undo step, and only the latest step is reachable.
 */
export function undoableTurnActionsId(items: TranscriptItem[]): string | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i]!;
    if (item.kind === "user") return null;
    if (item.kind === "actions") return item.id;
  }
  return null;
}

/** The most recent successful edit card (legacy tool-card form; kept for older transcripts). */
export function lastSuccessfulEditId(items: TranscriptItem[]): string | null {
  for (let i = items.length - 1; i >= 0; i--) {
    const item = items[i]!;
    if (item.kind === "tool" && isRenderlyEdit(item.name)) {
      return item.status === "done" ? item.id : null;
    }
  }
  return null;
}
