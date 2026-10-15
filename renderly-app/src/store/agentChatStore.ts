// Agent chat UI state: the right column's tab, the agent picker, the session, and the prompt
// box. The transcript itself is built by the pure reducer in lib/agentChat.ts; this store
// only wires IPC and editor state to it.

import { create } from "zustand";
import * as ipc from "../lib/ipc";
import {
  EMPTY_AGENT_CHAT,
  reduceAgentEvent,
  reducePermissionAnswered,
  reduceUserSent,
  type AgentChatState,
  type AgentEnvelope,
  type AgentId,
  type AgentInfo,
  type ChatContext,
} from "../lib/agentChat";
import { useEditorStore } from "./editorStore";

export type RightTab = "inspector" | "agent";
/** Context chips the user can remove before sending. */
export type ChipKey = "playhead" | "selection" | "sequence";

interface AgentChatStore {
  rightTab: RightTab;
  /** Bumped by Ctrl+L so the prompt box takes focus even when the tab is already open. */
  focusTick: number;
  agents: AgentInfo[] | null;
  agentsError: string | null;
  chat: AgentChatState;
  /** Edits apply without a prompt while on (design §4.3, the default). */
  autoEdits: boolean;
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
  setAutoEdits(enabled: boolean): Promise<void>;
  toggleChip(key: ChipKey): void;
  setDraft(text: string): void;
  onEvent(env: AgentEnvelope): void;
}

const errorText = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** Context chips for the next prompt, from the editor state and the chips still enabled. */
export function chipContext(removed: ChipKey[]): ChatContext | null {
  const s = useEditorStore.getState();
  const ctx: ChatContext = {};
  if (!removed.includes("playhead")) ctx.playheadSecs = s.playhead;
  if (!removed.includes("selection") && s.selections.length > 0) {
    ctx.selectedClipIds = s.selections.map((sel) => sel.clipId);
  }
  if (!removed.includes("sequence") && s.activeSequenceId) ctx.sequenceId = s.activeSequenceId;
  return Object.keys(ctx).length > 0 ? ctx : null;
}

export const useAgentChatStore = create<AgentChatStore>((set, get) => ({
  rightTab: "inspector",
  focusTick: 0,
  agents: null,
  agentsError: null,
  chat: EMPTY_AGENT_CHAT,
  autoEdits: true,
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
      const sessionId = await ipc.agentStart(agentId);
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

  async setAutoEdits(enabled) {
    set({ autoEdits: enabled });
    const sid = get().chat.sessionId;
    if (!sid) return;
    try {
      await ipc.agentSetAutoEdits(sid, enabled);
    } catch (e) {
      set((st) => ({ chat: { ...st.chat, error: errorText(e) } }));
    }
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
    set((st) => ({ chat: reduceAgentEvent(st.chat, env) }));
  },
}));

/** Subscribe the chat store to backend `agent:event`s. Call once from App. */
export function connectAgentChatEvents(): () => void {
  return ipc.onAgentEvent((env) => useAgentChatStore.getState().onEvent(env));
}
