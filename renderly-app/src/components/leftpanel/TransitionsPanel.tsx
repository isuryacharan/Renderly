import { Blend } from "lucide-react";
import { setClipTransition } from "../../lib/commands";
import { startTransitionDrag } from "../../lib/dragTransition";
import { refBase } from "../../lib/effects";
import {
  DEFAULT_TRANSITION_US,
  MIN_TRANSITION_US,
  maxTransitionUs,
  nextVideoClip,
  transitionDurationFor,
  transitionEntries,
} from "../../lib/transitions";
import { useCatalogue } from "../../lib/useCatalogue";
import { useSelectedClip } from "../../lib/useSelectedClip";
import { TransitionSection } from "../inspector/TransitionSection";
import { useEditorStore } from "../../store/editorStore";

/// Transitions tab: the catalogue's transitions (builtin and pack) as a gallery. Click applies
/// one to the selected clip's outgoing junction, drag drops it on any junction on the timeline,
/// hover previews it (a preview override, no command). Duration, easing and parameters of the
/// applied transition are edited in the same controls the inspector uses.
export function TransitionsPanel() {
  const catalogue = useCatalogue();
  const selected = useSelectedClip();
  const dispatch = useEditorStore((s) => s.dispatch);
  const previewTransition = useEditorStore((s) => s.previewTransitionOnSelection);
  const clearEffectPreview = useEditorStore((s) => s.clearEffectPreview);

  const track = selected?.track;
  const clip = selected?.clip;
  if (!track || track.kind !== "video" || !clip || (clip.type !== "video" && clip.type !== "text")) {
    return (
      <div className="panel-body">
        <h3>Transitions</h3>
        <p className="empty-hint">Select a video clip or title to set its outgoing transition.</p>
      </div>
    );
  }

  const next = nextVideoClip(track, clip);
  const current = clip.outgoing_transition ?? null;
  const locked = track.locked;
  const maxDur = maxTransitionUs(track, clip);
  const dragDur = Math.round(Math.min(DEFAULT_TRANSITION_US, Math.max(MIN_TRANSITION_US, maxDur)));

  return (
    <div className="panel-body transitions-panel">
      <h3>Transitions</h3>
      <p className="empty-hint">
        Outgoing blend into the next clip on this track. Hover to preview, or drag onto a junction
        on the timeline.
      </p>
      {!next ? (
        <p className="empty-hint">No following clip — add one after this clip.</p>
      ) : (
        <>
          <div className="effects-catalog">
            {transitionEntries(catalogue).map((t) => (
              <button
                key={t.ref}
                type="button"
                className={current && refBase(current.ref) === refBase(t.ref) ? "active" : undefined}
                disabled={locked}
                draggable={!locked}
                onDragStart={(e) => startTransitionDrag(e, t.ref, dragDur)}
                onMouseEnter={() => previewTransition(t, transitionDurationFor(track, clip))}
                onMouseLeave={clearEffectPreview}
                onFocus={() => previewTransition(t, transitionDurationFor(track, clip))}
                onBlur={clearEffectPreview}
                onClick={() => {
                  clearEffectPreview();
                  void dispatch(
                    setClipTransition(track.id, clip.id, t.ref, {
                      durationUs: transitionDurationFor(track, clip),
                    }),
                  );
                }}
              >
                <Blend size={14} strokeWidth={1.75} />
                {t.label}
              </button>
            ))}
          </div>
          {current && (
            <div style={{ marginTop: "0.75rem" }}>
              <TransitionSection track={track} clip={clip} />
            </div>
          )}
        </>
      )}
    </div>
  );
}
