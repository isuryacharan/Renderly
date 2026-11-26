// The viewer panel: the engine's canvas (rendered in a worker), fit and zoom, panning when zoomed,
// and the transport. The timecode follows the playhead through a ref, not React state.

import {
  formatTimecode,
  sequenceDuration,
  type Project,
  type ProjectStore,
  type ViewStore,
} from "@renderly/core";
import { createPreview } from "@renderly/engine";
import { colors } from "@renderly/ui/tokens";
import { Icon, IconButton, MenuButton, useToast } from "@renderly/ui/react";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { MediaStatus } from "../editor/media-status";
import { onThemeChange, resolvedTheme } from "../theme";
import { EngineLink } from "./link";

declare global {
  interface Window {
    /** Dev builds only: the viewer's link to the engine, for the end-to-end checks. */
    renderlyViewer?: EngineLink;
  }
}

const ZOOMS: { label: string; zoom: number | null }[] = [
  { label: "Fit", zoom: null },
  { label: "25%", zoom: 0.25 },
  { label: "50%", zoom: 0.5 },
  { label: "100%", zoom: 1 },
  { label: "200%", zoom: 2 },
];

const hex = (color: string) => Number.parseInt(color.replace("#", ""), 16);

export function Viewer({
  store,
  project,
  view,
  status,
}: {
  store: ProjectStore;
  project: Project;
  view: ViewStore;
  status: MediaStatus;
}) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const timecode = useRef<HTMLSpanElement>(null);
  const [link, setLink] = useState<EngineLink | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const [zoom, setZoom] = useState<number | null>(null);
  const pan = useRef({ x: 0, y: 0 });
  const toast = useToast();
  const latestToast = useRef(toast);
  latestToast.current = toast;

  const playing = useSyncExternalStore(
    (listener) => view.select((s) => s.playing, listener),
    () => view.get().playing,
  );
  const loop = useSyncExternalStore(
    (listener) => view.select((s) => s.loop, listener),
    () => view.get().loop,
  );

  // The engine: created once per project session. The canvas is handed to its worker for good.
  useEffect(() => {
    const element = canvas.current;
    if (!element) return;
    let created: EngineLink | null = null;
    let cancelled = false;
    createPreview(element)
      .then(async (preview) => {
        if (cancelled) return preview.dispose();
        created = new EngineLink(preview, store, view, status, (message) =>
          latestToast.current(message, { kind: "danger" }),
        );
        preview.onError((message) => latestToast.current(message, { kind: "danger" }));
        if (import.meta.env.DEV) window.renderlyViewer = created;
        await created.start();
        setLink(created);
      })
      .catch((error: unknown) => setFailed(error instanceof Error ? error.message : String(error)));
    return () => {
      cancelled = true;
      created?.dispose();
      if (window.renderlyViewer === created) delete window.renderlyViewer;
    };
  }, [store, view, status]);

  // How the picture sits: fitted or zoomed, panned, on the viewer colour of the theme.
  useEffect(() => {
    if (!link) return;
    const apply = () =>
      link.setView({ zoom, pan: pan.current }, hex(colors[resolvedTheme()].viewer));
    apply();
    return onThemeChange(apply);
  }, [link, zoom]);

  // Timecode: written straight into the DOM on every playhead change.
  const sequence =
    project.sequences.find((s) => s.id === view.get().sequence) ?? project.sequences[0];
  const fps = sequence?.fps;
  useEffect(() => {
    if (!fps) return;
    const write = (t: number) => {
      if (timecode.current) timecode.current.textContent = formatTimecode(t, fps);
    };
    write(view.get().playhead);
    return view.select((s) => s.playhead, write);
  }, [view, fps]);

  const ready = link !== null;
  const zoomLabel = ZOOMS.find((z) => z.zoom === zoom)?.label ?? "Fit";

  return (
    <section className="r-panel r-panel--viewer editor__viewer" aria-label="Viewer">
      <div className="r-panel__header viewer__header">
        <h2 className="text-body-strong viewer__title">Viewer</h2>
        <span className="r-toolbar__spacer" />
        <MenuButton
          label={
            <>
              {zoomLabel}
              <Icon name="chevron-down" />
            </>
          }
          aria-label={`Zoom: ${zoomLabel}`}
          tooltip="Zoom"
          className="r-button r-button--ghost r-button--sm"
          items={ZOOMS.map((option) => ({
            label: option.label,
            onSelect: () => {
              pan.current = { x: 0, y: 0 };
              setZoom(option.zoom);
            },
          }))}
        />
      </div>
      <div
        className={`viewer__stage${zoom === null ? "" : " viewer__stage--zoomed"}`}
        onPointerDown={(event) => {
          if (zoom === null || !link) return;
          const start = { x: event.clientX, y: event.clientY, pan: { ...pan.current } };
          const ratio = devicePixelRatio || 1;
          const target = event.currentTarget;
          target.setPointerCapture(event.pointerId);
          const move = (e: PointerEvent) => {
            pan.current = {
              x: start.pan.x + (e.clientX - start.x) * ratio,
              y: start.pan.y + (e.clientY - start.y) * ratio,
            };
            link.setView({ zoom, pan: pan.current }, hex(colors[resolvedTheme()].viewer));
          };
          const up = () => {
            target.removeEventListener("pointermove", move);
            target.removeEventListener("pointerup", up);
          };
          target.addEventListener("pointermove", move);
          target.addEventListener("pointerup", up);
        }}
      >
        <canvas ref={canvas} className="viewer__canvas" aria-label="Picture" />
        {failed && (
          <p className="viewer__message" role="alert">
            The preview cannot start: {failed}
          </p>
        )}
      </div>
      <div className="viewer__transport" role="group" aria-label="Transport">
        <span className="viewer__timecode r-tabular" ref={timecode} aria-label="Playhead" />
        <span className="r-toolbar__spacer" />
        <IconButton
          icon="to-start"
          label="Go to the start"
          size="sm"
          disabled={!ready}
          onClick={() => {
            void link?.pause().then(() => view.set({ playhead: 0 }));
          }}
        />
        <IconButton
          icon="previous-cut"
          label="Go to the previous cut"
          size="sm"
          disabled={!ready}
          onClick={() => void link?.jump(-1)}
        />
        <IconButton
          icon="frame-back"
          label="Step back one frame"
          size="sm"
          disabled={!ready}
          onClick={() => void link?.step(-1)}
        />
        <IconButton
          icon={playing ? "pause" : "play"}
          label={playing ? "Pause" : "Play"}
          disabled={!ready}
          onClick={() => link?.toggle()}
        />
        <IconButton
          icon="frame-forward"
          label="Step forward one frame"
          size="sm"
          disabled={!ready}
          onClick={() => void link?.step(1)}
        />
        <IconButton
          icon="next-cut"
          label="Go to the next cut"
          size="sm"
          disabled={!ready}
          onClick={() => void link?.jump(1)}
        />
        <IconButton
          icon="loop"
          label={loop ? "Stop looping" : "Loop the selection, or the whole sequence"}
          size="sm"
          aria-pressed={loop}
          disabled={!ready}
          onClick={() => view.set({ loop: !loop })}
        />
        <span className="r-toolbar__spacer" />
        <span className="viewer__duration r-tabular" aria-label="Duration">
          {sequence && fps ? formatTimecode(sequenceDuration(sequence), fps) : ""}
        </span>
      </div>
    </section>
  );
}
