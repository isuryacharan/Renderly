import { useCallback, useEffect, useState } from "react";
import { createPortal } from "react-dom";
import { useEditorStore } from "../../store/editorStore";
import { generateBackgroundMatte, setClipBackgroundRemoval } from "../../lib/commands";
import * as ipc from "../../lib/ipc";
import type { MatteStatus } from "../../lib/ipc";
import {
  cancelMatteJob,
  matteProgressPercent,
  matteStatusLabel,
  startMatteJob,
  useMatteJob,
} from "../../lib/matteJobs";
import type { BackgroundRemoval, MediaClip, Track } from "../../lib/types";
import { MenuSelect } from "../ui/MenuSelect";
import { ModelConsentDialog } from "../dialogs/ModelConsentDialog";

/** Manifest id of the weights behind the `rvm` method. */
const RVM_MANIFEST_ID = "rvm-mobilenetv3-fp32";

const FAST = "heuristic";
const AI = "rvm";

const DEFAULTS: BackgroundRemoval = {
  enabled: true,
  model_id: FAST,
  threshold: 0.15,
  feather: 0.1,
  temporal: false,
};

function methodOptions(current: string) {
  const options = [
    { value: FAST, label: "Fast (heuristic)" },
    { value: AI, label: "AI (Robust Video Matting)" },
  ];
  // A project may carry an external-CLI model id; keep it selectable rather than hiding it.
  if (!options.some((o) => o.value === current)) {
    options.push({ value: current, label: `External (${current})` });
  }
  return options;
}

/// Background removal controls for a media clip: method (fast heuristic or AI matting), look
/// (feather / threshold), and the matte lifecycle (generate, progress, cancel, staleness).
export function BackgroundRemovalSection({ track, clip }: { track: Track; clip: MediaClip }) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const bg = clip.background_removal ?? null;
  const enabled = !!bg && bg.enabled !== false;
  const model = bg?.model_id ?? FAST;
  const isAi = model === AI;
  const job = useMatteJob();
  const mine = job && job.clipId === clip.id ? job : null;
  const otherRunning = job !== null && mine === null;

  const [status, setStatus] = useState<MatteStatus | null>(null);
  const [consent, setConsent] = useState<null | { then: () => void }>(null);
  const [feather, setFeather] = useState(bg?.feather ?? 0);
  const [threshold, setThreshold] = useState(bg?.threshold ?? 0.15);
  useEffect(() => setFeather(bg?.feather ?? 0), [bg?.feather]);
  useEffect(() => setThreshold(bg?.threshold ?? 0.15), [bg?.threshold]);

  // Matte state: refetched when anything that decides it changes (config, the clip's source
  // range) and when a bake ends.
  const dir = bg?.matte_cache_dir ?? null;
  const stamp = bg?.matte_frames?.stamp ?? 0;
  const running = mine !== null;
  useEffect(() => {
    if (!enabled) {
      setStatus(null);
      return;
    }
    let live = true;
    ipc
      .backgroundMatteStatus(track.id, clip.id)
      .then((s) => live && setStatus(s))
      .catch(() => live && setStatus(null));
    return () => {
      live = false;
    };
  }, [enabled, track.id, clip.id, model, dir, stamp, clip.source_in_secs, clip.source_out_secs, running]);

  const write = useCallback(
    (patch: Partial<BackgroundRemoval>) =>
      dispatch(
        setClipBackgroundRemoval(track.id, clip.id, {
          ...DEFAULTS,
          ...(bg ?? {}),
          ...patch,
        }),
      ),
    [dispatch, track.id, clip.id, bg],
  );

  /** Run `go`, asking for the model download first when the AI weights are missing. */
  async function withModel(go: () => void) {
    try {
      const s = await ipc.mlModelStatus(RVM_MANIFEST_ID);
      if (!s.installed) {
        setConsent({ then: go });
        return;
      }
    } catch {
      // Status unavailable (browser harness): let the command report a missing model itself.
    }
    go();
  }

  function changeMethod(next: string) {
    const apply = () =>
      // A different method invalidates the baked matte: drop the cache reference.
      void write({ model_id: next, matte_cache_dir: null, matte_frames: null });
    if (next === AI) void withModel(apply);
    else apply();
  }

  function generate() {
    if (isAi) {
      void withModel(() => void startMatteJob(track.id, clip.id));
    } else {
      void dispatch(generateBackgroundMatte(track.id, clip.id));
    }
  }

  const locked = track.locked;
  const hasMatte = status !== null && status.state !== "not_generated";
  const pct = matteProgressPercent(mine);

  return (
    <>
      {!enabled && (
        <button
          type="button"
          disabled={locked}
          onClick={() => void write({ ...DEFAULTS, enabled: true })}
        >
          Enable background removal
        </button>
      )}

      {enabled && (
        <>
          <label className="insp-cell">
            <span>Method</span>
            <MenuSelect
              value={model}
              disabled={locked || running}
              options={methodOptions(model)}
              onChange={changeMethod}
            />
          </label>
          <label className="insp-cell">
            <span>Edge softness · {Math.round(feather * 100)}%</span>
            <input
              type="range"
              min={0}
              max={1}
              step={0.05}
              value={feather}
              disabled={locked}
              onChange={(e) => setFeather(parseFloat(e.target.value))}
              onMouseUp={() => void write({ feather })}
              onTouchEnd={() => void write({ feather })}
              onKeyUp={() => void write({ feather })}
            />
          </label>
          {!isAi && (
            <label className="insp-cell">
              <span>Brightness cut-off · {Math.round(threshold * 100)}%</span>
              <input
                type="range"
                min={0}
                max={1}
                step={0.05}
                value={threshold}
                disabled={locked}
                onChange={(e) => setThreshold(parseFloat(e.target.value))}
                onMouseUp={() => void write({ threshold })}
                onTouchEnd={() => void write({ threshold })}
                onKeyUp={() => void write({ threshold })}
              />
            </label>
          )}

          <p
            className={`matte-status${status?.state === "stale" && !mine ? " stale" : ""}`}
            role="status"
          >
            Matte: {matteStatusLabel(status, mine)}
          </p>
          {mine && (
            <div
              className="export-progress"
              role="progressbar"
              aria-valuenow={pct}
              aria-valuemin={0}
              aria-valuemax={100}
            >
              <div className="export-progress-bar" style={{ width: `${pct}%` }} />
            </div>
          )}
          {isAi && !hasMatte && !mine && (
            <p className="empty-hint">
              Generate the matte once; the preview and export then cut the subject out frame by
              frame. It runs on this computer.
            </p>
          )}
          {otherRunning && <p className="empty-hint">Another matte is being generated.</p>}

          <div className="insp-btn-row">
            {!mine && (
              <button type="button" disabled={locked || otherRunning} onClick={generate}>
                {hasMatte ? "Regenerate" : "Generate"}
              </button>
            )}
            {mine && (
              <button type="button" onClick={() => void cancelMatteJob()}>
                Cancel
              </button>
            )}
            <button
              type="button"
              disabled={locked || running}
              onClick={() => void dispatch(setClipBackgroundRemoval(track.id, clip.id, null))}
            >
              Remove effect
            </button>
          </div>
        </>
      )}

      {/* Portaled: inspector section styles (e.g. hidden h3) must not reach the dialog. */}
      {createPortal(
        <ModelConsentDialog
          open={consent !== null}
          modelId={RVM_MANIFEST_ID}
          onInstalled={() => {
            const next = consent;
            setConsent(null);
            next?.then();
          }}
          onClose={() => setConsent(null)}
        />,
        document.body,
      )}
    </>
  );
}
