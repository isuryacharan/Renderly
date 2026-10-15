import { describe, expect, it } from "vitest";
import {
  EMPTY_AGENT_CHAT,
  type AgentEvent,
  activityLabel,
  clipIdsTouchedByEdit,
  describeEdit,
  humanizeCommand,
  lastSuccessfulEditId,
  reduceAgentEvent,
  reducePermissionAnswered,
  reduceUserSent,
  undoableTurnActionsId,
  type AgentChatState,
  type AgentEnvelope,
  type TranscriptItem,
} from "./agentChat";

const SID = "s-1";
const ev = (e: AgentEvent, sid = SID): AgentEnvelope => ({ session_id: sid, ...e });

function run(events: AgentEnvelope[], start: AgentChatState = EMPTY_AGENT_CHAT): AgentChatState {
  return events.reduce(reduceAgentEvent, start);
}

describe("reduceAgentEvent: text", () => {
  it("appends consecutive deltas to one streaming agent message", () => {
    const s = run([
      ev({ type: "session_started", agent_id: "claude-code" }),
      ev({ type: "text_delta", text: "Hel" }),
      ev({ type: "text_delta", text: "lo" }),
    ]);
    expect(s.items).toHaveLength(1);
    expect(s.items[0]).toMatchObject({ kind: "agent", text: "Hello", streaming: true });
    expect(s.status).toBe("ready");
    expect(s.agentId).toBe("claude-code");
  });

  it("closes streaming text when a tool starts, so later text is a new message", () => {
    const s = run([
      ev({ type: "text_delta", text: "Let me trim." }),
      ev({ type: "tool_started", id: "t1", name: "mcp__renderly__apply_command", input: {} }),
      ev({ type: "text_delta", text: "Done." }),
    ]);
    expect(s.items.map((i) => i.kind)).toEqual(["agent", "tool", "agent"]);
    expect(s.items[0]).toMatchObject({ streaming: false });
    expect(s.items[2]).toMatchObject({ text: "Done.", streaming: true });
  });
});

describe("reduceAgentEvent: tools and permissions", () => {
  it("moves a tool card from running to done with its output", () => {
    const s = run([
      ev({ type: "tool_started", id: "t1", name: "mcp__renderly__get_project", input: {} }),
      ev({ type: "tool_finished", id: "t1", output_summary: "ok", is_error: false }),
    ]);
    expect(s.items[0]).toMatchObject({ kind: "tool", status: "done", output: "ok" });
  });

  it("marks a failed tool call as error", () => {
    const s = run([
      ev({ type: "tool_started", id: "t1", name: "Bash", input: { command: "x" } }),
      ev({ type: "tool_finished", id: "t1", output_summary: "boom", is_error: true }),
    ]);
    expect(s.items[0]).toMatchObject({ status: "error", output: "boom" });
  });

  it("adds a permission prompt and records the answer", () => {
    let s = run([
      ev({
        type: "permission_request",
        request_id: "perm-1",
        tool: "mcp__renderly__export",
        input: { output_path: "o.mp4" },
        options: [
          { id: "allow_once", label: "Allow once" },
          { id: "reject_once", label: "Deny" },
        ],
      }),
    ]);
    expect(s.items[0]).toMatchObject({ kind: "permission", answer: null });
    s = reducePermissionAnswered(s, "perm-1", "allow_once");
    expect(s.items[0]).toMatchObject({ answer: "allow_once" });
  });

  it("cancels still-open permission prompts when the session ends", () => {
    const s = run([
      ev({
        type: "permission_request",
        request_id: "perm-2",
        tool: "Bash",
        input: {},
        options: [{ id: "reject_once", label: "Deny" }],
      }),
      ev({ type: "session_ended", reason: "stopped" }),
    ]);
    expect(s.items[0]).toMatchObject({ kind: "permission", answer: "cancelled" });
    expect(s.status).toBe("ended");
  });
});

describe("reduceAgentEvent: turns, errors and sessions", () => {
  it("does not clutter the transcript with a normal end-of-turn", () => {
    const s = run([
      ev({ type: "text_delta", text: "hi" }),
      ev({ type: "turn_finished", stop_reason: "end_turn" }),
    ]);
    expect(s.items.some((i) => i.kind === "turn")).toBe(false);
    expect(s.items[0]).toMatchObject({ streaming: false });
    expect(s.status).toBe("ready");
  });

  it("shows cancelled and error stop reasons", () => {
    const s = run([ev({ type: "turn_finished", stop_reason: "cancelled" })]);
    expect(s.items[0]).toMatchObject({ kind: "turn", stopReason: "cancelled" });
  });

  it("shows errors as notices and remembers the message", () => {
    const s = run([ev({ type: "error", message: "claude is not installed" })]);
    expect(s.error).toBe("claude is not installed");
    expect(s.items[0]).toMatchObject({ kind: "notice", tone: "error" });
  });

  it("ignores events from a session other than the current one", () => {
    const start = run([ev({ type: "session_started", agent_id: "codex" })]);
    const s = run([ev({ type: "text_delta", text: "stray" }, "other")], start);
    expect(s).toBe(start);
  });

  it("adopts the first session id seen while idle", () => {
    const s = run([ev({ type: "session_started", agent_id: "codex" }, "fresh")]);
    expect(s.sessionId).toBe("fresh");
  });
});

describe("reduceUserSent", () => {
  it("adds the user message with its context and marks the session running", () => {
    const s = reduceUserSent(
      { ...EMPTY_AGENT_CHAT, sessionId: SID, status: "ready" },
      "trim it",
      { selectedClipIds: ["c1"] },
    );
    expect(s.status).toBe("running");
    expect(s.items[0]).toMatchObject({ kind: "user", text: "trim it", context: { selectedClipIds: ["c1"] } });
  });
});

describe("edit results as action lines", () => {
  const SID = "s1";
  const started = (id: string, input: unknown) =>
    reduceAgentEvent(
      { ...EMPTY_AGENT_CHAT, sessionId: SID, status: "running" },
      { session_id: SID, type: "tool_started", id, name: "mcp__renderly__edit", input },
    );

  it("turns a successful edit result into one action line per command, plus created ids", () => {
    const s0 = started("t1", { commands: [{ command: "TrimClip" }] });
    const s = reduceAgentEvent(s0, {
      session_id: SID,
      type: "tool_finished",
      id: "t1",
      is_error: false,
      output_summary:
        "Applied 2 command(s) as one undo step:\n- Trimmed 'Gameplay 1' to 0:00.0-0:05.0\n- Added caption 'Hi' at 0:02.0\nCreated: clip 1a2b3c4d; track 9f8e7d6c",
    });
    const actions = s.items.find((i) => i.kind === "actions");
    expect(actions).toMatchObject({
      kind: "actions",
      lines: ["Trimmed 'Gameplay 1' to 0:00.0-0:05.0", "Added caption 'Hi' at 0:02.0"],
      created: ["clip 1a2b3c4d", "track 9f8e7d6c"],
    });
  });

  it("adds no action lines for a failed edit", () => {
    const s0 = started("t2", { commands: [] });
    const s = reduceAgentEvent(s0, {
      session_id: SID,
      type: "tool_finished",
      id: "t2",
      is_error: true,
      output_summary: "invalid command",
    });
    expect(s.items.some((i) => i.kind === "actions")).toBe(false);
  });

  it("keeps perception calls out of the transcript but in the tool list", () => {
    const s0 = reduceAgentEvent(
      { ...EMPTY_AGENT_CHAT, sessionId: SID, status: "running" },
      { session_id: SID, type: "tool_started", id: "p1", name: "mcp__renderly__timeline", input: {} },
    );
    const s = reduceAgentEvent(s0, {
      session_id: SID,
      type: "tool_finished",
      id: "p1",
      is_error: false,
      output_summary: "outline",
    });
    expect(s.items.some((i) => i.kind === "actions")).toBe(false);
    expect(s.items.some((i) => i.kind === "tool" && i.name === "mcp__renderly__timeline")).toBe(true);
  });

  it("offers the turn's Undo only on the latest turn's action list", () => {
    const first: TranscriptItem[] = [
      { kind: "user", id: "u1", text: "a", context: null },
      { kind: "actions", id: "a1", toolId: "t", lines: ["x"], created: [] },
      { kind: "user", id: "u2", text: "b", context: null },
    ];
    expect(undoableTurnActionsId(first)).toBeNull();
    const latest: TranscriptItem[] = [
      ...first.slice(0, 2),
      { kind: "user", id: "u2", text: "b", context: null },
      { kind: "actions", id: "a2", toolId: "t2", lines: ["y"], created: [] },
    ];
    expect(undoableTurnActionsId(latest)).toBe("a2");
  });

  it("names what the agent is doing while a tool runs", () => {
    const running = reduceAgentEvent(
      { ...EMPTY_AGENT_CHAT, sessionId: SID, status: "running" },
      { session_id: SID, type: "tool_started", id: "p2", name: "mcp__renderly__look", input: {} },
    );
    expect(activityLabel(running.items)).toBe("Looking at the frame…");
    expect(activityLabel([])).toBeNull();
  });

  it("finds the clips an edit touched from short ids in its inputs", () => {
    const known = ["1a2b3c4d-0000-4000-8000-000000000001", "ffffffff-0000-4000-8000-000000000002"];
    expect(clipIdsTouchedByEdit({ commands: [{ command: "TrimClip", clip_id: "1a2b3c4d" }] }, known)).toEqual([
      known[0],
    ]);
    expect(clipIdsTouchedByEdit({ commands: [{ command: "TrimClip", clip_id: "00000000" }] }, known)).toEqual([]);
  });
});

describe("edit cards", () => {
  const edit = (id: string, status: "done" | "error" | "running"): TranscriptItem => ({
    kind: "tool",
    id,
    name: "mcp__renderly__apply_command",
    input: {},
    status,
    output: "",
  });

  it("offers Undo only on the most recent successful edit", () => {
    expect(lastSuccessfulEditId([edit("a", "done"), edit("b", "done")])).toBe("b");
    expect(lastSuccessfulEditId([edit("a", "done"), edit("b", "error")])).toBeNull();
    expect(lastSuccessfulEditId([edit("a", "running")])).toBeNull();
  });

  it("humanizes command names", () => {
    expect(humanizeCommand("SetClipOpacity")).toBe("Set clip opacity");
    expect(humanizeCommand("TrimClip")).toBe("Trim clip");
    expect(humanizeCommand("SetAudioFade")).toBe("Set audio fade");
  });

  it("describes a single apply_command with the clip it touched", () => {
    const d = describeEdit("mcp__renderly__apply_command", {
      command: { command: "TrimClip", clip_id: "3f2a9c1e-aaaa-bbbb-cccc-000000000000" },
    });
    expect(d).toEqual({ title: "Trim clip · clip 3f2a9c1e", count: 1 });
  });

  it("counts a batch", () => {
    const d = describeEdit("mcp__renderly__apply_commands", {
      commands: [{ command: "AddTrack" }, { command: "AddClip" }, { command: "AddCaption" }],
    });
    expect(d).toEqual({ title: "Add track (+2 more)", count: 3 });
  });

  it("falls back to the tool name when the input has no command", () => {
    expect(describeEdit("mcp__renderly__apply_command", {})).toEqual({
      title: "mcp__renderly__apply_command",
      count: 0,
    });
  });
});
