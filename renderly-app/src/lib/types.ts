// TS mirror of renderly-core's project schema v7 (docs/project-schema.md). Keep in sync.

export interface Project {
  schema_version: number;
  id: string;
  name: string;
  settings: ProjectSettings;
  media: MediaItem[];
  tracks: Track[];
  /** Nested sequences (schema v7); each is placed through its `kind: "sequence"` media item. */
  sequences?: Sequence[];
  asset_pack_paths?: string[];
  wasm_plugin_paths?: string[];
  multicam_groups?: MulticamGroup[];
  segmentation_model_path?: string | null;
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

export type Easing = "linear" | "ease_in" | "ease_out" | "ease_in_out" | "bezier";

export interface Keyframe {
  time_secs: number;
  value: number;
  easing?: Easing;
  handle_left?: [number, number];
  handle_right?: [number, number];
}

export interface KeyframeTrack {
  property: AnimProperty;
  keys: Keyframe[];
}

/** Normalised (0..1, origin top-left) rectangle in the clip's video frame, for `TrackMotion`. */
export interface TrackRegion {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface EffectInstance {
  id: string;
  effect_id: string;
  enabled: boolean;
  params: Record<string, number>;
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
  fps: number;
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
  duration_secs: number | null;
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
}

export interface MediaClip {
  type: "video" | "audio";
  id: string;
  media_id: string;
  position_secs: number;
  source_in_secs: number;
  source_out_secs: number;
  gain_db: number;
  enabled: boolean;
  fade_in_secs: number;
  fade_out_secs: number;
  /** Timeline playback rate; duration = (source_out - source_in) / speed. Default 1. */
  speed?: number;
  transform?: ClipTransform;
  keyframes?: KeyframeTrack[];
  effects?: EffectInstance[];
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

/// Index of a per-frame matte sequence: frame `i` belongs to SOURCE time `start_secs + i / fps`.
export interface MatteFrames {
  start_secs: number;
  fps: number;
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
  sync_offsets_secs?: number[];
}

export type TransitionKind =
  | "crossfade"
  | "fade_black"
  | "wipe_left"
  | "wipe_right"
  | "wipe_up"
  | "wipe_down"
  | "slide_left"
  | "slide_right"
  | "iris"
  | "blur_dissolve";

export const TRANSITION_KINDS: { id: TransitionKind; label: string }[] = [
  { id: "crossfade", label: "Crossfade" },
  { id: "fade_black", label: "Fade black" },
  { id: "wipe_left", label: "Wipe left" },
  { id: "wipe_right", label: "Wipe right" },
  { id: "wipe_up", label: "Wipe up" },
  { id: "wipe_down", label: "Wipe down" },
  { id: "slide_left", label: "Slide left" },
  { id: "slide_right", label: "Slide right" },
  { id: "iris", label: "Iris" },
  { id: "blur_dissolve", label: "Blur dissolve" },
];

export interface ClipTransition {
  kind: TransitionKind;
  duration_secs: number;
}

export interface CaptionClip {
  type: "caption";
  id: string;
  text: string;
  position_secs: number;
  duration_secs: number;
  style_id: string;
}

/** Effects applied to everything composited beneath it (schema v7). Video tracks only. */
export interface AdjustmentClip {
  type: "adjustment";
  id: string;
  position_secs: number;
  duration_secs: number;
  enabled: boolean;
  /** 0..1 mix between the unadjusted and adjusted picture. */
  opacity?: number;
  effects?: EffectInstance[];
  /** Limits where the adjustment applies; UVs are output-frame (canvas) coordinates. */
  mask?: ClipMask | null;
  /** Only `opacity` keyframes apply. */
  keyframes?: KeyframeTrack[];
}

export type Clip = MediaClip | CaptionClip | AdjustmentClip;

export function clipId(clip: Clip): string {
  return clip.id;
}

export function clipPositionSecs(clip: Clip): number {
  return clip.position_secs;
}

export function clipDurationSecs(clip: Clip): number {
  if (clip.type === "caption" || clip.type === "adjustment") return clip.duration_secs;
  const speed = clip.speed && clip.speed > 0 ? clip.speed : 1;
  return (clip.source_out_secs - clip.source_in_secs) / speed;
}

export function clipEndSecs(clip: Clip): number {
  return clipPositionSecs(clip) + clipDurationSecs(clip);
}

/** End of the last enabled video/audio/adjustment clip in a sequence (mirrors the engine). */
export function sequenceDurationSecs(project: Project, sequenceId: string): number {
  const seq = project.sequences?.find((s) => s.id === sequenceId);
  if (!seq) return 0;
  let end = 0;
  for (const track of seq.tracks) {
    for (const clip of track.clips) {
      if (clip.type === "caption" || !clip.enabled) continue;
      end = Math.max(end, clipEndSecs(clip));
    }
  }
  return end;
}

/** Duration of a media item; sequence media is derived from its inner timeline. */
export function mediaDurationSecs(project: Project, media: MediaItem): number | null {
  if (media.kind === "sequence") {
    return media.sequence_id ? sequenceDurationSecs(project, media.sequence_id) : null;
  }
  return media.duration_secs;
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
