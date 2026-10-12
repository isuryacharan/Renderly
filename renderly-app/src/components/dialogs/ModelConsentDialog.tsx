import { useEffect, useRef, useState } from "react";
import * as ipc from "../../lib/ipc";
import type { ModelStatus } from "../../lib/ipc";
import { formatModelSize } from "../../lib/matteJobs";

type Stage = "loading" | "confirm" | "downloading" | "error";

/// Asks for consent before fetching an ML model's weights (they are never bundled or fetched
/// implicitly): shows the model, size, upstream host, licence and where it is stored, then
/// downloads with a progress bar. Calls `onInstalled` once the verified file is in place.
export function ModelConsentDialog({
  open,
  modelId,
  onInstalled,
  onClose,
}: {
  open: boolean;
  modelId: string;
  onInstalled: () => void;
  onClose: () => void;
}) {
  const dialogRef = useRef<HTMLDialogElement | null>(null);
  const [status, setStatus] = useState<ModelStatus | null>(null);
  const [stage, setStage] = useState<Stage>("loading");
  const [done, setDone] = useState(0);
  const [total, setTotal] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    if (open && !dialog.open) {
      dialog.showModal();
      setStage("loading");
      setError(null);
      setDone(0);
      setTotal(null);
      let cancelled = false;
      ipc
        .mlModelStatus(modelId)
        .then((s) => {
          if (cancelled) return;
          setStatus(s);
          if (s.installed) {
            // Installed since (or via RENDERLY_MODEL_DIR): nothing to ask.
            onInstalled();
          } else {
            setStage("confirm");
          }
        })
        .catch((e) => {
          if (cancelled) return;
          setError(e instanceof Error ? e.message : String(e));
          setStage("error");
        });
      return () => {
        cancelled = true;
      };
    }
    if (!open && dialog.open) dialog.close();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- onInstalled is intentionally not a trigger
  }, [open, modelId]);

  useEffect(() => {
    if (stage !== "downloading") return;
    return ipc.onMlDownloadProgress((p) => {
      if (p.id !== modelId) return;
      setDone(p.done);
      setTotal(p.total);
    });
  }, [stage, modelId]);

  async function startDownload() {
    setStage("downloading");
    setDone(0);
    setError(null);
    try {
      await ipc.mlDownloadModel(modelId);
      onInstalled();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setStage("error");
    }
  }

  const pct = total && total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0;
  const checksumFailure = (error ?? "").toLowerCase().includes("checksum");

  return (
    <dialog
      ref={dialogRef}
      onClose={() => {
        if (stage !== "downloading") onClose();
      }}
      onCancel={(e) => {
        if (stage === "downloading") e.preventDefault();
      }}
    >
      <h3>Download AI model</h3>

      {stage === "loading" && <p className="dialog-sub">Checking…</p>}

      {(stage === "confirm" || stage === "downloading") && status && (
        <>
          <p className="dialog-sub">
            AI background removal needs the <strong>{status.name}</strong> model, which is not
            included with Renderly.
          </p>
          <ul className="model-consent-facts">
            <li>
              <strong>Size:</strong> {formatModelSize(status.size_bytes)}
            </li>
            <li>
              <strong>Downloaded from:</strong> {status.download_host ?? "the upstream project"}{" "}
              (the project&apos;s own release page, checked against a fixed SHA-256 checksum)
            </li>
            <li>
              <strong>Licence:</strong>{" "}
              <a href={status.license_url} target="_blank" rel="noreferrer">
                {status.license_spdx}
              </a>{" "}
              — the model is provided by its authors under this licence, not by Renderly
            </li>
            <li>
              <strong>Stored locally in:</strong> <code>{status.cache_dir}</code>
            </li>
          </ul>
          <p className="dialog-sub">
            Nothing is downloaded unless you press Download. Matting then runs entirely on this
            computer.
          </p>
        </>
      )}

      {stage === "downloading" && (
        <>
          <div
            className="export-progress"
            role="progressbar"
            aria-valuenow={pct}
            aria-valuemin={0}
            aria-valuemax={100}
          >
            <div className="export-progress-bar" style={{ width: `${pct}%` }} />
          </div>
          <div className="export-progress-meta">
            <span>{pct}%</span>
            <span>
              {total ? `${(done / 1_000_000).toFixed(1)} / ${(total / 1_000_000).toFixed(1)} MB` : ""}
            </span>
          </div>
        </>
      )}

      {stage === "error" && (
        <p className="dialog-sub" role="alert">
          {checksumFailure
            ? "The downloaded file did not match the expected checksum (SHA-256), so it was discarded and nothing was installed. Try again; if it keeps failing, the upstream file may have changed."
            : `Download failed: ${error ?? "unknown error"}`}
        </p>
      )}

      <menu>
        <button
          type="button"
          className="btn"
          disabled={stage === "downloading"}
          onClick={() => {
            dialogRef.current?.close();
          }}
        >
          Cancel
        </button>
        {(stage === "confirm" || stage === "error") && status && (
          <button type="button" className="btn-primary" onClick={() => void startDownload()}>
            {stage === "error" ? "Try again" : "Download"}
          </button>
        )}
      </menu>
    </dialog>
  );
}
