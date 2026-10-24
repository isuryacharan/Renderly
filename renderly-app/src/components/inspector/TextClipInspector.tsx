import { useEffect, useState } from "react";
import { Link2, Trash2 } from "lucide-react";
import { useEditorStore } from "../../store/editorStore";
import { deleteClip, setClipEnabled, setClipTransform, setTextStyle, setTextTiming } from "../../lib/commands";
import { normalizeFps, secsToUs, snapToFrameUs, usToSecs } from "../../lib/time";
import { IDENTITY_TRANSFORM, type ClipTransform, type TextClip, type Track } from "../../lib/types";
import { CaptionStyleGallery } from "../CaptionStyleGallery";
import { Tooltip } from "../ui/Tooltip";
import { BlendModeControl } from "./BlendModeControl";
import { EffectStackSection } from "./EffectStackSection";
import { KeyframeEditor } from "./KeyframeEditor";
import { MaskSection } from "./MaskSection";
import { NumCell } from "./MediaClipInspector";
import { AnimationsReadOnly, BodyTextField, StyleStatusNote } from "./TextFields";
import { TransitionSection } from "./TransitionSection";

function clipTransform(clip: TextClip): ClipTransform {
  return clip.transform ?? IDENTITY_TRANSFORM;
}

/// Inspector of a title: its text and style, then the same layer controls a video clip has
/// (transform, blend mode, keyframes, effect slots, transition, mask).
export function TextClipInspector({ track, clip }: { track: Track; clip: TextClip }) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const select = useEditorStore((s) => s.select);
  const setPreviewOverride = useEditorStore((s) => s.setPreviewOverride);
  const clearPreviewOverride = useEditorStore((s) => s.clearPreviewOverride);
  const endGesture = useEditorStore((s) => s.endGesture);
  const fps = normalizeFps(useEditorStore((s) => s.project?.settings.fps));
  const [transform, setTransform] = useState<ClipTransform>(() => clipTransform(clip));
  const [linkScale, setLinkScale] = useState(true);
  const [position, setPosition] = useState(usToSecs(clip.start_us));
  const [duration, setDuration] = useState(usToSecs(clip.duration_us));
  const target = [{ trackId: track.id, clipId: clip.id }];

  useEffect(() => {
    setTransform(clipTransform(clip));
    setPosition(usToSecs(clip.start_us));
    setDuration(usToSecs(clip.duration_us));
  }, [clip.id, clip.transform, clip.start_us, clip.duration_us]);

  /** Live edit: show the transform on the preview as a render-only override (no command). */
  function previewTransform(next: ClipTransform) {
    setTransform(next);
    setPreviewOverride(clip.id, { transform: next });
  }

  /** Commit one edit as a single undo step. */
  async function commitTransform(next: ClipTransform) {
    setTransform(next);
    if (JSON.stringify(next) !== JSON.stringify(clipTransform(clip))) {
      await dispatch(setClipTransform(track.id, clip.id, next), false, {
        mergeKey: `transform:${clip.id}`,
      });
    }
    clearPreviewOverride(clip.id);
    endGesture();
  }

  return (
    <div className="inspector">
      <div className="inspector-section">
        <div className="insp-clip-head">
          <div>
            <h3>Title</h3>
            <p className="insp-meta">{track.name}</p>
          </div>
          <label className="insp-enable">
            <input
              type="checkbox"
              checked={clip.enabled}
              disabled={track.locked}
              onChange={(e) => void dispatch(setClipEnabled(track.id, clip.id, e.target.checked))}
            />
            <span>On</span>
          </label>
        </div>
        <BodyTextField track={track} clip={clip} label="Text" placeholder="Enter title text…" />
        <div className="field">
          <label>Style</label>
          <CaptionStyleGallery
            label="Title styles"
            value={clip.style.ref}
            disabled={track.locked}
            onChange={(ref) => void dispatch(setTextStyle(target, { ref }))}
          />
          <StyleStatusNote ownerId={clip.id} styleRef={clip.style.ref} />
        </div>
      </div>

      <div className="inspector-section">
        <h3>Timing</h3>
        <div className="insp-grid-2">
          <NumCell
            label="Start (s)"
            value={position}
            step={0.1}
            min={0}
            disabled={track.locked}
            onChange={setPosition}
            onCommit={(v) =>
              void dispatch(
                setTextTiming(track.id, clip.id, { startUs: snapToFrameUs(fps, secsToUs(Math.max(0, v))) }),
              )
            }
          />
          <NumCell
            label="Duration (s)"
            value={duration}
            step={0.1}
            min={0.1}
            disabled={track.locked}
            onChange={setDuration}
            onCommit={(v) =>
              void dispatch(
                setTextTiming(track.id, clip.id, {
                  durationUs: Math.max(
                    1,
                    snapToFrameUs(fps, clip.start_us + secsToUs(Math.max(0.1, v))) - clip.start_us,
                  ),
                }),
              )
            }
          />
        </div>
      </div>

      <div className="inspector-section">
        <h3>Transform</h3>
        <div className="insp-grid-2">
          <NumCell
            label="X"
            value={transform.x}
            disabled={track.locked}
            onChange={(x) => previewTransform({ ...transform, x })}
            onCommit={(x) => void commitTransform({ ...transform, x })}
          />
          <NumCell
            label="Y"
            value={transform.y}
            disabled={track.locked}
            onChange={(y) => previewTransform({ ...transform, y })}
            onCommit={(y) => void commitTransform({ ...transform, y })}
          />
        </div>
        <div className="insp-scale-row">
          <NumCell
            label="Scale X"
            value={transform.scale_x}
            min={0.01}
            disabled={track.locked}
            onChange={(scale_x) =>
              previewTransform(linkScale ? { ...transform, scale_x, scale_y: scale_x } : { ...transform, scale_x })
            }
            onCommit={(scale_x) =>
              void commitTransform(linkScale ? { ...transform, scale_x, scale_y: scale_x } : { ...transform, scale_x })
            }
          />
          <Tooltip content={linkScale ? "Unlink scales" : "Link scales"}>
            <button
              type="button"
              className={`insp-link-btn${linkScale ? " active" : ""}`}
              disabled={track.locked}
              onClick={() => setLinkScale((v) => !v)}
            >
              <Link2 size={14} strokeWidth={1.75} />
            </button>
          </Tooltip>
          <NumCell
            label="Scale Y"
            value={transform.scale_y}
            min={0.01}
            disabled={track.locked}
            onChange={(scale_y) =>
              previewTransform(linkScale ? { ...transform, scale_x: scale_y, scale_y } : { ...transform, scale_y })
            }
            onCommit={(scale_y) =>
              void commitTransform(linkScale ? { ...transform, scale_x: scale_y, scale_y } : { ...transform, scale_y })
            }
          />
        </div>
        <div className="insp-grid-2">
          <NumCell
            label="Rotate °"
            value={transform.rotation_deg}
            step={1}
            disabled={track.locked}
            onChange={(rotation_deg) => previewTransform({ ...transform, rotation_deg })}
            onCommit={(rotation_deg) => void commitTransform({ ...transform, rotation_deg })}
          />
          <label className="insp-cell">
            <span>Opacity · {Math.round(transform.opacity * 100)}%</span>
            <input
              type="range"
              min={0}
              max={1}
              step={0.01}
              value={transform.opacity}
              disabled={track.locked}
              onChange={(e) => previewTransform({ ...transform, opacity: parseFloat(e.target.value) })}
              onMouseUp={(e) =>
                void commitTransform({ ...transform, opacity: parseFloat((e.target as HTMLInputElement).value) })
              }
              onTouchEnd={(e) =>
                void commitTransform({ ...transform, opacity: parseFloat((e.target as HTMLInputElement).value) })
              }
            />
          </label>
        </div>
        <BlendModeControl track={track} clip={clip} />
      </div>

      <AnimationsReadOnly animations={clip.animations} />

      <details className="insp-disclosure" open>
        <summary>Keyframes</summary>
        <KeyframeEditor track={track} clip={clip} nested />
      </details>

      <EffectStackSection track={track} clip={clip} />

      <details className="insp-disclosure" open={!!clip.outgoing_transition}>
        <summary>Transition{clip.outgoing_transition ? " · on" : ""}</summary>
        <div className="inspector-section insp-disclosure-body">
          <TransitionSection track={track} clip={clip} />
        </div>
      </details>

      <MaskSection
        track={track}
        clip={clip}
        hint="The mask limits where the title shows; its position is relative to the title."
      />

      <div className="inspector-actions">
        <Tooltip content="Delete title">
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
