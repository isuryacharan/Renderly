// Typed builders mirroring renderly-core's `Command` enum exactly (docs/command-api.md).
// Each returns a plain JSON object matching the `#[serde(tag = "command", rename_all =
// "PascalCase")]` wire shape, ready to pass to `ipc.applyCommand`.

import type {
  ClipTransform,
  EffectInstance,
  KeyframeTrack,
  TrackAudioRole,
  TrackKind,
  ClipTransition,
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
) {
  return { command: "AddTrack", kind, name, id, sequence_id: sequenceId };
}

export function addClip(
  trackId: string,
  mediaId: string,
  positionSecs: number,
  sourceInSecs: number,
  sourceOutSecs: number,
) {
  return {
    command: "AddClip",
    track_id: trackId,
    media_id: mediaId,
    position_secs: positionSecs,
    source_in_secs: sourceInSecs,
    source_out_secs: sourceOutSecs,
  };
}

export function splitClip(trackId: string, clipId: string, atSecs: number) {
  return { command: "SplitClip", track_id: trackId, clip_id: clipId, at_secs: atSecs };
}

export function trimClip(
  trackId: string,
  clipId: string,
  newSourceInSecs: number | null,
  newSourceOutSecs: number | null,
) {
  return {
    command: "TrimClip",
    track_id: trackId,
    clip_id: clipId,
    new_source_in_secs: newSourceInSecs,
    new_source_out_secs: newSourceOutSecs,
  };
}

export function moveClip(
  trackId: string,
  clipId: string,
  newPositionSecs: number,
  newTrackId: string | null = null,
) {
  return {
    command: "MoveClip",
    track_id: trackId,
    clip_id: clipId,
    new_position_secs: newPositionSecs,
    new_track_id: newTrackId,
  };
}

export function deleteClip(trackId: string, clipId: string, ripple: boolean) {
  return { command: "DeleteClip", track_id: trackId, clip_id: clipId, ripple };
}

export function addCaption(
  trackId: string,
  text: string,
  positionSecs: number,
  durationSecs: number,
  styleId: string,
) {
  return {
    command: "AddCaption",
    track_id: trackId,
    text,
    position_secs: positionSecs,
    duration_secs: durationSecs,
    style_id: styleId,
  };
}

export function setCaption(
  trackId: string,
  clipId: string,
  fields: {
    text?: string | null;
    positionSecs?: number | null;
    durationSecs?: number | null;
    styleId?: string | null;
  },
) {
  return {
    command: "SetCaption",
    track_id: trackId,
    clip_id: clipId,
    text: fields.text ?? null,
    position_secs: fields.positionSecs ?? null,
    duration_secs: fields.durationSecs ?? null,
    style_id: fields.styleId ?? null,
  };
}

export function setAudioGain(trackId: string, clipId: string, gainDb: number) {
  return { command: "SetAudioGain", track_id: trackId, clip_id: clipId, gain_db: gainDb };
}

export function setAudioFade(
  trackId: string,
  clipId: string,
  fadeInSecs: number,
  fadeOutSecs: number,
) {
  return {
    command: "SetAudioFade",
    track_id: trackId,
    clip_id: clipId,
    fade_in_secs: fadeInSecs,
    fade_out_secs: fadeOutSecs,
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
  fields: { muted?: boolean | null; locked?: boolean | null; hidden?: boolean | null },
) {
  return {
    command: "SetTrackFlags",
    track_id: trackId,
    muted: fields.muted ?? null,
    locked: fields.locked ?? null,
    hidden: fields.hidden ?? null,
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

export function setClipEffects(trackId: string, clipId: string, effects: EffectInstance[]) {
  return { command: "SetClipEffects", track_id: trackId, clip_id: clipId, effects };
}

export function setClipTransition(
  trackId: string,
  clipId: string,
  transition: ClipTransition | null,
) {
  return {
    command: "SetClipTransition",
    track_id: trackId,
    clip_id: clipId,
    transition,
  };
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
  positionSecs: number,
) {
  return {
    command: "AddStickerFromPack",
    pack_id: packId,
    sticker_id: stickerId,
    track_id: trackId,
    position_secs: positionSecs,
  };
}

export function addSfxFromPack(
  packId: string,
  sfxId: string,
  trackId: string,
  positionSecs: number,
) {
  return {
    command: "AddSfxFromPack",
    pack_id: packId,
    sfx_id: sfxId,
    track_id: trackId,
    position_secs: positionSecs,
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

export function applyTemplate(packId: string, templateId: string, positionSecs: number) {
  return {
    command: "ApplyTemplate",
    pack_id: packId,
    template_id: templateId,
    position_secs: positionSecs,
  };
}

export function generateSticker(
  prompt: string,
  trackId: string,
  positionSecs: number,
  outputPath: string,
) {
  return {
    command: "GenerateSticker",
    prompt,
    track_id: trackId,
    position_secs: positionSecs,
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
  opts: { region?: TrackRegion | null; trackRotationScale?: boolean } = {},
) {
  return {
    command: "TrackMotion",
    track_id: trackId,
    clip_id: clipId,
    sample_count: sampleCount,
    region: opts.region ?? null,
    track_rotation_scale: opts.trackRotationScale ?? false,
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
  styleId: string,
  timelineOffsetSecs = 0,
) {
  return {
    command: "GenerateCaptions",
    media_id: mediaId,
    track_id: trackId,
    style_id: styleId,
    timeline_offset_secs: timelineOffsetSecs,
  };
}

export type VoiceoverProvider =
  | { provider: "piper_local"; voice?: string }
  | { provider: "open_ai"; voice: string };

export function generateVoiceover(
  text: string,
  trackId: string,
  positionSecs: number,
  outputPath: string,
  provider: VoiceoverProvider,
) {
  return {
    command: "GenerateVoiceover",
    text,
    track_id: trackId,
    position_secs: positionSecs,
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

export function addAdjustmentLayer(trackId: string, positionSecs: number, durationSecs: number) {
  return {
    command: "AddAdjustmentLayer",
    track_id: trackId,
    position_secs: positionSecs,
    duration_secs: durationSecs,
  };
}

export function setAdjustmentLayer(
  trackId: string,
  clipId: string,
  fields: { positionSecs?: number | null; durationSecs?: number | null; opacity?: number | null },
) {
  return {
    command: "SetAdjustmentLayer",
    track_id: trackId,
    clip_id: clipId,
    position_secs: fields.positionSecs ?? null,
    duration_secs: fields.durationSecs ?? null,
    opacity: fields.opacity ?? null,
  };
}
