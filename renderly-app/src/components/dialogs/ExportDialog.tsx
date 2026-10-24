import { useEffect, useRef, useState } from "react";
import * as ipc from "../../lib/ipc";
import type { ExportPhase, ExportPresetArg } from "../../lib/ipc";
import { fileName } from "../../lib/format";
import { formatFps, fpsToNumber } from "../../lib/time";
import { useEditorStore } from "../../store/editorStore";

type PresetId = "tiktok" | "youtube" | "project";
type Stage = "choose" | "exporting" | "done" | "cancelled" | "error";
type VideoEncoderId = "auto" | "software" | "nvenc" | "qsv";

function phaseLabel(phase: ExportPhase): string {
  switch (phase) {
    case "video":
      return "Rendering video";
    case "audio":
      return "Mixing audio";
    case "mux":
      return "Muxing";
  }
}

function formatEta(elapsedMs: number, fraction: number): string {
  if (fraction < 0.02 || !Number.isFinite(fraction)) return "Estimating…";
  const remainingMs = (elapsedMs / fraction) * (1 - fraction);
  const secs = Math.max(0, Math.round(remainingMs / 1000));
  if (secs < 60) return `~${secs}s left`;
  const m = Math.floor(secs / 60);
  const s = secs % 60;
  return `~${m}m ${s}s left`;
}

function presetArg(id: PresetId, width: number, height: number, fps: number): ExportPresetArg {
  if (id === "tiktok") return "tiktok";
  if (id === "youtube") return "youtube";
  return { custom: { width, height, fps } };
}

export function ExportDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  const project = useEditorStore((s) => s.project);
  const toast = useEditorStore((s) => s.toast);

  const [preset, setPreset] = useState<PresetId>("tiktok");
  const [videoEncoder, setVideoEncoder] = useState<VideoEncoderId>("auto");
  const [crf, setCrf] = useState(18);
  const [audioBitrateK, setAudioBitrateK] = useState(192);
  const [stage, setStage] = useState<Stage>("choose");
  const [fraction, setFraction] = useState(0);
  const [phase, setPhase] = useState<ExportPhase>("video");
  const [frame, setFrame] = useState(0);
  const [totalFrames, setTotalFrames] = useState(0);
  const [startedAt, setStartedAt] = useState(0);
  const [now, setNow] = useState(0);
  const [outputPath, setOutputPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) {
      dialog.showModal();
      setStage("choose");
      setFraction(0);
      setError(null);
      setOutputPath(null);
    }
    if (!open && dialog.open) dialog.close();
  }, [open]);

  useEffect(() => {
    if (stage !== "exporting") return;
    const id = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(id);
  }, [stage]);

  useEffect(() => {
    if (stage !== "exporting") return;
    return ipc.onExportProgress((p) => {
      setPhase(p.phase);
      setFrame(p.frame);
      setTotalFrames(p.total_frames);
      setFraction(p.fraction);
    });
  }, [stage]);

  async function startExport() {
    if (!project) return;
    const path = await ipc.pickExportSavePath(`${project.name}.mp4`);
    if (!path) return;

    setOutputPath(path);
    setStage("exporting");
    setFraction(0);
    setPhase("video");
    setFrame(0);
    setTotalFrames(0);
    setError(null);
    const t0 = Date.now();
    setStartedAt(t0);
    setNow(t0);

    try {
      await ipc.exportProject(
        path,
        presetArg(preset, project.settings.width, project.settings.height, fpsToNumber(project.settings.fps)),
        { videoEncoder, crf, audioBitrateK },
      );
      setFraction(1);
      setStage("done");
      toast(`Exported to ${fileName(path)}`, "success");
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (msg.toLowerCase().includes("cancel")) {
        setStage("cancelled");
        toast("Export cancelled", "info");
      } else {
        setError(msg);
        setStage("error");
        toast(`Export failed: ${msg}`, "error");
      }
    }
  }

  async function onCancelExport() {
    await ipc.cancelExport();
  }

  function requestClose() {
    if (stage === "exporting") return;
    onClose();
  }

  const pct = Math.round(fraction * 100);
  const eta =
    stage === "exporting" ? formatEta(Math.max(0, now - startedAt), fraction) : null;

  return (
    <dialog
      ref={dialogRef}
      onClose={requestClose}
      onCancel={(e) => {
        if (stage === "exporting") e.preventDefault();
      }}
    >
      <h3>Export video</h3>

      {stage === "choose" && (
        <>
          <p className="dialog-sub">Choose a platform preset, then pick where to save your MP4.</p>
          <div className="preset-grid preset-grid-3">
            <button
              type="button"
              className={`preset-card${preset === "tiktok" ? " selected" : ""}`}
              onClick={() => setPreset("tiktok")}
            >
              <strong>TikTok / Reels</strong>
              <span>1080×1920 · 9:16</span>
            </button>
            <button
              type="button"
              className={`preset-card${preset === "youtube" ? " selected" : ""}`}
              onClick={() => setPreset("youtube")}
            >
              <strong>YouTube</strong>
              <span>1920×1080 · 16:9</span>
            </button>
            <button
              type="button"
              className={`preset-card${preset === "project" ? " selected" : ""}`}
              onClick={() => setPreset("project")}
              disabled={!project}
            >
              <strong>Project size</strong>
              <span>
                {project
                  ? `${project.settings.width}×${project.settings.height} · ${formatFps(project.settings.fps)} fps`
                  : "No project"}
              </span>
            </button>
          </div>

          <div className="export-encode-options">
            <div className="field">
              <label htmlFor="export-encoder">Video encoder</label>
              <select
                id="export-encoder"
                value={videoEncoder}
                onChange={(e) => setVideoEncoder(e.target.value as VideoEncoderId)}
              >
                <option value="auto">Auto</option>
                <option value="software">Software</option>
                <option value="nvenc">NVENC</option>
                <option value="qsv">QSV</option>
              </select>
            </div>
            <div className="field">
              <label htmlFor="export-crf">
                Quality (CRF) · {crf}
                <span className="field-hint"> lower = better</span>
              </label>
              <input
                id="export-crf"
                type="range"
                min={14}
                max={28}
                step={1}
                value={crf}
                onChange={(e) => setCrf(Number(e.target.value))}
              />
            </div>
            <div className="field">
              <label htmlFor="export-audio-bitrate">Audio bitrate</label>
              <select
                id="export-audio-bitrate"
                value={audioBitrateK}
                onChange={(e) => setAudioBitrateK(Number(e.target.value))}
              >
                <option value={128}>128 kbps</option>
                <option value={192}>192 kbps</option>
                <option value={256}>256 kbps</option>
              </select>
            </div>
          </div>

          <menu>
            <button type="button" className="btn" onClick={onClose}>
              Close
            </button>
            <button type="button" className="btn-primary" onClick={() => void startExport()}>
              Export MP4
            </button>
          </menu>
        </>
      )}

      {stage === "exporting" && (
        <>
          <p className="dialog-sub">
            {phaseLabel(phase)}
            {totalFrames > 0 ? ` · frame ${frame}/${totalFrames}` : ""}
          </p>
          <div className="export-progress" role="progressbar" aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100}>
            <div className="export-progress-bar" style={{ width: `${pct}%` }} />
          </div>
          <div className="export-progress-meta">
            <span>{pct}%</span>
            <span>{eta}</span>
          </div>
          <menu>
            <button type="button" className="btn" onClick={() => void onCancelExport()}>
              Cancel
            </button>
          </menu>
        </>
      )}

      {stage === "done" && (
        <>
          <p className="dialog-sub">
            Saved{outputPath ? ` to ${fileName(outputPath)}` : ""}.
          </p>
          <div className="export-progress">
            <div className="export-progress-bar" style={{ width: "100%" }} />
          </div>
          <menu>
            <button type="button" className="btn-primary" onClick={onClose}>
              Done
            </button>
          </menu>
        </>
      )}

      {stage === "cancelled" && (
        <>
          <p className="dialog-sub">Export was cancelled. Temp files were cleaned up.</p>
          <menu>
            <button type="button" className="btn" onClick={() => setStage("choose")}>
              Back
            </button>
            <button type="button" className="btn-primary" onClick={onClose}>
              Close
            </button>
          </menu>
        </>
      )}

      {stage === "error" && (
        <>
          <p className="dialog-sub export-error">{error ?? "Export failed"}</p>
          <menu>
            <button type="button" className="btn" onClick={() => setStage("choose")}>
              Back
            </button>
            <button type="button" className="btn-primary" onClick={onClose}>
              Close
            </button>
          </menu>
        </>
      )}
    </dialog>
  );
}
