import { useEditorStore } from "../../store/editorStore";
import { createMulticamGroup, setMulticamAngle } from "../../lib/commands";
import { findTrackAnywhere } from "../../lib/container";
import type { MediaClip, MulticamGroup, Project } from "../../lib/types";

function findClip(project: Project, trackId: string, clipId: string): MediaClip | null {
  const track = findTrackAnywhere(project, trackId);
  const clip = track?.clips.find((c) => c.id === clipId);
  return clip?.type === "video" ? (clip as MediaClip) : null;
}

export function MulticamSection({
  project,
  clip,
}: {
  project: Project;
  clip: MediaClip | null;
}) {
  const selections = useEditorStore((s) => s.selections);
  const dispatch = useEditorStore((s) => s.dispatch);
  const toast = useEditorStore((s) => s.toast);

  const groups = project.multicam_groups ?? [];
  const activeGroup: MulticamGroup | undefined = clip?.multicam_group_id
    ? groups.find((g) => g.id === clip.multicam_group_id)
    : undefined;

  const selectedVideoIds = selections
    .map((s) => findClip(project, s.trackId, s.clipId))
    .filter((c): c is MediaClip => c !== null)
    .map((c) => c.id);
  const canCreate = selectedVideoIds.length >= 2;

  async function createGroup() {
    const name = `Multicam ${groups.length + 1}`;
    const ok = await dispatch(createMulticamGroup(name, [...new Set(selectedVideoIds)]));
    if (ok) toast(`Created ${name}`, "success");
  }

  if (!activeGroup && groups.length === 0 && !canCreate) return null;

  return (
    <div className="inspector-section">
      <h3>Multicam</h3>
      {canCreate && (
        <button type="button" className="btn" onClick={() => void createGroup()}>
          Create group from {selectedVideoIds.length} clips
        </button>
      )}
      {activeGroup && (
        <>
          <p className="empty-hint">{activeGroup.name}</p>
          <div className="effects-catalog">
            {activeGroup.angle_clip_ids.map((clipId, i) => {
              const angleClip = project.tracks
                .flatMap((t) => t.clips)
                .find((c) => c.id === clipId);
              const label =
                angleClip?.type === "video"
                  ? `Angle ${i + 1}`
                  : `Angle ${i + 1}`;
              return (
                <button
                  key={clipId}
                  type="button"
                  className={(activeGroup.active_angle ?? 0) === i ? "active" : undefined}
                  onClick={() => void dispatch(setMulticamAngle(activeGroup.id, i))}
                >
                  {label}
                </button>
              );
            })}
          </div>
        </>
      )}
      {!activeGroup && groups.length > 0 && (
        <ul className="effect-list">
          {groups.map((g) => (
            <li key={g.id}>
              <span>{g.name}</span>
              <span className="empty-hint"> · {g.angle_clip_ids.length} angles</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
