import { activeTracks } from "./container";
import type { EffectClip, Track } from "./types";
import { useEditorStore } from "../store/editorStore";

/** The selected video/audio clip or adjustment layer in the open container (not captions). */
export function useSelectedClip(): { track: Track; clip: EffectClip } | null {
  const project = useEditorStore((s) => s.project);
  const selection = useEditorStore((s) => s.selection);
  const activeSequenceId = useEditorStore((s) => s.activeSequenceId);
  const track = activeTracks(project, activeSequenceId).find((t) => t.id === selection?.trackId);
  const clip = track?.clips.find((c) => c.id === selection?.clipId);
  if (!track || !clip || clip.type === "caption" || track.kind === "caption") return null;
  return { track, clip };
}
