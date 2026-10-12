import { useRef } from "react";
import { useEditorStore } from "../../store/editorStore";
import { selectActiveTracks } from "../../lib/container";
import { TimelineToolbar } from "./TimelineToolbar";
import { TimelineBreadcrumb } from "./TimelineBreadcrumb";
import { TimelineCanvas } from "./TimelineCanvas";
import { TrackHeaders } from "./TrackHeaders";
import { TimelineScrollbars } from "./TimelineScrollbars";

export function TimelineSection() {
  const canvasWrapRef = useRef<HTMLDivElement | null>(null);
  const project = useEditorStore((s) => s.project);
  const tracks = useEditorStore(selectActiveTracks);
  const hasClips = tracks.some((t) => t.clips.length > 0);

  return (
    <div className="timeline-wrap">
      <TimelineToolbar />
      <TimelineBreadcrumb />
      <div className="timeline-canvas-wrap" ref={canvasWrapRef}>
        <TimelineCanvas />
        <TrackHeaders containerRef={canvasWrapRef} />
        <TimelineScrollbars containerRef={canvasWrapRef} />
        {project && !hasClips && (
          <div className="timeline-empty" aria-live="polite">
            <strong>Timeline is empty</strong>
            <span>Import media or drag a clip from the Media panel onto a track.</span>
          </div>
        )}
      </div>
    </div>
  );
}
