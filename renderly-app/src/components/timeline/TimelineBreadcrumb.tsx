import { ChevronRight, Group } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { breadcrumbItems } from "../../lib/container";

/// Breadcrumb above the ruler (`Root › Sequence › …`) shown while a nested sequence is open.
/// Each ancestor is a button that navigates back to it; the last item is the open container.
/// Hidden at the root so projects that never use sequences look exactly as before.
export function TimelineBreadcrumb() {
  const project = useEditorStore((s) => s.project);
  const sequencePath = useEditorStore((s) => s.sequencePath);
  const navigateToSequence = useEditorStore((s) => s.navigateToSequence);

  if (!project || sequencePath.length === 0) return null;
  const items = breadcrumbItems(project, sequencePath);

  return (
    <nav className="timeline-breadcrumb" aria-label="Open sequence path">
      {items.map((item, i) => (
        <span key={item.sequenceId ?? "root"} style={{ display: "contents" }}>
          {i > 0 && (
            <span className="crumb-sep" aria-hidden>
              <ChevronRight size={12} strokeWidth={1.75} />
            </span>
          )}
          {i > 0 && item.current && (
            <span className="crumb-icon" aria-hidden>
              <Group size={12} strokeWidth={1.75} />
            </span>
          )}
          <button
            type="button"
            className={`crumb${item.current ? " current" : ""}`}
            aria-current={item.current ? "location" : undefined}
            disabled={item.current}
            title={item.current ? undefined : `Back to ${item.label}`}
            onClick={() => void navigateToSequence(item.sequenceId)}
          >
            {item.label}
          </button>
        </span>
      ))}
    </nav>
  );
}
