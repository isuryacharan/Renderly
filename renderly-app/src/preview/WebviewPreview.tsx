import { useEffect, useRef } from "react";
import * as ipc from "../lib/ipc";
import { useCatalogueStore } from "../store/catalogueStore";
import { useEditorStore } from "../store/editorStore";
import { registerPreviewEngine } from "./previewBridge";
import { WebviewPreviewEngine } from "./webviewPreviewEngine";

type ContentBounds = { left: number; top: number; width: number; height: number };

/// Same letterbox math as PreviewHandlesOverlay.tsx / PreviewMaskOverlay.tsx's
/// `contentBoundsInHost` — kept as its own copy (matching the existing pattern in those
/// two files) rather than a shared import, since each caller needs its own
/// ResizeObserver lifecycle.
function contentBoundsInHost(host: HTMLElement, aspect: number): ContentBounds | null {
  const rect = host.getBoundingClientRect();
  if (rect.width < 2 || rect.height < 2) return null;
  const hostAspect = rect.width / rect.height;
  let width: number;
  let height: number;
  if (hostAspect > aspect) {
    height = rect.height;
    width = height * aspect;
  } else {
    width = rect.width;
    height = width / aspect;
  }
  return {
    left: (rect.width - width) / 2,
    top: (rect.height - height) / 2,
    width,
    height,
  };
}

/// The webview preview surface: a <canvas> painted by `WebviewPreviewEngine` from
/// browser-decoded <video>/<img> elements. Positioned at the letterboxed content rect,
/// z-index BELOW the handles/mask overlays (they're 5/6; this is implicitly 0/auto) so
/// direct-manipulation handles are always visible — the whole point of this migration,
/// see docs/preview-webview.md.
export function WebviewPreview({
  hostRef,
  aspect,
}: {
  hostRef: React.RefObject<HTMLDivElement | null>;
  aspect: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const engineRef = useRef<WebviewPreviewEngine | null>(null);
  const project = useEditorStore((s) => s.project);
  const playing = useEditorStore((s) => s.playing);
  const playhead = useEditorStore((s) => s.playhead);
  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);

  // Engine lifecycle: created once per mount. init() resolves the render-mode ladder
  // (WebGPU wasm compositor → Canvas2D fallback, see webviewPreviewEngine.ts) — draws
  // before it resolves are no-ops, and the state effects below re-fire afterwards anyway.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const engine = new WebviewPreviewEngine(canvas, ipc.assetUrl, ipc.previewAssetBackend);
    engineRef.current = engine;
    registerPreviewEngine(engine);
    (globalThis as unknown as { __previewEngine?: WebviewPreviewEngine }).__previewEngine = engine;
    void engine.init().then(() => {
      // Push whatever state accumulated while the mode ladder resolved.
      engine.setProject(useEditorStore.getState().project ?? null);
      engine.setPreviewOverrides(useEditorStore.getState().previewOverrides);
      engine.setActiveSequence(useEditorStore.getState().activeSequenceId);
      engine.seek(useEditorStore.getState().playhead);
    });
    // The installed library changed (pack installed / updated / removed): refetch pack files.
    const unsubscribeCatalogue = useCatalogueStore.subscribe((state, prev) => {
      if (state.epoch !== prev.epoch) engine.invalidatePackItems();
    });
    return () => {
      unsubscribeCatalogue();
      registerPreviewEngine(null);
      engine.dispose();
      engineRef.current = null;
    };
  }, []);

  // Bounds tracking (letterboxed content rect within the host).
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    const sync = () => {
      const bounds = contentBoundsInHost(host, aspect);
      const canvas = canvasRef.current;
      const engine = engineRef.current;
      if (!bounds || !canvas || !engine) return;
      canvas.style.left = `${bounds.left}px`;
      canvas.style.top = `${bounds.top}px`;
      canvas.style.width = `${bounds.width}px`;
      canvas.style.height = `${bounds.height}px`;
      engine.setSize(bounds.width, bounds.height);
    };
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(host);
    return () => ro.disconnect();
  }, [hostRef, aspect]);

  // Project changes (media pool reconciliation + immediate redraw).
  useEffect(() => {
    engineRef.current?.setProject(project ?? null);
  }, [project]);

  // The open sequence (schema v7): the preview renders that container at the local playhead.
  // After `setProject` above so a sequence that was just deleted resolves against the new project.
  useEffect(() => {
    engineRef.current?.setActiveSequence(activeSequenceId);
  }, [activeSequenceId, project]);

  // Store subscription: while paused, redraw on any project mutation (e.g. optimistic
  // transform-drag patch from PreviewHandlesOverlay) so local edits show at full frame
  // rate without a backend round trip (see docs/preview-webview.md item 7).
  useEffect(() => {
    return useEditorStore.subscribe((state, prevState) => {
      if (state.project !== prevState.project) {
        engineRef.current?.notifyProjectPatched();
      }
      // Live drags and hover previews change only the render-only overrides (no project change).
      if (state.previewOverrides !== prevState.previewOverrides) {
        engineRef.current?.setPreviewOverrides(state.previewOverrides);
      }
    });
  }, []);

  // Play/pause transitions.
  useEffect(() => {
    const engine = engineRef.current;
    if (!engine) return;
    if (playing) {
      engine.play(useEditorStore.getState().playhead);
    } else {
      engine.pause(useEditorStore.getState().playhead);
    }
  }, [playing]);

  // Scrub: paused-state scrub always redraws; a scrub *during* playback is also routed
  // through here (BUG 3) — `engine.seek()` itself decides whether a jump this size, while
  // playing, is worth rebasing the local clock for (see its doc comment), so ordinary
  // tick-driven playhead updates (which stay close to the clock's own prediction) are a
  // cheap no-op here rather than fighting `onTick`'s smoothing.
  useEffect(() => {
    engineRef.current?.seek(playhead);
  }, [playhead, playing]);

  // Backend playback:tick drift correction (no-op in the browser harness where ticks
  // never fire — see WebviewPreviewEngine.onTick's doc comment).
  useEffect(() => {
    return ipc.onPlaybackTick((p) => engineRef.current?.onTick(p.time_us));
  }, []);

  return <canvas ref={canvasRef} className="preview-webview-canvas" />;
}
