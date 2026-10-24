import { useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import {
  deleteClip,
  setCaption,
  setTextStyle,
  setTrackCaptionStyle,
  splitCaptions,
  SPLIT_CAPTIONS_DEFAULTS,
} from "../../lib/commands";
import { normalizeFps, secsToUs, snapToFrameUs, usToSecs } from "../../lib/time";
import { effectiveCaptionStyle, styleFor, styleLabel } from "../../lib/text";
import type { CaptionClip, Track } from "../../lib/types";
import { CaptionStyleGallery } from "../CaptionStyleGallery";
import { Tooltip } from "../ui/Tooltip";
import { AnimationsReadOnly, BodyTextField, StyleStatusNote, WordList } from "./TextFields";

export function CaptionInspector({ track, clip }: { track: Track; clip: CaptionClip }) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const select = useEditorStore((s) => s.select);
  const toast = useEditorStore((s) => s.toast);
  const fps = normalizeFps(useEditorStore((s) => s.project?.settings.fps));
  // Timing is edited in seconds and committed as frame-snapped integer µs.
  const [position, setPosition] = useState(usToSecs(clip.start_us));
  const [duration, setDuration] = useState(usToSecs(clip.duration_us));

  useEffect(() => {
    setPosition(usToSecs(clip.start_us));
    setDuration(usToSecs(clip.duration_us));
  }, [clip.id, clip.start_us, clip.duration_us]);

  const target = [{ trackId: track.id, clipId: clip.id }];
  const trackStyle = effectiveCaptionStyle(track, { ...clip, style: null });

  return (
    <div className="inspector">
      <div className="inspector-section">
        <h3>Caption</h3>
        <BodyTextField track={track} clip={clip} label="Text" placeholder="Enter caption text…" />
        <div className="field">
          <label>Style</label>
          <CaptionStyleGallery
            value={clip.style?.ref ?? ""}
            disabled={track.locked}
            onChange={(ref) => void dispatch(setTextStyle(target, { ref }))}
            onInherit={() => void dispatch(setCaption(track.id, clip.id, { style: null }))}
          />
          {clip.style == null && (
            <p className="empty-hint">Following the track style: {styleLabel(trackStyle.ref)}.</p>
          )}
          <StyleStatusNote ownerId={clip.id} styleRef={clip.style?.ref} />
        </div>
      </div>

      <WordList track={track} clip={clip} />
      <AnimationsReadOnly animations={clip.animations} />

      <div className="inspector-section">
        <h3>Timing</h3>
        <div className="field">
          <label>Start (seconds)</label>
          <input
            type="number"
            step="0.1"
            value={position}
            disabled={track.locked}
            onChange={(e) => setPosition(parseFloat(e.target.value))}
            onBlur={() =>
              void dispatch(
                setCaption(track.id, clip.id, {
                  startUs: snapToFrameUs(fps, secsToUs(Number.isFinite(position) ? position : 0)),
                }),
              )
            }
          />
        </div>
        <div className="field">
          <label>Duration (seconds)</label>
          <input
            type="number"
            step="0.1"
            value={duration}
            disabled={track.locked}
            onChange={(e) => setDuration(parseFloat(e.target.value))}
            onBlur={() =>
              void dispatch(
                setCaption(track.id, clip.id, {
                  durationUs: Math.max(
                    1,
                    snapToFrameUs(fps, clip.start_us + secsToUs(Number.isFinite(duration) ? duration : 0)) - clip.start_us,
                  ),
                }),
              )
            }
          />
        </div>
      </div>

      <CaptionTrackSection track={track} onDone={(m) => toast(m, "success")} />

      <div className="inspector-actions">
        <Tooltip content="Delete clip">
          <button
            type="button"
            className="btn-danger"
            disabled={track.locked}
            onClick={async () => {
              await dispatch(deleteClip(track.id, clip.id, false));
              select(null);
            }}
          >
            <Trash2 size={14} strokeWidth={1.75} className="btn-lucide" />
          </button>
        </Tooltip>
      </div>
    </div>
  );
}

/** Settings of the whole caption track: the style captions inherit, and re-splitting the
 *  word-timed captions into pages. */
export function CaptionTrackSection({
  track,
  onDone,
  open = false,
}: {
  track: Track;
  onDone?: (message: string) => void;
  open?: boolean;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const [maxChars, setMaxChars] = useState(SPLIT_CAPTIONS_DEFAULTS.maxChars);
  const [maxLines, setMaxLines] = useState(SPLIT_CAPTIONS_DEFAULTS.maxLines);
  const [maxSecs, setMaxSecs] = useState(usToSecs(SPLIT_CAPTIONS_DEFAULTS.maxDurationUs));
  const [restyleAll, setRestyleAll] = useState(false);
  const hasWords = track.clips.some((c) => c.type === "caption" && c.body.words.length > 0);

  return (
    <details className="insp-disclosure" open={open}>
      <summary>Caption track · {track.name}</summary>
      <div className="inspector-section insp-disclosure-body">
        <div className="field">
          <label>Track style</label>
          <CaptionStyleGallery
            label="Track caption styles"
            value={track.caption_style?.ref ?? ""}
            disabled={track.locked}
            onChange={async (ref) => {
              const ok = await dispatch(setTrackCaptionStyle(track.id, styleFor(ref), restyleAll));
              if (ok) onDone?.(restyleAll ? "Every caption restyled" : "Track style set");
            }}
            onInherit={() => void dispatch(setTrackCaptionStyle(track.id, null, restyleAll))}
          />
          <StyleStatusNote ownerId={track.id} styleRef={track.caption_style?.ref} />
          <label className="check-row">
            <input
              type="checkbox"
              checked={restyleAll}
              disabled={track.locked}
              onChange={(e) => setRestyleAll(e.target.checked)}
            />
            Also restyle captions that have their own style
          </label>
        </div>

        <div className="field">
          <label>Re-split captions</label>
          <div className="insp-grid-3">
            <label className="insp-cell">
              <span>Chars / line</span>
              <input
                type="number"
                min={1}
                value={maxChars}
                onChange={(e) => setMaxChars(Math.max(1, parseInt(e.target.value, 10) || 1))}
              />
            </label>
            <label className="insp-cell">
              <span>Lines</span>
              <input
                type="number"
                min={1}
                value={maxLines}
                onChange={(e) => setMaxLines(Math.max(1, parseInt(e.target.value, 10) || 1))}
              />
            </label>
            <label className="insp-cell">
              <span>Max secs</span>
              <input
                type="number"
                min={0.5}
                step={0.5}
                value={maxSecs}
                onChange={(e) => setMaxSecs(Math.max(0.1, parseFloat(e.target.value) || 0.1))}
              />
            </label>
          </div>
          <button
            type="button"
            className="btn"
            disabled={track.locked || !hasWords}
            onClick={async () => {
              const ok = await dispatch(
                splitCaptions(track.id, {
                  maxChars,
                  maxLines,
                  maxDurationUs: secsToUs(maxSecs),
                }),
              );
              if (ok) onDone?.("Captions re-split");
            }}
          >
            Re-split captions
          </button>
          <p className="empty-hint">
            {hasWords
              ? "Rebuilds the speech captions into pages with these limits. Typed captions stay as they are."
              : "Needs captions made from speech (they carry word timings)."}
          </p>
        </div>
      </div>
    </details>
  );
}
