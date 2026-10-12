import { useEffect, useRef, useState, type CSSProperties, type RefObject } from "react";
import { Film, Music, Type, VolumeX, Lock, EyeOff, MoreHorizontal, GripVertical } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { selectActiveView } from "../../lib/container";
import { deleteTrack, moveTrack, renameTrack, setTrackFlags } from "../../lib/commands";
import {
  displayOrder,
  moveTrackNewIndex,
  trackLayout,
  zoneRowBounds,
  TRACK_GAP,
  TRACK_LABEL_W,
} from "../../timeline/layout";
import type { Track } from "../../lib/types";
import { Tooltip } from "../ui/Tooltip";

interface DragView {
  trackId: string;
  startRow: number;
  overRow: number;
  deltaY: number;
}

function TrackKindIcon({ kind }: { kind: Track["kind"] }) {
  const props = { size: 12, strokeWidth: 1.75 } as const;
  if (kind === "video") return <Film {...props} />;
  if (kind === "audio") return <Music {...props} />;
  return <Type {...props} />;
}

/// DOM column of track headers overlaid on the TRACK_LABEL_W gutter the canvas reserves
/// but no longer draws into (see renderer.ts) — real interactive controls (dbl-click
/// rename, mute/lock/hide toggles, delete) instead of canvas hit-testing.
export function TrackHeaders({ containerRef }: { containerRef: RefObject<HTMLElement | null> }) {
  const project = useEditorStore(selectActiveView);
  const dispatch = useEditorStore((s) => s.dispatch);
  const addAdjustmentLayerAtPlayhead = useEditorStore((s) => s.addAdjustmentLayerAtPlayhead);
  const [height, setHeight] = useState(0);
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  const [menuOpen, setMenuOpen] = useState<string | null>(null);
  const [dragView, setDragView] = useState<DragView | null>(null);
  const rootRef = useRef<HTMLDivElement | null>(null);
  // Enter/Escape both call `setRenaming(null)`, which unmounts the focused rename
  // `<input>` — removing a focused DOM node fires a native blur, which React re-delivers
  // as `onBlur` before the node is gone. Without this flag, that blur would re-invoke
  // `commitRename` a second time after Enter (double-dispatching the same rename), or
  // invoke it for the first time after Escape (silently committing the very edit Escape
  // was supposed to discard). Set right before either explicit action, checked (and
  // cleared) in `onBlur` so only a genuine "clicked elsewhere" blur commits.
  const explicitRenameActionRef = useRef(false);

  useEffect(() => {
    const el = containerRef.current;
    if (!el) return;
    const update = () => setHeight(el.clientHeight);
    update();
    const observer = new ResizeObserver(update);
    observer.observe(el);
    return () => observer.disconnect();
  }, [containerRef]);

  useEffect(() => {
    const onDocClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setMenuOpen(null);
    };
    document.addEventListener("click", onDocClick);
    return () => document.removeEventListener("click", onDocClick);
  }, []);

  const scrollY = useEditorStore((s) => s.scrollY);
  if (!project || project.tracks.length === 0 || height === 0) return null;

  // Same visual row order as the canvas (renderer.ts/interactions.ts) — see
  // `timeline/layout.ts`'s `displayOrder` doc comment for the CapCut-style mapping.
  const order = displayOrder(project);
  const { trackH, laneTop } = trackLayout(height, order.length, scrollY);

  function startRename(track: Track) {
    explicitRenameActionRef.current = false;
    setRenaming(track.id);
    setRenameValue(track.name);
  }

  function commitRename(track: Track) {
    explicitRenameActionRef.current = true;
    setRenaming(null);
    const trimmed = renameValue.trim();
    if (trimmed && trimmed !== track.name) void dispatch(renameTrack(track.id, trimmed));
  }

  function cancelRename() {
    explicitRenameActionRef.current = true;
    setRenaming(null);
  }

  // Pointer-drag reorder, initiated from the row's grip handle only (so mute/lock/hide/
  // rename/menu click targets keep working normally — no drag threshold needed). Zone
  // constraint (CapCut behavior): the drag target row is clamped to the track's own zone
  // (`zoneRowBounds`) so a video/caption row can never be dropped among audio rows and
  // vice versa. `moveTrackNewIndex` (timeline/layout.ts) does the display-order-inversion
  // math to turn the drop's visual row into the core's `MoveTrack.new_index`.
  function onGripPointerDown(e: React.PointerEvent, track: Track, row: number) {
    e.preventDefault();
    e.stopPropagation();
    const proj = selectActiveView(useEditorStore.getState());
    if (!proj) return;
    const [zoneStart, zoneEnd] = zoneRowBounds(proj, track.kind);
    const startClientY = e.clientY;
    const startRow = row;
    setDragView({ trackId: track.id, startRow, overRow: row, deltaY: 0 });

    const onMove = (ev: PointerEvent) => {
      const deltaY = ev.clientY - startClientY;
      const rowDelta = Math.round(deltaY / (trackH + TRACK_GAP));
      const overRow = Math.min(zoneEnd, Math.max(zoneStart, startRow + rowDelta));
      setDragView({ trackId: track.id, startRow, overRow, deltaY });
    };
    const onUp = () => {
      window.removeEventListener("pointermove", onMove);
      window.removeEventListener("pointerup", onUp);
      setDragView((current) => {
        if (current && current.trackId === track.id && current.overRow !== current.startRow) {
          const latestProject = selectActiveView(useEditorStore.getState());
          if (latestProject) {
            const newIndex = moveTrackNewIndex(latestProject, track.id, current.overRow);
            if (newIndex != null) void dispatch(moveTrack(track.id, newIndex));
          }
        }
        return null;
      });
    };
    window.addEventListener("pointermove", onMove);
    window.addEventListener("pointerup", onUp);
  }

  function onRenameBlur(track: Track) {
    // A genuine "user clicked elsewhere" blur, not the unmount-triggered one that follows
    // Enter/Escape — commit like leaving any other text field.
    if (explicitRenameActionRef.current) return;
    commitRename(track);
  }

  return (
    <div className="track-headers" style={{ width: TRACK_LABEL_W }} ref={rootRef}>
      {order.map(({ track }, i) => {
        const dragging = dragView?.trackId === track.id;
        const rowStyle: CSSProperties = { top: laneTop(i), height: trackH };
        if (dragging) {
          rowStyle.transform = `translateY(${dragView!.deltaY}px)`;
          rowStyle.zIndex = 10;
          rowStyle.opacity = 0.85;
        }
        return (
        <div
          key={track.id}
          className={`track-header-row kind-${track.kind}${dragging ? " dragging" : ""}`}
          style={rowStyle}
        >
          <button
            type="button"
            className="track-header-grip"
            aria-label="Drag to reorder track"
            onPointerDown={(e) => onGripPointerDown(e, track, i)}
          >
            <GripVertical size={12} strokeWidth={1.75} />
          </button>
          <span className="track-header-icon">
            <TrackKindIcon kind={track.kind} />
          </span>
          {renaming === track.id ? (
            <input
              autoFocus
              className="track-header-rename"
              value={renameValue}
              onChange={(e) => setRenameValue(e.target.value)}
              onBlur={() => onRenameBlur(track)}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitRename(track);
                if (e.key === "Escape") cancelRename();
              }}
            />
          ) : (
            <Tooltip content="Double-click to rename">
              <span
                className={`track-header-name${track.hidden || track.muted ? " dimmed" : ""}`}
                onDoubleClick={() => startRename(track)}
              >
                {track.name}
              </span>
            </Tooltip>
          )}
          <div className="track-header-actions">
            <Tooltip content={track.muted ? "Unmute" : "Mute"}>
              <button
                type="button"
                className={`track-flag-btn${track.muted ? " active" : ""}`}
                onClick={() => void dispatch(setTrackFlags(track.id, { muted: !track.muted }))}
              >
                <VolumeX size={12} strokeWidth={1.75} />
              </button>
            </Tooltip>
            <Tooltip content={track.locked ? "Unlock" : "Lock"}>
              <button
                type="button"
                className={`track-flag-btn${track.locked ? " active" : ""}`}
                onClick={() => void dispatch(setTrackFlags(track.id, { locked: !track.locked }))}
              >
                <Lock size={12} strokeWidth={1.75} />
              </button>
            </Tooltip>
            <Tooltip content={track.hidden ? "Show" : "Hide"}>
              <button
                type="button"
                className={`track-flag-btn${track.hidden ? " active" : ""}`}
                onClick={() => void dispatch(setTrackFlags(track.id, { hidden: !track.hidden }))}
              >
                <EyeOff size={12} strokeWidth={1.75} />
              </button>
            </Tooltip>
            <div className="track-header-menu">
              <Tooltip content="More" disabled={menuOpen === track.id}>
                <button
                  type="button"
                  className="track-flag-btn"
                  aria-label="Track options"
                  aria-haspopup="menu"
                  aria-expanded={menuOpen === track.id}
                  onClick={() => setMenuOpen(menuOpen === track.id ? null : track.id)}
                >
                  <MoreHorizontal size={12} strokeWidth={1.75} />
                </button>
              </Tooltip>
              {menuOpen === track.id && (
                <div className="track-header-menu-pop">
                  {track.kind === "video" && (
                    <button
                      type="button"
                      className="neutral"
                      disabled={track.locked}
                      onClick={async () => {
                        setMenuOpen(null);
                        await addAdjustmentLayerAtPlayhead(track.id);
                      }}
                    >
                      Add adjustment layer
                    </button>
                  )}
                  <button
                    type="button"
                    onClick={async () => {
                      setMenuOpen(null);
                      await dispatch(deleteTrack(track.id));
                    }}
                  >
                    Delete track
                  </button>
                </div>
              )}
            </div>
          </div>
        </div>
        );
      })}
      {dragView && dragView.overRow !== dragView.startRow && (
        <div
          className="track-header-drop-indicator"
          style={{
            top:
              dragView.overRow < dragView.startRow
                ? laneTop(dragView.overRow) - TRACK_GAP / 2
                : laneTop(dragView.overRow) + trackH + TRACK_GAP / 2,
          }}
        />
      )}
    </div>
  );
}
