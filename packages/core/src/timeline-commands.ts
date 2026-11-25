// Timeline editing commands: tracks, and adding, moving, trimming, splitting and deleting clips.
// The rules in timeline.ts hold after every one of them:
// - Times are snapped to the sequence's frame grid (nearest frame).
// - Trims stop at the source's ends and at neighbouring clips instead of failing.
// - On a magnetic track clips stay contiguous: removing, trimming or inserting ripples the
//   clips after it, and a clip placed on it goes in at the nearest clip boundary.
// - Clips on locked tracks cannot be edited.
//
// Commands that create clips without being told their ids (the right half of a split or an
// overwrite) derive them from the original clip's id and the cut time, so replays match.

import type { Draft } from "immer";
import { z } from "zod";
import { CommandError, defineCommand } from "./commands";
import { deriveId } from "./ids";
import { Flicks, Id, type Clip, type Project, type Sequence, type Track } from "./schema";
import { clipEnd, sourceDuration, sourceSpan } from "./timeline";
import { flicksPerFrame } from "./time";

type DProject = Draft<Project>;
type DSequence = Draft<Sequence>;
type DTrack = Draft<Track>;
type DClip = Draft<Clip>;

const IdList = z
  .array(Id)
  .min(1)
  .refine((ids) => new Set(ids).size === ids.length, "lists an id twice");
const Mode = z.enum(["overwrite", "insert"]);
const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

// --- finding things ---

function locateTrack(project: DProject, id: string) {
  for (const sequence of project.sequences) {
    const track = sequence.tracks.find((t) => t.id === id);
    if (track) return { sequence, track };
  }
  throw new CommandError("not-found", `No track ${id}`, "tracks");
}

function locateClip(project: DProject, id: string) {
  for (const sequence of project.sequences) {
    for (const track of sequence.tracks) {
      const clip = track.clips.find((c) => c.id === id);
      if (clip) return { sequence, track, clip };
    }
  }
  throw new CommandError("not-found", `No clip ${id}`, "clips");
}

/** All clips of a list, which must share one sequence. */
function locateClips(project: DProject, ids: readonly string[]) {
  const found = ids.map((id) => locateClip(project, id));
  const sequence = found[0]?.sequence;
  if (found.some((f) => f.sequence !== sequence))
    throw new CommandError("rejected", "The clips are in different sequences", "clips");
  for (const { track } of found) unlocked(track);
  return found;
}

function unlocked(track: DTrack): void {
  if (track.locked) throw new CommandError("rejected", `Track ${track.id} is locked`, "tracks");
}

function idTaken(project: DProject, id: string): boolean {
  for (const sequence of project.sequences)
    for (const track of sequence.tracks) if (track.clips.some((c) => c.id === id)) return true;
  return false;
}

/** A deterministic id for a clip a command creates; never one already in use. */
function freshId(project: DProject, seed: string): string {
  let id = deriveId(seed);
  for (let n = 1; idTaken(project, id); n++) id = deriveId(`${seed}#${n}`);
  return id;
}

// --- frame grid ---

const frameOf = (sequence: DSequence) => flicksPerFrame(sequence.fps);
const snap = (t: number, frame: number, mode: "round" | "floor" | "ceil" = "round") =>
  Math[mode](t / frame) * frame;

// --- clip surgery (all on drafts; every helper keeps the track sorted) ---

const clone = (clip: Clip | DClip): Clip => JSON.parse(JSON.stringify(clip)) as Clip;

type Keyframes = Record<string, { t: number }[]>;

/** Moves a clip's keyframes `by` flicks earlier; keys that fall before the clip start go. */
function shiftKeys(clip: Clip | DClip, by: number): void {
  for (const component of clip.components as Record<string, unknown>[]) {
    const keys = component["keys"] as Keyframes | undefined;
    if (!keys) continue;
    for (const name of Object.keys(keys)) {
      const list = keys[name] ?? [];
      for (const key of list) key.t -= by;
      for (let i = list.length - 1; i >= 0; i--) if ((list[i]?.t ?? 0) < 0) list.splice(i, 1);
      if (list.length === 0) Reflect.deleteProperty(keys, name);
    }
    if (Object.keys(keys).length === 0) delete component["keys"];
  }
}

/** Cuts `clip` at `t` (strictly inside it); the right half is a new clip after it. */
function splitAt(project: DProject, track: DTrack, clip: DClip, t: number): DClip {
  const offset = t - clip.start;
  const right = clone(clip);
  right.id = freshId(project, `${clip.id}@${t}`);
  right.start = t;
  right.duration = clipEnd(clip) - t;
  right.in = clip.in + Math.round(offset * clip.speed);
  shiftKeys(right, offset);
  clip.duration = offset;
  track.clips.splice(track.clips.indexOf(clip) + 1, 0, right);
  return right as DClip;
}

/** Moves a clip's start edge to `start`, keeping its end: the source in-point moves with it. */
function trimStartTo(clip: DClip, start: number): void {
  const offset = start - clip.start;
  clip.in += Math.round(offset * clip.speed);
  clip.duration -= offset;
  clip.start = start;
  shiftKeys(clip, offset);
}

function removeClip(track: DTrack, clip: DClip): void {
  track.clips.splice(track.clips.indexOf(clip), 1);
}

/** Overwrite: empties [from, to) by removing, shortening or cutting the clips there. */
function clearRange(project: DProject, track: DTrack, from: number, to: number): void {
  for (const clip of [...track.clips]) {
    const end = clipEnd(clip);
    if (end <= from || clip.start >= to) continue;
    if (clip.start >= from && end <= to) removeClip(track, clip);
    else if (clip.start < from && end > to) {
      splitAt(project, track, clip, to);
      clip.duration = from - clip.start;
    } else if (clip.start < from) clip.duration = from - clip.start;
    else trimStartTo(clip, to);
  }
}

/** Insert: cuts the clip spanning `at` and pushes everything from `at` on by `length`. */
function openRange(project: DProject, track: DTrack, at: number, length: number): void {
  const spanning = track.clips.find((c) => c.start < at && clipEnd(c) > at);
  if (spanning) splitAt(project, track, spanning, at);
  for (const clip of track.clips) if (clip.start >= at) clip.start += length;
}

const sortClips = (track: DTrack) => track.clips.sort((a, b) => a.start - b.start);

/** Closes every gap on a magnetic track, keeping clip order. */
function pack(track: DTrack): void {
  if (!track.magnetic) return;
  sortClips(track);
  let t = 0;
  for (const clip of track.clips) {
    clip.start = t;
    t += clip.duration;
  }
}

/** Where a clip placed at time `t` goes on a magnetic track: after every clip whose middle is before t. */
function magneticIndex(track: DTrack, t: number, skip: ReadonlySet<string> = new Set()): number {
  return track.clips.filter((c) => !skip.has(c.id) && c.start + c.duration / 2 < t).length;
}

/**
 * Puts new clips (absolute starts) on a track. Overwrite clears what is under each clip; insert
 * pushes everything from the first clip's start on by the span they cover. On a magnetic track
 * they go in at `index`, in time order, and the track is packed.
 */
function place(
  project: DProject,
  track: DTrack,
  clips: Clip[],
  mode: "overwrite" | "insert",
  index: number,
): void {
  const sorted = [...clips].sort((a, b) => a.start - b.start);
  if (track.magnetic) {
    track.clips.splice(index, 0, ...sorted);
    let t = 0;
    for (const clip of track.clips) {
      clip.start = t;
      t += clip.duration;
    }
    return;
  }
  const first = sorted[0];
  if (!first) return;
  if (mode === "insert") {
    const end = Math.max(...sorted.map(clipEnd));
    openRange(project, track, first.start, end - first.start);
  } else {
    for (const clip of sorted) clearRange(project, track, clip.start, clipEnd(clip));
  }
  track.clips.push(...sorted);
  sortClips(track);
}

/** True when sequence `from` is `target` or contains it through compound clips. */
function reaches(
  project: DProject,
  from: string,
  target: string,
  seen = new Set<string>(),
): boolean {
  if (from === target) return true;
  if (seen.has(from)) return false;
  seen.add(from);
  const sequence = project.sequences.find((s) => s.id === from);
  if (!sequence) return false;
  return sequence.tracks.some((t) =>
    t.clips.some((c) => c.sequence !== undefined && reaches(project, c.sequence, target, seen)),
  );
}

// --- tracks ---

export const AddTrack = defineCommand({
  type: "track.add",
  payload: z.strictObject({
    sequence: Id,
    id: Id,
    kind: z.enum(["video", "audio", "text", "effect"]),
    /** Position in the track list; the end when left out. */
    index: z.int().min(0).optional(),
    name: z.string().default(""),
    magnetic: z.boolean().default(false),
  }),
  label: ({ kind }) => `Add ${kind} track`,
  apply: (project, { sequence: sequenceId, id, kind, index, name, magnetic }) => {
    const sequence = project.sequences.find((s) => s.id === sequenceId);
    if (!sequence) throw new CommandError("not-found", `No sequence ${sequenceId}`, "sequence");
    const at = Math.min(index ?? sequence.tracks.length, sequence.tracks.length);
    sequence.tracks.splice(at, 0, {
      id,
      kind,
      name,
      locked: false,
      hidden: false,
      muted: false,
      magnetic,
      clips: [],
    });
  },
});

export const RemoveTracks = defineCommand({
  type: "track.remove",
  payload: z.strictObject({ tracks: IdList }),
  label: ({ tracks }) => `Remove ${plural(tracks.length, "track")}`,
  apply: (project, { tracks }) => {
    for (const id of tracks) {
      const { sequence, track } = locateTrack(project, id);
      unlocked(track);
      sequence.tracks.splice(sequence.tracks.indexOf(track), 1);
    }
  },
});

export const SetTrackFlags = defineCommand({
  type: "track.set",
  payload: z
    .strictObject({
      tracks: IdList,
      name: z.string().optional(),
      locked: z.boolean().optional(),
      hidden: z.boolean().optional(),
      muted: z.boolean().optional(),
      /** Turning this on closes every gap on the track. */
      magnetic: z.boolean().optional(),
    })
    .refine((p) => Object.keys(p).length > 1, "sets nothing"),
  label: ({ tracks }) => `Change ${plural(tracks.length, "track")}`,
  apply: (project, { tracks, ...flags }) => {
    for (const id of tracks) {
      const { track } = locateTrack(project, id);
      const closesGaps = flags.magnetic === true && !track.magnetic;
      if (closesGaps && track.locked && flags.locked !== false)
        throw new CommandError("rejected", `Track ${id} is locked`, "tracks");
      if (flags.name !== undefined) track.name = flags.name;
      if (flags.locked !== undefined) track.locked = flags.locked;
      if (flags.hidden !== undefined) track.hidden = flags.hidden;
      if (flags.muted !== undefined) track.muted = flags.muted;
      if (flags.magnetic !== undefined) track.magnetic = flags.magnetic;
      if (closesGaps) pack(track);
    }
  },
});

// --- clips ---

const NewClip = z
  .strictObject({
    id: Id,
    media: Id.optional(),
    sequence: Id.optional(),
    start: Flicks,
    duration: z.int().positive(),
    in: Flicks.default(0),
    speed: z.number().positive().default(1),
    enabled: z.boolean().default(true),
    components: z.array(z.record(z.string(), z.unknown())).default([]),
  })
  .refine((c) => (c.media === undefined) !== (c.sequence === undefined), {
    message: "must have exactly one of `media` or `sequence`",
  });

export const AddClips = defineCommand({
  type: "clip.add",
  payload: z.strictObject({
    track: Id,
    mode: Mode.default("overwrite"),
    clips: z.array(NewClip).min(1),
  }),
  label: ({ clips }) => `Add ${plural(clips.length, "clip")}`,
  apply: (project, { track: trackId, mode, clips }) => {
    const { sequence, track } = locateTrack(project, trackId);
    unlocked(track);
    const frame = frameOf(sequence);
    const made: Clip[] = clips.map((input, i) => {
      const where = `clips[${i}]`;
      const start = snap(input.start, frame);
      const duration = snap(input.duration, frame);
      if (duration < frame)
        throw new CommandError("rejected", "is shorter than one frame", `${where}.duration`);
      if (input.media !== undefined) {
        const media = project.media.find((m) => m.id === input.media);
        if (!media)
          throw new CommandError("not-found", `No media ${input.media}`, `${where}.media`);
        const fits =
          (track.kind === "video" && media.video) || (track.kind === "audio" && media.audio);
        if (!fits)
          throw new CommandError(
            "rejected",
            `cannot go on a ${track.kind} track`,
            `${where}.media`,
          );
      } else {
        if (track.kind !== "video")
          throw new CommandError(
            "rejected",
            "a sequence goes on a video track",
            `${where}.sequence`,
          );
        if (!project.sequences.some((s) => s.id === input.sequence))
          throw new CommandError("not-found", `No sequence ${input.sequence}`, `${where}.sequence`);
        if (reaches(project, input.sequence ?? "", sequence.id))
          throw new CommandError("rejected", "would contain itself", `${where}.sequence`);
      }
      const clip: Clip = {
        id: input.id,
        ...(input.media === undefined ? {} : { media: input.media }),
        ...(input.sequence === undefined ? {} : { sequence: input.sequence }),
        start,
        duration,
        in: input.in,
        speed: input.speed,
        enabled: input.enabled,
        components: input.components,
      };
      if (clip.in + sourceSpan(clip) > sourceDuration(project as Project, clip))
        throw new CommandError("rejected", "reads past the end of its source", `${where}.duration`);
      return clip;
    });
    const sorted = [...made].sort((a, b) => a.start - b.start);
    for (let i = 1; i < sorted.length; i++) {
      const previous = sorted[i - 1];
      const current = sorted[i];
      if (previous && current && current.start < clipEnd(previous))
        throw new CommandError("rejected", "The new clips overlap each other", "clips");
    }
    place(project, track, made, mode, magneticIndex(track, sorted[0]?.start ?? 0));
  },
});

export const MoveClips = defineCommand({
  type: "clip.move",
  payload: z.strictObject({
    clips: IdList,
    /** Time offset in flicks; snapped to the frame grid, and limited so no clip starts before 0. */
    delta: z.int(),
    /** Track offset: +1 moves each clip to the next track of the same sequence. */
    tracks: z.int().default(0),
    mode: Mode.default("overwrite"),
  }),
  label: ({ clips }) => `Move ${plural(clips.length, "clip")}`,
  apply: (project, { clips, delta: rawDelta, tracks: trackOffset, mode }) => {
    const found = locateClips(project, clips);
    const sequence = found[0]?.sequence;
    if (!sequence) return;
    const frame = frameOf(sequence);
    const earliest = Math.min(...found.map((f) => f.clip.start));
    const delta = Math.max(snap(rawDelta, frame), -earliest);
    if (delta === 0 && trackOffset === 0) return;

    const moving = new Set(clips);
    const groups = new Map<DTrack, { clips: Clip[]; index: number }>();
    for (const { track, clip } of found) {
      const target = sequence.tracks[sequence.tracks.indexOf(track) + trackOffset];
      if (!target) throw new CommandError("rejected", "No track to move to", "tracks");
      if (target.kind !== track.kind)
        throw new CommandError("rejected", `cannot move to a ${target.kind} track`, "tracks");
      unlocked(target);
      const copy = clone(clip);
      copy.start = clip.start + delta;
      const group = groups.get(target) ?? { clips: [], index: 0 };
      group.clips.push(copy);
      groups.set(target, group);
    }
    // Magnetic insertion points are chosen against the clips that stay, before anything moves.
    for (const [target, group] of groups) {
      const first = Math.min(...group.clips.map((c) => c.start));
      group.index = magneticIndex(target, first, moving);
    }
    for (const { track, clip } of found) removeClip(track, clip);
    for (const { track } of found) pack(track);
    for (const [target, group] of groups) place(project, target, group.clips, mode, group.index);
  },
});

export const TrimClips = defineCommand({
  type: "clip.trim",
  payload: z.strictObject({
    clips: IdList,
    edge: z.enum(["start", "end"]),
    /** Flicks to move the edge by (positive is later). Stops at the source and at neighbours. */
    delta: z.int(),
  }),
  label: ({ clips }) => `Trim ${plural(clips.length, "clip")}`,
  apply: (project, { clips, edge, delta }) => {
    for (const { sequence, track, clip } of locateClips(project, clips)) {
      const frame = frameOf(sequence);
      const step = snap(delta, frame);
      const index = track.clips.indexOf(clip);
      const previous = track.magnetic ? undefined : track.clips[index - 1];
      const next = track.magnetic ? undefined : track.clips[index + 1];
      const end = clipEnd(clip);
      if (edge === "start") {
        // Earliest: where the source begins, the previous clip's end, or 0.
        let earliest = clip.start - snap(clip.in / clip.speed, frame, "floor");
        if (!track.magnetic) earliest = Math.max(earliest, previous ? clipEnd(previous) : 0);
        const start = Math.min(Math.max(clip.start + step, earliest), end - frame);
        if (start !== clip.start) trimStartTo(clip, start);
      } else {
        const room = sourceDuration(project as Project, clip) - clip.in;
        let latest = clip.start + snap(room / clip.speed, frame, "floor");
        if (next) latest = Math.min(latest, next.start);
        const newEnd = Math.max(Math.min(end + step, latest), clip.start + frame);
        if (newEnd !== end) clip.duration = newEnd - clip.start;
      }
      pack(track);
    }
  },
});

export const SplitClips = defineCommand({
  type: "clip.split",
  payload: z.strictObject({ clips: IdList, t: Flicks }),
  label: ({ clips }) => `Split ${plural(clips.length, "clip")}`,
  apply: (project, { clips, t }) => {
    let cut = 0;
    for (const { sequence, track, clip } of locateClips(project, clips)) {
      const at = snap(t, frameOf(sequence));
      if (clip.start < at && at < clipEnd(clip)) {
        splitAt(project, track, clip, at);
        cut++;
      }
    }
    if (cut === 0) throw new CommandError("rejected", "None of the clips spans that time", "t");
  },
});

export const DeleteClips = defineCommand({
  type: "clip.delete",
  payload: z.strictObject({ clips: IdList }),
  label: ({ clips }) => `Delete ${plural(clips.length, "clip")}`,
  apply: (project, { clips }) => {
    const found = locateClips(project, clips);
    for (const { track, clip } of found) removeClip(track, clip);
    for (const { track } of found) pack(track);
  },
});

/** Deletes clips and closes the space each one took on its own track. */
export const RippleDeleteClips = defineCommand({
  type: "clip.rippleDelete",
  payload: z.strictObject({ clips: IdList }),
  label: ({ clips }) => `Ripple delete ${plural(clips.length, "clip")}`,
  apply: (project, { clips }) => {
    const found = locateClips(project, clips);
    const removed = new Map<DTrack, { start: number; end: number }[]>();
    for (const { track, clip } of found) {
      const list = removed.get(track) ?? [];
      list.push({ start: clip.start, end: clipEnd(clip) });
      removed.set(track, list);
      removeClip(track, clip);
    }
    for (const [track, ranges] of removed) {
      for (const clip of track.clips) {
        let shift = 0;
        for (const range of ranges) if (range.end <= clip.start) shift += range.end - range.start;
        clip.start -= shift;
      }
      pack(track);
    }
  },
});

/** Closes the empty space at time `t` on each track by pulling the later clips back. */
export const CloseGaps = defineCommand({
  type: "track.closeGap",
  payload: z.strictObject({ tracks: IdList, t: Flicks }),
  label: ({ tracks }) => `Close ${plural(tracks.length, "gap")}`,
  apply: (project, { tracks, t }) => {
    let closed = 0;
    for (const id of tracks) {
      const { track } = locateTrack(project, id);
      unlocked(track);
      if (track.clips.some((c) => c.start <= t && t < clipEnd(c))) continue; // no gap here
      const after = track.clips.filter((c) => c.start > t);
      const first = after[0];
      if (!first) continue; // space after the last clip is not a gap
      const before = track.clips.filter((c) => clipEnd(c) <= t);
      const gapStart = Math.max(0, ...before.map(clipEnd));
      const length = first.start - gapStart;
      for (const clip of after) clip.start -= length;
      closed++;
    }
    if (closed === 0) throw new CommandError("rejected", "There is no gap at that time", "t");
  },
});
