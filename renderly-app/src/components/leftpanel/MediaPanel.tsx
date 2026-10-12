import { useMemo, useState } from "react";
import { Film, Music, Plus, Search } from "lucide-react";
import { useEditorStore, type ThumbnailAsset } from "../../store/editorStore";
import { fileName } from "../../lib/format";
import { mediaDurationSecs, sequenceMediaName } from "../../lib/types";
import { pickAndImportMedia, importFromPath } from "../../lib/projectFlows";
import { startMediaDrag } from "../../lib/dragMedia";
import { assetUrl } from "../../lib/ipc";
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
] as const;
type KindFilter = (typeof KIND_FILTERS)[number]["id"];

export function MediaPanel() {
  const project = useEditorStore((s) => s.project);
  const mediaAssets = useEditorStore((s) => s.mediaAssets);
  const placeMediaOnTimeline = useEditorStore((s) => s.placeMediaOnTimeline);
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
    const c: Record<string, number> = { all: allItems.length, video: 0, audio: 0, image: 0 };
    for (const m of allItems) c[m.kind] = (c[m.kind] ?? 0) + 1;
    return c;
  }, [allItems]);

  return (
    <div className={`panel-body media-bin${emptyProject ? " media-import-first" : ""}`}>
      {!emptyProject && (
        <div className="media-toolbar">
          <div className="media-search">
            <Search size={13} strokeWidth={2} />
            <input
              type="text"
              placeholder="Search media"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
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
          return (
            <div
              key={item.id}
              className="media-item"
              draggable
              onDragStart={(e) => startMediaDrag(e, item.id, item.kind, item.duration_secs ?? 5)}
              onClick={() => void placeMediaOnTimeline(item.id, item.kind)}
            >
              {thumb && thumb.image ? (
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
                <div className="name">
                  {project ? (sequenceMediaName(project, item) ?? fileName(item.path)) : fileName(item.path)}
                </div>
                <div className="sub">
                  {mediaMetaLine(
                    project ? { ...item, duration_secs: mediaDurationSecs(project, item) } : item,
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
