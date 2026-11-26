// S1.8: the benchmark driven from a real project. With `?project` on the preview page, every edit
// list the benchmark loads is first built as a Renderly project through the typed commands
// (tracks, clips, transforms), evaluated by core `evaluateSequence`, lowered by engine
// `cutsFromGraph` (as the editor does), and sent through the engine's in-place `update` (as the
// editor does). The lowered list must equal the benchmark's own; `window.projectCheck` says so.

import {
  createId,
  evaluateSequence,
  newProject,
  ProjectStore,
  type Command,
  type Project,
} from "@renderly/core";
import {
  cutsFromGraph,
  IDENTITY,
  toTransform,
  type Cut,
  type LoadedTimeline,
  type Preview,
  type Size,
} from "@renderly/engine";
import type { MediaInput } from "@renderly/media";

export interface ProjectCheck {
  cuts: number;
  exact: boolean;
  firstDifference: string | null;
}

const normalise = (cuts: readonly Cut[]) =>
  cuts
    .map((cut) => ({
      source: cut.source,
      in: cut.in,
      out: cut.out,
      start: cut.start,
      layer: cut.layer ?? 0,
      transform: toTransform(cut.transform),
    }))
    .sort((a, b) => a.layer - b.layer || a.start - b.start);

/** Builds the project for `cuts` over `sources` (with their opened sizes and durations). */
function buildProject(
  info: { width: number; height: number; duration: number }[],
  cuts: readonly Cut[],
  size: Size,
): { project: Project; mediaIds: string[] } {
  const project = newProject("Benchmark", { width: size.width, height: size.height, fps: [60, 1] });
  const store = new ProjectStore(project);
  const run = (command: Command) => {
    const result = store.dispatch(command);
    if (!result.ok) throw new Error(`${command.type}: ${result.error.message}`);
  };
  const mediaIds = info.map(() => createId());
  run({
    type: "media.import",
    payload: {
      items: info.map((source, i) => ({
        id: mediaIds[i],
        name: `source-${i}`,
        link: "copy",
        path: `media/source-${i}`,
        fingerprint: { size: 1, modified: 0, head: "bench" },
        duration: source.duration,
        video: { width: source.width, height: source.height, fps: [60, 1], codec: "avc" },
      })),
    },
  });
  const sequence = store.project.sequences[0];
  if (!sequence) throw new Error("No sequence");
  const layers = [...new Set(cuts.map((cut) => cut.layer ?? 0))].sort((a, b) => a - b);
  const tracks = new Map(layers.map((layer) => [layer, createId()]));
  for (const layer of layers)
    run({
      type: "track.add",
      payload: { sequence: sequence.id, id: tracks.get(layer), kind: "video" },
    });
  for (const layer of layers) {
    const clips = cuts
      .filter((cut) => (cut.layer ?? 0) === layer)
      .map((cut) => {
        const t = toTransform(cut.transform);
        const moved = JSON.stringify(t) !== JSON.stringify(IDENTITY);
        return {
          id: createId(),
          media: mediaIds[cut.source],
          start: cut.start,
          duration: cut.out - cut.in,
          in: cut.in,
          components: moved
            ? [
                {
                  type: "transform",
                  position: [t.x, t.y],
                  scale: [t.scaleX, t.scaleY],
                  rotation: t.rotation,
                  anchor: [t.anchorX, t.anchorY],
                  flipX: t.flipX,
                  flipY: t.flipY,
                  opacity: t.opacity,
                },
              ]
            : [],
        };
      });
    run({ type: "clip.add", payload: { track: tracks.get(layer), clips } });
  }
  return { project: store.project, mediaIds };
}

/** Replaces `preview.load` and `preview.open` with the project route. */
export function routeThroughProject(preview: Preview, report: (check: ProjectCheck) => void): void {
  const direct = preview.load.bind(preview);
  const load = async (
    sources: MediaInput[],
    cuts: Cut[] | null,
    size?: Size,
  ): Promise<LoadedTimeline> => {
    // Open the files once (no cuts) to learn their sizes and lengths, as an import would.
    const opened = await direct(sources, []);
    const info = opened.sources.map((result) =>
      result.ok ? { width: result.width, height: result.height, duration: result.duration } : null,
    );
    if (info.some((i) => i === null)) return opened;
    const known = info as { width: number; height: number; duration: number }[];
    const list = cuts ?? [{ source: 0, in: 0, out: known[0]?.duration ?? 0, start: 0 }];
    const bottom = list.find((cut) => (cut.layer ?? 0) === 0) ?? list[0];
    const frameSize = size ?? known[bottom?.source ?? 0] ?? { width: 1920, height: 1080 };
    const { project, mediaIds } = buildProject(known, list, frameSize);
    const sequence = project.sequences[0];
    if (!sequence) throw new Error("No sequence");
    const lowered = cutsFromGraph(evaluateSequence(project, sequence.id), (media) => {
      const index = mediaIds.indexOf(media);
      return index < 0 ? null : index;
    });
    const a = normalise(list);
    const b = normalise(lowered);
    const index = a.findIndex((cut, i) => JSON.stringify(cut) !== JSON.stringify(b[i]));
    report({
      cuts: lowered.length,
      exact: a.length === b.length && index < 0,
      firstDifference:
        index < 0 ? null : `${JSON.stringify(a[index])} became ${JSON.stringify(b[index])}`,
    });
    // As the editor does: start empty at the sequence size, then update in place.
    await direct([], [], { width: sequence.width, height: sequence.height });
    const loaded = await preview.update(sources, lowered, {
      width: sequence.width,
      height: sequence.height,
    });
    return { ...loaded, sources: opened.sources };
  };
  preview.load = load;
  preview.open = async (input) => {
    const loaded = await load([input], null);
    return {
      video: loaded.sources[0] ?? {
        ok: false,
        reason: "unsupported-format",
        detail: "Nothing loaded",
      },
      audio: loaded.audio[0] ?? { ok: false, reason: "no-audio-track" },
    };
  };
}
