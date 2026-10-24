import {
  MousePointer2,
  Scissors,
  Magnet,
  AlignCenterVertical,
  Flag,
  ZoomIn,
  ZoomOut,
  Maximize,
  SplitSquareHorizontal,
  CircleDashed,
} from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { activeTracks } from "../../lib/container";
import { magnetTargetTrack } from "../../lib/text";
import { getAction, invokeAction } from "../../lib/actions";
import { IconButton } from "../ui/IconButton";
import { Tooltip } from "../ui/Tooltip";
import { AddTrackMenu } from "./AddTrackMenu";

function tip(id: string, fallback: string): string {
  const a = getAction(id);
  if (!a) return fallback;
  return a.shortcut ? `${a.label} (${a.shortcut})` : a.label;
}

export function TimelineToolbar() {
  const toolMode = useEditorStore((s) => s.toolMode);
  const snapEnabled = useEditorStore((s) => s.snapEnabled);
  const setSnap = useEditorStore((s) => s.setSnap);
  const pxPerSec = useEditorStore((s) => s.pxPerSec);
  const fitZoom = useEditorStore((s) => s.fitZoom);
  const toast = useEditorStore((s) => s.toast);
  const project = useEditorStore((s) => s.project);
  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const selectedTrackId = useEditorStore((s) => s.selection?.trackId);
  // The magnet acts on the selected video track, else the first one (Decision 1).
  const magnetTrack = magnetTargetTrack(activeTracks(project, activeSequenceId), selectedTrackId);
  const magnetOn = magnetTrack?.magnetic === true;

  return (
    <div className="timeline-tools">
      <div className="tool-group">
        <Tooltip content={tip("tool.select", "Select (V)")}>
          <button
            type="button"
            className={`tool-btn icon-only${toolMode === "select" ? " active" : ""}`}
            onClick={() => invokeAction("tool.select")}
          >
            <MousePointer2 size={15} strokeWidth={1.75} />
          </button>
        </Tooltip>
        <Tooltip content={tip("tool.razor", "Razor (C)")}>
          <button
            type="button"
            className={`tool-btn icon-only${toolMode === "razor" ? " active" : ""}`}
            onClick={() => {
              invokeAction("tool.razor");
              toast("Click a clip to split it", "info");
            }}
          >
            <Scissors size={15} strokeWidth={1.75} />
          </button>
        </Tooltip>
        <Tooltip content={tip("tool.mask", "Mask (Shift+M)")}>
          <button
            type="button"
            className={`tool-btn icon-only${toolMode === "mask" ? " active" : ""}`}
            onClick={() => {
              invokeAction("tool.mask");
              toast("Drag on the paused preview to draw a mask", "info");
            }}
          >
            <CircleDashed size={15} strokeWidth={1.75} />
          </button>
        </Tooltip>
      </div>

      <IconButton
        icon={SplitSquareHorizontal}
        iconOnly
        size="sm"
        tooltip={tip("edit.split", "Split at playhead")}
        disabled={!project}
        onClick={() => invokeAction("edit.split")}
      />

      <Tooltip
        content={
          magnetTrack
            ? `Magnetic track: ${magnetTrack.name} ${magnetOn ? "(on)" : "(off)"} - clips close up when you edit`
            : "Magnetic track (add a video track first)"
        }
      >
        <button
          type="button"
          className={`tool-btn icon-only${magnetOn ? " active" : ""}`}
          aria-pressed={magnetOn}
          aria-label="Magnetic track"
          disabled={!magnetTrack}
          onClick={() => invokeAction("timeline.toggle-magnet")}
        >
          <Magnet size={15} strokeWidth={1.75} />
        </button>
      </Tooltip>

      <Tooltip content="Snap to clip edges, markers and the playhead">
        <button
          type="button"
          className={`tool-btn icon-only${snapEnabled ? " active" : ""}`}
          aria-pressed={snapEnabled}
          aria-label="Snapping"
          onClick={() => setSnap(!snapEnabled)}
        >
          <AlignCenterVertical size={15} strokeWidth={1.75} />
        </button>
      </Tooltip>

      <Tooltip content={tip("timeline.add-marker", "Add marker (M)")}>
        <button
          type="button"
          className="tool-btn icon-only"
          aria-label="Add marker"
          disabled={!project || activeSequenceId != null}
          onClick={() => invokeAction("timeline.add-marker")}
        >
          <Flag size={15} strokeWidth={1.75} />
        </button>
      </Tooltip>

      <AddTrackMenu />

      <span className="tool-divider" />

      <div className="zoom-controls">
        <IconButton
          icon={ZoomOut}
          iconOnly
          size="sm"
          tooltip={tip("timeline.zoom-out", "Zoom out")}
          onClick={() => invokeAction("timeline.zoom-out")}
        />
        <span className="zoom-label">{Math.round((pxPerSec / 80) * 100)}%</span>
        <IconButton
          icon={ZoomIn}
          iconOnly
          size="sm"
          tooltip={tip("timeline.zoom-in", "Zoom in")}
          onClick={() => invokeAction("timeline.zoom-in")}
        />
        <IconButton
          icon={Maximize}
          iconOnly
          size="sm"
          tooltip="Fit timeline to window"
          onClick={() => fitZoom()}
        />
      </div>
    </div>
  );
}
