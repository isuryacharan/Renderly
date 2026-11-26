// The evaluator: what a sequence shows. Pure functions of the project, with no GPU objects, so
// they run in workers and tests, and preview and export read the same answer.
//
// - `evaluateSequence` lowers a sequence to layers of clips over media: what the engine plays.
// - `frameAt` is the frame graph at one time: which media, at which source time, on which layer,
//   with which transform. Its `hash` is stable: two times that show the same thing hash the same.
//
// M1 scope: video tracks only (text and effect tracks draw nothing yet), static transforms
// (keyframes are not evaluated yet), clips at speed 1, no compound clips. Hidden tracks and
// disabled clips draw nothing. Layer 0 is the bottom video track; its sound is what plays until
// the audio engine mixes tracks (M2), unless the track is muted.

import { sequenceDuration } from "./timeline";
import type { Clip, Project, Sequence } from "./schema";
import type { Rate } from "./time";

/** Where a layer sits: centre offset as a fraction of the frame, scale (1 fits), opacity. */
export interface LayerTransform {
  x: number;
  y: number;
  scale: number;
  opacity: number;
}

export const IDENTITY_TRANSFORM: LayerTransform = { x: 0, y: 0, scale: 1, opacity: 1 };

export interface GraphClip {
  clip: string;
  media: string;
  /** Source time at the clip's start, and where its source span ends. */
  in: number;
  out: number;
  start: number;
  transform: LayerTransform;
}

export interface GraphLayer {
  track: string;
  muted: boolean;
  /** Sorted by start, never overlapping. */
  clips: GraphClip[];
}

export interface SequenceGraph {
  sequence: string;
  width: number;
  height: number;
  fps: Rate;
  /** Bottom first. Only tracks that draw something. */
  layers: GraphLayer[];
  /** The sequence's length, including tracks that draw nothing (audio). */
  duration: number;
  /** Clips left out because this version cannot play them yet, with the reason. */
  skipped: { clip: string; reason: "speed" | "compound" | "unavailable" }[];
}

export interface EvaluateOptions {
  /** False for media that cannot be read now (offline, or no decoder): its clips draw nothing. */
  available?: (media: string) => boolean;
}

function transformOf(clip: Clip): LayerTransform {
  const component = (clip.components as { type: string }[]).find((c) => c.type === "transform") as
    { position?: [number, number]; scale?: number; opacity?: number } | undefined;
  if (!component) return IDENTITY_TRANSFORM;
  return {
    x: component.position?.[0] ?? 0,
    y: component.position?.[1] ?? 0,
    scale: component.scale ?? 1,
    opacity: component.opacity ?? 1,
  };
}

export function evaluateSequence(
  project: Project,
  sequenceId: string,
  options: EvaluateOptions = {},
): SequenceGraph {
  const sequence = project.sequences.find((s) => s.id === sequenceId);
  if (!sequence) throw new RangeError(`No sequence ${sequenceId}`);
  const available = options.available ?? (() => true);
  const layers: GraphLayer[] = [];
  const skipped: SequenceGraph["skipped"] = [];
  for (const track of sequence.tracks) {
    if (track.kind !== "video" || track.hidden) continue;
    const clips: GraphClip[] = [];
    for (const clip of track.clips) {
      if (!clip.enabled) continue;
      if (clip.media === undefined) {
        skipped.push({ clip: clip.id, reason: "compound" });
        continue;
      }
      if (clip.speed !== 1) {
        skipped.push({ clip: clip.id, reason: "speed" });
        continue;
      }
      if (!available(clip.media)) {
        skipped.push({ clip: clip.id, reason: "unavailable" });
        continue;
      }
      clips.push({
        clip: clip.id,
        media: clip.media,
        in: clip.in,
        out: clip.in + clip.duration,
        start: clip.start,
        transform: transformOf(clip),
      });
    }
    layers.push({ track: track.id, muted: track.muted, clips });
  }
  return {
    sequence: sequence.id,
    width: sequence.width,
    height: sequence.height,
    fps: sequence.fps,
    layers,
    duration: sequenceDuration(sequence as Sequence),
    skipped,
  };
}

export interface FrameLayer {
  /** Index into `graph.layers`: 0 is the bottom. */
  layer: number;
  track: string;
  clip: string;
  media: string;
  sourceTime: number;
  transform: LayerTransform;
}

export interface FrameGraph {
  t: number;
  width: number;
  height: number;
  /** Bottom first; layers with nothing at `t` are left out. */
  layers: FrameLayer[];
  /** Media read for this frame, without repeats, in layer order. */
  sources: string[];
  /** Stable over everything that decides the picture (not `t` itself, not clip ids). */
  hash: string;
}

/** The clip on a layer at time `t`: start <= t < end. */
function clipAtTime(clips: readonly GraphClip[], t: number): GraphClip | undefined {
  let lo = 0;
  let hi = clips.length - 1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    const clip = clips[mid] as GraphClip;
    if (t < clip.start) hi = mid - 1;
    else if (t >= clip.start + (clip.out - clip.in)) lo = mid + 1;
    else return clip;
  }
  return undefined;
}

export function frameAt(graph: SequenceGraph, t: number): FrameGraph {
  const layers: FrameLayer[] = [];
  graph.layers.forEach((layer, index) => {
    const clip = clipAtTime(layer.clips, t);
    if (!clip) return;
    layers.push({
      layer: index,
      track: layer.track,
      clip: clip.clip,
      media: clip.media,
      sourceTime: clip.in + (t - clip.start),
      transform: clip.transform,
    });
  });
  const sources = [...new Set(layers.map((l) => l.media))];
  const hash = stableHash([
    graph.width,
    graph.height,
    layers.map((l) => [l.layer, l.media, l.sourceTime, l.transform]),
  ]);
  return { t, width: graph.width, height: graph.height, layers, sources, hash };
}

/** A content hash of the whole graph: changes exactly when what the sequence plays changes. */
export function graphHash(graph: SequenceGraph): string {
  return stableHash([
    graph.width,
    graph.height,
    graph.fps,
    graph.duration,
    graph.layers.map((l) => [
      l.muted,
      l.clips.map((c) => [c.media, c.in, c.out, c.start, c.transform]),
    ]),
  ]);
}

/**
 * 64-bit FNV-1a (as two 32-bit halves) of a value's canonical JSON, as 16 hex characters. Object
 * keys are sorted, so the hash does not depend on the order fields were written in.
 */
export function stableHash(value: unknown): string {
  const text = canonical(value);
  let h1 = 0x811c9dc5;
  let h2 = 0x050c5d1f;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ c, 0x01000193);
    h2 = Math.imul(h2 ^ c, 0x01000193) ^ (h1 >>> 15);
  }
  const hex = (n: number) => (n >>> 0).toString(16).padStart(8, "0");
  return hex(h1) + hex(h2);
}

function canonical(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value) ?? "null";
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  const entries = Object.entries(value as Record<string, unknown>)
    .filter(([, v]) => v !== undefined)
    .sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(",")}}`;
}

/** Start times of every cut on drawing layers: where "jump to cut" stops. Sorted, unique. */
export function cutPoints(graph: SequenceGraph): number[] {
  const times = new Set<number>([0]);
  for (const layer of graph.layers)
    for (const clip of layer.clips) {
      times.add(clip.start);
      times.add(clip.start + (clip.out - clip.in));
    }
  return [...times].sort((a, b) => a - b);
}
