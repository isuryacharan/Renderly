import { useEffect, useState } from "react";
import { Play, Pause, SkipBack, SkipForward, Maximize2, Minimize2 } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { formatTimecode, parseTimecode } from "../../lib/format";
import { containerDurationSecs, selectActiveTracks } from "../../lib/container";
import { IconButton } from "../ui/IconButton";
import { Tooltip } from "../ui/Tooltip";
import { RatioMenu } from "./RatioMenu";

export function TransportBar({
  fullscreen,
  onToggleFullscreen,
}: {
  fullscreen: boolean;
  onToggleFullscreen: () => void;
}) {
  const project = useEditorStore((s) => s.project);
  const playhead = useEditorStore((s) => s.playhead);
  const playing = useEditorStore((s) => s.playing);
  const startPlayback = useEditorStore((s) => s.startPlayback);
  const stopPlayback = useEditorStore((s) => s.stopPlayback);
  const seekTo = useEditorStore((s) => s.seekTo);

  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(formatTimecode(playhead));

  useEffect(() => {
    if (!editing) setDraft(formatTimecode(playhead));
  }, [playhead, editing]);

  const tracks = useEditorStore(selectActiveTracks);
  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const hasClips = tracks.some((t) => t.clips.length > 0);
  // The open container's own duration (a sequence's, while one is open).
  const duration =
    project && hasClips ? formatTimecode(Math.max(containerDurationSecs(project, activeSequenceId), 1)) : "—";
  const fps = project?.settings.fps || 60;
  const frameStep = 1 / fps;

  function commitTimecode() {
    const secs = parseTimecode(draft);
    if (secs !== null) void seekTo(secs);
    else setDraft(formatTimecode(playhead));
    setEditing(false);
  }

  return (
    <div className="transport-bar">
      <IconButton
        icon={SkipBack}
        iconOnly
        className="transport-step"
        tooltip="Previous frame (←)"
        disabled={!project}
        onClick={() => void seekTo(Math.max(0, playhead - frameStep))}
      />
      <Tooltip content="Play / Pause (Space)">
        <button
          type="button"
          className={`play-btn${playing ? " playing" : ""}`}
          disabled={!project}
          onClick={() => (playing ? stopPlayback() : void startPlayback())}
        >
          {playing ? (
            <Pause size={18} strokeWidth={0} fill="currentColor" />
          ) : (
            <Play size={18} strokeWidth={0} fill="currentColor" style={{ marginLeft: 2 }} />
          )}
        </button>
      </Tooltip>
      <IconButton
        icon={SkipForward}
        iconOnly
        className="transport-step"
        tooltip="Next frame (→)"
        disabled={!project}
        onClick={() => void seekTo(playhead + frameStep)}
      />
      <Tooltip content="Edit timecode (Enter to seek)">
        <input
          type="text"
          className="timecode"
          value={editing ? draft : formatTimecode(playhead)}
          disabled={!project}
          onFocus={() => {
            setEditing(true);
            setDraft(formatTimecode(playhead));
          }}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commitTimecode}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              commitTimecode();
              (e.target as HTMLInputElement).blur();
            }
            if (e.key === "Escape") {
              setDraft(formatTimecode(playhead));
              setEditing(false);
              (e.target as HTMLInputElement).blur();
            }
          }}
        />
      </Tooltip>
      <span className="timecode-sub">/ {duration}</span>
      <span className="spacer" />
      <RatioMenu compact />
      <IconButton
        icon={fullscreen ? Minimize2 : Maximize2}
        iconOnly
        tooltip={fullscreen ? "Exit fullscreen (Esc)" : "Fullscreen preview"}
        disabled={!project}
        className={fullscreen ? "active" : ""}
        onClick={onToggleFullscreen}
      />
    </div>
  );
}
