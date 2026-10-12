import { useMemo, useRef, useState } from "react";
import { ExternalLink, Film, Group, Music, Pencil, Plus, Search, Settings2, Trash2 } from "lucide-react";
import { useEditorStore, type ThumbnailAsset } from "../../store/editorStore";
import { fileName } from "../../lib/format";
import { mediaDurationSecs, sequenceMediaName } from "../../lib/types";
import { pickAndImportMedia, importFromPath } from "../../lib/projectFlows";
import { startMediaDrag } from "../../lib/dragMedia";
import { assetUrl } from "../../lib/ipc";
import { createSequence, deleteSequence, renameSequence } from "../../lib/commands";
import { SequenceSettingsDialog } from "../dialogs/SequenceSettingsDialog";
import { Tooltip } from "../ui/Tooltip";

/// Filmstrip thumbnail with hover-scrub: moving the mouse across the card selects which
/// strip tile to show, CapCut-style, instead of a single static frame.
function FilmstripThumb({ thumb, durationSecs }: { thumb: ThumbnailAsset; durationSecs: number }) {
  const [hoverFrac, setHoverFrac] = useState<number | null>(null);
  const tileCount = thumb.cols * thumb.rows;
  const frac = hoverFrac ?? 0;
  const sourceTime = frac * Math.max(durationSecs, 0);
  const tileIndex = Math.max(
    0,
    Math.min(tileCount - 1, thumb.intervalSecs > 0 ? Math.round(sourceTime / thumb.intervalSecs) : 0),
  );
  const col = tileIndex % thumb.cols;
  const row = Math.floor(tileIndex / thumb.cols);

  return (
    <div
      className="media-thumb video filmstrip"
      onMouseMove={(e) => {
        const rect = e.currentTarget.getBoundingClientRect();
        setHoverFrac(Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width)));
      }}
      onMouseLeave={() => setHoverFrac(null)}
      style={{
        backgroundImage: `url(${thumb.stripUrl})`,
        // Percentage-based so one strip tile always fills the card exactly, whatever the
        // card's rendered size (the bin is now a fluid grid, not fixed 36px squares).
        backgroundPosition: `${thumb.cols > 1 ? (col / (thumb.cols - 1)) * 100 : 0}% ${
          thumb.rows > 1 ? (row / (thumb.rows - 1)) * 100 : 0
        }%`,
        backgroundSize: `${thumb.cols * 100}% ${thumb.rows * 100}%`,
      }}
    />
  );
}

function WaveformThumb({ peaks }: { peaks: number[] }) {
  // Downsample to a handful of bars for the bin thumbnail.
  const bars = 24;
  const step = Math.max(1, Math.floor(peaks.length / bars));
  const sampled: number[] = [];
  for (let i = 0; i < bars; i++) {
    let max = 0;
    for (let j = 0; j < step && i * step + j < peaks.length; j++) {
      max = Math.max(max, Math.abs(peaks[i * step + j] ?? 0));
    }
    sampled.push(max);
  }
  const peak = Math.max(0.01, ...sampled);
  return (
    <div className="media-thumb audio waveform-thumb" aria-hidden>
      {sampled.map((v, i) => (
        <span key={i} style={{ height: `${Math.max(8, (v / peak) * 100)}%` }} />
      ))}
    </div>
  );
}

function mediaMetaLine(item: {
  kind: string;
  duration_secs?: number | null;
  width?: number | null;
  height?: number | null;
  fps?: number | null;
}): string {
  const parts: string[] = [item.kind];
  if (item.width && item.height) parts.push(`${item.width}×${item.height}`);
  if (item.fps) parts.push(`${item.fps.toFixed(0)} fps`);
  if (item.duration_secs != null) parts.push(`${item.duration_secs.toFixed(1)}s`);
  return parts.join(" · ");
}

const KIND_FILTERS = [
  { id: "all", label: "All" },
  { id: "video", label: "Video" },
  { id: "audio", label: "Audio" },
  { id: "image", label: "Image" },
  { id: "sequence", label: "Sequences" },
] as const;
type KindFilter = (typeof KIND_FILTERS)[number]["id"];

export function MediaPanel() {
  const project = useEditorStore((s) => s.project);
  const mediaAssets = useEditorStore((s) => s.mediaAssets);
  const placeMediaOnTimeline = useEditorStore((s) => s.placeMediaOnTimeline);
  const dispatch = useEditorStore((s) => s.dispatch);
  const toast = useEditorStore((s) => s.toast);
  const openSequence = useEditorStore((s) => s.openSequence);
  const [settingsFor, setSettingsFor] = useState<string | null>(null);
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameValue, setRenameValue] = useState("");
  // Enter/Escape unmount the focused input, which fires a native blur; this flag keeps that
  // blur from committing (or double-committing) - same approach as TrackHeaders' rename.
  const explicitRenameRef = useRef(false);
  const [dragOver, setDragOver] = useState(false);
  const [query, setQuery] = useState("");
  const [kind, setKind] = useState<KindFilter>("all");

  // G4: show all asset kinds in the bin (audio used to be filtered out).
  const allItems = project?.media ?? [];
  const emptyProject = allItems.length === 0;
  const items = useMemo(() => {
    const q = query.trim().toLowerCase();
    return allItems.filter(
      (m) =>
        (kind === "all" || m.kind === kind) &&
        (q === "" ||
          (project ? (sequenceMediaName(project, m) ?? fileName(m.path)) : fileName(m.path))
            .toLowerCase()
            .includes(q)),
    );
  }, [allItems, kind, query, project]);
  const counts = useMemo(() => {
    const c: Record<string, number> = { all: allItems.length, video: 0, audio: 0, image: 0, sequence: 0 };
    for (const m of allItems) c[m.kind] = (c[m.kind] ?? 0) + 1;
    return c;
  }, [allItems]);

  async function newSequence() {
    const n = (project?.sequences?.length ?? 0) + 1;
    const ok = await dispatch(createSequence(`Sequence ${n}`));
    if (ok) {
      setKind("sequence");
      toast(`Created Sequence ${n} - drag it to the timeline or open it to add clips`, "success");
    }
  }

  function startRename(sequenceId: string, name: string) {
    explicitRenameRef.current = false;
    setRenamingId(sequenceId);
    setRenameValue(name);
  }

  function commitRename(sequenceId: string, current: string) {
    explicitRenameRef.current = true;
    setRenamingId(null);
    const trimmed = renameValue.trim();
    if (trimmed && trimmed !== current) void dispatch(renameSequence(sequenceId, trimmed));
  }

  return (
    <div className={`panel-body media-bin${emptyProject ? " media-import-first" : ""}`}>
      <SequenceSettingsDialog sequenceId={settingsFor} onClose={() => setSettingsFor(null)} />
      {!emptyProject && (
        <div className="media-toolbar">
          <div className="media-toolbar-row">
            <div className="media-search">
              <Search size={13} strokeWidth={2} />
              <input
                type="text"
                placeholder="Search media"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
              />
            </div>
            <Tooltip content="New sequence">
              <button
                type="button"
                className="btn-icon-only"
                aria-label="New sequence"
                onClick={() => void newSequence()}
              >
                <Group size={14} strokeWidth={1.75} />
              </button>
            </Tooltip>
          </div>
          <div className="media-chips">
            {KIND_FILTERS.map((f) => (
              <button
                key={f.id}
                type="button"
                className={`chip${kind === f.id ? " on" : ""}`}
                onClick={() => setKind(f.id)}
              >
                {f.label}
                {counts[f.id] ? <span className="chip-count">{counts[f.id]}</span> : null}
              </button>
            ))}
          </div>
        </div>
      )}

      <div
        className={`drop-zone${dragOver ? " drag-over" : ""}${emptyProject ? " drop-zone-hero" : ""}`}
        onClick={() => void pickAndImportMedia()}
        onDragOver={(e) => {
          e.preventDefault();
          setDragOver(true);
        }}
        onDragLeave={() => setDragOver(false)}
        onDrop={(e) => {
          e.preventDefault();
          setDragOver(false);
          const file = e.dataTransfer.files?.[0];
          // In Tauri, OS drops usually arrive as `tauri://drag-drop` with real paths.
          // Browser-style drops may only expose a File name — still try when a path-like
          // string is present (some webviews populate `path` on the File object).
          const path =
            (file as File & { path?: string })?.path ||
            e.dataTransfer.getData("text/plain") ||
            "";
          if (path && /[/\\]/.test(path)) void importFromPath(path);
        }}
      >
        <strong>{emptyProject ? "Import media to start" : "Drop media here"}</strong>
        <span>Video, image, or audio — click to browse</span>
      </div>

      {emptyProject ? (
        <div className="empty-state">
          <div className="empty-state-icon">
            <Film size={28} strokeWidth={1.5} />
          </div>
          <p>
            <strong>No media yet</strong>
          </p>
          <p className="empty-hint">Drop a file above or click the import area to browse.</p>
        </div>
      ) : items.length === 0 ? (
        <div className="empty-state">
          <p className="empty-hint">No media matches your search.</p>
        </div>
      ) : (
        <div className="media-grid">
          {items.map((item) => {
          const thumb = mediaAssets[item.id]?.thumbnails;
          const waveform = mediaAssets[item.id]?.waveform;
          const pendingThumb = item.kind === "video" && !thumb;
          const isSequence = item.kind === "sequence";
          const seqName = project ? sequenceMediaName(project, item) : undefined;
          const itemDuration = project ? mediaDurationSecs(project, item) : item.duration_secs;
          return (
            <div
              key={item.id}
              className={`media-item${isSequence ? " media-item-sequence" : ""}`}
              draggable
              onDragStart={(e) => startMediaDrag(e, item.id, item.kind, itemDuration ?? 5)}
              onClick={() => void placeMediaOnTimeline(item.id, item.kind)}
              onDoubleClick={() => {
                if (isSequence && item.sequence_id)
                  void openSequence(item.sequence_id, { fromRoot: true });
              }}
            >
              {isSequence && item.sequence_id && (
                <div className="media-item-actions" onClick={(e) => e.stopPropagation()}>
                  <Tooltip content="Open sequence">
                    <button
                      type="button"
                      aria-label="Open sequence"
                      onClick={() => void openSequence(item.sequence_id!, { fromRoot: true })}
                    >
                      <ExternalLink size={12} strokeWidth={1.75} />
                    </button>
                  </Tooltip>
                  <Tooltip content="Rename">
                    <button
                      type="button"
                      aria-label="Rename sequence"
                      onClick={() => startRename(item.sequence_id!, seqName ?? "")}
                    >
                      <Pencil size={12} strokeWidth={1.75} />
                    </button>
                  </Tooltip>
                  <Tooltip content="Sequence settings">
                    <button
                      type="button"
                      aria-label="Sequence settings"
                      onClick={() => setSettingsFor(item.sequence_id!)}
                    >
                      <Settings2 size={12} strokeWidth={1.75} />
                    </button>
                  </Tooltip>
                  <Tooltip content="Delete sequence">
                    <button
                      type="button"
                      aria-label="Delete sequence"
                      onClick={() => void dispatch(deleteSequence(item.sequence_id!))}
                    >
                      <Trash2 size={12} strokeWidth={1.75} />
                    </button>
                  </Tooltip>
                </div>
              )}
              {isSequence ? (
                <div className="media-thumb sequence" aria-hidden>
                  <Group size={20} strokeWidth={1.5} />
                </div>
              ) : thumb && thumb.image ? (
                <FilmstripThumb thumb={thumb} durationSecs={item.duration_secs ?? 0} />
              ) : item.kind === "audio" && waveform?.peaks?.length ? (
                <WaveformThumb peaks={waveform.peaks} />
              ) : item.kind === "image" ? (
                <div
                  className="media-thumb image"
                  style={{
                    backgroundImage: `url(${assetUrl(item.path)})`,
                    backgroundSize: "cover",
                    backgroundPosition: "center",
                  }}
                />
              ) : pendingThumb ? (
                <div className="media-thumb skeleton" aria-label="Generating thumbnails" />
              ) : (
                <div className={`media-thumb ${item.kind}`}>
                  {item.kind === "audio" ? (
                    <Music size={18} strokeWidth={1.5} />
                  ) : (
                    <Film size={18} strokeWidth={1.5} />
                  )}
                </div>
              )}
              <div className="media-meta">
                {isSequence && item.sequence_id && renamingId === item.sequence_id ? (
                  <input
                    autoFocus
                    className="media-item-rename"
                    aria-label="Sequence name"
                    value={renameValue}
                    onClick={(e) => e.stopPropagation()}
                    onChange={(e) => setRenameValue(e.target.value)}
                    onBlur={() => {
                      if (!explicitRenameRef.current) commitRename(item.sequence_id!, seqName ?? "");
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") commitRename(item.sequence_id!, seqName ?? "");
                      if (e.key === "Escape") {
                        explicitRenameRef.current = true;
                        setRenamingId(null);
                      }
                    }}
                  />
                ) : (
                  <div className="name">
                    {project ? (seqName ?? fileName(item.path)) : fileName(item.path)}
                  </div>
                )}
                <div className="sub">
                  {mediaMetaLine(
                    project ? { ...item, duration_secs: itemDuration } : item,
                  )}
                  {pendingThumb ? " · generating…" : ""}
                </div>
              </div>
              <Tooltip content="Add to timeline">
                <span className="media-add-hint">
                  <Plus size={14} strokeWidth={2} />
                </span>
              </Tooltip>
            </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
