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
import { Icon, MenuButton, useToast } from "@renderly/ui/react";
import { ActionButton, actionItem, useActions } from "../actions/react";
import { useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { MediaStatus } from "../editor/media-status";
import { onThemeChange, resolvedTheme } from "../theme";
import { EngineLink } from "./link";
import { ViewerOverlay } from "./overlay";

declare global {
  interface Window {
    /** Dev builds only: the viewer's link to the engine, for the end-to-end checks. */
    renderlyViewer?: EngineLink;
    /** Dev builds only: the viewer overlay (handles), for the end-to-end checks. */
    renderlyOverlay?: ViewerOverlay;
  }
}

/** The zoom menu: each entry is an action (see editor-actions.ts). */
const ZOOMS: { label: string; zoom: number | null; action: string }[] = [
  { label: "Fit", zoom: null, action: "viewer.fit" },
  { label: "25%", zoom: 0.25, action: "viewer.zoom25" },
  { label: "50%", zoom: 0.5, action: "viewer.zoom50" },
  { label: "100%", zoom: 1, action: "viewer.zoom100" },
  { label: "200%", zoom: 2, action: "viewer.zoom200" },
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
  const zoomNow = useRef<number | null>(null);
  zoomNow.current = zoom;
  const stage = useRef<HTMLDivElement>(null);
  const overlay = useRef<ViewerOverlay | null>(null);
  const linkNow = useRef<EngineLink | null>(null);
  const [overlays, setOverlays] = useState({ safeArea: false, thirds: false });
  const toast = useToast();
  const latestToast = useRef(toast);
  latestToast.current = toast;
  const actions = useActions();

  const playing = useSyncExternalStore(
    (listener) => view.select((s) => s.playing, listener),
    () => view.get().playing,
  );

  // The zoom actions reach the viewer through the registry.
  useEffect(() => {
    actions.attach({
      viewer: {
        zoom,
        setZoom: (next) => {
          pan.current = { x: 0, y: 0 };
          setZoom(next);
        },
        ...overlays,
        setOverlay: (change) => setOverlays((current) => ({ ...current, ...change })),
      },
    });
    if (overlay.current) {
      overlay.current.safeArea = overlays.safeArea;
      overlay.current.thirds = overlays.thirds;
      overlay.current.invalidate();
    }
  }, [actions, zoom, overlays]);

  // Handles over the picture: move, scale and rotate the selected clips.
  useEffect(() => {
    if (!stage.current) return;
    const created = new ViewerOverlay(stage.current, {
      store,
      view,
      viewer: () => ({ zoom: zoomNow.current, pan: pan.current }),
      readOnly: () => actions.ctx.readOnly,
      onError: (message) => latestToast.current(message, { kind: "danger" }),
      pause: () => void linkNow.current?.pause(),
    });
    overlay.current = created;
    if (import.meta.env.DEV) window.renderlyOverlay = created;
    return () => {
      created.destroy();
      overlay.current = null;
      if (window.renderlyOverlay === created) delete window.renderlyOverlay;
    };
  }, [store, view, actions]);
  useEffect(() => () => actions.attach({ viewer: null }), [actions]);

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
        linkNow.current = created;
        actions.attach({ link: created });
      })
      .catch((error: unknown) => setFailed(error instanceof Error ? error.message : String(error)));
    return () => {
      cancelled = true;
      actions.attach({ link: null });
      created?.dispose();
      if (window.renderlyViewer === created) delete window.renderlyViewer;
    };
  }, [store, view, status, actions]);

  // How the picture sits: fitted or zoomed, panned, on the viewer colour of the theme.
  useEffect(() => {
    if (!link) return;
    const apply = () => {
      link.setView({ zoom, pan: pan.current }, hex(colors[resolvedTheme()].viewer));
      overlay.current?.invalidate();
    };
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

  const zoomLabel = ZOOMS.find((z) => z.zoom === zoom)?.label ?? "Fit";

  return (
    <section className="r-panel r-panel--viewer editor__viewer" aria-label="Viewer">
      <div className="r-panel__header viewer__header">
        <h2 className="text-body-strong viewer__title">Viewer</h2>
        <span className="r-toolbar__spacer" />
        <ActionButton id="viewer.safeArea" />
        <ActionButton id="viewer.thirds" />
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
          items={ZOOMS.map((option) => actionItem(actions, option.action))}
        />
      </div>
      <div
        ref={stage}
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
            overlay.current?.invalidate();
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
        <ActionButton id="playback.start" />
        <ActionButton id="playback.previousCut" />
        <ActionButton id="playback.frameBack" />
        <ActionButton id={playing ? "playback.pause" : "playback.play"} size="md" />
        <ActionButton id="playback.frameForward" />
        <ActionButton id="playback.nextCut" />
        <ActionButton id="playback.loop" />
        <span className="r-toolbar__spacer" />
        <span className="viewer__duration r-tabular" aria-label="Duration">
          {sequence && fps ? formatTimecode(sequenceDuration(sequence), fps) : ""}
        </span>
      </div>
    </section>
  );
}
