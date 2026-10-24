import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useEditorStore } from "../store/editorStore";
import { addMarkers, removeMarkers } from "../lib/commands";
import { MARKER_COLORS } from "../lib/text";
import { formatTimecode } from "../lib/format";

/// Right-click menu of a ruler marker: rename, colour, delete. Rename and recolour are a
/// `RemoveMarkers` + `AddMarkers` (same id, same time) in one undo step - core has no
/// "edit marker" command, and re-adding with the same id keeps the marker's identity.
export function MarkerMenu() {
  const menu = useEditorStore((s) => s.markerMenu);
  const marker = useEditorStore((s) =>
    s.markerMenu ? s.project?.markers?.find((m) => m.id === s.markerMenu!.markerId) : undefined,
  );
  const dispatchBatch = useEditorStore((s) => s.dispatchBatch);
  const dispatch = useEditorStore((s) => s.dispatch);
  const close = useEditorStore((s) => s.closeMarkerMenu);
  const seekTo = useEditorStore((s) => s.seekTo);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);
  const [label, setLabel] = useState("");

  useEffect(() => {
    setLabel(marker?.label ?? "");
  }, [marker?.id, marker?.label]);

  useEffect(() => {
    if (!menu) return;
    const onDown = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
    };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [menu, close]);

  useLayoutEffect(() => {
    if (!menu || !rootRef.current) {
      setPos(null);
      return;
    }
    const rect = rootRef.current.getBoundingClientRect();
    const margin = 4;
    setPos({
      left: Math.max(margin, Math.min(menu.x, window.innerWidth - rect.width - margin)),
      top: Math.max(margin, Math.min(menu.y, window.innerHeight - rect.height - margin)),
    });
  }, [menu]);

  if (!menu || !marker) return null;

  /** Replace the marker by itself with changed fields (one undo step). */
  function update(fields: { label?: string; color?: string }) {
    if (!marker) return;
    void dispatchBatch([
      removeMarkers([marker.id]),
      addMarkers([
        {
          id: marker.id,
          timeUs: marker.time_us,
          label: fields.label ?? marker.label,
          color: fields.color ?? marker.color,
          kind: marker.kind,
        },
      ]),
    ]);
  }

  return (
    <div
      className="context-menu marker-menu"
      ref={rootRef}
      style={{ left: pos?.left ?? menu.x, top: pos?.top ?? menu.y }}
    >
      <div className="marker-menu-head">
        {marker.kind === "beat" ? "Beat marker" : "Marker"} · {formatTimecode(marker.time_us).slice(0, 11)}
      </div>
      <form
        className="marker-menu-rename"
        onSubmit={(e) => {
          e.preventDefault();
          if (label !== marker.label) update({ label });
          close();
        }}
      >
        <input
          autoFocus
          value={label}
          placeholder="Marker name"
          aria-label="Marker name"
          onChange={(e) => setLabel(e.target.value)}
        />
      </form>
      <div className="marker-menu-colors" role="group" aria-label="Marker colour">
        {MARKER_COLORS.map((c) => (
          <button
            key={c.value || "default"}
            type="button"
            className={`marker-swatch${marker.color === c.value ? " selected" : ""}`}
            title={c.label}
            aria-label={c.label}
            style={c.value ? { background: c.value } : undefined}
            onClick={() => {
              if (c.value !== marker.color) update({ color: c.value });
              close();
            }}
          />
        ))}
      </div>
      <div className="context-menu-divider" />
      <button
        type="button"
        onClick={() => {
          close();
          void seekTo(marker.time_us);
        }}
      >
        Go to marker
      </button>
      <button
        type="button"
        className="danger"
        onClick={() => {
          close();
          void dispatch(removeMarkers([marker.id]));
        }}
      >
        Delete marker
      </button>
    </div>
  );
}
