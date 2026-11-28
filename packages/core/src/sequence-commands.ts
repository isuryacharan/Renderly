// Sequence (canvas) settings: size, frame rate and background (S2.3).
//
// Positions are fractions of the frame from its centre and scale 1 fits a clip to the frame, so
// a new size or aspect ratio keeps every clip centred and fitted without touching it. A new frame
// rate moves every clip edge and marker to the nearest frame of the new grid: rounding keeps the
// order of edges, so neighbours still meet and nothing comes to overlap. An edit that would make a
// clip shorter than one new frame, or read past its media, is refused.

import type { Draft } from "immer";
import { z } from "zod";
import { CommandError, defineCommand } from "./commands";
import { Background, Id, type Project } from "./schema";
import { clipEnd, sourceDuration } from "./timeline";
import { flicksPerFrame, isSupportedRate, type Rate } from "./time";

const Size = z
  .int()
  .min(16)
  .max(16384)
  .refine((n) => n % 2 === 0, "must be even (video is stored in 2 × 2 blocks)");

export const SetSequence = defineCommand({
  type: "sequence.set",
  payload: z
    .strictObject({
      sequence: Id,
      width: Size.optional(),
      height: Size.optional(),
      fps: z
        .tuple([z.int().positive(), z.int().positive()])
        .refine((rate) => isSupportedRate(rate), "is not a supported frame rate")
        .optional(),
      /** Fields left out keep their value. */
      background: Background.partial().optional(),
    })
    .refine((p) => Object.keys(p).length > 1, "sets nothing"),
  label: ({ fps, background, width, height }) =>
    fps
      ? "Change frame rate"
      : width !== undefined || height !== undefined
        ? "Change canvas size"
        : background
          ? "Change background"
          : "Change sequence",
  apply: (project, { sequence: id, width, height, fps, background }) => {
    const sequence = project.sequences.find((s) => s.id === id);
    if (!sequence) throw new CommandError("not-found", `No sequence ${id}`, "sequence");
    if (width !== undefined) sequence.width = width;
    if (height !== undefined) sequence.height = height;
    if (background) Object.assign(sequence.background, background);
    if (fps && (fps[0] !== sequence.fps[0] || fps[1] !== sequence.fps[1])) {
      regrid(project, sequence, fps);
      sequence.fps = fps;
    }
  },
});

type DProject = Draft<Project>;

/** Moves every clip edge and marker of `sequence` onto the frame grid of `rate`. */
function regrid(project: DProject, sequence: DProject["sequences"][number], rate: Rate): void {
  const frame = flicksPerFrame(rate);
  const snap = (t: number) => Math.round(t / frame) * frame;
  for (const track of sequence.tracks)
    for (const clip of track.clips) {
      const start = snap(clip.start);
      let end = snap(clipEnd(clip));
      if (clip.media !== undefined && clip.speed > 0) {
        // Rounding up may read a little past the media: stop at the last whole frame inside.
        const room = sourceDuration(project as Project, clip) - clip.in;
        const latest = start + Math.floor(room / clip.speed / frame) * frame;
        end = Math.min(end, latest);
      }
      if (end - start < frame)
        throw new CommandError(
          "rejected",
          `Clip ${clip.id} would be shorter than one frame at ${rate[0] / rate[1]} fps`,
          "fps",
        );
      clip.start = start;
      clip.duration = end - start;
    }
  for (const marker of sequence.markers) marker.t = snap(marker.t);
}
