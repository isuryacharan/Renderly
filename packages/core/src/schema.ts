// Project schema, version 1. Defined once here: the runtime validator and the generated JSON
// Schema (docs/schema/) both come from it. Field order here is the order in which a project is
// written, so a canonical document round-trips byte for byte.
//
// Times are integer flicks. Rates are exact [numerator, denominator] pairs.

import { z } from "zod";
import { componentSchema, registryVersion } from "./components";
import { ID_PATTERN } from "./ids";
import { isMediaRate, isSupportedRate, MEDIA_SAMPLE_RATES, SUPPORTED_SAMPLE_RATES } from "./time";

export const CURRENT_VERSION = 1;

export const Id = z.string().regex(ID_PATTERN, "must be a 16-character id");
export const Flicks = z.int().min(0);
const Duration = z.int().positive();

const Rate = z
  .tuple([z.int().positive(), z.int().positive()])
  .refine((rate) => isSupportedRate(rate), "is not a supported frame rate");

const SampleRate = z
  .int()
  .refine((rate) => (SUPPORTED_SAMPLE_RATES as readonly number[]).includes(rate), {
    message: `must be one of ${SUPPORTED_SAMPLE_RATES.join(", ")}`,
  });

/** A source file's own rates: wider than a sequence's (see MEDIA_RATES in time.ts). */
const MediaRate = z
  .tuple([z.int().positive(), z.int().positive()])
  .refine((rate) => isMediaRate(rate), "is not a supported source frame rate");

const MediaSampleRate = z
  .int()
  .refine((rate) => (MEDIA_SAMPLE_RATES as readonly number[]).includes(rate), {
    message: `must be one of ${MEDIA_SAMPLE_RATES.join(", ")}`,
  });

export const PackRef = z.strictObject({
  id: z.string().min(1),
  version: z.string().regex(/^\d+\.\d+\.\d+$/, "must be a semantic version"),
  hash: z.string().regex(/^sha256-[A-Za-z0-9+/=]+$/, "must be a sha256 content hash"),
});

export const Media = z.strictObject({
  id: Id,
  name: z.string().min(1),
  /** "handle": linked by a stored file handle; "copy": a file inside the project folder. */
  link: z.enum(["handle", "copy"]),
  path: z.string().min(1),
  /** Identifies the file when relinking: size, last-modified time and a hash of its head. */
  fingerprint: z.strictObject({
    size: z.int().min(0),
    modified: z.int().min(0),
    head: z.string().min(1),
  }),
  duration: Duration,
  video: z
    .strictObject({
      width: z.int().positive(),
      height: z.int().positive(),
      fps: MediaRate,
      codec: z.string().min(1),
      rotation: z.union([z.literal(0), z.literal(90), z.literal(180), z.literal(270)]).default(0),
    })
    .optional(),
  audio: z
    .strictObject({
      sampleRate: MediaSampleRate,
      channels: z.int().min(1).max(32),
      codec: z.string().min(1),
    })
    .optional(),
});

function clipSchema() {
  return z
    .strictObject({
      id: Id,
      /** The source: a media item, or another sequence (a compound clip). Exactly one is set. */
      media: Id.optional(),
      sequence: Id.optional(),
      start: Flicks,
      duration: Duration,
      /** Source time at the clip's start. */
      in: Flicks,
      speed: z.number().positive().default(1),
      enabled: z.boolean().default(true),
      components: z.array(componentSchema()).default([]),
    })
    .refine((clip) => (clip.media === undefined) !== (clip.sequence === undefined), {
      message: "must have exactly one of `media` or `sequence`",
    });
}

function trackSchema() {
  return z.strictObject({
    id: Id,
    kind: z.enum(["video", "audio", "text", "effect"]),
    name: z.string().default(""),
    locked: z.boolean().default(false),
    hidden: z.boolean().default(false),
    muted: z.boolean().default(false),
    /** Clips on a magnetic track stay contiguous (S1.3). */
    magnetic: z.boolean().default(false),
    clips: z.array(clipSchema()).default([]),
  });
}

export const Marker = z.strictObject({
  id: Id,
  t: Flicks,
  name: z.string().default(""),
  color: z.string().default("accent"),
});

function sequenceSchema() {
  return z.strictObject({
    id: Id,
    name: z.string().min(1),
    width: z.int().min(16).max(16384),
    height: z.int().min(16).max(16384),
    fps: Rate,
    sampleRate: SampleRate.default(48_000),
    tracks: z.array(trackSchema()).default([]),
    markers: z.array(Marker).default([]),
  });
}

function buildProjectSchema() {
  return z
    .strictObject({
      format: z.literal("renderly"),
      version: z.literal(CURRENT_VERSION),
      id: Id,
      name: z.string().min(1),
      /** Increases on every change; the automation contract's `expectedRevision` checks it. */
      revision: z.int().min(0),
      packs: z.array(PackRef).default([]),
      media: z.array(Media).default([]),
      sequences: z.array(sequenceSchema()).min(1),
    })
    .superRefine((project, ctx) => {
      // What a schema cannot say: ids are unique, and references point at something.
      const seen = new Map<string, string>();
      const claim = (id: string, path: (string | number)[]) => {
        const first = seen.get(id);
        if (first)
          ctx.addIssue({
            code: "custom",
            path: [...path, "id"],
            message: `duplicates the id at ${first}`,
          });
        else seen.set(id, formatPath([...path, "id"]));
      };
      const mediaIds = new Set(project.media.map((m) => m.id));
      const sequenceIds = new Set(project.sequences.map((s) => s.id));
      project.media.forEach((m, i) => claim(m.id, ["media", i]));
      project.sequences.forEach((sequence, s) => {
        claim(sequence.id, ["sequences", s]);
        sequence.markers.forEach((marker, m) => claim(marker.id, ["sequences", s, "markers", m]));
        sequence.tracks.forEach((track, t) => {
          claim(track.id, ["sequences", s, "tracks", t]);
          track.clips.forEach((clip, c) => {
            const path = ["sequences", s, "tracks", t, "clips", c];
            claim(clip.id, path);
            if (clip.media !== undefined && !mediaIds.has(clip.media)) {
              ctx.addIssue({
                code: "custom",
                path: [...path, "media"],
                message: "refers to no media item",
              });
            }
            if (clip.sequence !== undefined && !sequenceIds.has(clip.sequence)) {
              ctx.addIssue({
                code: "custom",
                path: [...path, "sequence"],
                message: "refers to no sequence",
              });
            }
            if (clip.sequence === sequence.id) {
              ctx.addIssue({
                code: "custom",
                path: [...path, "sequence"],
                message: "cannot contain its own sequence",
              });
            }
          });
        });
      });
    });
}

let cached: { version: number; schema: ReturnType<typeof buildProjectSchema> } | null = null;

/** The project schema for the component types registered so far. */
export function projectSchema() {
  if (!cached || cached.version !== registryVersion()) {
    cached = { version: registryVersion(), schema: buildProjectSchema() };
  }
  return cached.schema;
}

export type Project = z.infer<ReturnType<typeof buildProjectSchema>>;
export type Sequence = Project["sequences"][number];
export type Track = Sequence["tracks"][number];
export type Clip = Track["clips"][number];
export type MediaItem = z.infer<typeof Media>;

/** `sequences[0].tracks[1].clips[2].start` from a path array. */
export function formatPath(path: readonly PropertyKey[]): string {
  return path
    .map((part, i) =>
      typeof part === "number" ? `[${part}]` : `${i === 0 ? "" : "."}${String(part)}`,
    )
    .join("");
}
