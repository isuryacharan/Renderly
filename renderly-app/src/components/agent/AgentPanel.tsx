// Agent tab of the right column (docs/agent-chat-design.md §4). The transcript comes from
// lib/agentChat.ts. An edit shows as plain action lines with one Undo for the whole turn; the
// raw tool calls sit under a per-turn "Details" disclosure. Agent text is plain text (no HTML).

import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Bot, Check, MessageSquare, RefreshCw, Send, Settings2, Square, TriangleAlert, Undo2, X } from "lucide-react";
import { useAgentChatStore, type ChipKey } from "../../store/agentChatStore";
import { useEditorStore } from "../../store/editorStore";
import {
  EFFORT_CHOICES,
  MODEL_CHOICES,
  activityLabel,
  undoableTurnActionsId,
  type AgentInfo,
  type TranscriptItem,
} from "../../lib/agentChat";
import { IconButton } from "../ui/IconButton";

const VENDOR_NOTICE: Record<string, string> = {
  "claude-code": "Messages and project data are sent to Anthropic under your Claude Code account.",
  codex: "Messages and project data are sent to OpenAI under your Codex account.",
};

/** Model, effort and the built-in tools switch. Applies to the next session started. */
function LaunchFields({ note }: { note: string | null }) {
  const launch = useAgentChatStore((s) => s.launch);
  const setLaunch = useAgentChatStore((s) => s.setLaunch);
  return (
    <div className="agent-launch">
      <label className="agent-launch-field">
        <span>Model</span>
        <select value={launch.model} onChange={(e) => setLaunch({ model: e.target.value })}>
          {MODEL_CHOICES.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </select>
      </label>
      <label className="agent-launch-field">
        <span>Effort</span>
        <select value={launch.effort} onChange={(e) => setLaunch({ effort: e.target.value })}>
          {EFFORT_CHOICES.map((level) => (
            <option key={level} value={level}>
              {level}
            </option>
          ))}
        </select>
      </label>
      <label className="agent-toggle">
        <input
          type="checkbox"
          checked={launch.nativeTools}
          onChange={(e) => setLaunch({ nativeTools: e.target.checked })}
        />
        Let the agent use its own tools (asks first)
      </label>
      {note ? <p className="agent-muted">{note}</p> : null}
    </div>
  );
}

function AgentPicker({ agents, error }: { agents: AgentInfo[] | null; error: string | null }) {
  const startSession = useAgentChatStore((s) => s.startSession);
  const loadAgents = useAgentChatStore((s) => s.loadAgents);
  const chat = useAgentChatStore((s) => s.chat);
  const busy = chat.status === "starting";

  return (
    <div className="agent-picker">
      <p className="agent-intro">
        Drive an agent you already have installed and signed in. Renderly never asks for or stores
        your agent credentials.
      </p>
      {error ? <p className="agent-error">Could not check agents: {error}</p> : null}
      {agents === null && !error ? <p className="agent-muted">Checking installed agents…</p> : null}
      {(agents ?? []).map((agent) => {
        const ready = agent.installed && agent.signIn !== "signed_out";
        return (
          <div key={agent.id} className="agent-card">
            <div className="agent-card-head">
              <span className="agent-card-name">{agent.name}</span>
              <span className={`agent-badge ${agent.installed ? "ok" : "missing"}`}>
                {agent.installed ? (agent.version ?? "installed") : "not installed"}
              </span>
            </div>
            {!agent.installed ? (
              <p className="agent-muted">{agent.installHint}</p>
            ) : agent.signIn === "signed_out" ? (
              <p className="agent-muted">
                Not signed in. Open a terminal and run <code>{agent.signInCommand}</code>, then check
                again.
              </p>
            ) : agent.signIn === "unknown" && agent.signInCommand ? (
              <p className="agent-muted">
                Sign-in is not checked here. If it fails, run <code>{agent.signInCommand}</code> in a
                terminal.
              </p>
            ) : null}
            {agent.note ? <p className="agent-muted">{agent.note}</p> : null}
            <button
              type="button"
              className="btn btn-primary btn-sm agent-start"
              disabled={!ready || busy}
              onClick={() => void startSession(agent.id)}
            >
              {busy && chat.agentId === agent.id ? "Starting…" : `Start ${agent.name}`}
            </button>
          </div>
        );
      })}
      <LaunchFields note={null} />
      <button type="button" className="btn btn-ghost btn-sm" onClick={() => void loadAgents()}>
        <RefreshCw size={14} strokeWidth={1.75} /> Check again
      </button>
    </div>
  );
}

/** The turn's edits as plain lines, with the turn's single Undo when it is the latest turn. */
function ActionsView({
  item,
  onUndo,
  undoable,
}: {
  item: Extract<TranscriptItem, { kind: "actions" }>;
  onUndo: () => void;
  undoable: boolean;
}) {
  return (
    <div className="agent-actions">
      <ul className="agent-action-lines">
        {item.lines.map((line, i) => (
          <li key={`${item.id}-${i}`}>{line}</li>
        ))}
      </ul>
      {undoable ? (
        <button type="button" className="btn btn-sm agent-undo" onClick={onUndo} title="Undo everything the agent changed this turn">
          <Undo2 size={14} strokeWidth={1.75} /> Undo this turn
        </button>
      ) : null}
    </div>
  );
}

/** Raw tool calls for one turn: hidden unless the user opens Details. */
function ToolDetails({ tools }: { tools: Extract<TranscriptItem, { kind: "tool" }>[] }) {
  return (
    <details className="agent-details">
      <summary>Details ({tools.length} {tools.length === 1 ? "call" : "calls"})</summary>
      {tools.map((t) => (
        <div key={t.id} className={`agent-tool agent-tool-${t.status}`}>
          <div className="agent-tool-head">
            <span className="agent-tool-title">{t.name.replace(/^mcp__renderly__/, "renderly ")}</span>
            <span className="agent-tool-status">
              {t.status === "running" ? "running" : t.status === "done" ? "done" : "failed"}
            </span>
          </div>
          <pre>{JSON.stringify(t.input, null, 2)}</pre>
          {t.output ? <div className="agent-tool-output">{t.output}</div> : null}
        </div>
      ))}
    </details>
  );
}

function PermissionCard({
  item,
  onAnswer,
}: {
  item: Extract<TranscriptItem, { kind: "permission" }>;
  onAnswer: (optionId: string) => void;
}) {
  const answered = item.answer !== null;
  const label: Record<string, string> = {
    allow_once: "Allowed once",
    allow_always: "Allowed for this session",
    reject_once: "Denied",
    cancelled: "Cancelled",
  };
  return (
    <div className="agent-permission" role="group" aria-label="Permission request">
      <div className="agent-permission-title">
        <TriangleAlert size={14} strokeWidth={1.75} /> The agent wants to run <code>{item.tool}</code>
      </div>
      <pre className="agent-permission-input">{JSON.stringify(item.input, null, 2)}</pre>
      {answered ? (
        <div className="agent-muted">{label[item.answer ?? ""] ?? item.answer}</div>
      ) : (
        <div className="agent-permission-actions">
          {item.options.map((o) => (
            <button
              key={o.id}
              type="button"
              className={`btn btn-sm${o.id === "allow_once" || o.id === "allow_always" ? " btn-primary" : ""}`}
              onClick={() => onAnswer(o.id)}
            >
              {o.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function TranscriptItemView({
  item,
  undoId,
  onUndo,
  onAnswer,
}: {
  item: TranscriptItem;
  undoId: string | null;
  onUndo: () => void;
  onAnswer: (requestId: string, optionId: string) => void;
}) {
  switch (item.kind) {
    case "user":
      return (
        <div className="agent-msg agent-msg-user">
          <div className="agent-bubble">{item.text}</div>
          {item.context?.selectedClipIds?.length ? (
            <div className="agent-msg-context">
              {item.context.selectedClipIds.length} selected
            </div>
          ) : null}
        </div>
      );
    case "agent":
      return (
        <div className="agent-msg agent-msg-agent">
          <div className="agent-text">
            {item.text}
            {item.streaming ? <span className="agent-caret" aria-hidden /> : null}
          </div>
        </div>
      );
    case "actions":
      return <ActionsView item={item} undoable={undoId === item.id} onUndo={onUndo} />;
    case "tool":
      // Tool calls render only under the turn's Details (see renderTranscript).
      return null;
    case "permission":
      return <PermissionCard item={item} onAnswer={(opt) => onAnswer(item.id, opt)} />;
    case "notice":
      return <div className={`agent-notice agent-notice-${item.tone}`}>{item.text}</div>;
    case "turn": {
      const label =
        item.stopReason === "cancelled"
          ? "Stopped. Changes already applied are still in the project."
          : `Turn ended: ${item.stopReason}`;
      return <div className="agent-turn-end">{label}</div>;
    }
  }
}

/** The transcript with each turn's tool calls gathered into one Details block. */
function renderTranscript(
  items: TranscriptItem[],
  undoId: string | null,
  onUndo: () => void,
  onAnswer: (requestId: string, optionId: string) => void,
) {
  const out: ReactNode[] = [];
  let tools: Extract<TranscriptItem, { kind: "tool" }>[] = [];
  const flush = (key: string) => {
    if (tools.length > 0) {
      out.push(<ToolDetails key={`details-${key}`} tools={tools} />);
      tools = [];
    }
  };
  for (const item of items) {
    if (item.kind === "tool") {
      tools.push(item);
      continue;
    }
    if (item.kind === "user") flush(item.id);
    out.push(
      <TranscriptItemView
        key={`${item.kind}-${item.id}`}
        item={item}
        undoId={undoId}
        onUndo={onUndo}
        onAnswer={onAnswer}
      />,
    );
  }
  flush("end");
  return out;
}

const CHIP_LABEL: Record<ChipKey, string> = {
  selection: "Selection",
};

/** The user's selection, the one piece of context the app does not send automatically. */
function ContextChips() {
  const removed = useAgentChatStore((s) => s.removedChips);
  const toggle = useAgentChatStore((s) => s.toggleChip);
  const selected = useEditorStore((s) => s.selections.length);
  if (selected === 0) return null;
  const off = removed.includes("selection");
  return (
    <div className="agent-chips" aria-label="Context sent with the next message">
      <button
        type="button"
        className={`agent-chip${off ? " is-off" : ""}`}
        onClick={() => toggle("selection")}
        aria-pressed={!off}
        title={off ? "Include the selection in the next message" : "Remove the selection from the next message"}
      >
        <span>
          {CHIP_LABEL.selection}: {selected === 1 ? "1 clip" : `${selected} clips`}
        </span>
        {off ? <Check size={12} strokeWidth={1.75} /> : <X size={12} strokeWidth={1.75} />}
      </button>
    </div>
  );
}

function SettingsPopover({ active }: { active: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="agent-settings">
      <IconButton
        iconOnly
        icon={Settings2}
        aria-label="Agent settings"
        tooltip="Model, effort and tools"
        onClick={() => setOpen((o) => !o)}
      />
      {open ? (
        <div className="agent-settings-pop" role="dialog" aria-label="Agent settings">
          <LaunchFields note={active ? "Changes apply to the next session." : null} />
        </div>
      ) : null}
    </div>
  );
}

export function AgentPanel() {
  const agents = useAgentChatStore((s) => s.agents);
  const agentsError = useAgentChatStore((s) => s.agentsError);
  const loadAgents = useAgentChatStore((s) => s.loadAgents);
  const chat = useAgentChatStore((s) => s.chat);
  const draft = useAgentChatStore((s) => s.draft);
  const setDraft = useAgentChatStore((s) => s.setDraft);
  const sendPrompt = useAgentChatStore((s) => s.sendPrompt);
  const cancelTurn = useAgentChatStore((s) => s.cancelTurn);
  const stopSession = useAgentChatStore((s) => s.stopSession);
  const answerPermission = useAgentChatStore((s) => s.answerPermission);
  const focusTick = useAgentChatStore((s) => s.focusTick);
  const canUndo = useEditorStore((s) => s.canUndo);
  const undo = useEditorStore((s) => s.undo);

  const promptRef = useRef<HTMLTextAreaElement>(null);
  const transcriptRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (agents === null) void loadAgents();
  }, [agents, loadAgents]);

  useEffect(() => {
    if (focusTick > 0) promptRef.current?.focus();
  }, [focusTick]);

  // Keep the newest item in view as text streams in.
  useEffect(() => {
    const el = transcriptRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [chat.items]);

  const active = chat.sessionId !== null && (chat.status === "ready" || chat.status === "running");
  const running = chat.status === "running";
  const undoId = canUndo ? undoableTurnActionsId(chat.items) : null;
  const activity = running ? activityLabel(chat.items) : null;
  const vendor = chat.agentId ? VENDOR_NOTICE[chat.agentId] : null;
  const agentName = agents?.find((a) => a.id === chat.agentId)?.name ?? chat.agentId ?? "";
  const statusText =
    chat.status === "starting"
      ? "Starting…"
      : chat.status === "running"
        ? "Working…"
        : chat.status === "ready"
          ? "Ready"
          : chat.status === "ended"
            ? "Ended"
            : "";

  const onPromptKey = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      void sendPrompt();
    }
  };

  if (!active && chat.status !== "starting" && chat.items.length === 0) {
    return (
      <div className="agent-panel">
        <AgentPicker agents={agents} error={agentsError} />
      </div>
    );
  }

  return (
    <div className="agent-panel">
      <div className="agent-header">
        <div className="agent-header-title">
          <Bot size={14} strokeWidth={1.75} />
          <span>{agentName || "Agent"}</span>
          {statusText ? <span className="agent-status">{statusText}</span> : null}
        </div>
        <div className="agent-header-actions">
          <SettingsPopover active={active} />
          {chat.status === "ended" || chat.status === "idle" ? null : (
            <IconButton
              iconOnly
              icon={Square}
              aria-label="Stop agent"
              tooltip="Stop the agent (kills its process)"
              onClick={() => void stopSession()}
            />
          )}
        </div>
      </div>

      {vendor ? <div className="agent-vendor-notice">{vendor}</div> : null}

      {!active && chat.status !== "starting" ? (
        <div className="agent-ended-picker">
          <p className="agent-muted">This session has ended. Start a new one:</p>
          <AgentPicker agents={agents} error={agentsError} />
        </div>
      ) : null}

      <div className="agent-transcript" ref={transcriptRef} aria-live="polite">
        {chat.items.length === 0 ? (
          <p className="agent-muted">
            Ask for an edit, for example “trim the first clip to 5 seconds”. The agent sees your
            timeline, so you can refer to clips by name or position.
          </p>
        ) : null}
        {renderTranscript(chat.items, undoId, () => void undo(), (rid, opt) => void answerPermission(rid, opt))}
        {activity ? <div className="agent-activity">{activity}</div> : null}
      </div>

      {chat.error ? <div className="agent-error">{chat.error}</div> : null}

      <div className="agent-composer">
        {active ? <ContextChips /> : null}
        <textarea
          ref={promptRef}
          className="agent-prompt"
          rows={3}
          placeholder={active ? "Ask for an edit…" : "Start an agent to chat"}
          value={draft}
          disabled={!active}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={onPromptKey}
          aria-label="Message to the agent"
        />
        <div className="agent-composer-actions">
          {running ? (
            <button type="button" className="btn btn-sm" onClick={() => void cancelTurn()}>
              <Square size={12} strokeWidth={1.75} /> Cancel
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={!active || draft.trim() === ""}
              onClick={() => void sendPrompt()}
            >
              <Send size={14} strokeWidth={1.75} /> Send
            </button>
          )}
          <span className="agent-muted agent-hint">
            <MessageSquare size={12} strokeWidth={1.75} /> Enter to send · Shift+Enter for a new line
          </span>
        </div>
      </div>
    </div>
  );
}
