import { useEffect, useState } from "react";
import { TopBar } from "./components/TopBar";
import { LeftPanel } from "./components/leftpanel/LeftPanel";
import { PreviewPanel } from "./components/preview/PreviewPanel";
import { InspectorPanel } from "./components/inspector/InspectorPanel";
import { TimelineSection } from "./components/timeline/TimelineSection";
import { ExportDialog } from "./components/dialogs/ExportDialog";
import { Toasts } from "./components/Toasts";
import { WelcomeScreen } from "./components/WelcomeScreen";
import { ContextMenu } from "./components/ContextMenu";
import { connectStoreToBackendEvents, useEditorStore } from "./store/editorStore";
import * as ipc from "./lib/ipc";
import { importFromPath } from "./lib/projectFlows";
import { handleGlobalKeyDown } from "./lib/actions";

export function App() {
  const [exportOpen, setExportOpen] = useState(false);
  const projectPath = useEditorStore((s) => s.projectPath);

  useEffect(() => connectStoreToBackendEvents(), []);

  // Root-cause fix for thumbnails/waveforms intermittently missing on reopen: generation
  // on the backend is push-only (`media:thumbnails-ready` / `media:waveform-ready`), and
  // those events fire the moment `open_project` resolves — which can race the frontend's
  // (async) event-listener registration, or simply land while nothing was mounted to
  // receive them yet. There was no pull-side fallback anywhere, so a missed event meant
  // that media item never got a filmstrip/waveform until something incidentally
  // re-triggered generation. This pulls once per project load via the already-cheap
  // `get_media_assets` (a single cached-file read on the backend, no regeneration) for
  // every media item the store doesn't already have an entry for, closing that gap
  // regardless of which left-panel tab is mounted.
  useEffect(() => {
    if (!projectPath) return;
    let cancelled = false;
    void (async () => {
      const items = useEditorStore.getState().project?.media ?? [];
      for (const item of items) {
        if (cancelled) return;
        // A nested sequence has no file to thumbnail or waveform.
        if (item.kind === "sequence") continue;
        if (useEditorStore.getState().mediaAssets[item.id]) continue;
        try {
          const assets = await ipc.getMediaAssets(item.id);
          if (cancelled) return;
          if (assets.thumbnails) useEditorStore.getState().onThumbnailsReady(assets.thumbnails);
          if (assets.waveform) useEditorStore.getState().onWaveformReady(assets.waveform);
        } catch (e) {
          console.warn("get media assets:", item.id, e);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [projectPath]);

  useEffect(() => {
    const unsubDrop = ipc.onDragDrop((paths) => {
      const path = paths.find((p) =>
        /\.(mp4|mov|mkv|webm|avi|png|jpe?g|webp|gif|bmp|mp3|wav|m4a|aac|flac|ogg)$/i.test(p),
      );
      if (path) void importFromPath(path);
    });
    const unsubEnter = ipc.onDragEnter(() => {
      document.querySelector(".drop-zone")?.classList.add("drag-over");
    });
    const unsubLeave = ipc.onDragLeave(() => {
      document.querySelector(".drop-zone")?.classList.remove("drag-over");
    });
    return () => {
      unsubDrop();
      unsubEnter();
      unsubLeave();
    };
  }, []);

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      handleGlobalKeyDown(e, { openExport: () => setExportOpen(true) });
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  return (
    <>
      <WelcomeScreen />
      <TopBar onExport={() => setExportOpen(true)} />
      <div className="workspace">
        <LeftPanel />
        <PreviewPanel />
        <InspectorPanel />
      </div>
      <TimelineSection />
      <ExportDialog open={exportOpen} onClose={() => setExportOpen(false)} />
      <ContextMenu />
      <Toasts />
    </>
  );
}
