// Agent tab of the right column (docs/agent-chat-design.md §4). The transcript and tool
// cards come from lib/agentChat.ts; this component renders them and sends user intent back
// through agentChatStore. Agent text is rendered as plain text (no HTML, no Markdown engine).

import { useEffect, useRef, type KeyboardEvent } from "react";
import { Bot, Check, MessageSquare, RefreshCw, Send, Square, TriangleAlert, Undo2, X } from "lucide-react";
import { useAgentChatStore, type ChipKey } from "../../store/agentChatStore";
import { useEditorStore } from "../../store/editorStore";
import {
  describeEdit,
  isRenderlyEdit,
  lastSuccessfulEditId,
  type AgentInfo,
  type TranscriptItem,
} from "../../lib/agentChat";
import { IconButton } from "../ui/IconButton";

const VENDOR_NOTICE: Record<string, string> = {
  "claude-code": "Messages and project data are sent to Anthropic under your Claude Code account.",
  codex: "Messages and project data are sent to OpenAI under your Codex account.",
};

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
      <button type="button" className="btn btn-ghost btn-sm" onClick={() => void loadAgents()}>
        <RefreshCw size={14} strokeWidth={1.75} /> Check again
      </button>
    </div>
  );
}

function ToolCard({
  item,
  undoId,
  onUndo,
}: {
  item: Extract<TranscriptItem, { kind: "tool" }>;
  undoId: string | null;
  onUndo: () => void;
}) {
  const edit = isRenderlyEdit(item.name);
  const title = edit ? describeEdit(item.name, item.input).title : item.name;
  const statusLabel = item.status === "running" ? "running" : item.status === "done" ? "done" : "failed";
  return (
    <div className={`agent-tool agent-tool-${item.status}${edit ? " agent-tool-edit" : ""}`}>
      <div className="agent-tool-head">
        <span className="agent-tool-title">{title}</span>
        <span className="agent-tool-status">{statusLabel}</span>
      </div>
      {item.output ? <div className="agent-tool-output">{item.output}</div> : null}
      {edit && undoId === item.id ? (
        <button type="button" className="btn btn-sm agent-undo" onClick={onUndo} title="Undo the latest change (Ctrl+Z)">
          <Undo2 size={14} strokeWidth={1.75} /> Undo
        </button>
      ) : null}
      <details className="agent-tool-details">
        <summary>Details</summary>
        <pre>{JSON.stringify(item.input, null, 2)}</pre>
      </details>
    </div>
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
          {item.context ? (
            <div className="agent-msg-context">
              {item.context.playheadSecs !== undefined ? `at ${item.context.playheadSecs.toFixed(2)} s` : null}
              {item.context.selectedClipIds?.length ? ` · ${item.context.selectedClipIds.length} selected` : null}
              {item.context.sequenceId ? " · sequence open" : null}
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
    case "tool":
      return <ToolCard item={item} undoId={undoId} onUndo={onUndo} />;
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

const CHIP_LABEL: Record<ChipKey, string> = {
  playhead: "Playhead",
  selection: "Selection",
  sequence: "Open sequence",
};

function ContextChips() {
  const removed = useAgentChatStore((s) => s.removedChips);
  const toggle = useAgentChatStore((s) => s.toggleChip);
  const playhead = useEditorStore((s) => s.playhead);
  const selected = useEditorStore((s) => s.selections.length);
  const sequence = useEditorStore((s) => s.activeSequenceId);
  const chips: { key: ChipKey; text: string; present: boolean }[] = [
    { key: "playhead", text: `${playhead.toFixed(2)} s`, present: true },
    { key: "selection", text: selected === 1 ? "1 clip" : `${selected} clips`, present: selected > 0 },
    { key: "sequence", text: "sequence", present: !!sequence },
  ];
  return (
    <div className="agent-chips" aria-label="Context sent with the next message">
      {chips
        .filter((c) => c.present)
        .map((c) => {
          const off = removed.includes(c.key);
          return (
            <button
              key={c.key}
              type="button"
              className={`agent-chip${off ? " is-off" : ""}`}
              onClick={() => toggle(c.key)}
              aria-pressed={!off}
              title={off ? `Include ${CHIP_LABEL[c.key].toLowerCase()} in the next message` : `Remove ${CHIP_LABEL[c.key].toLowerCase()} from the next message`}
            >
              <span>
                {CHIP_LABEL[c.key]}: {c.text}
              </span>
              {off ? <Check size={12} strokeWidth={1.75} /> : <X size={12} strokeWidth={1.75} />}
            </button>
          );
        })}
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
  const autoEdits = useAgentChatStore((s) => s.autoEdits);
  const setAutoEdits = useAgentChatStore((s) => s.setAutoEdits);
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
  const undoId = lastSuccessfulEditId(chat.items);
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

      <label className="agent-toggle">
        <input
          type="checkbox"
          checked={autoEdits}
          onChange={(e) => void setAutoEdits(e.target.checked)}
        />
        Apply edits without asking (undoable)
      </label>

      <div className="agent-transcript" ref={transcriptRef} aria-live="polite">
        {chat.items.length === 0 ? (
          <p className="agent-muted">
            Ask for an edit, for example “trim the first clip to 5 seconds”. Edits appear in the
            timeline as they happen.
          </p>
        ) : null}
        {chat.items.map((item) => (
          <TranscriptItemView
            key={`${item.kind}-${item.id}`}
            item={item}
            undoId={canUndo ? undoId : null}
            onUndo={() => void undo()}
            onAnswer={(rid, opt) => void answerPermission(rid, opt)}
          />
        ))}
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
