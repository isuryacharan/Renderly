// Timeline containers (schema v7). The root timeline lives in `project.tracks`; each nested
// sequence has its own `tracks`. The editor shows ONE container at a time
// (`activeSequenceId`, null = root), and everything that edits or displays "the timeline"
// reads its tracks through `activeTracks` (or the `containerView` built from it) — never
// `project.tracks` directly. Export / project duration / project-level facts stay on the
// root. Pure helpers, no store dependency, so they are unit-tested in container.test.ts.

import {
  clipEndSecs,
  type Clip,
  type MediaItem,
  type Project,
  type Sequence,
  type Track,
} from "./types";

const NO_TRACKS: Track[] = [];

export function findSequence(project: Project, sequenceId: string): Sequence | undefined {
  return project.sequences?.find((s) => s.id === sequenceId);
}

/** `sequenceId` if it names a sequence that exists in `project`, else null (the root). */
export function resolveActiveSequenceId(
  project: Project | null,
  sequenceId: string | null,
): string | null {
  if (!project || !sequenceId) return null;
  return findSequence(project, sequenceId) ? sequenceId : null;
}

/**
 * Tracks of the open container: the sequence's tracks, or the root's. An unknown
 * `activeSequenceId` (deleted / undone away) falls back to the root. Returns the project's
 * own array (stable identity) so it is safe as a zustand selector result.
 */
export function activeTracks(project: Project | null, activeSequenceId: string | null): Track[] {
  if (!project) return NO_TRACKS;
  const seq = activeSequenceId ? findSequence(project, activeSequenceId) : undefined;
  return seq ? seq.tracks : project.tracks;
}

const viewCache = new WeakMap<Project, Map<string, Project>>();

/**
 * Output size of the open container: the sequence's own `width`/`height`, each falling back to
 * the project's when null (the same inheritance `Project::sequence_size` applies in core). The
 * root's size is the project's. This is the canvas aspect the preview and its overlays use.
 */
export function activeContainerSize(
  project: Project,
  activeSequenceId: string | null,
): { width: number; height: number } {
  const id = resolveActiveSequenceId(project, activeSequenceId);
  const seq = id ? findSequence(project, id) : undefined;
  return {
    width: seq?.width ?? project.settings.width,
    height: seq?.height ?? project.settings.height,
  };
}

/**
 * The project as the open container sees it: identical to `project` but with `tracks` set to
 * `activeTracks(...)` and `settings.width`/`height` set to `activeContainerSize(...)`, so
 * geometry / snapping / rendering helpers written against `project.tracks` and
 * `project.settings` work on a sequence unchanged. For the root it is `project` itself. Cached
 * per (project, sequence) so repeated reads are referentially stable. READ-ONLY: never put a
 * view into the store — write edits back with `replaceActiveTracks`.
 */
export function containerView(project: Project, activeSequenceId: string | null): Project {
  const id = resolveActiveSequenceId(project, activeSequenceId);
  if (!id) return project;
  let perProject = viewCache.get(project);
  if (!perProject) {
    perProject = new Map();
    viewCache.set(project, perProject);
  }
  let view = perProject.get(id);
  if (!view) {
    const { width, height } = activeContainerSize(project, id);
    view = {
      ...project,
      tracks: activeTracks(project, id),
      settings: { ...project.settings, width, height },
    };
    perProject.set(id, view);
  }
  return view;
}

/** Zustand-style selector: the open container's view of the project (null without one). */
export function selectActiveView(s: {
  project: Project | null;
  activeSequenceId: string | null;
}): Project | null {
  return s.project ? containerView(s.project, s.activeSequenceId) : null;
}

/** Zustand-style selector: the open container's tracks. */
export function selectActiveTracks(s: {
  project: Project | null;
  activeSequenceId: string | null;
}): Track[] {
  return activeTracks(s.project, s.activeSequenceId);
}

/**
 * Write a (locally edited) set of tracks back into the open container of `project` and
 * return the new project. Used by the optimistic drag patterns, which compute their preview
 * on `containerView(...)` and must store it on the real project.
 */
export function replaceActiveTracks(
  project: Project,
  activeSequenceId: string | null,
  tracks: Track[],
): Project {
  const id = resolveActiveSequenceId(project, activeSequenceId);
  if (!id) return { ...project, tracks };
  return {
    ...project,
    sequences: (project.sequences ?? []).map((s) => (s.id === id ? { ...s, tracks } : s)),
  };
}

/** Apply `fn` to every track of every container (root and sequences); untouched tracks keep identity. */
export function mapAllTracks(project: Project, fn: (track: Track) => Track): Project {
  const mapList = (tracks: Track[]): Track[] => {
    let changed = false;
    const next = tracks.map((t) => {
      const r = fn(t);
      if (r !== t) changed = true;
      return r;
    });
    return changed ? next : tracks;
  };
  const root = mapList(project.tracks);
  let seqChanged = false;
  const sequences = (project.sequences ?? []).map((s) => {
    const tracks = mapList(s.tracks);
    if (tracks === s.tracks) return s;
    seqChanged = true;
    return { ...s, tracks };
  });
  if (root === project.tracks && !seqChanged) return project;
  return { ...project, tracks: root, ...(project.sequences ? { sequences } : {}) };
}

/**
 * Optimistically update one clip wherever it lives (track ids are globally unique). Used by the
 * live-drag patterns (timeline drag, preview handles) that edit the store project before the
 * single committing command.
 */
export function updateClip(
  project: Project,
  trackId: string,
  clipId: string,
  update: (clip: Clip) => Clip,
): Project {
  return mapAllTracks(project, (t) =>
    t.id !== trackId
      ? t
      : { ...t, clips: t.clips.map((c) => (c.id === clipId ? update(c) : c)) },
  );
}

/** Find a track in the root or any sequence (track ids are globally unique). */
export function findTrackAnywhere(project: Project, trackId: string): Track | undefined {
  const root = project.tracks.find((t) => t.id === trackId);
  if (root) return root;
  for (const seq of project.sequences ?? []) {
    const hit = seq.tracks.find((t) => t.id === trackId);
    if (hit) return hit;
  }
  return undefined;
}

/** Id of the sequence whose timeline contains `trackId`, null for the root, undefined if unknown. */
export function containerOfTrack(project: Project, trackId: string): string | null | undefined {
  if (project.tracks.some((t) => t.id === trackId)) return null;
  for (const seq of project.sequences ?? []) {
    if (seq.tracks.some((t) => t.id === trackId)) return seq.id;
  }
  return undefined;
}

/** The sequence a `kind: "sequence"` media item places, if any. */
export function sequenceOfMedia(project: Project, mediaId: string): Sequence | undefined {
  const media = project.media.find((m) => m.id === mediaId);
  if (!media || media.kind !== "sequence" || !media.sequence_id) return undefined;
  return findSequence(project, media.sequence_id);
}

/** The media item that places `sequenceId` on a timeline. */
export function mediaOfSequence(project: Project, sequenceId: string): MediaItem | undefined {
  return project.media.find((m) => m.kind === "sequence" && m.sequence_id === sequenceId);
}

/** End of the last enabled non-caption clip in the open container (0 when empty). */
export function containerDurationSecs(project: Project, activeSequenceId: string | null): number {
  let end = 0;
  for (const track of activeTracks(project, activeSequenceId)) {
    for (const clip of track.clips) {
      if (clip.type === "caption" || !clip.enabled) continue;
      end = Math.max(end, clipEndSecs(clip));
    }
  }
  return end;
}

/**
 * Longest prefix of `path` (the sequences opened from the root, outermost first) that still
 * exists. A sequence that was deleted or undone away cuts the path there, so the editor falls
 * back to the nearest surviving ancestor (the root when the first entry is gone).
 */
export function normalizeSequencePath(project: Project | null, path: string[]): string[] {
  if (!project) return [];
  const out: string[] = [];
  for (const id of path) {
    if (!findSequence(project, id)) break;
    out.push(id);
  }
  return out;
}

export interface BreadcrumbItem {
  /** null for the root crumb. */
  sequenceId: string | null;
  label: string;
  /** The last item is the open container (not a link). */
  current: boolean;
}

/** Breadcrumb for the open path: `Root › Intro › Titles`. Always starts with the root crumb. */
export function breadcrumbItems(project: Project | null, path: string[]): BreadcrumbItem[] {
  const valid = normalizeSequencePath(project, path);
  const items: BreadcrumbItem[] = [
    { sequenceId: null, label: project?.name ?? "Project", current: valid.length === 0 },
  ];
  valid.forEach((id, i) => {
    const seq = project ? findSequence(project, id) : undefined;
    items.push({ sequenceId: id, label: seq?.name ?? "Sequence", current: i === valid.length - 1 });
  });
  return items;
}

/** Path after navigating to a breadcrumb entry (`null` = root). */
export function pathForBreadcrumb(path: string[], sequenceId: string | null): string[] {
  if (sequenceId == null) return [];
  const idx = path.indexOf(sequenceId);
  return idx === -1 ? path : path.slice(0, idx + 1);
}

/** Path after opening `sequenceId` from the currently open container. */
export function pathAfterOpen(path: string[], sequenceId: string): string[] {
  const idx = path.indexOf(sequenceId);
  return idx === -1 ? [...path, sequenceId] : path.slice(0, idx + 1);
}

/** Minimum length of an adjustment layer: one frame at 30 fps keeps it grabbable. */
export const MIN_ADJUSTMENT_SECS = 0.1;

/**
 * Trim math for an adjustment layer's edges (it has no source range, so a trim is just
 * position + duration): dragging the left edge to `edgeSecs` keeps the right edge fixed;
 * dragging the right edge keeps the position. Clamped so the layer stays at least
 * `MIN_ADJUSTMENT_SECS` long and never starts before 0.
 */
export function trimAdjustment(
  edge: "left" | "right",
  position: number,
  duration: number,
  edgeSecs: number,
): { position: number; duration: number } {
  if (edge === "right") {
    return { position, duration: Math.max(MIN_ADJUSTMENT_SECS, edgeSecs - position) };
  }
  const end = position + duration;
  const newPos = Math.max(0, Math.min(end - MIN_ADJUSTMENT_SECS, edgeSecs));
  return { position: newPos, duration: end - newPos };
}

/**
 * Which track kinds a dragged media item may land on: video/image -> video tracks, audio ->
 * audio tracks, a nested sequence -> either (a video clip shows its picture, an audio clip
 * plays its mix). Caption tracks take none.
 */
export function trackAcceptsMedia(
  trackKind: Track["kind"],
  mediaKind: "video" | "audio" | "image" | "sequence",
): boolean {
  if (trackKind === "caption") return false;
  if (mediaKind === "sequence") return true;
  return mediaKind === "audio" ? trackKind === "audio" : trackKind === "video";
}

/** Kind of track to auto-create for a drop outside every lane: `above` = above the topmost lane. */
export function newTrackKindForDrop(
  mediaKind: "video" | "audio" | "image" | "sequence",
  above: boolean,
): "video" | "audio" {
  if (mediaKind === "sequence") return above ? "video" : "audio";
  return mediaKind === "audio" ? "audio" : "video";
}

/** Whether `clip` is a clip that places a nested sequence (a media clip on `kind: "sequence"` media). */
export function isNestedClip(project: Project, clip: Clip): boolean {
  if (clip.type !== "video" && clip.type !== "audio") return false;
  const media = project.media.find((m) => m.id === clip.media_id);
  return media?.kind === "sequence";
}
