import { useEffect, useRef, useState } from "react";
import { Film, Upload, Play } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { selectActiveTracks } from "../../lib/container";
import { TransportBar } from "./TransportBar";
import { PreviewHandlesOverlay } from "./PreviewHandlesOverlay";
import { PreviewMaskOverlay } from "./PreviewMaskOverlay";
import { PerfHud } from "./PerfHud";
import { WebviewPreview } from "../../preview/WebviewPreview";

export function PreviewPanel() {
  const project = useEditorStore((s) => s.project);
  const importBusy = useEditorStore((s) => s.importBusy);
  const playing = useEditorStore((s) => s.playing);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [fullscreen, setFullscreen] = useState(false);

  const tracks = useEditorStore(selectActiveTracks);
  const hasClips = tracks.some((t) => t.clips.length > 0);
  const aspect = project ? project.settings.width / project.settings.height : 9 / 16;

  useEffect(() => {
    if (!fullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        setFullscreen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [fullscreen]);

  let hintContent: React.ReactNode = null;
  if (importBusy) {
    hintContent = (
      <>
        <span className="icon spinner" />
        <span>Importing…</span>
      </>
    );
  } else if (!project) {
    hintContent = (
      <>
        <span className="icon">
          <Film size={22} strokeWidth={1.5} />
        </span>
        <span>Import or open a project</span>
      </>
    );
  } else if (!hasClips) {
    hintContent = (
      <>
        <span className="icon">
          <Upload size={22} strokeWidth={1.5} />
        </span>
        <span>Drop media in the bin to begin</span>
      </>
    );
  } else if (!playing) {
    hintContent = (
      <>
        <span className="icon">
          <Play size={22} strokeWidth={1.5} />
        </span>
        <span>Space to play</span>
      </>
    );
  }

  return (
    <div className={`preview-column${fullscreen ? " preview-fullscreen" : ""}`}>
      <section
        id="preview-host"
        ref={hostRef}
        className={`preview-host${hasClips ? " has-clips" : ""}${importBusy ? " loading" : ""}${playing ? " is-playing" : ""}`}
      >
        {hintContent && <div className="hint">{hintContent}</div>}
        {hasClips && project ? <WebviewPreview hostRef={hostRef} aspect={aspect} /> : null}
        {hasClips && project ? (
          <>
            <PreviewHandlesOverlay hostRef={hostRef} aspect={aspect} />
            <PreviewMaskOverlay hostRef={hostRef} aspect={aspect} />
          </>
        ) : null}
        <PerfHud />
      </section>
      <TransportBar
        fullscreen={fullscreen}
        onToggleFullscreen={() => setFullscreen((v) => !v)}
      />
    </div>
  );
}
