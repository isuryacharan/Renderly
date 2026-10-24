// Typed builders mirroring renderly-core's `Command` enum exactly (docs/command-api.md).
// Each returns a plain JSON object matching the `#[serde(tag = "command", rename_all =
// "PascalCase")]` wire shape, ready to pass to `ipc.applyCommand`.
//
// Schema v8: every time parameter is a core `Time`; the app always sends integer microseconds
// (the `*Us` arguments). The deprecated float-seconds parameter aliases are never used.

import type {
  BlendMode,
  ClipTransform,
  EasingChoice,
  EffectInstance,
  EffectParams,
  KeyframeProperty,
  KeyframeTrack,
  MarkerKind,
  TextAnimations,
  TextBody,
  TextRun,
  TextStyleRef,
  TransitionEasingArg,
  TrackAudioRole,
  TrackKind,
  TrackRegion,
} from "./types";

export function importMedia(path: string) {
  return { command: "ImportMedia", path };
}

export function addTrack(
  kind: TrackKind,
  name: string,
  id: string | null = null,
  sequenceId: string | null = null,
  magnetic: boolean | null = null,
) {
  const cmd: Record<string, unknown> = { command: "AddTrack", kind, name, id, sequence_id: sequenceId };
  // Omitted: core makes the first video track of the root timeline magnetic, others not.
  if (magnetic !== null) cmd.magnetic = magnetic;
  return cmd;
}

export function addClip(
  trackId: string,
  mediaId: string,
  startUs: number,
  sourceInUs: number,
  sourceOutUs: number,
) {
  return {
    command: "AddClip",
    track_id: trackId,
    media_id: mediaId,
    start: startUs,
    source_in: sourceInUs,
    source_out: sourceOutUs,
  };
}

export function splitClip(trackId: string, clipId: string, atUs: number) {
  return { command: "SplitClip", track_id: trackId, clip_id: clipId, at: atUs };
}

export function trimClip(
  trackId: string,
  clipId: string,
  newSourceInUs: number | null,
  newSourceOutUs: number | null,
) {
  return {
    command: "TrimClip",
    track_id: trackId,
    clip_id: clipId,
    source_in: newSourceInUs,
    source_out: newSourceOutUs,
  };
}

export function moveClip(
  trackId: string,
  clipId: string,
  newStartUs: number,
  newTrackId: string | null = null,
) {
  return {
    command: "MoveClip",
    track_id: trackId,
    clip_id: clipId,
    start: newStartUs,
    new_track_id: newTrackId,
  };
}

export function deleteClip(trackId: string, clipId: string, ripple: boolean) {
  return { command: "DeleteClip", track_id: trackId, clip_id: clipId, ripple };
}

/** A caption's or title's text. `words` are speech timings relative to the clip start; omitted
 *  on a new clip = none, on an existing one = unchanged. */
export interface BodyArg {
  text: string;
  runs?: TextRun[];
  words?: WordArg[];
}

export interface WordArg {
  text: string;
  /** µs relative to the clip start. */
  start: number;
  end: number;
  emphasis?: boolean;
  word_id?: string;
}

/** The command form of a stored body (word times relative to the clip start, µs). `runs` and
 *  `words` are left out when empty. */
export function bodyArg(body: TextBody): BodyArg {
  const arg: BodyArg = { text: body.text };
  if (body.runs && body.runs.length > 0) arg.runs = body.runs;
  if (body.words && body.words.length > 0) {
    arg.words = body.words.map((w) => ({
      text: w.text,
      start: w.start_us,
      end: w.end_us,
      ...(w.emphasis ? { emphasis: true } : {}),
      ...(w.word_id ? { word_id: w.word_id } : {}),
    }));
  }
  return arg;
}

function hasAnimations(a: TextAnimations | undefined): a is TextAnimations {
  return !!a && Object.keys(a).length > 0;
}

export function addCaption(
  trackId: string,
  body: string | BodyArg,
  startUs: number,
  durationUs: number,
  style?: TextStyleRef | null,
  animations?: TextAnimations,
) {
  const cmd: Record<string, unknown> = {
    command: "AddCaption",
    track_id: trackId,
    body: typeof body === "string" ? { text: body } : body,
    start: startUs,
    duration: durationUs,
  };
  // Omitted: the caption inherits its track caption style.
  if (style) cmd.style = style;
  if (hasAnimations(animations)) cmd.animations = animations;
  return cmd;
}

/** Update a caption. `text` goes through `body` (the speech words are kept); `style: null`
 *  inherits the track's style again, `undefined` leaves it alone. */
export function setCaption(
  trackId: string,
  clipId: string,
  fields: {
    text?: string | null;
    startUs?: number | null;
    durationUs?: number | null;
    style?: TextStyleRef | null;
    animations?: TextAnimations;
  },
) {
  const cmd: Record<string, unknown> = {
    command: "SetCaption",
    track_id: trackId,
    clip_id: clipId,
    start: fields.startUs ?? null,
    duration: fields.durationUs ?? null,
  };
  if (fields.text != null) cmd.body = { text: fields.text };
  if (fields.style !== undefined) cmd.style = fields.style;
  if (fields.animations !== undefined) cmd.animations = fields.animations;
  return cmd;
}

export function addText(
  trackId: string,
  body: string | BodyArg,
  startUs: number,
  durationUs: number,
  style?: TextStyleRef | null,
  animations?: TextAnimations,
) {
  const cmd: Record<string, unknown> = {
    command: "AddText",
    track_id: trackId,
    body: typeof body === "string" ? { text: body } : body,
    start: startUs,
    duration: durationUs,
  };
  // Omitted: the default title style, centred.
  if (style) cmd.style = style;
  if (hasAnimations(animations)) cmd.animations = animations;
  return cmd;
}

export function setTextTiming(
  trackId: string,
  clipId: string,
  fields: { startUs?: number | null; durationUs?: number | null },
) {
  return {
    command: "SetTextTiming",
    track_id: trackId,
    clip_id: clipId,
    start: fields.startUs ?? null,
    duration: fields.durationUs ?? null,
  };
}

function clipRefs(targets: { trackId: string; clipId: string }[]) {
  return targets.map((t) => ({ track_id: t.trackId, clip_id: t.clipId }));
}

/** Change the text of captions / titles; speech words are never touched. */
export function setTextBody(targets: { trackId: string; clipId: string }[], text: string) {
  return { command: "SetTextBody", targets: clipRefs(targets), text };
}

/** Switch the style of captions / titles. `ref: null` clears it (a caption inherits its track,
 *  a title returns to the default style). A given `ref` keeps the stored params and overrides. */
export function setTextStyle(
  targets: { trackId: string; clipId: string }[],
  change: {
    ref?: string | null;
    hash?: string | null;
    params?: Record<string, unknown>;
    overrides?: Record<string, unknown>;
  },
) {
  const cmd: Record<string, unknown> = { command: "SetTextStyle", targets: clipRefs(targets) };
  if (change.ref !== undefined) cmd.ref = change.ref;
  if (change.hash !== undefined) cmd.hash = change.hash;
  if (change.params !== undefined) cmd.params = change.params;
  if (change.overrides !== undefined) cmd.overrides = change.overrides;
  return cmd;
}

export function setTextAnimation(
  targets: { trackId: string; clipId: string }[],
  phase: "in" | "out" | "loop" | "active",
  animRef: string | null,
) {
  return { command: "SetTextAnimation", targets: clipRefs(targets), phase, ref: animRef };
}

/** Set (or, with `null`, clear) the style captions of a track inherit. `resetClips` also clears
 *  the clips' own styles so the whole track follows the new style. */
export function setTrackCaptionStyle(
  trackId: string,
  style: TextStyleRef | null,
  resetClips = false,
) {
  return { command: "SetTrackCaptionStyle", track_id: trackId, style, reset_clips: resetClips };
}

export function setCaptionWords(trackId: string, clipId: string, words: WordArg[]) {
  return { command: "SetCaptionWords", track_id: trackId, clip_id: clipId, words };
}

/** Mark (default) or unmark the words at `indices` of a caption's `body.words` as key words. */
export function setWordEmphasis(
  trackId: string,
  clipId: string,
  indices: number[],
  emphasis = true,
) {
  return { command: "SetWordEmphasis", track_id: trackId, clip_id: clipId, indices, emphasis };
}

/** Defaults of `SplitCaptions` (core's): 32 characters per line, 2 lines, 5 seconds per page. */
export const SPLIT_CAPTIONS_DEFAULTS = { maxChars: 32, maxLines: 2, maxDurationUs: 5_000_000 };

export function splitCaptions(
  trackId: string,
  opts: { maxChars?: number; maxLines?: number; maxDurationUs?: number } = {},
) {
  return {
    command: "SplitCaptions",
    track_id: trackId,
    max_chars: opts.maxChars ?? SPLIT_CAPTIONS_DEFAULTS.maxChars,
    max_lines: opts.maxLines ?? SPLIT_CAPTIONS_DEFAULTS.maxLines,
    max_duration: opts.maxDurationUs ?? SPLIT_CAPTIONS_DEFAULTS.maxDurationUs,
  };
}

export interface MarkerArg {
  id?: string;
  timeUs: number;
  label?: string;
  /** `#RRGGBB`; empty = the default colour. */
  color?: string;
  kind?: MarkerKind;
}

export function addMarkers(markers: MarkerArg[]) {
  return {
    command: "AddMarkers",
    markers: markers.map((m) => ({
      ...(m.id ? { id: m.id } : {}),
      time: Math.round(m.timeUs),
      label: m.label ?? "",
      color: m.color ?? "",
      kind: m.kind ?? "user",
    })),
  };
}

export function removeMarkers(ids: string[], kind?: MarkerKind) {
  const cmd: Record<string, unknown> = { command: "RemoveMarkers", ids };
  if (kind) cmd.kind = kind;
  return cmd;
}

/** Mark words (and every word carrying one of `tags`) deleted and cut them out of the timeline. */
export function applyTranscriptCuts(transcriptId: string, wordIds: string[], tags: string[] = []) {
  return {
    command: "ApplyTranscriptCuts",
    transcript_id: transcriptId,
    word_ids: wordIds,
    tags,
  };
}

/** Put deleted words back (all deleted words when `wordIds` is empty). */
export function restoreTranscriptWords(transcriptId: string, wordIds: string[] = []) {
  return { command: "RestoreTranscriptWords", transcript_id: transcriptId, word_ids: wordIds };
}

export function setAudioGain(trackId: string, clipId: string, gainDb: number) {
  return { command: "SetAudioGain", track_id: trackId, clip_id: clipId, gain_db: gainDb };
}

export function setAudioFade(
  trackId: string,
  clipId: string,
  fadeInUs: number,
  fadeOutUs: number,
) {
  return {
    command: "SetAudioFade",
    track_id: trackId,
    clip_id: clipId,
    fade_in: fadeInUs,
    fade_out: fadeOutUs,
  };
}

export function setTrackAudioRole(trackId: string, role: TrackAudioRole | null) {
  return { command: "SetTrackAudioRole", track_id: trackId, role };
}

export function setProjectSettings(fields: {
  width?: number | null;
  height?: number | null;
  fps?: number | null;
}) {
  return {
    command: "SetProjectSettings",
    width: fields.width ?? null,
    height: fields.height ?? null,
    fps: fields.fps ?? null,
  };
}

export function setTrackFlags(
  trackId: string,
  fields: {
    muted?: boolean | null;
    locked?: boolean | null;
    hidden?: boolean | null;
    magnetic?: boolean | null;
  },
) {
  return {
    command: "SetTrackFlags",
    track_id: trackId,
    muted: fields.muted ?? null,
    locked: fields.locked ?? null,
    hidden: fields.hidden ?? null,
    magnetic: fields.magnetic ?? null,
  };
}

export function renameTrack(trackId: string, name: string) {
  return { command: "RenameTrack", track_id: trackId, name };
}

export function deleteTrack(trackId: string) {
  return { command: "DeleteTrack", track_id: trackId };
}

export function moveTrack(trackId: string, newIndex: number) {
  return { command: "MoveTrack", track_id: trackId, new_index: newIndex };
}

export function setClipEnabled(trackId: string, clipId: string, enabled: boolean) {
  return { command: "SetClipEnabled", track_id: trackId, clip_id: clipId, enabled };
}

export function setClipTransform(trackId: string, clipId: string, transform: ClipTransform) {
  return { command: "SetClipTransform", track_id: trackId, clip_id: clipId, transform };
}

export function setClipKeyframes(trackId: string, clipId: string, keyframes: KeyframeTrack[]) {
  return { command: "SetClipKeyframes", track_id: trackId, clip_id: clipId, keyframes };
}

/** An effect to put in a slot. `ref` is the only required field. `id` keeps an existing
 *  instance (so its keyframes stay); omitted `params` keep an existing instance's params, or
 *  write the effect's `initial` values on a new one. */
export interface EffectSpec {
  ref: string;
  id?: string;
  hash?: string;
  enabled?: boolean;
  params?: EffectParams;
}

/** The spec that keeps `fx` exactly as it is (same id, hash, enabled state and params). */
export function effectSpec(fx: EffectInstance): EffectSpec {
  const spec: EffectSpec = { id: fx.id, ref: fx.ref, enabled: fx.enabled, params: fx.params };
  if (fx.hash) spec.hash = fx.hash;
  return spec;
}

/** What `SetClipEffects` changes. A field left `undefined` is unchanged (not sent); `null` clears
 *  that slot; `effects` replaces only the `effects[]` list. */
export interface EffectsChange {
  filter?: EffectSpec | null;
  key?: EffectSpec | null;
  adjust?: EffectSpec | null;
  effects?: EffectSpec[];
  blendMode?: BlendMode;
}

export function setClipEffects(trackId: string, clipId: string, change: EffectsChange) {
  const cmd: Record<string, unknown> = {
    command: "SetClipEffects",
    track_id: trackId,
    clip_id: clipId,
  };
  if (change.filter !== undefined) cmd.filter = change.filter;
  if (change.key !== undefined) cmd.key = change.key;
  if (change.adjust !== undefined) cmd.adjust = change.adjust;
  if (change.effects !== undefined) cmd.effects = change.effects;
  if (change.blendMode !== undefined) cmd.blend_mode = change.blendMode;
  return cmd;
}

/** Replace the `effects[]` list, leaving the slots and blend mode alone. */
export function setEffectsList(trackId: string, clipId: string, effects: EffectInstance[]) {
  return setClipEffects(trackId, clipId, { effects: effects.map(effectSpec) });
}

export function setBlendMode(trackId: string, clipId: string, blendMode: BlendMode) {
  return setClipEffects(trackId, clipId, { blendMode });
}

/** How a key's easing is chosen: a name, or CSS cubic-Bezier control points. The presets are
 *  written by core as `bezier` handles. */
export function setKeyframeEasing(
  trackId: string,
  clipId: string,
  property: KeyframeProperty,
  timeUs: number,
  easing: EasingChoice,
) {
  return {
    command: "SetKeyframeEasing",
    track_id: trackId,
    clip_id: clipId,
    property,
    time: timeUs,
    easing,
  };
}

/** Set or clear the outgoing transition. `ref: null` clears it. `duration` is required when
 *  setting a new transition (an existing one keeps its duration); omitted `params` and `easing`
 *  keep the current ones, `easing: null` uses the transition's declared default. */
export function setClipTransition(
  trackId: string,
  clipId: string,
  ref: string | null,
  opts: { durationUs?: number; params?: EffectParams; easing?: TransitionEasingArg | null } = {},
) {
  const cmd: Record<string, unknown> = {
    command: "SetClipTransition",
    track_id: trackId,
    clip_id: clipId,
    ref,
  };
  if (opts.durationUs !== undefined) cmd.duration = Math.round(opts.durationUs);
  if (opts.params !== undefined) cmd.params = opts.params;
  if (opts.easing !== undefined) cmd.easing = opts.easing;
  return cmd;
}

export function setClipSpeed(trackId: string, clipId: string, speed: number) {
  return { command: "SetClipSpeed", track_id: trackId, clip_id: clipId, speed };
}

export function loadAssetPack(path: string) {
  return { command: "LoadAssetPack", path };
}

export function unloadAssetPack(packId: string) {
  return { command: "UnloadAssetPack", pack_id: packId };
}

export function loadWasmPlugin(path: string) {
  return { command: "LoadWasmPlugin", path };
}

export function unloadWasmPlugin(pluginId: string) {
  return { command: "UnloadWasmPlugin", plugin_id: pluginId };
}

export function addStickerFromPack(
  packId: string,
  stickerId: string,
  trackId: string,
  startUs: number,
) {
  return {
    command: "AddStickerFromPack",
    pack_id: packId,
    sticker_id: stickerId,
    track_id: trackId,
    start: startUs,
  };
}

export function addSfxFromPack(
  packId: string,
  sfxId: string,
  trackId: string,
  startUs: number,
) {
  return {
    command: "AddSfxFromPack",
    pack_id: packId,
    sfx_id: sfxId,
    track_id: trackId,
    start: startUs,
  };
}

export function setClipMask(
  trackId: string,
  clipId: string,
  mask: import("./types").ClipMask | null,
) {
  return { command: "SetClipMask", track_id: trackId, clip_id: clipId, mask };
}

export function setClipBackgroundRemoval(
  trackId: string,
  clipId: string,
  config: import("./types").BackgroundRemoval | null,
) {
  return {
    command: "SetClipBackgroundRemoval",
    track_id: trackId,
    clip_id: clipId,
    config,
  };
}

export function generateBackgroundMatte(trackId: string, clipId: string) {
  return { command: "GenerateBackgroundMatte", track_id: trackId, clip_id: clipId };
}

/// Point a clip at an already-baked `rvm` matte sequence (the app's matte bake runs off the
/// project lock and applies this afterwards; GenerateBackgroundMatte does it for you).
export function attachBackgroundMatte(trackId: string, clipId: string, cacheDir: string) {
  return {
    command: "AttachBackgroundMatte",
    track_id: trackId,
    clip_id: clipId,
    cache_dir: cacheDir,
  };
}

export function setClipAudioDenoise(
  trackId: string,
  clipId: string,
  config: import("./types").AudioDenoise | null,
) {
  return {
    command: "SetClipAudioDenoise",
    track_id: trackId,
    clip_id: clipId,
    config,
  };
}

export function applyTemplate(packId: string, templateId: string, startUs: number) {
  return {
    command: "ApplyTemplate",
    pack_id: packId,
    template_id: templateId,
    start: startUs,
  };
}

export function generateSticker(
  prompt: string,
  trackId: string,
  startUs: number,
  outputPath: string,
) {
  return {
    command: "GenerateSticker",
    prompt,
    track_id: trackId,
    start: startUs,
    output_path: outputPath,
  };
}

export function createMulticamGroup(name: string, clipIds: string[]) {
  return { command: "CreateMulticamGroup", name, clip_ids: clipIds };
}

export function setMulticamAngle(groupId: string, activeAngle: number) {
  return { command: "SetMulticamAngle", group_id: groupId, active_angle: activeAngle };
}

export function trackMotion(
  trackId: string,
  clipId: string,
  sampleCount = 12,
  opts: {
    region?: TrackRegion | null;
    trackRotationScale?: boolean;
    /** Write the keyframes to this video clip instead (it follows the tracked point). */
    target?: { trackId: string; clipId: string } | null;
  } = {},
) {
  return {
    command: "TrackMotion",
    track_id: trackId,
    clip_id: clipId,
    sample_count: sampleCount,
    region: opts.region ?? null,
    track_rotation_scale: opts.trackRotationScale ?? false,
    target: opts.target ? { track_id: opts.target.trackId, clip_id: opts.target.clipId } : null,
  };
}

export function stabilizeClip(
  trackId: string,
  clipId: string,
  opts: { strength?: number | null; sampleCount?: number | null } = {},
) {
  return {
    command: "StabilizeClip",
    track_id: trackId,
    clip_id: clipId,
    strength: opts.strength ?? null,
    sample_count: opts.sampleCount ?? null,
  };
}

export function autoReframeClip(trackId: string, clipId: string, targetAspect: number) {
  return {
    command: "AutoReframeClip",
    track_id: trackId,
    clip_id: clipId,
    target_aspect: targetAspect,
  };
}

export function generateCaptions(
  mediaId: string,
  trackId: string,
  style: TextStyleRef | null,
  timelineOffsetUs = 0,
) {
  const cmd: Record<string, unknown> = {
    command: "GenerateCaptions",
    media_id: mediaId,
    track_id: trackId,
    timeline_offset: timelineOffsetUs,
  };
  // Omitted: the new captions inherit the track's caption style.
  if (style) cmd.style = style;
  return cmd;
}

export type VoiceoverProvider =
  | { provider: "piper_local"; voice?: string }
  | { provider: "open_ai"; voice: string };

export function generateVoiceover(
  text: string,
  trackId: string,
  startUs: number,
  outputPath: string,
  provider: VoiceoverProvider,
) {
  return {
    command: "GenerateVoiceover",
    text,
    track_id: trackId,
    start: startUs,
    output_path: outputPath,
    provider,
  };
}

// ---- Schema v7: nested sequences and adjustment layers ------------------------------------

export function createSequence(
  name: string,
  opts: { width?: number | null; height?: number | null; id?: string | null } = {},
) {
  return {
    command: "CreateSequence",
    name,
    width: opts.width ?? null,
    height: opts.height ?? null,
    id: opts.id ?? null,
  };
}

export function renameSequence(sequenceId: string, name: string) {
  return { command: "RenameSequence", sequence_id: sequenceId, name };
}

/** Both sizes are set as given: null resets that axis to the project size. */
export function setSequenceSettings(
  sequenceId: string,
  size: { width?: number | null; height?: number | null },
) {
  return {
    command: "SetSequenceSettings",
    sequence_id: sequenceId,
    width: size.width ?? null,
    height: size.height ?? null,
  };
}

export function deleteSequence(sequenceId: string) {
  return { command: "DeleteSequence", sequence_id: sequenceId };
}

export function nestClips(clips: { trackId: string; clipId: string }[], name: string) {
  return {
    command: "NestClips",
    clips: clips.map((c) => ({ track_id: c.trackId, clip_id: c.clipId })),
    name,
  };
}

export function unnestClip(trackId: string, clipId: string) {
  return { command: "UnnestClip", track_id: trackId, clip_id: clipId };
}

export function addAdjustmentLayer(trackId: string, startUs: number, durationUs: number) {
  return {
    command: "AddAdjustmentLayer",
    track_id: trackId,
    start: startUs,
    duration: durationUs,
  };
}

export function setAdjustmentLayer(
  trackId: string,
  clipId: string,
  fields: { startUs?: number | null; durationUs?: number | null; opacity?: number | null },
) {
  return {
    command: "SetAdjustmentLayer",
    track_id: trackId,
    clip_id: clipId,
    start: fields.startUs ?? null,
    duration: fields.durationUs ?? null,
    opacity: fields.opacity ?? null,
  };
}
