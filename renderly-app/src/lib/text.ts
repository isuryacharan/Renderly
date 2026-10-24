// Pure helpers for text (captions and titles), markers, transcripts and snapping (schema v8).
// No store or IPC, so every rule is unit-testable.

import type {
  CaptionClip,
  Clip,
  Marker,
  Project,
  TextAnimations,
  TextBody,
  TextClip,
  TextStyleRef,
  Track,
  Transcript,
} from "./types";

/** Style a caption without one (and without a track style) is drawn with. */
export const DEFAULT_CAPTION_STYLE_REF = "builtin:tiktok-bold-yellow@1";
/** Style a new title starts with. */
export const DEFAULT_TEXT_STYLE_REF = "builtin:tiktok-minimal@1";

/** The builtin caption/title styles the pickers offer (the text renderer ships these four). */
export const BUILTIN_TEXT_STYLES = [
  { id: "tiktok-bold-yellow", label: "Bold yellow" },
  { id: "tiktok-minimal", label: "Minimal" },
  { id: "tiktok-box", label: "Box" },
  { id: "youtube-lower-thirds", label: "Lower thirds" },
] as const;

export function builtinStyleRef(id: string): string {
  return `builtin:${id}@1`;
}

/** A style ref for a new clip or track: just the ref, no params or overrides. */
export function styleFor(ref: string): TextStyleRef {
  return { ref, params: {}, overrides: {} };
}

/** A style ref for a new title: centred, like core default title style. */
export function titleStyleFor(ref: string): TextStyleRef {
  return { ref, params: {}, overrides: { anchor: "center" } };
}

/** The item id of a style ref (`builtin:tiktok-box@1` -> `tiktok-box`, `pack:p/x@2` -> `x`). */
export function styleItemId(ref: string): string {
  const body = ref.replace(/^(builtin|pack):/, "");
  const item = body.slice(body.lastIndexOf("/") + 1);
  const at = item.indexOf("@");
  return at >= 0 ? item.slice(0, at) : item;
}

/** Human label of a style ref: the builtin's name, else the item id made readable. */
export function styleLabel(ref: string): string {
  const id = styleItemId(ref);
  const builtin = BUILTIN_TEXT_STYLES.find((s) => s.id === id);
  if (builtin) return builtin.label;
  const words = id.replace(/[_-]+/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/** The style a caption is drawn with: its own, else its track's, else the default. */
export function effectiveCaptionStyle(track: Track, clip: CaptionClip): TextStyleRef {
  return clip.style ?? track.caption_style ?? styleFor(DEFAULT_CAPTION_STYLE_REF);
}

/** True when the caption follows its track's style (it has none of its own). */
export function inheritsTrackStyle(clip: CaptionClip): boolean {
  return clip.style == null;
}

/** Text clip or caption. */
export function isTextual(clip: Clip): clip is CaptionClip | TextClip {
  return clip.type === "caption" || clip.type === "text";
}

/** Text shown for a caption/title: first line(s) joined for one-line labels. */
export function bodyLabel(body: TextBody | undefined, max = 200): string {
  const text = (body?.text ?? "").replace(/\s*\n\s*/g, " ").trim();
  return text.length > max ? text.slice(0, max) : text;
}

/** Number of phases that have a stored animation. */
export function animationEntries(
  animations: TextAnimations | undefined,
): { phase: "in" | "out" | "loop" | "active"; ref: string; durationUs?: number }[] {
  const out: { phase: "in" | "out" | "loop" | "active"; ref: string; durationUs?: number }[] = [];
  if (!animations) return out;
  for (const phase of ["in", "out", "loop", "active"] as const) {
    const a = animations[phase];
    if (a) out.push({ phase, ref: a.ref, durationUs: a.duration_us });
  }
  return out;
}

// ---- Snapping (mirrors renderly-core `Project::snap_points`) ----

/**
 * The times other edits snap to besides the playhead: clip edges on the root timeline and
 * markers, sorted and de-duplicated. The definition is core's `Project::snap_points()`
 * (renderly-core/src/project/text.rs): every clip's start and end on every root track, plus
 * every marker. `tracks` is the open container's tracks (a nested sequence has no markers);
 * `markers` are the root timeline's.
 */
export function snapPoints(
  tracks: readonly Track[],
  markers: readonly Marker[] | undefined,
  clipEnd: (clip: Clip) => number,
  /** A clip being dragged: its own edges are not targets (core has no such exclusion). */
  excludeClipId?: string,
): number[] {
  const points: number[] = [];
  for (const track of tracks) {
    for (const clip of track.clips) {
      if (clip.id === excludeClipId) continue;
      points.push(clip.start_us, clipEnd(clip));
    }
  }
  for (const m of markers ?? []) points.push(m.time_us);
  points.sort((a, b) => a - b);
  return points.filter((p, i) => i === 0 || p !== points[i - 1]);
}

// ---- Markers ----

/** Default colour of a user marker when none is stored. Palette tokens live in CSS; this is the
 *  colour *value* sent to core (`#RRGGBB`), so it is a data constant, not a theme colour. */
export const MARKER_COLORS = [
  { value: "", label: "Default" },
  { value: "#E5484D", label: "Red" },
  { value: "#F5A524", label: "Orange" },
  { value: "#F7D154", label: "Yellow" },
  { value: "#46A758", label: "Green" },
  { value: "#3E9BF0", label: "Blue" },
  { value: "#8E6CEF", label: "Purple" },
] as const;

/** Marker nearest to `us` within `maxDistUs`, or undefined. */
export function nearestMarker(
  markers: readonly Marker[] | undefined,
  us: number,
  maxDistUs: number,
): Marker | undefined {
  let best: Marker | undefined;
  let bestDist = maxDistUs;
  for (const m of markers ?? []) {
    const d = Math.abs(m.time_us - us);
    if (d <= bestDist) {
      best = m;
      bestDist = d;
    }
  }
  return best;
}

// ---- Transcripts ----

/** Transcript of a media item, if the project has one. */
export function transcriptForMedia(project: Project | null, mediaId: string): Transcript | undefined {
  return project?.transcripts?.find((t) => t.media_id === mediaId);
}

export function deletedWordCount(t: Transcript): number {
  return t.words.reduce((n, w) => n + (w.deleted ? 1 : 0), 0);
}

/** Ids in `selected` that are not yet deleted (what "cut" acts on). */
export function cuttableWordIds(t: Transcript, selected: ReadonlySet<string>): string[] {
  return t.words.filter((w) => selected.has(w.id) && !w.deleted).map((w) => w.id);
}

/** Ids in `selected` that are deleted (what "restore" acts on). */
export function restorableWordIds(t: Transcript, selected: ReadonlySet<string>): string[] {
  return t.words.filter((w) => selected.has(w.id) && w.deleted).map((w) => w.id);
}

/** Word ids from the first to the last of a range selection (shift-click), in transcript order. */
export function wordRange(t: Transcript, fromId: string, toId: string): string[] {
  const a = t.words.findIndex((w) => w.id === fromId);
  const b = t.words.findIndex((w) => w.id === toId);
  if (a < 0 || b < 0) return [];
  const [lo, hi] = a <= b ? [a, b] : [b, a];
  return t.words.slice(lo, hi + 1).map((w) => w.id);
}

// ---- Magnetic tracks ----

/** The track the toolbar magnet acts on: the track holding the selection if it is a video track,
 *  else the first video track of the container. */
export function magnetTargetTrack(tracks: readonly Track[], selectedTrackId?: string | null): Track | undefined {
  const selected = selectedTrackId ? tracks.find((t) => t.id === selectedTrackId) : undefined;
  if (selected && selected.kind === "video") return selected;
  return tracks.find((t) => t.kind === "video");
}
