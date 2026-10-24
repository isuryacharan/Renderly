// TS mirror of renderly-core's project schema v8 (docs/project-schema.md). Keep in sync.
// Every time is an integer number of microseconds (`*_us`); `settings.fps` is rational.

import type { Fps } from "./time";

export interface Project {
  schema_version: number;
  id: string;
  name: string;
  settings: ProjectSettings;
  media: MediaItem[];
  tracks: Track[];
  /** Nested sequences (schema v7); each is placed through its `kind: "sequence"` media item. */
  sequences?: Sequence[];
  /** Transitional (schema v8): pack/plugin folders a v7 project listed; hosts adopt them on open. */
  legacy_pack_paths?: string[];
  multicam_groups?: MulticamGroup[];
  segmentation_model_path?: string | null;
  /** Named points on the root timeline (schema v8); omitted when empty. Snapping includes them. */
  markers?: Marker[];
  /** Word-level transcripts of media items, in source media time (schema v8); omitted when empty. */
  transcripts?: Transcript[];
}

export type MarkerKind = "user" | "beat";

export interface Marker {
  id: string;
  time_us: number;
  label: string;
  /** `#RRGGBB`; empty = the default marker colour. */
  color: string;
  kind: MarkerKind;
}

export interface TranscriptWord {
  /** Stable id within the transcript (`w-1`, ...). */
  id: string;
  text: string;
  /** Source media time. */
  start_us: number;
  end_us: number;
  deleted: boolean;
  /** Detection tags: `filler`, `pause`, `repeat`. */
  tags: string[];
}

export interface Transcript {
  id: string;
  media_id: string;
  language: string;
  model: string;
  words: TranscriptWord[];
}

export interface ClipTransform {
  x: number;
  y: number;
  scale_x: number;
  scale_y: number;
  rotation_deg: number;
  opacity: number;
  /** Cover-fit window position across the source, -1..1 (0 = centred). Schema v7, additive. */
  fit_x: number;
  /** As `fit_x`, vertically (-1 = top edge, +1 = bottom edge). */
  fit_y: number;
}

export type AnimProperty =
  | "pos_x"
  | "pos_y"
  | "scale_x"
  | "scale_y"
  | "rotation"
  | "opacity"
  | "volume"
  | "speed"
  | "fit_x"
  | "fit_y";

/** How the span after a key is interpolated (stored form). `ease_in_out` is the legacy v7 curve. */
export type Easing = "linear" | "hold" | "bezier" | "auto" | "ease_in_out";

/** What `SetKeyframeEasing` accepts: the stored forms, plus presets written as `bezier` handles. */
export type EasingChoice =
  | "linear"
  | "hold"
  | "auto"
  | "ease_in"
  | "ease_out"
  | "ease_in_out"
  | { bezier: [number, number, number, number] };

/** A keyframe value: a number, or `[x, y]` / `[r, g, b, a]` for `vec2` / `color` effect params. */
export type KeyValue = number | number[];

export interface Keyframe {
  /** Microseconds, relative to the clip start. */
  time_us: number;
  value: KeyValue;
  easing?: Easing;
  /** `[time_offset_us, value_offset]` relative to this key. */
  handle_left?: [number, number];
  handle_right?: [number, number];
}

/** `pos_x`, ... or `effect:<instance-id>/<param>` (see `effectTrackProperty`). */
export type KeyframeProperty = AnimProperty | `effect:${string}/${string}`;

export interface KeyframeTrack {
  property: KeyframeProperty;
  keys: Keyframe[];
}

/** The control value of one effect parameter, typed as the effect declares it. */
export type EffectParamValue = number | boolean | number[] | string;
export type EffectParams = Record<string, EffectParamValue>;

/** Normalised (0..1, origin top-left) rectangle in the clip's video frame, for `TrackMotion`. */
export interface TrackRegion {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** One effect on a clip, in a slot (`filter`, `key`, `adjust`) or in `effects[]`. */
export interface EffectInstance {
  id: string;
  /** `builtin:<id>@<v>`, `pack:<pack-id>/<item>@<v>`, or `wasm:<plugin>`. */
  ref: string;
  /** Content hash of the pack item when it was applied (pack effects only). */
  hash?: string;
  enabled: boolean;
  params: EffectParams;
}

export type EffectSlot = "filter" | "key" | "adjust";

/** The 13 compositing blend modes; `normal` is omitted from files. */
export type BlendMode =
  | "normal"
  | "multiply"
  | "screen"
  | "overlay"
  | "darken"
  | "lighten"
  | "color_dodge"
  | "color_burn"
  | "soft_light"
  | "hard_light"
  | "difference"
  | "exclusion"
  | "add";

export const BLEND_MODES: { value: BlendMode; label: string }[] = [
  { value: "normal", label: "Normal" },
  { value: "multiply", label: "Multiply" },
  { value: "screen", label: "Screen" },
  { value: "overlay", label: "Overlay" },
  { value: "darken", label: "Darken" },
  { value: "lighten", label: "Lighten" },
  { value: "color_dodge", label: "Color dodge" },
  { value: "color_burn", label: "Color burn" },
  { value: "soft_light", label: "Soft light" },
  { value: "hard_light", label: "Hard light" },
  { value: "difference", label: "Difference" },
  { value: "exclusion", label: "Exclusion" },
  { value: "add", label: "Add" },
];

/** Keyframe property of one effect parameter. */
export function effectTrackProperty(instanceId: string, param: string): KeyframeProperty {
  return `effect:${instanceId}/${param}`;
}

/** `[instance id, param]` for an `effect:<id>/<param>` property, else null. */
export function parseEffectProperty(property: string): [string, string] | null {
  if (!property.startsWith("effect:")) return null;
  const rest = property.slice("effect:".length);
  const slash = rest.indexOf("/");
  if (slash <= 0 || slash === rest.length - 1) return null;
  return [rest.slice(0, slash), rest.slice(slash + 1)];
}

export const IDENTITY_TRANSFORM: ClipTransform = {
  x: 0,
  y: 0,
  scale_x: 1,
  scale_y: 1,
  rotation_deg: 0,
  opacity: 1,
  fit_x: 0,
  fit_y: 0,
};

export interface ProjectSettings {
  fps: Fps;
  width: number;
  height: number;
  sample_rate: number;
  duck_db: number;
}

export type MediaKind = "video" | "audio" | "image" | "sequence";

/** A nested timeline (schema v7). `width`/`height` of null inherit the project size. */
export interface Sequence {
  id: string;
  name: string;
  width?: number | null;
  height?: number | null;
  tracks: Track[];
}

export interface MediaItem {
  id: string;
  path: string;
  kind: MediaKind;
  duration_us: number | null;
  width: number | null;
  height: number | null;
  fps: number | null;
  /** Present iff `kind === "sequence"`; path is empty and duration/size are derived. */
  sequence_id?: string | null;
}

export type TrackKind = "video" | "audio" | "caption";
export type TrackAudioRole = "voiceover" | "dialog" | "music" | "ambience";

export interface Track {
  id: string;
  kind: TrackKind;
  name: string;
  clips: Clip[];
  audio_role?: TrackAudioRole | null;
  muted: boolean;
  locked: boolean;
  hidden: boolean;
  /** Clips stay contiguous from time 0 and core ripples edits (schema v8); omitted when false. */
  magnetic?: boolean;
  /** Caption tracks: the style captions without their own `style` inherit. */
  caption_style?: TextStyleRef | null;
}

export interface MediaClip {
  type: "video" | "audio";
  id: string;
  media_id: string;
  start_us: number;
  source_in_us: number;
  source_out_us: number;
  gain_db: number;
  enabled: boolean;
  fade_in_us: number;
  fade_out_us: number;
  /** Timeline playback rate; duration = (source_out - source_in) / speed. Default 1. */
  speed?: number;
  transform?: ClipTransform;
  keyframes?: KeyframeTrack[];
  /** Effect slots in render order: `filter`, `key`, `adjust`, then `effects[]` (omitted when empty). */
  filter?: EffectInstance | null;
  key?: EffectInstance | null;
  adjust?: EffectInstance | null;
  effects?: EffectInstance[];
  /** How this clip combines with what is beneath; omitted = `normal`. */
  blend_mode?: BlendMode;
  outgoing_transition?: ClipTransition | null;
  mask?: ClipMask | null;
  background_removal?: BackgroundRemoval | null;
  audio_denoise?: AudioDenoise | null;
  multicam_group_id?: string | null;
}

export type ClipMaskKind =
  | { type: "none" }
  | { type: "rect"; x: number; y: number; width: number; height: number }
  | { type: "ellipse"; cx: number; cy: number; rx: number; ry: number }
  | { type: "raster"; path: string }
  | { type: "generated"; cache_dir: string };

export interface ClipMask {
  enabled?: boolean;
  invert?: boolean;
  feather?: number;
  kind: ClipMaskKind;
}

export interface BackgroundRemoval {
  enabled?: boolean;
  model_id?: string;
  threshold?: number;
  feather?: number;
  temporal?: boolean;
  matte_cache_dir?: string | null;
  /// Present when `matte_cache_dir` holds a per-frame matte sequence (model `rvm`).
  matte_frames?: MatteFrames | null;
}

/// Index of a per-frame matte sequence: frame `i` belongs to SOURCE time `start_us + i / fps`.
export interface MatteFrames {
  start_us: number;
  fps: Fps;
  count: number;
  width: number;
  height: number;
  stamp?: number;
}

export interface AudioDenoise {
  enabled?: boolean;
  backend?: string;
  strength?: number;
}

export interface MulticamGroup {
  id: string;
  name: string;
  angle_clip_ids: string[];
  active_angle?: number;
  sync_offsets_us?: number[];
}

/** Easing of a transition's progress; `null` on the transition uses the item's declared default. */
export type ProgressEasing = "linear" | { bezier: [number, number, number, number] };

/** What `SetClipTransition.easing` accepts: the stored forms, plus presets core writes as Bezier points. */
export type TransitionEasingArg = ProgressEasing | "ease_in" | "ease_out" | "ease_in_out";

/** Outgoing transition at the end of a video clip (into the next clip on the same track). */
export interface ClipTransition {
  /** `builtin:<id>@<v>` or `pack:<pack>/<item>@<v>` naming a transition item. */
  ref: string;
  hash?: string;
  duration_us: number;
  params: EffectParams;
  easing: ProgressEasing | null;
}

/** A style reference: `builtin:<id>@<v>` or `pack:<pack>/<item>@<v>`, its knobs and clip overrides. */
export interface TextStyleRef {
  ref: string;
  hash?: string;
  params: Record<string, unknown>;
  overrides: Record<string, unknown>;
}

export interface TextAnimationRef {
  ref: string;
  hash?: string;
  params: Record<string, unknown>;
  duration_us?: number;
}

/** At most one animation per phase. Stored now, rendered in a later release. */
export interface TextAnimations {
  in?: TextAnimationRef;
  out?: TextAnimationRef;
  loop?: TextAnimationRef;
  active?: TextAnimationRef;
}

/** One range of the text with its own style properties (grapheme indices, end-exclusive). */
export interface TextRun {
  start: number;
  end: number;
  style: Record<string, unknown>;
}

/** A recognised word; times are relative to the clip start. */
export interface BodyWord {
  text: string;
  start_us: number;
  end_us: number;
  emphasis?: boolean;
  word_id?: string;
}

/** What is displayed, plus the recognition evidence behind it (never rewritten by text edits). */
export interface TextBody {
  text: string;
  runs: TextRun[];
  words: BodyWord[];
}

export interface TranscriptRef {
  transcript_id: string;
  first_word: string;
  last_word: string;
}

export interface CaptionClip {
  type: "caption";
  id: string;
  start_us: number;
  duration_us: number;
  body: TextBody;
  transcript_ref?: TranscriptRef;
  /** `null` inherits the track's `caption_style`. */
  style: TextStyleRef | null;
  animations: TextAnimations;
}

/** A title on a video track: a layer with transform, keyframes, effect slots (no `key`) and a mask. */
export interface TextClip {
  type: "text";
  id: string;
  start_us: number;
  duration_us: number;
  enabled: boolean;
  body: TextBody;
  style: TextStyleRef;
  animations: TextAnimations;
  transform?: ClipTransform;
  keyframes?: KeyframeTrack[];
  filter?: EffectInstance | null;
  adjust?: EffectInstance | null;
  effects?: EffectInstance[];
  blend_mode?: BlendMode;
  mask?: ClipMask | null;
  outgoing_transition?: ClipTransition | null;
}

/** Effects applied to everything composited beneath it (schema v7). Video tracks only. */
export interface AdjustmentClip {
  type: "adjustment";
  id: string;
  start_us: number;
  duration_us: number;
  enabled: boolean;
  /** 0..1 mix between the unadjusted and adjusted picture. */
  opacity?: number;
  /** Slots in render order: `filter`, `adjust`, then `effects[]` (no `key` slot here). */
  filter?: EffectInstance | null;
  adjust?: EffectInstance | null;
  effects?: EffectInstance[];
  blend_mode?: BlendMode;
  /** Limits where the adjustment applies; UVs are output-frame (canvas) coordinates. */
  mask?: ClipMask | null;
  /** Only `opacity` keyframes apply. */
  keyframes?: KeyframeTrack[];
}

export type Clip = MediaClip | CaptionClip | TextClip | AdjustmentClip;

/** Clips that carry the shared layer fields (effect slots, blend mode, mask, keyframes). */
export type EffectClip = MediaClip | AdjustmentClip | TextClip;

/** Every effect instance of a clip in render order: filter, key, adjust, then `effects[]`. */
export function effectStack(clip: Clip): EffectInstance[] {
  if (clip.type === "caption") return [];
  const out: EffectInstance[] = [];
  if (clip.filter) out.push(clip.filter);
  if (clip.type !== "adjustment" && clip.type !== "text" && clip.key) out.push(clip.key);
  if (clip.adjust) out.push(clip.adjust);
  if (clip.effects) out.push(...clip.effects);
  return out;
}

export function clipId(clip: Clip): string {
  return clip.id;
}

/** Timeline start in µs. */
export function clipStartUs(clip: Clip): number {
  return clip.start_us;
}

/** Timeline duration in µs (rounded; speed keyframes are not integrated here). */
export function clipDurationUs(clip: Clip): number {
  if (clip.type === "caption" || clip.type === "adjustment" || clip.type === "text") return clip.duration_us;
  const speed = clip.speed && clip.speed > 0 ? clip.speed : 1;
  return Math.round((clip.source_out_us - clip.source_in_us) / speed);
}

export function clipEndUs(clip: Clip): number {
  return clipStartUs(clip) + clipDurationUs(clip);
}

/** End of the last enabled video/audio/adjustment clip in a sequence (mirrors the engine). */
export function sequenceDurationUs(project: Project, sequenceId: string): number {
  const seq = project.sequences?.find((s) => s.id === sequenceId);
  if (!seq) return 0;
  let end = 0;
  for (const track of seq.tracks) {
    for (const clip of track.clips) {
      if (clip.type === "caption" || !clip.enabled) continue;
      end = Math.max(end, clipEndUs(clip));
    }
  }
  return end;
}

/** Duration of a media item; sequence media is derived from its inner timeline. */
export function mediaDurationUs(project: Project, media: MediaItem): number | null {
  if (media.kind === "sequence") {
    return media.sequence_id ? sequenceDurationUs(project, media.sequence_id) : null;
  }
  return media.duration_us;
}

/** Name shown for a sequence media item; undefined for ordinary file-backed media. */
export function sequenceMediaName(project: Project, media: MediaItem): string | undefined {
  if (media.kind !== "sequence") return undefined;
  return project.sequences?.find((s) => s.id === media.sequence_id)?.name ?? "Sequence";
}

export const CAPTION_STYLES = [
  "tiktok-bold-yellow",
  "tiktok-minimal",
  "tiktok-box",
  "youtube-lower-thirds",
] as const;

export const CAPTION_STYLE_META: Record<
  (typeof CAPTION_STYLES)[number],
  { label: string; preview: string }
> = {
  "tiktok-bold-yellow": { label: "Bold yellow", preview: "Aa" },
  "tiktok-minimal": { label: "Minimal", preview: "Aa" },
  "tiktok-box": { label: "Box", preview: "Aa" },
  "youtube-lower-thirds": { label: "Lower thirds", preview: "Aa" },
};

export const ASPECT_PRESETS = [
  { id: "9:16", label: "9:16", width: 1080, height: 1920 },
  { id: "16:9", label: "16:9", width: 1920, height: 1080 },
  { id: "1:1", label: "1:1", width: 1080, height: 1080 },
  { id: "4:3", label: "4:3", width: 1440, height: 1080 },
  { id: "3:4", label: "3:4", width: 1080, height: 1440 },
] as const;

export const TRACK_KINDS = ["video", "audio", "caption"] as const;

export interface Selection {
  trackId: string;
  clipId: string;
}
