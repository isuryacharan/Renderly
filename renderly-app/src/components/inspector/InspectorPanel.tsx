import { useEditorStore } from "../../store/editorStore";
import { ProjectInspector } from "./ProjectInspector";
import { MediaClipInspector } from "./MediaClipInspector";
import { CaptionInspector } from "./CaptionInspector";

export function InspectorPanel() {
  const project = useEditorStore((s) => s.project);
  const selection = useEditorStore((s) => s.selection);

  const track = selection && project?.tracks.find((t) => t.id === selection.trackId);
  const clip = track?.clips.find((c) => c.id === selection?.clipId);

  return (
    <aside className="panel panel-right">
      <div className="panel-header">
        <h2>Properties</h2>
      </div>
      <div className="panel-body">
        {!track || !clip || clip.type === "adjustment" ? (
          // Adjustment layers get their inspector in the sequences UI work (schema v7 phase B).
          <ProjectInspector />
        ) : clip.type === "caption" ? (
          <CaptionInspector key={clip.id} track={track} clip={clip} />
        ) : (
          <MediaClipInspector key={clip.id} track={track} clip={clip} />
        )}
      </div>
    </aside>
  );
}
