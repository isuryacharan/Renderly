import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { project as makeProject, track, videoClip } from "../test/fixtures";
import { EMPTY_AGENT_CHAT, type AgentEnvelope, type TranscriptItem } from "../lib/agentChat";

// The store reaches the backend only through lib/ipc; mock it so Tauri stays out of the graph.
vi.mock("../lib/ipc", () => ({
  agentList: vi.fn(),
  agentStart: vi.fn(),
  agentSend: vi.fn(),
  agentStop: vi.fn(),
  agentCancel: vi.fn(),
  agentAnswerPermission: vi.fn(),
  onAgentEvent: vi.fn(() => () => {}),
}));

import { HIGHLIGHT_MS, loadLaunchOptions, useAgentChatStore } from "./agentChatStore";
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

describe("agent chat store: launch options", () => {
  const store: Record<string, string> = {};

  beforeEach(() => {
    for (const k of Object.keys(store)) delete store[k];
    vi.stubGlobal("localStorage", {
      getItem: (k: string) => (k in store ? store[k]! : null),
      setItem: (k: string, v: string) => {
        store[k] = v;
      },
      removeItem: (k: string) => {
        delete store[k];
      },
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("defaults to sonnet, low effort and no built-in tools", () => {
    expect(loadLaunchOptions()).toEqual({ model: "sonnet", effort: "low", nativeTools: false });
  });

  it("remembers the chosen options for the next run", () => {
    useAgentChatStore.getState().setLaunch({ model: "opus", nativeTools: true });
    expect(loadLaunchOptions()).toEqual({ model: "opus", effort: "low", nativeTools: true });
  });

  it("falls back to the defaults when the stored value is not valid JSON", () => {
    store["renderly.agent.launch"] = "{not json";
    expect(loadLaunchOptions().model).toBe("sonnet");
  });
});
