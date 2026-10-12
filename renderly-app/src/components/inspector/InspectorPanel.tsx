import { useEditorStore } from "../../store/editorStore";
import { activeTracks } from "../../lib/container";
import { ProjectInspector } from "./ProjectInspector";
import { MediaClipInspector } from "./MediaClipInspector";
import { CaptionInspector } from "./CaptionInspector";
import { AdjustmentInspector } from "./AdjustmentInspector";

export function InspectorPanel() {
  const project = useEditorStore((s) => s.project);
  const selection = useEditorStore((s) => s.selection);

  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);

  const track = selection && activeTracks(project, activeSequenceId).find((t) => t.id === selection.trackId);
  const clip = track?.clips.find((c) => c.id === selection?.clipId);

  return (
    <aside className="panel panel-right">
      <div className="panel-header">
        <h2>Properties</h2>
      </div>
      <div className="panel-body">
        {!track || !clip ? (
          <ProjectInspector />
        ) : clip.type === "adjustment" ? (
          <AdjustmentInspector key={clip.id} track={track} clip={clip} />
        ) : clip.type === "caption" ? (
          <CaptionInspector key={clip.id} track={track} clip={clip} />
        ) : (
          <MediaClipInspector key={clip.id} track={track} clip={clip} />
        )}
      </div>
    </aside>
  );
}
