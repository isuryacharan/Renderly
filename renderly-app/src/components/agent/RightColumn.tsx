// The right column hosts the Inspector and the Agent panel as tabs (docs/agent-chat-design.md
// §4.1). The inspector is rendered unchanged; the tab strip only chooses which one is shown.

import { useAgentChatStore, type RightTab } from "../../store/agentChatStore";
import { InspectorPanel } from "../inspector/InspectorPanel";
import { AgentPanel } from "./AgentPanel";

const TABS: { id: RightTab; label: string }[] = [
  { id: "inspector", label: "Inspector" },
  { id: "agent", label: "Agent" },
];

export function RightColumn() {
  const tab = useAgentChatStore((s) => s.rightTab);
  const setTab = useAgentChatStore((s) => s.setRightTab);

  return (
    <div className="right-column">
      <div className="right-tabs" role="tablist" aria-label="Right panel">
        {TABS.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`right-tab-${t.id}`}
            aria-selected={tab === t.id}
            aria-controls={`right-panel-${t.id}`}
            className={`right-tab${tab === t.id ? " is-active" : ""}`}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>
      {/* Both stay mounted so the inspector keeps its local state while the Agent tab is open. */}
      <div
        id="right-panel-inspector"
        role="tabpanel"
        aria-labelledby="right-tab-inspector"
        className="right-tabpanel"
        hidden={tab !== "inspector"}
      >
        <InspectorPanel />
      </div>
      <div
        id="right-panel-agent"
        role="tabpanel"
        aria-labelledby="right-tab-agent"
        className="right-tabpanel"
        hidden={tab !== "agent"}
      >
        <AgentPanel />
      </div>
    </div>
  );
}
