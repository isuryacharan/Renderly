import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { useEditorStore } from "../store/editorStore";
import { activeTracks, isNestedClip, sequenceOfMedia } from "../lib/container";
import { setClipEnabled, splitClip, trackMotion, stabilizeClip, autoReframeClip } from "../lib/commands";
import { invokeAction } from "../lib/actions";
import { motionTargetChoices, motionTargetRef } from "../lib/motionTarget";

export function ContextMenu() {
  const menu = useEditorStore((s) => s.contextMenu);
  const project = useEditorStore((s) => s.project);
  const dispatch = useEditorStore((s) => s.dispatch);
  const closeContextMenu = useEditorStore((s) => s.closeContextMenu);
  const clipboard = useEditorStore((s) => s.clipboard);
  const toast = useEditorStore((s) => s.toast);
  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const selections = useEditorStore((s) => s.selections);
  const openSequence = useEditorStore((s) => s.openSequence);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useEffect(() => {
    if (!menu) return;
    const onDocClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) closeContextMenu();
    };
    const onEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeContextMenu();
    };
    document.addEventListener("mousedown", onDocClick);
    document.addEventListener("keydown", onEscape);
    return () => {
      document.removeEventListener("mousedown", onDocClick);
      document.removeEventListener("keydown", onEscape);
    };
  }, [menu, closeContextMenu]);

  // Menu opens at the raw click point; once its real size is known (after this layout
  // pass, before paint — no visible flash), clamp it back on-screen. Without this, a
  // right-click near the window's right/bottom edge rendered the menu partially or fully
  // off-screen with its bottom items ("Delete"/"Ripple delete") unreachable.
  useLayoutEffect(() => {
    if (!menu || !rootRef.current) {
      setPos(null);
      return;
    }
    const rect = rootRef.current.getBoundingClientRect();
    const margin = 4;
    const left = Math.max(margin, Math.min(menu.x, window.innerWidth - rect.width - margin));
    const top = Math.max(margin, Math.min(menu.y, window.innerHeight - rect.height - margin));
    setPos({ left, top });
  }, [menu]);

  if (!menu) return null;

  const track = activeTracks(project, activeSequenceId).find((t) => t.id === menu.trackId);
  const clip = track?.clips.find((c) => c.id === menu.clipId);
  const locked = track?.locked ?? false;
  // Adjustment layers also take enable/disable (SetClipEnabled).
  const canToggleEnabled = !!clip && clip.type !== "caption";
  const nestedSequence =
    project && clip && isNestedClip(project, clip) && (clip.type === "video" || clip.type === "audio")
      ? sequenceOfMedia(project, clip.media_id)
      : undefined;
  // "Nest clips" works on the whole multi-selection (all clips must share one container,
  // which is always the open one); captions cannot be nested.
  const multi = selections.length > 1 && selections.some((s) => s.clipId === menu.clipId);
  const nestable = (multi ? selections : [{ trackId: menu.trackId, clipId: menu.clipId }]).every((s) => {
    const c = activeTracks(project, activeSequenceId)
      .find((t) => t.id === s.trackId)
      ?.clips.find((x) => x.id === s.clipId);
    return !!c && c.type !== "caption";
  });
  const isVideo = clip?.type === "video";
  const targetAspect =
    project && project.settings.height > 0
      ? project.settings.width / project.settings.height
      : 9 / 16;

  async function runAnalysis(label: string, fn: () => Promise<boolean>) {
    toast(`${label}…`, "info");
    const ok = await fn();
    if (ok) toast(`${label} complete`, "success");
  }

  function run(fn: () => void) {
    closeContextMenu();
    fn();
  }

  return (
    <div
      className="context-menu"
      ref={rootRef}
      style={{ left: pos?.left ?? menu.x, top: pos?.top ?? menu.y }}
    >
      <button
        type="button"
        disabled={locked}
        onClick={() => run(() => void dispatch(splitClip(menu.trackId, menu.clipId, menu.atSecs)))}
      >
        Split at click
      </button>
      <button type="button" disabled={locked} onClick={() => run(() => invokeAction("edit.duplicate"))}>
        Duplicate
      </button>
      <button type="button" onClick={() => run(() => invokeAction("edit.copy"))}>
        Copy
      </button>
      <button
        type="button"
        disabled={locked || !clipboard}
        onClick={() => run(() => invokeAction("edit.paste"))}
      >
        Paste
      </button>
      {canToggleEnabled && clip && (
        <button
          type="button"
          disabled={locked}
          onClick={() => run(() => void dispatch(setClipEnabled(menu.trackId, menu.clipId, !clip.enabled)))}
        >
          {clip.enabled ? "Disable clip" : "Enable clip"}
        </button>
      )}
      <div className="context-menu-divider" />
      <button
        type="button"
        disabled={locked || !nestable}
        onClick={() => run(() => invokeAction("edit.nest"))}
      >
        {multi ? `Nest ${selections.length} clips` : "Nest clip"}
        <span className="context-menu-hint">Ctrl+G</span>
      </button>
      {nestedSequence && (
        <>
          <button type="button" onClick={() => run(() => void openSequence(nestedSequence.id))}>
            Open sequence
          </button>
          <button
            type="button"
            disabled={locked}
            onClick={() => run(() => invokeAction("edit.unnest"))}
          >
            Unnest
            <span className="context-menu-hint">Ctrl+Shift+G</span>
          </button>
        </>
      )}
      {isVideo && (
        <>
          <div className="context-menu-divider" />
          <button
            type="button"
            disabled={locked}
            onClick={() =>
              run(() =>
                void runAnalysis("Track motion", () =>
                  dispatch(
                    trackMotion(menu.trackId, menu.clipId, 12, {
                      // Same default as the inspector's "Apply to" picker.
                      target:
                        track && clip && clip.type === "video"
                          ? motionTargetRef(
                              motionTargetChoices(
                                project,
                                activeTracks(project, activeSequenceId),
                                track,
                                clip,
                              ).defaultValue,
                            )
                          : null,
                    }),
                  ),
                ),
              )
            }
          >
            Track motion
          </button>
          <button
            type="button"
            disabled={locked}
            onClick={() =>
              run(() =>
                void runAnalysis("Stabilize", () =>
                  dispatch(stabilizeClip(menu.trackId, menu.clipId)),
                ),
              )
            }
          >
            Stabilize
          </button>
          <button
            type="button"
            disabled={locked}
            onClick={() =>
              run(() =>
                void runAnalysis("Auto-reframe", () =>
                  dispatch(autoReframeClip(menu.trackId, menu.clipId, targetAspect)),
                ),
              )
            }
          >
            Auto-reframe
          </button>
        </>
      )}
      <div className="context-menu-divider" />
      <button
        type="button"
        className="danger"
        disabled={locked}
        onClick={() =>
          run(async () => {
            // Context menu deletes the right-clicked clip (may differ from store selection).
            await useEditorStore.getState().dispatch({
              command: "DeleteClip",
              track_id: menu.trackId,
              clip_id: menu.clipId,
              ripple: false,
            });
            useEditorStore.getState().select(null);
          })
        }
      >
        Delete
      </button>
      <button
        type="button"
        className="danger"
        disabled={locked}
        onClick={() =>
          run(async () => {
            await useEditorStore.getState().dispatch({
              command: "DeleteClip",
              track_id: menu.trackId,
              clip_id: menu.clipId,
              ripple: true,
            });
            useEditorStore.getState().select(null);
          })
        }
      >
        Ripple delete
      </button>
    </div>
  );
}
