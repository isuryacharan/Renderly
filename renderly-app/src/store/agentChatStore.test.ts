import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { project as makeProject, track, videoClip } from "../test/fixtures";
import {
  DEFAULT_LAUNCH_PREFS,
  EMPTY_AGENT_CHAT,
  type AgentEnvelope,
  type AgentModels,
  type TranscriptItem,
} from "../lib/agentChat";
import * as ipc from "../lib/ipc";

// The store reaches the backend only through lib/ipc; mock it so Tauri stays out of the graph.
vi.mock("../lib/ipc", () => ({
  agentList: vi.fn(),
  agentModels: vi.fn(),
  agentStart: vi.fn(),
  agentSend: vi.fn(),
  agentStop: vi.fn(),
  agentCancel: vi.fn(),
  agentAnswerPermission: vi.fn(),
  onAgentEvent: vi.fn(() => () => {}),
}));

import { HIGHLIGHT_MS, loadLaunchPrefs, useAgentChatStore } from "./agentChatStore";
import { useEditorStore } from "./editorStore";

const SID = "s-1";
const CLIP = "ab12cd34-0000-4000-8000-00000000000a";
const OTHER = "ffff0000-0000-4000-8000-00000000000b";

function editRunning(id: string, input: unknown): TranscriptItem[] {
  return [
    { kind: "tool", id, name: "mcp__renderly__edit", input, status: "running", output: "" },
  ];
}

function sendEnv(e: AgentEnvelope) {
  useAgentChatStore.getState().onEvent(e);
}

describe("agent chat store: edit highlight", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useEditorStore.setState({
      project: makeProject([track("v1", "video", [videoClip(CLIP, 0, 0, 5), videoClip(OTHER, 6, 0, 2)])]),
    });
    useAgentChatStore.setState({
      chat: { ...EMPTY_AGENT_CHAT, sessionId: SID, status: "running", items: editRunning("t1", { commands: [{ command: "TrimClip", clip_id: "ab12cd34" }] }) },
      highlightClipIds: [],
      lastStoppedSessionId: null,
    });
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("highlights the clips a successful edit touched, then clears after a moment", () => {
    sendEnv({
      session_id: SID,
      type: "tool_finished",
      id: "t1",
      is_error: false,
      output_summary: "Applied 1 command(s) as one undo step:\n- Trimmed 'Gameplay' to 0:00.0-0:05.0",
    });
    expect(useAgentChatStore.getState().highlightClipIds).toEqual([CLIP]);
    vi.advanceTimersByTime(HIGHLIGHT_MS + 1);
    expect(useAgentChatStore.getState().highlightClipIds).toEqual([]);
  });

  it("includes clips the edit created, from the result's Created line", () => {
    sendEnv({
      session_id: SID,
      type: "tool_finished",
      id: "t1",
      is_error: false,
      output_summary: "Applied 1 command(s) as one undo step:\n- Added caption 'Hi' at 0:02.0\nCreated: clip ffff0000",
    });
    // The edit's input names the first clip; the result names the created one.
    expect(useAgentChatStore.getState().highlightClipIds).toEqual([CLIP, OTHER]);
  });

  it("does not highlight after a failed edit", () => {
    sendEnv({ session_id: SID, type: "tool_finished", id: "t1", is_error: true, output_summary: "invalid command" });
    expect(useAgentChatStore.getState().highlightClipIds).toEqual([]);
  });

  it("does not highlight for perception tools", () => {
    useAgentChatStore.setState({
      chat: { ...EMPTY_AGENT_CHAT, sessionId: SID, status: "running", items: [{ kind: "tool", id: "p1", name: "mcp__renderly__timeline", input: {}, status: "running", output: "" }] },
    });
    sendEnv({ session_id: SID, type: "tool_finished", id: "p1", is_error: false, output_summary: "outline" });
    expect(useAgentChatStore.getState().highlightClipIds).toEqual([]);
  });
});

describe("agent chat store: per-agent launch preferences", () => {
  const saved: Record<string, string> = {};

  beforeEach(() => {
    for (const k of Object.keys(saved)) delete saved[k];
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => (k in saved ? saved[k]! : null),
      setItem: (k: string, v: string) => {
        saved[k] = v;
      },
      removeItem: (k: string) => {
        delete saved[k];
      },
    });
    vi.mocked(ipc.agentStart).mockReset();
    vi.mocked(ipc.agentModels).mockReset();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("starts with no choices, so each agent's own defaults apply", () => {
    expect(loadLaunchPrefs()).toEqual(DEFAULT_LAUNCH_PREFS);
  });

  it("remembers the model and effort per agent, and the tools switch", () => {
    const s = useAgentChatStore.getState();
    s.setModelChoice("codex", "gpt-6-terra");
    s.setEffortChoice("codex", "medium");
    s.setModelChoice("claude-code", "sonnet");
    s.setNativeTools(true);
    const prefs = loadLaunchPrefs();
    expect(prefs.models).toEqual({ codex: "gpt-6-terra", "claude-code": "sonnet" });
    expect(prefs.efforts).toEqual({ codex: "medium" });
    expect(prefs.nativeTools).toBe(true);
  });

  it("falls back to the defaults when the stored value is not valid JSON", () => {
    saved["renderly.agent.launch.v2"] = "{not json";
    expect(loadLaunchPrefs()).toEqual(DEFAULT_LAUNCH_PREFS);
  });

  it("starts with the saved model when the agent still offers it", async () => {
    vi.mocked(ipc.agentModels).mockResolvedValue(codexList());
    vi.mocked(ipc.agentStart).mockResolvedValue("s-2");
    useAgentChatStore.setState({ agentModels: {}, modelNotice: {}, chat: EMPTY_AGENT_CHAT });
    useAgentChatStore.getState().setModelChoice("codex", "gpt-6-luna");
    await useAgentChatStore.getState().startSession("codex");
    expect(ipc.agentStart).toHaveBeenCalledWith("codex", expect.objectContaining({ model: "gpt-6-luna" }));
    expect(useAgentChatStore.getState().modelNotice.codex).toBeUndefined();
  });

  it("uses the agent's default and says so when the saved model is no longer offered", async () => {
    vi.mocked(ipc.agentModels).mockResolvedValue(codexList());
    vi.mocked(ipc.agentStart).mockResolvedValue("s-3");
    useAgentChatStore.setState({ agentModels: {}, modelNotice: {}, chat: EMPTY_AGENT_CHAT });
    useAgentChatStore.getState().setModelChoice("codex", "gpt-retired");
    await useAgentChatStore.getState().startSession("codex");
    expect(ipc.agentStart).toHaveBeenCalledWith("codex", expect.objectContaining({ model: "gpt-6-terra" }));
    expect(useAgentChatStore.getState().modelNotice.codex).toContain("gpt-retired");
    expect(useAgentChatStore.getState().modelNotice.codex).toContain("gpt-6-terra");
  });

  it("sends the Ollama host only for Ollama", async () => {
    vi.mocked(ipc.agentModels).mockResolvedValue(codexList());
    vi.mocked(ipc.agentStart).mockResolvedValue("s-4");
    useAgentChatStore.setState({ agentModels: {}, modelNotice: {}, chat: EMPTY_AGENT_CHAT });
    useAgentChatStore.getState().setOllamaHost("http://10.0.0.5:11434");
    await useAgentChatStore.getState().startSession("codex");
    expect(ipc.agentStart).toHaveBeenCalledWith("codex", expect.objectContaining({ ollamaHost: null }));
  });
});

/** A Codex model list in the backend's shape, for the store tests. */
function codexList(): AgentModels {
  return {
    agentId: "codex",
    status: "ready",
    models: [
      { id: "gpt-6-terra", label: "GPT-6-Terra", description: null, efforts: ["low", "medium"], defaultEffort: "medium", contextLength: null, vision: false, thinking: false },
      { id: "gpt-6-luna", label: "GPT-6-Luna", description: null, efforts: [], defaultEffort: null, contextLength: null, vision: false, thinking: false },
    ],
    defaultModel: "gpt-6-terra",
    message: null,
  };
}
