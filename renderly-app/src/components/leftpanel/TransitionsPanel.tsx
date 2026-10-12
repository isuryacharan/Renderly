import { useEffect, useState } from "react";
import { Blend } from "lucide-react";
import { setClipTransition } from "../../lib/commands";
import { startTransitionDrag } from "../../lib/dragTransition";
import * as ipc from "../../lib/ipc";
import type { ClipTransition, MediaClip, TransitionKind } from "../../lib/types";
import { clipDurationSecs, TRANSITION_KINDS } from "../../lib/types";
import { activeTracks } from "../../lib/container";
import { useEditorStore } from "../../store/editorStore";

export function TransitionsPanel() {
  const project = useEditorStore((s) => s.project);
  const selection = useEditorStore((s) => s.selection);
  const dispatch = useEditorStore((s) => s.dispatch);
  const [packAliases, setPackAliases] = useState<
    { kind: TransitionKind; label: string; duration: number }[]
  >([]);

  useEffect(() => {
    if (!project) {
      setPackAliases([]);
      return;
    }
    void ipc
      .listExtensions()
      .then((c) => {
        const aliases: { kind: TransitionKind; label: string; duration: number }[] = [];
        for (const p of c.packs) {
          for (const t of p.transitions) {
            if (TRANSITION_KINDS.some((k) => k.id === t.kind)) {
              aliases.push({
                kind: t.kind as TransitionKind,
                label: `${p.name}: ${t.label}`,
                duration: t.default_duration_secs,
              });
            }
          }
        }
        setPackAliases(aliases);
      })
      .catch(() => setPackAliases([]));
  }, [project, project?.asset_pack_paths?.join("|")]);

  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const track = activeTracks(project, activeSequenceId).find((t) => t.id === selection?.trackId);
  const clip = track?.clips.find((c) => c.id === selection?.clipId);
  const mediaClip = clip && clip.type === "video" ? (clip as MediaClip) : null;
  const locked = track?.locked ?? true;

  if (!track || track.kind !== "video" || !mediaClip) {
    return (
      <div className="panel-body">
        <h3>Transitions</h3>
        <p className="empty-hint">Select a video clip to set its outgoing transition.</p>
      </div>
    );
  }

  const next = [...track.clips]
    .filter((c) => c.type === "video")
    .sort((a, b) => a.position_secs - b.position_secs)
    .find((c) => c.position_secs >= mediaClip.position_secs + clipDurationSecs(mediaClip) - 1e-6);

  const current = mediaClip.outgoing_transition ?? null;
  const maxDur = next
    ? Math.min(clipDurationSecs(mediaClip), clipDurationSecs(next as MediaClip)) / 2
    : clipDurationSecs(mediaClip) / 2;

  async function apply(transition: ClipTransition | null) {
    await dispatch(setClipTransition(track!.id, mediaClip!.id, transition));
  }

  function pick(kind: TransitionKind, defaultDur?: number) {
    void apply({
      kind,
      duration_secs:
        current?.duration_secs ?? Math.min(defaultDur ?? 0.5, Math.max(0.05, maxDur)),
    });
  }

  return (
    <div className="panel-body transitions-panel">
      <h3>Transitions</h3>
      <p className="empty-hint">Outgoing blend into the next clip on this track. Drag onto a junction on the timeline.</p>
      {!next ? (
        <p className="empty-hint">No following clip — add one after this clip.</p>
      ) : (
        <>
          <div className="effects-catalog">
            {TRANSITION_KINDS.map((t) => (
              <button
                key={t.id}
                type="button"
                className={current?.kind === t.id ? "active" : undefined}
                disabled={locked}
                draggable={!locked}
                onDragStart={(e) =>
                  startTransitionDrag(e, t.id, Math.min(0.5, Math.max(0.05, maxDur)))
                }
                onClick={() => pick(t.id)}
              >
                <Blend size={14} strokeWidth={1.75} />
                {t.label}
              </button>
            ))}
            {packAliases.map((t, i) => (
              <button
                key={`pack-${i}-${t.kind}`}
                type="button"
                className={current?.kind === t.kind ? "active" : undefined}
                disabled={locked}
                draggable={!locked}
                onDragStart={(e) =>
                  startTransitionDrag(e, t.kind, Math.min(t.duration, Math.max(0.05, maxDur)))
                }
                onClick={() => pick(t.kind, t.duration)}
              >
                <Blend size={14} strokeWidth={1.75} />
                {t.label}
              </button>
            ))}
          </div>
          {current && (
            <div className="field" style={{ marginTop: "0.75rem" }}>
              <label>Duration (seconds)</label>
              <input
                type="number"
                step="0.05"
                min={0.05}
                max={maxDur}
                value={current.duration_secs}
                disabled={locked}
                onChange={(e) => {
                  const duration_secs = Math.min(
                    maxDur,
                    Math.max(0.05, parseFloat(e.target.value) || 0.05),
                  );
                  void apply({ kind: current.kind, duration_secs });
                }}
              />
              <button
                type="button"
                className="btn-ghost"
                disabled={locked}
                onClick={() => void apply(null)}
              >
                Clear
              </button>
            </div>
          )}
        </>
      )}
    </div>
  );
}
