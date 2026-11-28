// The evaluator: what a sequence shows. Pure functions of the project, with no GPU objects, so
// they run in workers and tests, and preview and export read the same answer.
//
// - `evaluateSequence` lowers a sequence to layers of clips over media: what the engine plays.
// - `frameAt` is the frame graph at one time: which media, at which source time, on which layer,
//   with which transform. Its `hash` is stable: two times that show the same thing hash the same.
//
// Scope so far: video tracks only (text and effect tracks draw nothing yet), clips at speed 1 or
// frozen (speed 0), no compound clips. Hidden tracks and disabled clips draw nothing. Layer 0 is the bottom video track; its sound is what plays until
// the audio engine mixes tracks (M2), unless the track is muted.

import type { BlendMode } from "./components";
import { valueAt, type Key } from "./keyframes";
import { sequenceDuration } from "./timeline";
import type { BackgroundSettings, Clip, Project, Sequence } from "./schema";
import type { Rate } from "./time";

/** How a picture of another shape fills the frame at scale 1 (the transform's `fit`). */
export type Fit = "fit" | "fill" | "stretch";

/**
 * How a layer is drawn: the transform component flattened (the anchor's place as a fraction of
 * the frame from its centre, x right and y down; scale per axis, 1 fits; rotation in degrees
 * clockwise about the anchor; the anchor as a fraction of the layer from its centre; mirroring;
 * opacity; how it fits the frame), then the crop component (each edge as a fraction of the fitted
 * picture) and the blend component's mode.
 */
export interface LayerTransform {
  x: number;
  y: number;
  scaleX: number;
  scaleY: number;
  rotation: number;
  anchorX: number;
  anchorY: number;
  flipX: boolean;
  flipY: boolean;
  opacity: number;
  fit: Fit;
  cropLeft: number;
  cropTop: number;
  cropRight: number;
  cropBottom: number;
  blend: BlendMode;
}

export const IDENTITY_TRANSFORM: LayerTransform = {
  x: 0,
  y: 0,
  scaleX: 1,
  scaleY: 1,
  rotation: 0,
  anchorX: 0,
  anchorY: 0,
  flipX: false,
  flipY: false,
  opacity: 1,
  fit: "fit",
  cropLeft: 0,
  cropTop: 0,
  cropRight: 0,
  cropBottom: 0,
  blend: "normal",
};

/** Animated fields: keyframes per field, in clip time (the crop's edges as cropLeft and so on). */
export type TransformKeys = Partial<
  Record<
    | "position"
    | "scale"
    | "rotation"
    | "anchor"
    | "opacity"
    | "cropLeft"
    | "cropTop"
    | "cropRight"
    | "cropBottom",
    Key[]
  >
>;

export interface GraphClip {
  clip: string;
  media: string;
  /**
   * Source time at the clip's start, and where its source span ends. For a freeze frame only `in`
   * is shown, and `out - in` is just its length on the timeline.
   */
  in: number;
  out: number;
  start: number;
  /** A freeze frame (speed 0): shows source time `in` throughout. */
  freeze?: true;
  /** The transform without animation (and where nothing is keyed). */
  transform: LayerTransform;
  /** Keyframes of the transform's fields, when it is animated. */
  keys?: TransformKeys;
}

/** A transform at clip time `t`: each keyed field from its keys, the rest as set. */
export function transformAt(
  transform: LayerTransform,
  keys: TransformKeys | undefined,
  t: number,
): LayerTransform {
  if (!keys) return transform;
  const at = (field: keyof TransformKeys): number | number[] | undefined => {
    const list = keys[field];
    return list && list.length > 0 ? valueAt(list, t) : undefined;
  };
  const pair = (value: number | number[] | undefined, fallback: [number, number]) =>
    Array.isArray(value) ? [value[0] ?? fallback[0], value[1] ?? fallback[1]] : fallback;
  const position = pair(at("position"), [transform.x, transform.y]);
  const scale = pair(at("scale"), [transform.scaleX, transform.scaleY]);
  const anchor = pair(at("anchor"), [transform.anchorX, transform.anchorY]);
  const rotation = at("rotation");
  const opacity = at("opacity");
  const edge = (field: keyof TransformKeys, fallback: number) => {
    const value = at(field);
    return typeof value === "number" ? Math.min(1, Math.max(0, value)) : fallback;
  };
  return {
    ...transform,
    x: position[0] ?? transform.x,
    y: position[1] ?? transform.y,
    scaleX: Math.max(1e-6, scale[0] ?? transform.scaleX),
    scaleY: Math.max(1e-6, scale[1] ?? transform.scaleY),
    anchorX: anchor[0] ?? transform.anchorX,
    anchorY: anchor[1] ?? transform.anchorY,
    rotation: typeof rotation === "number" ? rotation : transform.rotation,
    opacity: typeof opacity === "number" ? Math.min(1, Math.max(0, opacity)) : transform.opacity,
    cropLeft: edge("cropLeft", transform.cropLeft),
    cropTop: edge("cropTop", transform.cropTop),
    cropRight: edge("cropRight", transform.cropRight),
    cropBottom: edge("cropBottom", transform.cropBottom),
  };
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
  background: BackgroundSettings;
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

interface TransformComponent {
  keys?: Record<string, Key[]>;
  position?: [number, number];
  scale?: [number, number];
  rotation?: number;
  anchor?: [number, number];
  flipX?: boolean;
  flipY?: boolean;
  opacity?: number;
  fit?: Fit;
}

interface CropComponent {
  keys?: Record<string, Key[]>;
  left?: number;
  top?: number;
  right?: number;
  bottom?: number;
}

function componentOf<T>(clip: Clip, type: string): T | undefined {
  return (clip.components as { type: string }[]).find((x) => x.type === type) as T | undefined;
}

function transformOf(clip: Clip): LayerTransform {
  const c = componentOf<TransformComponent>(clip, "transform");
  const crop = componentOf<CropComponent>(clip, "crop");
  const blend = componentOf<{ mode?: BlendMode }>(clip, "blend")?.mode ?? "normal";
  return {
    x: c?.position?.[0] ?? 0,
    y: c?.position?.[1] ?? 0,
    scaleX: c?.scale?.[0] ?? 1,
    scaleY: c?.scale?.[1] ?? 1,
    rotation: c?.rotation ?? 0,
    anchorX: c?.anchor?.[0] ?? 0,
    anchorY: c?.anchor?.[1] ?? 0,
    flipX: c?.flipX ?? false,
    flipY: c?.flipY ?? false,
    opacity: c?.opacity ?? 1,
    fit: c?.fit ?? "fit",
    cropLeft: crop?.left ?? 0,
    cropTop: crop?.top ?? 0,
    cropRight: crop?.right ?? 0,
    cropBottom: crop?.bottom ?? 0,
    blend,
  };
}

const ANIMATABLE = ["position", "scale", "rotation", "anchor", "opacity"] as const;
const CROP_EDGES = [
  ["left", "cropLeft"],
  ["top", "cropTop"],
  ["right", "cropRight"],
  ["bottom", "cropBottom"],
] as const;

/** The keyframes of the transform and the crop, when any field is animated. */
function keysOf(clip: Clip): TransformKeys | undefined {
  const keys: TransformKeys = {};
  const transform = componentOf<TransformComponent>(clip, "transform")?.keys;
  for (const field of ANIMATABLE) {
    const list = transform?.[field];
    if (list && list.length > 0) keys[field] = list;
  }
  const crop = componentOf<CropComponent>(clip, "crop")?.keys;
  for (const [field, name] of CROP_EDGES) {
    const list = crop?.[field];
    if (list && list.length > 0) keys[name] = list;
  }
  return Object.keys(keys).length > 0 ? keys : undefined;
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
      if (clip.speed !== 1 && clip.speed !== 0) {
        skipped.push({ clip: clip.id, reason: "speed" });
        continue;
      }
      if (!available(clip.media)) {
        skipped.push({ clip: clip.id, reason: "unavailable" });
        continue;
      }
      const keys = keysOf(clip);
      clips.push({
        clip: clip.id,
        media: clip.media,
        in: clip.in,
        out: clip.in + clip.duration,
        start: clip.start,
        transform: transformOf(clip),
        ...(clip.speed === 0 ? { freeze: true as const } : {}),
        ...(keys ? { keys } : {}),
      });
    }
    layers.push({ track: track.id, muted: track.muted, clips });
  }
  return {
    sequence: sequence.id,
    width: sequence.width,
    height: sequence.height,
    fps: sequence.fps,
    background: { ...sequence.background },
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
      sourceTime: clip.freeze ? clip.in : clip.in + (t - clip.start),
      transform: transformAt(clip.transform, clip.keys, t - clip.start),
    });
  });
  const sources = [...new Set(layers.map((l) => l.media))];
  const hash = stableHash([
    graph.width,
    graph.height,
    graph.background,
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
    graph.background,
    graph.layers.map((l) => [
      l.muted,
      l.clips.map((c) => [
        c.media,
        c.in,
        c.out,
        c.start,
        c.freeze ?? false,
        c.transform,
        c.keys ?? null,
      ]),
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
