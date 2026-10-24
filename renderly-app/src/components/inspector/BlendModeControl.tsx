import { setBlendMode } from "../../lib/commands";
import { BLEND_MODES, type BlendMode, type EffectClip, type Track } from "../../lib/types";
import { useEditorStore } from "../../store/editorStore";
import { MenuSelect } from "../ui/MenuSelect";

/// How a video clip or adjustment layer combines with what is beneath it (13 blend modes).
/// One `SetClipEffects { blend_mode }` per pick.
export function BlendModeControl({
  track,
  clip,
}: {
  track: Track;
  clip: EffectClip;
}) {
  const dispatch = useEditorStore((s) => s.dispatch);
  const mode: BlendMode = clip.blend_mode ?? "normal";
  return (
    <label className="insp-cell blend-mode-control">
      <span>Blend mode</span>
      <MenuSelect
        value={mode}
        disabled={track.locked}
        options={BLEND_MODES}
        onChange={(v) => {
          if (v !== mode) void dispatch(setBlendMode(track.id, clip.id, v as BlendMode));
        }}
      />
    </label>
  );
}
