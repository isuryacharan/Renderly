// Agent chat UI state: the right column's tab, the agent picker, the session, and the prompt
// box. The transcript itself is built by the pure reducer in lib/agentChat.ts; this store
// only wires IPC and editor state to it.

import { create } from "zustand";
import * as ipc from "../lib/ipc";
import {
  DEFAULT_LAUNCH_PREFS,
  EMPTY_AGENT_CHAT,
  clipIdsTouchedByEdit,
  isEditTool,
  parseEditOutput,
  parseLaunchPrefs,
  reduceAgentEvent,
  reducePermissionAnswered,
  reduceUserSent,
  resolveLaunchOptions,
  type AgentChatState,
  type AgentEnvelope,
  type AgentId,
  type AgentInfo,
  type AgentModels,
  type ChatContext,
  type LaunchPrefs,
} from "../lib/agentChat";
import { useEditorStore } from "./editorStore";

export type RightTab = "inspector" | "agent";
/** Context chips the user can remove before sending. */
export type ChipKey = "selection";

/** How long timeline clips stay highlighted after an agent edit touches them. */
export const HIGHLIGHT_MS = 2400;
const LAUNCH_KEY = "renderly.agent.launch.v2";

/** Display names, for notices. The backend's `AgentInfo.name` is the source for the panel. */
export const AGENT_NAMES: Record<AgentId, string> = {
  "claude-code": "Claude Code",
  codex: "Codex",
  ollama: "Ollama (local)",
};

/** Preferences remembered per user (a viewer preference, so localStorage is enough). */
export function loadLaunchPrefs(): LaunchPrefs {
  try {
    return parseLaunchPrefs(globalThis.localStorage?.getItem(LAUNCH_KEY) ?? null);
  } catch {
    return DEFAULT_LAUNCH_PREFS;
  }
}

function saveLaunchPrefs(prefs: LaunchPrefs): void {
  try {
    globalThis.localStorage?.setItem(LAUNCH_KEY, JSON.stringify(prefs));
  } catch {
    // Storage can be blocked; the choice then lasts for this run only.
  }
}

let highlightTimer: ReturnType<typeof setTimeout> | null = null;

interface AgentChatStore {
  rightTab: RightTab;
  /** Bumped by Ctrl+L so the prompt box takes focus even when the tab is already open. */
  focusTick: number;
  agents: AgentInfo[] | null;
  agentsError: string | null;
  chat: AgentChatState;
  /** The user's remembered model, effort and tools choices, per agent. */
  launchPrefs: LaunchPrefs;
  /** Each agent's model list as the agent reported it, fetched lazily and kept per app session. */
  agentModels: Partial<Record<AgentId, AgentModels>>;
  modelsLoading: Partial<Record<AgentId, boolean>>;
  /** A line about a saved choice that was replaced when the session started. */
  modelNotice: Partial<Record<AgentId, string>>;
  /** The agent the settings popover and the picker's model list describe. */
  settingsAgent: AgentId;
  /** Full clip ids the last agent edit touched; the timeline highlights them briefly. */
  highlightClipIds: string[];
  removedChips: ChipKey[];
  draft: string;
  /** Sessions the user stopped: late events for them are dropped. */
  lastStoppedSessionId: string | null;

  setRightTab(tab: RightTab): void;
  toggleAgentTab(): void;
  requestPromptFocus(): void;
  loadAgents(): Promise<void>;
  startSession(agentId: AgentId): Promise<void>;
  stopSession(): Promise<void>;
  sendPrompt(): Promise<void>;
  cancelTurn(): Promise<void>;
  answerPermission(requestId: string, optionId: string): Promise<void>;
  loadModels(agentId: AgentId, refresh?: boolean): Promise<void>;
  setSettingsAgent(agentId: AgentId): void;
  setModelChoice(agentId: AgentId, model: string): void;
  setEffortChoice(agentId: AgentId, effort: string | null): void;
  setNativeTools(enabled: boolean): void;
  setOllamaHost(host: string | null): void;
  toggleChip(key: ChipKey): void;
  setDraft(text: string): void;
  onEvent(env: AgentEnvelope): void;
}

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Context for the next prompt: the user's selection, unless its chip was removed. */
export function chipContext(removed: ChipKey[]): ChatContext | null {
  const s = useEditorStore.getState();
  if (removed.includes("selection") || s.selections.length === 0) return null;
  return { selectedClipIds: s.selections.map((sel) => sel.clipId) };
}

/** Every clip id on the root timeline and in sequences, for resolving short ids. */
function knownClipIds(): string[] {
  const ids: string[] = [];
  const project = useEditorStore.getState().project;
  const tracks = [...(project?.tracks ?? []), ...(project?.sequences ?? []).flatMap((s) => s.tracks)];
  for (const t of tracks) for (const c of t.clips) ids.push(c.id);
  return ids;
}

export const useAgentChatStore = create<AgentChatStore>((set, get) => ({
  rightTab: "inspector",
  focusTick: 0,
  agents: null,
  agentsError: null,
  chat: EMPTY_AGENT_CHAT,
  launchPrefs: loadLaunchPrefs(),
  agentModels: {},
  modelsLoading: {},
  modelNotice: {},
  settingsAgent: "claude-code",
  highlightClipIds: [],
  removedChips: [],
  draft: "",
  lastStoppedSessionId: null,

  setRightTab(tab) {
    set({ rightTab: tab });
  },

  toggleAgentTab() {
    set((st) => ({ rightTab: st.rightTab === "agent" ? "inspector" : "agent" }));
  },

  requestPromptFocus() {
    set((st) => ({ rightTab: "agent", focusTick: st.focusTick + 1 }));
  },

  async loadAgents() {
    try {
      const agents = await ipc.agentList();
      set({ agents, agentsError: null });
    } catch (e) {
      set({ agentsError: errorText(e) });
    }
  },

  async startSession(agentId) {
    const { chat } = get();
    if (chat.status === "starting" || chat.status === "ready" || chat.status === "running") return;
    set({
      chat: { ...EMPTY_AGENT_CHAT, agentId, status: "starting" },
      lastStoppedSessionId: null,
    });
    try {
      if (!get().agentModels[agentId]) await get().loadModels(agentId);
      const name = get().agents?.find((a) => a.id === agentId)?.name ?? AGENT_NAMES[agentId];
      const { options, notice } = resolveLaunchOptions(
        agentId,
        name,
        get().agentModels[agentId] ?? null,
        get().launchPrefs,
      );
      if (notice) {
        set((st) => ({ modelNotice: { ...st.modelNotice, [agentId]: notice } }));
      }
      const sessionId = await ipc.agentStart(agentId, options);
      const cur = get().chat;
      // Events that arrived during start may already have adopted this id.
      set({ chat: { ...cur, sessionId: cur.sessionId ?? sessionId } });
    } catch (e) {
      set((st) => ({
        chat: {
          ...st.chat,
          status: "idle",
          error: errorText(e),
          items: [
            ...st.chat.items,
            { kind: "notice", id: `start-${st.chat.seq + 1}`, tone: "error", text: errorText(e) },
          ],
          seq: st.chat.seq + 1,
        },
      }));
    }
  },

  async stopSession() {
    const sid = get().chat.sessionId;
    if (!sid) return;
    set({ lastStoppedSessionId: sid });
    try {
      await ipc.agentStop(sid);
    } catch (e) {
      set((st) => ({ chat: { ...st.chat, error: errorText(e) } }));
    }
    set((st) => ({ chat: { ...st.chat, status: "ended" } }));
  },

  async sendPrompt() {
    const { chat, draft, removedChips } = get();
    const text = draft.trim();
    if (!text || !chat.sessionId || (chat.status !== "ready" && chat.status !== "running")) return;
    if (chat.status === "running") return;
    const context = chipContext(removedChips);
    set({ draft: "", chat: reduceUserSent(chat, text, context) });
    try {
      await ipc.agentSend(chat.sessionId, text, context);
    } catch (e) {
      set((st) => ({
        chat: {
          ...st.chat,
          status: "ready",
          error: errorText(e),
          items: [
            ...st.chat.items,
            { kind: "notice", id: `send-${st.chat.seq + 1}`, tone: "error", text: errorText(e) },
          ],
          seq: st.chat.seq + 1,
        },
      }));
    }
  },

  async cancelTurn() {
    const sid = get().chat.sessionId;
    if (!sid) return;
    try {
      await ipc.agentCancel(sid);
    } catch (e) {
      set((st) => ({ chat: { ...st.chat, error: errorText(e) } }));
    }
  },

  async answerPermission(requestId, optionId) {
    const sid = get().chat.sessionId;
    if (!sid) return;
    try {
      await ipc.agentAnswerPermission(sid, requestId, optionId);
      set((st) => ({ chat: reducePermissionAnswered(st.chat, requestId, optionId) }));
    } catch (e) {
      set((st) => ({ chat: { ...st.chat, error: errorText(e) } }));
    }
  },

  async loadModels(agentId, refresh = false) {
    if (get().modelsLoading[agentId]) return;
    set((st) => ({ modelsLoading: { ...st.modelsLoading, [agentId]: true } }));
    const host = get().launchPrefs.ollamaHost;
    try {
      const list = await ipc.agentModels(agentId, agentId === "ollama" ? host : null, refresh);
      set((st) => ({ agentModels: { ...st.agentModels, [agentId]: list } }));
    } catch (e) {
      // Shown as the agent's status line; the list stays as it was.
      const failed: AgentModels = {
        agentId,
        status: "error",
        models: [],
        defaultModel: null,
        message: errorText(e),
      };
      set((st) => ({ agentModels: { ...st.agentModels, [agentId]: failed } }));
    } finally {
      set((st) => ({ modelsLoading: { ...st.modelsLoading, [agentId]: false } }));
    }
  },

  setSettingsAgent(agentId) {
    set({ settingsAgent: agentId });
  },

  setModelChoice(agentId, model) {
    const prefs = { ...get().launchPrefs, models: { ...get().launchPrefs.models, [agentId]: model } };
    saveLaunchPrefs(prefs);
    set((st) => ({
      launchPrefs: prefs,
      modelNotice: { ...st.modelNotice, [agentId]: undefined },
    }));
  },

  setEffortChoice(agentId, effort) {
    const efforts = { ...get().launchPrefs.efforts };
    if (effort) efforts[agentId] = effort;
    else delete efforts[agentId];
    const prefs = { ...get().launchPrefs, efforts };
    saveLaunchPrefs(prefs);
    set({ launchPrefs: prefs });
  },

  setNativeTools(enabled) {
    const prefs = { ...get().launchPrefs, nativeTools: enabled };
    saveLaunchPrefs(prefs);
    set({ launchPrefs: prefs });
  },

  setOllamaHost(host) {
    const trimmed = host?.trim() ? host.trim() : null;
    const prefs = { ...get().launchPrefs, ollamaHost: trimmed };
    saveLaunchPrefs(prefs);
    set({ launchPrefs: prefs });
    // The list depends on the server, so it is read again from the new address.
    void get().loadModels("ollama", true);
  },

  toggleChip(key) {
    set((st) => ({
      removedChips: st.removedChips.includes(key)
        ? st.removedChips.filter((k) => k !== key)
        : [...st.removedChips, key],
    }));
  },

  setDraft(text) {
    set({ draft: text });
  },

  onEvent(env) {
    if (env.session_id === get().lastStoppedSessionId) return;
    const before = get().chat;
    set((st) => ({ chat: reduceAgentEvent(st.chat, env) }));
    if (env.type === "tool_finished" && !env.is_error) {
      const call = before.items.find((item) => item.kind === "tool" && item.id === env.id);
      if (call && call.kind === "tool" && isEditTool(call.name)) {
        highlightEdit(call.input, env.output_summary);
      }
    }
  },
}));

/**
 * Briefly highlight the clips an edit touched (from its inputs) and created (from its result's
 * "Created: clip <short id>" line), then clear. The newest edit wins.
 */
function highlightEdit(input: unknown, output: string): void {
  const known = knownClipIds();
  const created = parseEditOutput(output).created.flatMap((c) => {
    const short = c.startsWith("clip ") ? c.slice("clip ".length).trim() : "";
    const full = short ? known.find((id) => id.startsWith(short)) : undefined;
    return full ? [full] : [];
  });
  const ids = [...new Set([...clipIdsTouchedByEdit(input, known), ...created])];
  if (highlightTimer) clearTimeout(highlightTimer);
  useAgentChatStore.setState({ highlightClipIds: ids });
  highlightTimer = setTimeout(() => {
    highlightTimer = null;
    useAgentChatStore.setState({ highlightClipIds: [] });
  }, HIGHLIGHT_MS);
}

/** Subscribe the chat store to backend `agent:event`s. Call once from App. */
export function connectAgentChatEvents(): () => void {
  return ipc.onAgentEvent((env) => useAgentChatStore.getState().onEvent(env));
}
