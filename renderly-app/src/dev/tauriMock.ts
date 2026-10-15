// Browser dev harness ONLY. Aliased in place of the four @tauri-apps modules by
// vite.harness.config.ts so the real app (which needs a running Tauri backend) can be
// rendered and iterated on in a plain browser. Never imported by the production build.

import { compare } from "fast-json-patch";
import type { AdjustmentClip, Clip, MediaClip, Project, Track } from "../lib/types";

// P2: the Rust project schema types every id as a UUID, and the wasm compositor's
// set_project parses the store project with serde — so the sample project's ids must be
// REAL UUID strings, not the readable slugs P1 used. MOCK_IDS keeps the old readable
// names addressable for harness scripts (window-side tests import nothing; use e.g.
// `__store.getState().project.tracks.find(...)` or copy the literals below).
export const MOCK_IDS = {
  "mock-project": "00000000-0000-4000-8000-000000000000",
  "m-round1": "aaaaaaaa-0000-4000-8000-000000000001",
  "m-killcam": "aaaaaaaa-0000-4000-8000-000000000002",
  "m-round2": "aaaaaaaa-0000-4000-8000-000000000003",
  "m-facecam": "aaaaaaaa-0000-4000-8000-000000000004",
  "m-vo": "aaaaaaaa-0000-4000-8000-000000000005",
  "m-bgm": "aaaaaaaa-0000-4000-8000-000000000006",
  "m-logo": "aaaaaaaa-0000-4000-8000-000000000007",
  "t-v2": "bbbbbbbb-0000-4000-8000-000000000001",
  "t-v1": "bbbbbbbb-0000-4000-8000-000000000002",
  "t-a1": "bbbbbbbb-0000-4000-8000-000000000003",
  "t-c1": "bbbbbbbb-0000-4000-8000-000000000004",
  "c-facecam": "cccccccc-0000-4000-8000-000000000001",
  "c-round1": "cccccccc-0000-4000-8000-000000000002",
  "c-killcam": "cccccccc-0000-4000-8000-000000000003",
  "c-round2": "cccccccc-0000-4000-8000-000000000004",
  "c-vo": "cccccccc-0000-4000-8000-000000000005",
  "c-bgm": "cccccccc-0000-4000-8000-000000000006",
  "c-cap1": "cccccccc-0000-4000-8000-000000000007",
  "c-cap2": "cccccccc-0000-4000-8000-000000000008",
  "c-cap3": "cccccccc-0000-4000-8000-000000000009",
  "e1": "eeeeeeee-0000-4000-8000-000000000001",
  // Same-media split-clip crossfade regression (previously untestable — see
  // docs/preview-webview.md "Same-media transitions"): two adjacent clips that BOTH
  // reference m-round1 (the common "split a clip, crossfade the cut" case), on their own
  // track so they never overlap the existing t-v1/t-v2 content in time.
  "t-v3": "bbbbbbbb-0000-4000-8000-000000000005",
  "c-round1a": "cccccccc-0000-4000-8000-00000000000a",
  "c-round1b": "cccccccc-0000-4000-8000-00000000000b",
  // Schema v7 (nested sequences + adjustment layers) sample content: one sequence "Intro"
  // (placed by media item m-intro, a video clip on t-nest) holding a video track with two
  // clips plus its own adjustment layer, and one root adjustment layer on t-adj.
  "m-intro": "aaaaaaaa-0000-4000-8000-000000000008",
  "s-intro": "dddddddd-0000-4000-8000-000000000001",
  "t-intro-v": "bbbbbbbb-0000-4000-8000-000000000006",
  "t-intro-adj": "bbbbbbbb-0000-4000-8000-000000000007",
  "t-nest": "bbbbbbbb-0000-4000-8000-000000000008",
  "t-adj": "bbbbbbbb-0000-4000-8000-000000000009",
  "c-intro-a": "cccccccc-0000-4000-8000-00000000000c",
  "c-intro-b": "cccccccc-0000-4000-8000-00000000000d",
  "c-intro-adj": "cccccccc-0000-4000-8000-00000000000e",
  "c-nested": "cccccccc-0000-4000-8000-00000000000f",
  "c-adj": "cccccccc-0000-4000-8000-000000000010",
  "e2": "eeeeeeee-0000-4000-8000-000000000002",
  "e3": "eeeeeeee-0000-4000-8000-000000000003",
} as const;

const sampleProject: Project = {
  schema_version: 7,
  id: MOCK_IDS["mock-project"],
  name: "Ultra Bruno ep.12",
  settings: { fps: 60, width: 1080, height: 1920, sample_rate: 48000, duck_db: -12 },
  // NOTE: paths here are only ever resolved through `convertFileSrc` below (the harness
  // maps them to the real generated clips in dev-assets/ served at the site root by
  // vite.harness.config.ts's `publicDir`) — durations/dims match those real files
  // (1920x1080@60, 12s) so timeline eval (source_in/out) lines up with what actually
  // decodes. See docs/preview-webview.md "Harness verification".
  media: [
    { id: MOCK_IDS["m-round1"], path: "C:/clips/round1.mp4", kind: "video", duration_secs: 12, width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-killcam"], path: "C:/clips/kill-cam.mp4", kind: "video", duration_secs: 12, width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-round2"], path: "C:/clips/round2.mp4", kind: "video", duration_secs: 12, width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-facecam"], path: "C:/clips/facecam.mp4", kind: "video", duration_secs: 12, width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-vo"], path: "C:/audio/vo-take2.wav", kind: "audio", duration_secs: 65, width: null, height: null, fps: null },
    { id: MOCK_IDS["m-bgm"], path: "C:/audio/bgm-loop.mp3", kind: "audio", duration_secs: 90, width: null, height: null, fps: null },
    { id: MOCK_IDS["m-logo"], path: "C:/img/logo.png", kind: "image", duration_secs: null, width: 512, height: 512, fps: null },
    // Sequence media: path empty, duration/size derived from the inner timeline.
    { id: MOCK_IDS["m-intro"], path: "", kind: "sequence", sequence_id: MOCK_IDS["s-intro"], duration_secs: null, width: null, height: null, fps: null },
  ],
  sequences: [
    {
      id: MOCK_IDS["s-intro"], name: "Intro", width: null, height: null,
      tracks: [
        {
          id: MOCK_IDS["t-intro-v"], kind: "video", name: "Video 1", muted: false, locked: false, hidden: false,
          clips: [
            { type: "video", id: MOCK_IDS["c-intro-a"], media_id: MOCK_IDS["m-round1"], position_secs: 0, source_in_secs: 1, source_out_secs: 4, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0 },
            { type: "video", id: MOCK_IDS["c-intro-b"], media_id: MOCK_IDS["m-killcam"], position_secs: 3, source_in_secs: 2, source_out_secs: 5, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0 },
          ],
        },
        {
          id: MOCK_IDS["t-intro-adj"], kind: "video", name: "Grade", muted: false, locked: false, hidden: false,
          clips: [
            { type: "adjustment", id: MOCK_IDS["c-intro-adj"], position_secs: 1, duration_secs: 4, enabled: true, opacity: 1, effects: [{ id: MOCK_IDS["e3"], effect_id: "builtin:color_adjust", enabled: true, params: { saturation: 0 } }] },
          ],
        },
      ],
    },
  ],
  tracks: [
    {
      id: MOCK_IDS["t-v2"], kind: "video", name: "Overlay", muted: false, locked: false, hidden: false,
      clips: [
        { type: "video", id: MOCK_IDS["c-facecam"], media_id: MOCK_IDS["m-facecam"], position_secs: 10, source_in_secs: 0, source_out_secs: 6.2, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0, transform: { x: 0, y: -0.55, scale_x: 0.32, scale_y: 0.32, rotation_deg: 0, opacity: 1, fit_x: 0, fit_y: 0 } },
      ],
    },
    {
      id: MOCK_IDS["t-v1"], kind: "video", name: "Gameplay", muted: false, locked: false, hidden: false,
      clips: [
        { type: "video", id: MOCK_IDS["c-round1"], media_id: MOCK_IDS["m-round1"], position_secs: 0, source_in_secs: 0, source_out_secs: 9.4, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0, outgoing_transition: { kind: "crossfade", duration_secs: 0.5 } },
        { type: "video", id: MOCK_IDS["c-killcam"], media_id: MOCK_IDS["m-killcam"], position_secs: 9.4, source_in_secs: 2, source_out_secs: 10.3, gain_db: -3, enabled: true, fade_in_secs: 0, fade_out_secs: 0, transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0, opacity: 1, fit_x: 0, fit_y: 0 }, effects: [{ id: MOCK_IDS["e1"], effect_id: "builtin:color_adjust", enabled: true, params: { contrast: 1.1 } }] },
        { type: "video", id: MOCK_IDS["c-round2"], media_id: MOCK_IDS["m-round2"], position_secs: 18, source_in_secs: 0, source_out_secs: 11.1, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0 },
      ],
    },
    {
      id: MOCK_IDS["t-a1"], kind: "audio", name: "Voiceover", audio_role: "voiceover", muted: false, locked: false, hidden: false,
      clips: [
        { type: "audio", id: MOCK_IDS["c-vo"], media_id: MOCK_IDS["m-vo"], position_secs: 2.5, source_in_secs: 0, source_out_secs: 18, gain_db: 0, enabled: true, fade_in_secs: 0.2, fade_out_secs: 0.4 },
        { type: "audio", id: MOCK_IDS["c-bgm"], media_id: MOCK_IDS["m-bgm"], position_secs: 22, source_in_secs: 0, source_out_secs: 12, gain_db: -12, enabled: true, fade_in_secs: 0, fade_out_secs: 1 },
      ],
    },
    {
      // Same-media split-clip crossfade: c-round1a/c-round1b both reference m-round1 (BUG
      // regression case — the pre-fix element pool keyed by media id shared ONE <video> for
      // both sides, so `manageElements` drift-corrected it toward two different source
      // times every frame and the transition window rendered black; see
      // docs/preview-webview.md "Same-media transitions"). Window is [45.0, 45.5).
      id: MOCK_IDS["t-v3"], kind: "video", name: "Split Clip Test", muted: false, locked: false, hidden: false,
      clips: [
        { type: "video", id: MOCK_IDS["c-round1a"], media_id: MOCK_IDS["m-round1"], position_secs: 40, source_in_secs: 0, source_out_secs: 5.5, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0, outgoing_transition: { kind: "crossfade", duration_secs: 0.5 } },
        // source_in deliberately far from the outgoing side's 0..5.5 range so the two
        // sides sample visibly different frames of m-round1 during the blend window —
        // makes a genuine crossfade distinguishable from "both sides show the same frame"
        // (the pre-fix symptom) when pixel-sampled.
        { type: "video", id: MOCK_IDS["c-round1b"], media_id: MOCK_IDS["m-round1"], position_secs: 45.5, source_in_secs: 8.0, source_out_secs: 11.0, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0 },
      ],
    },
    {
      id: MOCK_IDS["t-nest"], kind: "video", name: "Nested", muted: false, locked: false, hidden: false,
      clips: [
        // Intro is 6 s long (c-intro-b ends at 3 + 3), placed untrimmed at speed 1.
        { type: "video", id: MOCK_IDS["c-nested"], media_id: MOCK_IDS["m-intro"], position_secs: 52, source_in_secs: 0, source_out_secs: 6, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0 },
      ],
    },
    {
      id: MOCK_IDS["t-adj"], kind: "video", name: "Adjustments", muted: false, locked: false, hidden: false,
      clips: [
        { type: "adjustment", id: MOCK_IDS["c-adj"], position_secs: 20, duration_secs: 6, enabled: true, opacity: 0.8, effects: [{ id: MOCK_IDS["e2"], effect_id: "builtin:blur", enabled: true, params: { radius: 4 } }] },
      ],
    },
    {
      id: MOCK_IDS["t-c1"], kind: "caption", name: "Captions", muted: false, locked: false, hidden: false,
      clips: [
        { type: "caption", id: MOCK_IDS["c-cap1"], text: "okay so THIS run", position_secs: 3, duration_secs: 2.4, style_id: "tiktok-bold-yellow" },
        { type: "caption", id: MOCK_IDS["c-cap2"], text: "was absolutely cursed", position_secs: 6, duration_secs: 3, style_id: "tiktok-bold-yellow" },
        { type: "caption", id: MOCK_IDS["c-cap3"], text: "watch the corner", position_secs: 11, duration_secs: 2.2, style_id: "tiktok-bold-yellow" },
      ],
    },
  ],
  asset_pack_paths: [],
  wasm_plugin_paths: [],
};

const extensionCatalog = {
  packs: [
    { id: "neon-pack", name: "Neon Gamer Pack", path: "C:/packs/neon", stickers: [{ id: "s1", label: "Boom", default_duration_secs: 1 }, { id: "s2", label: "GG", default_duration_secs: 1.5 }], sfx: [{ id: "sfx1", label: "Whoosh" }], luts: [{ id: "lut1", label: "Neon Night" }], transitions: [], templates: [{ id: "tpl1", label: "Montage Intro" }] },
  ],
  plugins: [
    { id: "glow", name: "Glow FX", path: "C:/plugins/glow", has_frame: true, has_audio: false },
    { id: "bass", name: "Bass Boost", path: "C:/plugins/bass", has_frame: false, has_audio: true },
  ],
};

let revision = 1;

let mockModelInstalled = false;
function mockModelStatus(id: string) {
  return {
    id,
    name: "Robust Video Matting (MobileNetV3, fp32)",
    license_spdx: "GPL-3.0-only",
    license_url: "https://github.com/PeterL1n/RobustVideoMatting/blob/master/LICENSE",
    source_url: "https://github.com/PeterL1n/RobustVideoMatting",
    download_host: "github.com",
    size_bytes: 14_975_696,
    sha256: "88d4531297118f595bf2fd60f6f566aec2e559393802d1f436c380f0cbbd2828",
    installed: mockModelInstalled,
    path: mockModelInstalled ? "C:/mock/models/rvm_mobilenetv3_fp32.onnx" : null,
    cache_dir: "C:/Users/you/AppData/Local/renderly/models",
  };
}

// ---- Mutable project state + a tiny command applier, so `apply_command`/`apply_commands`
// below can echo a REAL RFC-6902 patch (via fast-json-patch's `compare`, the same diff
// approach the real backend's `json_patch::diff` uses) instead of the empty `patch: []`
// this mock used to return unconditionally. That distinction matters: the store
// (editorStore.ts's `dispatch`/`dispatchBatch`) applies the returned patch itself and
// skips its post-mutation refetch — an empty patch made every mock-driven edit silently
// no-op the store's project, which would have hidden bug 1/2's real root cause (a broken
// `applyPatch` call) from harness testing. Only the command shapes exercised by the app's
// UI/harness scripts are implemented; unrecognized commands log a warning and no-op rather
// than throwing, so an unhandled command degrades to "nothing visibly changed" instead of
// breaking the mock's other commands or hanging the promise.
let currentProject: Project = structuredClone(sampleProject);
// Project snapshots for the mock's undo/redo (history is global, across every container).
const undoStack: Project[] = [];
const redoStack: Project[] = [];

// Track ids are globally unique across the root timeline and every sequence (as in the
// engine), so commands address a track with no container parameter.
function mockFindTrack(project: Project, trackId: string): Track | undefined {
  return mockContainers(project)
    .flatMap((c) => c.tracks)
    .find((t) => t.id === trackId);
}

/** Every timeline container: the root (`id: null`) and each sequence. */
function mockContainers(project: Project): { id: string | null; tracks: Track[] }[] {
  return [
    { id: null, tracks: project.tracks },
    ...(project.sequences ?? []).map((s) => ({ id: s.id, tracks: s.tracks })),
  ];
}

/** The container that holds `trackId`, as a mutable `Track[]` owner. */
function mockContainerOfTrack(project: Project, trackId: string): { id: string | null; tracks: Track[] } | undefined {
  return mockContainers(project).find((c) => c.tracks.some((t) => t.id === trackId));
}

function mockSetTracks(project: Project, containerId: string | null, tracks: Track[]): void {
  if (containerId == null) project.tracks = tracks;
  else {
    const seq = project.sequences?.find((s) => s.id === containerId);
    if (seq) seq.tracks = tracks;
  }
}

function mockClipDuration(clip: Clip): number {
  if (clip.type === "caption" || clip.type === "adjustment") return clip.duration_secs;
  const speed = clip.speed && clip.speed > 0 ? clip.speed : 1;
  return (clip.source_out_secs - clip.source_in_secs) / speed;
}

function mockSequenceDuration(project: Project, sequenceId: string): number {
  const seq = project.sequences?.find((s) => s.id === sequenceId);
  if (!seq) return 0;
  let end = 0;
  for (const t of seq.tracks)
    for (const c of t.clips) if (c.type !== "caption" && c.enabled) end = Math.max(end, c.position_secs + mockClipDuration(c));
  return end;
}

function mockOverlaps(track: Track, pos: number, dur: number, ignoreId?: string): boolean {
  return track.clips.some(
    (c) => c.id !== ignoreId && pos < c.position_secs + mockClipDuration(c) - 1e-9 && c.position_secs < pos + dur - 1e-9,
  );
}

/** Whether placing `sequenceId` inside `containerId` would make a sequence contain itself. */
function mockWouldCycle(project: Project, containerId: string | null, sequenceId: string): boolean {
  if (containerId == null) return false;
  if (containerId === sequenceId) return true;
  const seq = project.sequences?.find((s) => s.id === sequenceId);
  if (!seq) return false;
  for (const t of seq.tracks)
    for (const c of t.clips) {
      if (c.type !== "video" && c.type !== "audio") continue;
      const inner = project.media.find((m) => m.id === c.media_id);
      if (inner?.kind === "sequence" && inner.sequence_id && mockWouldCycle(project, containerId, inner.sequence_id)) return true;
    }
  return false;
}

function mockReferencesSequence(project: Project, sequenceId: string): boolean {
  const mediaId = project.media.find((m) => m.kind === "sequence" && m.sequence_id === sequenceId)?.id;
  return mockContainers(project).some((c) =>
    c.tracks.some((t) => t.clips.some((cl) => (cl.type === "video" || cl.type === "audio") && cl.media_id === mediaId)),
  );
}

function mockFindClip(track: Track | undefined, clipId: string): Clip | undefined {
  return track?.clips.find((c) => c.id === clipId);
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function applyMockCommand(project: Project, cmd: any): void {
  switch (cmd.command) {
    case "AddTrack": {
      if (cmd.sequence_id) {
        if (!project.sequences?.some((s) => s.id === cmd.sequence_id)) throw new Error(`sequence not found: ${cmd.sequence_id}`);
        if (cmd.kind === "caption") throw new Error("not supported for sequences (schema v7): caption tracks are root-only");
      }
      const track: Track = {
        id: cmd.id ?? crypto.randomUUID(),
        kind: cmd.kind,
        name: cmd.name,
        muted: false,
        locked: false,
        hidden: false,
        clips: [],
      };
      if (cmd.sequence_id) project.sequences!.find((s) => s.id === cmd.sequence_id)!.tracks.push(track);
      else project.tracks.push(track);
      break;
    }
    case "AddClip": {
      const track = mockFindTrack(project, cmd.track_id);
      const media = project.media.find((m) => m.id === cmd.media_id);
      if (!track || !media) break;
      if (media.kind === "sequence" && media.sequence_id) {
        const container = mockContainerOfTrack(project, cmd.track_id);
        if (mockWouldCycle(project, container?.id ?? null, media.sequence_id))
          throw new Error(`placing sequence ${media.sequence_id} here would make a sequence contain itself`);
      }
      if (track.kind === "caption") throw new Error("track kind mismatch");
      if (mockOverlaps(track, cmd.position_secs, cmd.source_out_secs - cmd.source_in_secs))
        throw new Error(`clip range [${cmd.position_secs}, ${cmd.position_secs + cmd.source_out_secs - cmd.source_in_secs}) overlaps an existing clip on track ${track.id}`);
      const clip: MediaClip = {
        type: track.kind === "audio" ? "audio" : "video",
        id: crypto.randomUUID(),
        media_id: cmd.media_id,
        position_secs: cmd.position_secs,
        source_in_secs: cmd.source_in_secs,
        source_out_secs: cmd.source_out_secs,
        gain_db: 0,
        enabled: true,
        fade_in_secs: 0,
        fade_out_secs: 0,
      };
      track.clips.push(clip);
      break;
    }
    case "AddCaption": {
      const track = mockFindTrack(project, cmd.track_id);
      if (!track) break;
      track.clips.push({
        type: "caption",
        id: crypto.randomUUID(),
        text: cmd.text,
        position_secs: cmd.position_secs,
        duration_secs: cmd.duration_secs,
        style_id: cmd.style_id,
      });
      break;
    }
    case "DeleteClip": {
      const track = mockFindTrack(project, cmd.track_id);
      if (!track) break;
      track.clips = track.clips.filter((c) => c.id !== cmd.clip_id);
      break;
    }
    case "DeleteTrack": {
      const container = mockContainerOfTrack(project, cmd.track_id);
      if (container) mockSetTracks(project, container.id, container.tracks.filter((t) => t.id !== cmd.track_id));
      break;
    }
    case "RenameTrack": {
      const track = mockFindTrack(project, cmd.track_id);
      if (track) track.name = cmd.name;
      break;
    }
    case "MoveTrack": {
      // Mirrors renderly-core's move_track: remove from current index, clamp new_index to
      // the valid range, reinsert. No-op if the clamped index equals the current one.
      const container = mockContainerOfTrack(project, cmd.track_id);
      if (!container) break;
      const list = container.tracks;
      const idx = list.findIndex((t) => t.id === cmd.track_id);
      const clamped = Math.min(cmd.new_index, list.length - 1);
      if (clamped === idx) break;
      const [track] = list.splice(idx, 1);
      list.splice(clamped, 0, track);
      break;
    }
    case "SetTrackFlags": {
      const track = mockFindTrack(project, cmd.track_id);
      if (!track) break;
      if (cmd.muted != null) track.muted = cmd.muted;
      if (cmd.locked != null) track.locked = cmd.locked;
      if (cmd.hidden != null) track.hidden = cmd.hidden;
      break;
    }
    case "SetClipEnabled": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.enabled = cmd.enabled;
      break;
    }
    case "SetClipTransform": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) clip.transform = cmd.transform;
      break;
    }
    case "SetClipKeyframes": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type === "adjustment") {
        if ((cmd.keyframes as { property: string }[]).some((k) => k.property !== "opacity"))
          throw new Error("invalid adjustment layer: only opacity keyframes apply");
        clip.keyframes = cmd.keyframes;
      } else if (clip && (clip.type === "video" || clip.type === "audio")) clip.keyframes = cmd.keyframes;
      break;
    }
    case "SetClipEffects": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type === "adjustment") {
        if ((cmd.effects as { effect_id: string }[]).some((e) => e.effect_id.startsWith("wasm:")))
          throw new Error("invalid adjustment layer: WASM plugin effects cannot run on an adjustment layer");
        clip.effects = cmd.effects;
      } else if (clip && (clip.type === "video" || clip.type === "audio")) clip.effects = cmd.effects;
      break;
    }
    case "SetClipTransition": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) clip.outgoing_transition = cmd.transition;
      break;
    }
    case "SetClipSpeed": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) clip.speed = cmd.speed;
      break;
    }
    case "SetAudioGain": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) clip.gain_db = cmd.gain_db;
      break;
    }
    case "SetAudioFade": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) {
        clip.fade_in_secs = cmd.fade_in_secs;
        clip.fade_out_secs = cmd.fade_out_secs;
      }
      break;
    }
    case "SetCaption": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type === "caption") {
        if (cmd.text != null) clip.text = cmd.text;
        if (cmd.position_secs != null) clip.position_secs = cmd.position_secs;
        if (cmd.duration_secs != null) clip.duration_secs = cmd.duration_secs;
        if (cmd.style_id != null) clip.style_id = cmd.style_id;
      }
      break;
    }
    case "SetClipMask": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.mask = cmd.mask;
      break;
    }
    case "SetClipBackgroundRemoval": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) clip.background_removal = cmd.config;
      break;
    }
    case "GenerateBackgroundMatte":
    case "AttachBackgroundMatte": {
      // Pretend a baked matte sequence exists so the inspector shows its Ready state.
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio") && clip.background_removal) {
        clip.background_removal.matte_cache_dir = "C:/mock/matte-cache";
        if (clip.background_removal.model_id === "rvm") {
          clip.background_removal.matte_frames = {
            start_secs: clip.source_in_secs,
            fps: 30,
            count: Math.round((clip.source_out_secs - clip.source_in_secs) * 30) + 1,
            width: 1280,
            height: 720,
            stamp: Date.now(),
          };
        }
      }
      break;
    }
    case "MoveClip": {
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      if (!track || !clip) break;
      {
        const dest = cmd.new_track_id ? mockFindTrack(project, cmd.new_track_id) : track;
        if (dest && mockOverlaps(dest, cmd.new_position_secs, mockClipDuration(clip), clip.id))
          throw new Error(`clip range overlaps an existing clip on track ${dest.id}`);
      }
      clip.position_secs = cmd.new_position_secs;
      if (cmd.new_track_id && cmd.new_track_id !== cmd.track_id) {
        const dest = mockFindTrack(project, cmd.new_track_id);
        if (dest) {
          track.clips = track.clips.filter((c) => c.id !== cmd.clip_id);
          dest.clips.push(clip);
        }
      }
      break;
    }
    case "TrimClip": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) {
        if (cmd.new_source_in_secs != null) clip.source_in_secs = cmd.new_source_in_secs;
        if (cmd.new_source_out_secs != null) clip.source_out_secs = cmd.new_source_out_secs;
      }
      break;
    }
    case "SetProjectSettings": {
      if (cmd.width != null) project.settings.width = cmd.width;
      if (cmd.height != null) project.settings.height = cmd.height;
      if (cmd.fps != null) project.settings.fps = cmd.fps;
      break;
    }
    case "SplitClip": {
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      if (!track || !clip) break;
      const at: number = cmd.at_secs;
      const dur = mockClipDuration(clip);
      if (at <= clip.position_secs + 1e-6 || at >= clip.position_secs + dur - 1e-6) throw new Error("invalid split point");
      const left = at - clip.position_secs;
      const right = structuredClone(clip) as Clip;
      right.id = crypto.randomUUID();
      right.position_secs = at;
      if (clip.type === "adjustment" || clip.type === "caption") {
        clip.duration_secs = left;
        (right as AdjustmentClip).duration_secs = dur - left;
      } else {
        const speed = clip.speed && clip.speed > 0 ? clip.speed : 1;
        const cut = clip.source_in_secs + left * speed;
        (right as MediaClip).source_in_secs = cut;
        clip.source_out_secs = cut;
      }
      track.clips.push(right);
      break;
    }
    case "CreateSequence": {
      const id: string = cmd.id ?? crypto.randomUUID();
      if (project.sequences?.some((s) => s.id === id)) throw new Error(`sequence id ${id} already exists`);
      if (cmd.width === 0 || cmd.height === 0) throw new Error("invalid dimensions");
      (project.sequences ??= []).push({ id, name: cmd.name, width: cmd.width ?? null, height: cmd.height ?? null, tracks: [] });
      project.media.push({ id: crypto.randomUUID(), path: "", kind: "sequence", sequence_id: id, duration_secs: null, width: null, height: null, fps: null });
      break;
    }
    case "RenameSequence": {
      const seq = project.sequences?.find((s) => s.id === cmd.sequence_id);
      if (!seq) throw new Error(`sequence not found: ${cmd.sequence_id}`);
      seq.name = cmd.name;
      break;
    }
    case "SetSequenceSettings": {
      const seq = project.sequences?.find((s) => s.id === cmd.sequence_id);
      if (!seq) throw new Error(`sequence not found: ${cmd.sequence_id}`);
      if (cmd.width === 0 || cmd.height === 0) throw new Error("invalid dimensions");
      seq.width = cmd.width ?? null;
      seq.height = cmd.height ?? null;
      break;
    }
    case "DeleteSequence": {
      if (!project.sequences?.some((s) => s.id === cmd.sequence_id)) throw new Error(`sequence not found: ${cmd.sequence_id}`);
      if (mockReferencesSequence(project, cmd.sequence_id))
        throw new Error(`sequence ${cmd.sequence_id} is still placed by at least one clip`);
      project.sequences = project.sequences.filter((s) => s.id !== cmd.sequence_id);
      project.media = project.media.filter((m) => !(m.kind === "sequence" && m.sequence_id === cmd.sequence_id));
      break;
    }
    case "NestClips": {
      const refs = cmd.clips as { track_id: string; clip_id: string }[];
      if (refs.length === 0) throw new Error("cannot nest: no clips given");
      const container = mockContainerOfTrack(project, refs[0].track_id);
      if (!container) throw new Error(`track not found: ${refs[0].track_id}`);
      const picked: { track: Track; clip: Clip }[] = [];
      for (const r of refs) {
        if (mockContainerOfTrack(project, r.track_id)?.id !== container.id) throw new Error("cannot nest: clips must share one timeline");
        const track = mockFindTrack(project, r.track_id)!;
        const clip = mockFindClip(track, r.clip_id);
        if (!clip) throw new Error(`clip not found: ${r.clip_id}`);
        if (clip.type === "caption") throw new Error("not supported for sequences (schema v7): caption clips cannot be nested");
        picked.push({ track, clip });
      }
      const start = Math.min(...picked.map((p) => p.clip.position_secs));
      const end = Math.max(...picked.map((p) => p.clip.position_secs + mockClipDuration(p.clip)));
      const seqId = crypto.randomUUID();
      const mediaId = crypto.randomUUID();
      // Inner tracks mirror the source tracks (kind/name/flags); clips keep their ids and are
      // shifted so the sequence starts at 0.
      const inner: Track[] = [];
      for (const src of container.tracks) {
        const mine = picked.filter((p) => p.track.id === src.id);
        if (mine.length === 0) continue;
        inner.push({ ...src, id: crypto.randomUUID(), clips: mine.map((p) => ({ ...p.clip, position_secs: p.clip.position_secs - start })) });
      }
      for (const p of picked) p.track.clips = p.track.clips.filter((c) => c.id !== p.clip.id);
      (project.sequences ??= []).push({ id: seqId, name: cmd.name, width: null, height: null, tracks: inner });
      project.media.push({ id: mediaId, path: "", kind: "sequence", sequence_id: seqId, duration_secs: null, width: null, height: null, fps: null });
      const hasVideo = picked.some((p) => p.track.kind === "video");
      const hasAudio = picked.some((p) => p.track.kind === "audio");
      const place = (kind: "video" | "audio") => {
        const candidates = container.tracks.filter((t) => t.kind === kind);
        let target = [...candidates].reverse().find((t) => !mockOverlaps(t, start, end - start));
        if (!target) {
          target = { id: crypto.randomUUID(), kind, name: kind === "video" ? "Nested video" : "Nested audio", muted: false, locked: false, hidden: false, clips: [] };
          container.tracks.push(target);
        }
        target.clips.push({ type: kind, id: crypto.randomUUID(), media_id: mediaId, position_secs: start, source_in_secs: 0, source_out_secs: end - start, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0 });
      };
      if (hasVideo) place("video");
      if (hasAudio) place("audio");
      break;
    }
    case "UnnestClip": {
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      const container = mockContainerOfTrack(project, cmd.track_id);
      if (!track || !clip || !container) throw new Error(`clip not found: ${cmd.clip_id}`);
      const media = clip.type === "video" || clip.type === "audio" ? project.media.find((m) => m.id === clip.media_id) : undefined;
      const seq = media?.kind === "sequence" ? project.sequences?.find((s) => s.id === media.sequence_id) : undefined;
      if (!seq || (clip.type !== "video" && clip.type !== "audio")) throw new Error("cannot unnest: the clip does not place a sequence");
      const fullDur = mockSequenceDuration(project, seq.id);
      if (clip.source_in_secs > 1e-6 || Math.abs(clip.source_out_secs - fullDur) > 1e-6) throw new Error("cannot unnest: the clip is trimmed");
      if (clip.speed && Math.abs(clip.speed - 1) > 1e-6) throw new Error("cannot unnest: the clip has a speed change");
      if ((clip.effects?.length ?? 0) > 0) throw new Error("cannot unnest: the clip has effects");
      const wantAudio = clip.type === "audio";
      const copied: Track[] = [];
      for (const inner of seq.tracks) {
        if ((inner.kind === "audio") !== wantAudio || inner.kind === "caption") continue;
        copied.push({ ...inner, id: crypto.randomUUID(), clips: inner.clips.map((c) => ({ ...structuredClone(c), id: crypto.randomUUID(), position_secs: c.position_secs + clip.position_secs })) });
      }
      track.clips = track.clips.filter((c) => c.id !== clip.id);
      const at = container.tracks.findIndex((t) => t.id === track.id);
      container.tracks.splice(at + 1, 0, ...copied);
      break;
    }
    case "AddAdjustmentLayer": {
      const track = mockFindTrack(project, cmd.track_id);
      if (!track) throw new Error(`track not found: ${cmd.track_id}`);
      if (track.kind !== "video") throw new Error("invalid adjustment layer: adjustment layers go on video tracks");
      if (!(cmd.duration_secs > 0)) throw new Error("invalid duration");
      if (mockOverlaps(track, cmd.position_secs, cmd.duration_secs))
        throw new Error(`clip range [${cmd.position_secs}, ${cmd.position_secs + cmd.duration_secs}) overlaps an existing clip on track ${track.id}`);
      track.clips.push({ type: "adjustment", id: crypto.randomUUID(), position_secs: cmd.position_secs, duration_secs: cmd.duration_secs, enabled: true, opacity: 1, effects: [], mask: null, keyframes: [] });
      break;
    }
    case "SetAdjustmentLayer": {
      if (cmd.position_secs == null && cmd.duration_secs == null && cmd.opacity == null)
        throw new Error("SetAdjustmentLayer requires at least one of position_secs/duration_secs/opacity");
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      if (!track || !clip) throw new Error(`clip not found: ${cmd.clip_id}`);
      if (clip.type !== "adjustment") throw new Error(`clip ${clip.id} is not an adjustment layer`);
      const pos = cmd.position_secs ?? clip.position_secs;
      const dur = cmd.duration_secs ?? clip.duration_secs;
      if (pos < 0 || !(dur > 0)) throw new Error("invalid adjustment layer: bad position or duration");
      if (cmd.opacity != null && (cmd.opacity < 0 || cmd.opacity > 1)) throw new Error("invalid adjustment layer: opacity out of range");
      if (mockOverlaps(track, pos, dur, clip.id)) throw new Error(`clip range overlaps an existing clip on track ${track.id}`);
      clip.position_secs = pos;
      clip.duration_secs = dur;
      if (cmd.opacity != null) clip.opacity = cmd.opacity;
      break;
    }
    default:
      console.warn(`[tauriMock] apply_command: unhandled command "${cmd.command}" — no-op`);
  }
}

// ---- Tiny event bus so `listen` actually delivers events (P1 harness upgrade —
// see docs/preview-webview.md "Harness verification"). Real Tauri's `listen` subscribes
// to backend-emitted events; here `emit` (below) is the only producer, driven by the
// mock `play`/`pause`/`seek` handling.
type Listener = (payload: unknown) => void;
const listeners = new Map<string, Set<Listener>>();

function emit(event: string, payload: unknown): void {
  for (const cb of listeners.get(event) ?? []) cb(payload);
}

// ---- Mock playback clock: `play` starts a ~33ms interval emitting `playback:tick` /
// `playback:state` (mirrors the real backend's TICK_INTERVAL, see playback.rs) so the
// harness can exercise WebviewPreviewEngine's tick drift-correction path in addition to
// its pure-rAF fallback. `pause` stops it.
let playTimer: ReturnType<typeof setInterval> | null = null;
let mockTimeSecs = 0;

function stopMockPlayback(): number {
  if (playTimer != null) {
    clearInterval(playTimer);
    playTimer = null;
  }
  return mockTimeSecs;
}

function startMockPlayback(fromSecs: number): void {
  stopMockPlayback();
  mockTimeSecs = fromSecs;
  const start = performance.now();
  const base = fromSecs;
  playTimer = setInterval(() => {
    mockTimeSecs = base + (performance.now() - start) / 1000;
    emit("playback:tick", { time_secs: mockTimeSecs, playing: true });
  }, 33);
  emit("playback:state", { playing: true, time_secs: fromSecs });
}

// ---- Scripted fake agent (agent chat, docs/agent-chat-design.md). Stands in for the Rust
// agent_chat backend so the Agent tab can be exercised in the browser harness. Prompts with
// "export" script a permission prompt; any other prompt trims the first video clip for real
// through the mock's apply_command (so the edit card's Undo works against the mock's stack).
// Nothing here talks to a model or a vendor CLI.
const mockAgent = {
  sessionId: null as string | null,
  turn: 0,
  pending: new Map<string, (option: string) => void>(),
};

const MOCK_AGENTS = [
  {
    id: "claude-code",
    name: "Claude Code",
    installed: true,
    version: "2.1.274 (Claude Code)",
    signIn: "signed_in",
    signInCommand: "claude auth login",
    installHint: "Install Claude Code (see https://code.claude.com/docs/en/setup), then sign in.",
    note: null,
  },
  {
    id: "codex",
    name: "Codex",
    installed: false,
    version: null,
    signIn: "unknown",
    signInCommand: "codex login",
    installHint: "Install the Codex CLI from OpenAI, then run `codex login`.",
    note: "Codex support is untested against the real CLI.",
  },
];

function agentEmit(event: Record<string, unknown>): void {
  if (mockAgent.sessionId) emit("agent:event", { session_id: mockAgent.sessionId, ...event });
}

function firstVideoClip(): { trackId: string; clipId: string; sourceOut: number } | null {
  for (const track of currentProject.tracks) {
    if (track.kind !== "video") continue;
    const clip = track.clips.find((c) => c.type === "video");
    if (clip && clip.type === "video") {
      return { trackId: track.id, clipId: clip.id, sourceOut: clip.source_out_secs };
    }
  }
  return null;
}

async function runMockTurn(prompt: string): Promise<void> {
  const turn = ++mockAgent.turn;
  const live = () => mockAgent.turn === turn && mockAgent.sessionId !== null;
  const pause = (ms: number) => new Promise((r) => setTimeout(r, ms));
  const say = async (text: string) => {
    for (const piece of text.split(/(?<= )/)) {
      if (!live()) return;
      agentEmit({ type: "text_delta", text: piece });
      await pause(35);
    }
  };

  if (/export/i.test(prompt)) {
    await say("Rendering writes a file, so I need your permission first. ");
    const requestId = `mock-perm-${turn}`;
    const option = await new Promise<string>((resolve) => {
      mockAgent.pending.set(requestId, resolve);
      agentEmit({
        type: "permission_request",
        request_id: requestId,
        tool: "mcp__renderly__export",
        input: { output_path: "out/mock-export.mp4", preset: "tiktok" },
        options: [
          { id: "allow_once", label: "Allow once" },
          { id: "reject_once", label: "Deny" },
        ],
      });
    });
    mockAgent.pending.delete(requestId);
    if (!live()) return;
    if (option === "allow_once") {
      agentEmit({ type: "tool_started", id: `mock-tu-${turn}`, name: "mcp__renderly__export", input: { preset: "tiktok" } });
      await pause(400);
      agentEmit({ type: "tool_finished", id: `mock-tu-${turn}`, output_summary: "Mock export: nothing was written.", is_error: false });
      await say("Export finished (mock).");
    } else {
      await say("Okay, I won't export.");
    }
    agentEmit({ type: "turn_finished", stop_reason: "end_turn" });
    return;
  }

  await say("I'll trim the first video clip to 5 seconds. ");
  const target = firstVideoClip();
  if (!target) {
    await say("There is no video clip to trim.");
    agentEmit({ type: "turn_finished", stop_reason: "end_turn" });
    return;
  }
  const command = {
    command: "TrimClip",
    track_id: target.trackId,
    clip_id: target.clipId,
    new_source_out_secs: Math.min(5, target.sourceOut),
  };
  await pause(300);
  if (!live()) return;
  const toolId = `mock-tu-${turn}`;
  agentEmit({ type: "tool_started", id: toolId, name: "mcp__renderly__apply_command", input: { command } });
  try {
    const result = (await invoke<{ revision: number }>("apply_command", { command })) as { revision: number };
    await pause(250);
    agentEmit({ type: "tool_finished", id: toolId, output_summary: `revision ${result.revision}`, is_error: false });
  } catch (e) {
    agentEmit({ type: "tool_finished", id: toolId, output_summary: String(e), is_error: true });
  }
  if (!live()) return;
  await say("Done. The edit card has an Undo button.");
  agentEmit({ type: "turn_finished", stop_reason: "end_turn" });
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function invoke<T = unknown>(cmd: string, args?: any): Promise<T> {
  const reply = (v: unknown) => Promise.resolve(v as T);
  switch (cmd) {
    case "play":
      startMockPlayback((args?.timeSecs as number) ?? 0);
      return reply(undefined);
    case "pause": {
      const t = stopMockPlayback();
      emit("playback:state", { playing: false, time_secs: t });
      return reply(t);
    }
    case "seek": {
      mockTimeSecs = (args?.timeSecs as number) ?? mockTimeSecs;
      return reply(undefined);
    }
    case "list_projects":
      return reply([
        { path: "C:/proj/ep12.renderly.json", name: "Ultra Bruno ep.12", durationSecs: 84, modifiedMs: Date.now() - 7.2e6, width: 1080, height: 1920, fps: 60 },
        { path: "C:/proj/montage.renderly.json", name: "ranked grind montage", durationSecs: 58, modifiedMs: Date.now() - 9e7, width: 1080, height: 1920, fps: 60 },
        { path: "C:/proj/recap.renderly.json", name: "tourney recap", durationSecs: 192, modifiedMs: Date.now() - 3.4e8, width: 1920, height: 1080, fps: 30 },
      ]);
    case "get_project":
      return reply({ project: structuredClone(currentProject), revision });
    case "open_project":
    case "quick_start_project":
    case "new_project":
      // Real backend behavior: (re)opening resets session state. Reset the mock's mutable
      // project back to the pristine sample too, so re-running a harness script against a
      // freshly "opened" project starts from known state instead of carrying over the
      // previous script's edits.
      currentProject = structuredClone(sampleProject);
      undoStack.length = 0;
      redoStack.length = 0;
      revision = 1;
      return reply("C:/proj/ep12.renderly.json");
    // OpenTimelineIO interop: export is a no-op; import "creates" the sample project file and
    // returns one warning so the harness can exercise the toast path.
    case "export_otio":
      return reply(undefined);
    case "import_otio":
      return reply({
        projectPath: "C:/proj/ep12.renderly.json",
        warnings: [{ kind: "missing_media", path: "tracks.children[0]", message: "mock warning" }],
      });
    case "list_extensions":
      return reply(extensionCatalog);
    case "list_registry":
      return reply([
        { id: "retro-pack", kind: "pack", summary: "Retro CRT filters and stickers", schema_version: 1, resolved_path: "C:/reg/retro" },
        { id: "denoise-pro", kind: "plugin", summary: "Studio-grade audio denoise", schema_version: 1, resolved_path: "C:/reg/denoise" },
      ]);
    case "apply_command":
    case "apply_commands": {
      // Atomic like the engine: a failing command rejects (with the error text, as Tauri does)
      // and leaves the project untouched. Successful edits go on the shared undo stack.
      const before = structuredClone(currentProject);
      const cmds = cmd === "apply_command" ? [args?.command] : ((args?.commands as unknown[]) ?? []);
      try {
        for (const c of cmds) applyMockCommand(currentProject, c);
      } catch (e) {
        currentProject = before;
        return Promise.reject(e instanceof Error ? e.message : String(e));
      }
      // Deep-copied: `compare` returns references into `currentProject`, and fast-json-patch
      // inserts `add` values by reference - the store would then share clip objects with the
      // mock and later mock mutations would corrupt (double-apply to) the store's project.
      const patch = structuredClone(compare(before as unknown as object, currentProject as unknown as object));
      if (patch.length > 0) {
        undoStack.push(before);
        redoStack.length = 0;
      }
      revision += 1;
      emit("project:changed", {
        revision,
        mutation_id: args?.mutationId,
        can_undo: undoStack.length > 0,
        can_redo: redoStack.length > 0,
      });
      return reply(
        cmd === "apply_command"
          ? { revision, patch, outcome: "ok" }
          : { revision, patch, outcomes: cmds.map(() => "ok") },
      );
    }
    case "undo":
    case "redo": {
      const from = cmd === "undo" ? undoStack : redoStack;
      const to = cmd === "undo" ? redoStack : undoStack;
      const target = from.pop();
      let patch: ReturnType<typeof compare> = [];
      if (target) {
        to.push(structuredClone(currentProject));
        patch = structuredClone(compare(currentProject as unknown as object, target as unknown as object));
        currentProject = target;
      }
      revision += 1;
      emit("project:changed", {
        revision,
        mutation_id: args?.mutationId,
        can_undo: undoStack.length > 0,
        can_redo: redoStack.length > 0,
      });
      return reply({ can_undo: undoStack.length > 0, can_redo: redoStack.length > 0, revision, patch });
    }
    case "get_media_assets":
      return reply({});
    // Preview host assets (raster/generated mattes, pack LUTs). The sample project references
    // none; harness scripts that inject a Raster mask / pack:<id>:lut:<id> effect via
    // `window.__store` get a synthetic left-to-right gradient matte and a channel-rotating
    // 2x2x2 .cube so the wasm path is exercisable without a filesystem.
    case "read_preview_matte":
      return mockGradientMattePng() as Promise<T>;
    case "preview_matte_stamps":
      return reply(((args?.paths as string[]) ?? []).map(() => 1));
    case "read_preview_pack_luts":
      return reply(
        ((args?.wanted as { pack_id: string; lut_id: string }[]) ?? []).map((w) => ({
          ...w,
          cube: MOCK_ROTATE_CUBE,
        })),
      );
    // Local ML models: the matting weights start "not installed" so the consent dialog is
    // reachable in the harness; the mock "download" flips them to installed.
    case "ml_model_status":
      return reply(mockModelStatus(String(args?.id ?? "")));
    case "ml_download_model": {
      mockModelInstalled = true;
      return reply(mockModelStatus(String(args?.id ?? "")));
    }
    case "background_matte_status": {
      const clip = mockFindClip(mockFindTrack(currentProject, String(args?.trackId)), String(args?.clipId));
      const bg = clip && (clip.type === "video" || clip.type === "audio") ? clip.background_removal : null;
      if (!bg?.matte_cache_dir) return reply({ state: "not_generated" });
      return reply({ state: "ready", frames: bg.matte_frames?.count ?? null });
    }
    case "cancel_background_matte":
      return reply(undefined);
    case "project_thumbnail":
      return reply(null);
    case "agent_list":
      return reply(MOCK_AGENTS);
    case "agent_start": {
      if (args?.agentId === "codex") {
        return Promise.reject("Codex is not installed (mock). Install it, then start again.");
      }
      mockAgent.sessionId = `mock-session-${Date.now()}`;
      mockAgent.turn = 0;
      mockAgent.pending.clear();
      setTimeout(() => agentEmit({ type: "session_started", agent_id: args?.agentId ?? "claude-code" }), 0);
      return reply(mockAgent.sessionId);
    }
    case "agent_send": {
      if (!mockAgent.sessionId || args?.sessionId !== mockAgent.sessionId) {
        return Promise.reject("no agent session");
      }
      void runMockTurn(String(args?.text ?? ""));
      return reply(undefined);
    }
    case "agent_answer_permission": {
      const resolve = mockAgent.pending.get(args?.requestId);
      if (!resolve) return Promise.reject(`no pending permission request ${args?.requestId}`);
      resolve(String(args?.optionId));
      return reply(undefined);
    }
    case "agent_cancel": {
      if (mockAgent.sessionId) {
        mockAgent.turn += 1; // invalidates the running script
        for (const resolve of mockAgent.pending.values()) resolve("reject_once");
        mockAgent.pending.clear();
        agentEmit({ type: "turn_finished", stop_reason: "cancelled" });
      }
      return reply(undefined);
    }
    case "agent_set_auto_edits":
      return reply(undefined);
    case "agent_stop": {
      const sid = mockAgent.sessionId;
      mockAgent.turn += 1;
      for (const resolve of mockAgent.pending.values()) resolve("reject_once");
      mockAgent.pending.clear();
      if (sid) emit("agent:event", { session_id: sid, type: "session_ended", reason: "stopped" });
      mockAgent.sessionId = null;
      return reply(undefined);
    }
    default:
      return reply(undefined);
  }
}

// Maps the sample project's fake media paths to the two real, visually-distinct clips
// generated by ffmpeg into renderly-app/dev-assets/ (served at the site root by
// vite.harness.config.ts's `publicDir: "dev-assets"`) — see docs/preview-webview.md
// "Harness verification". Round1/round2 → clip-a (testsrc2, colorful test pattern);
// kill-cam/facecam → clip-b (smptebars) so a cut between tracks/clips is visibly
// distinguishable by sampling a pixel.
const REAL_CLIP_BY_PATH: Record<string, string> = {
  "C:/clips/round1.mp4": "/clip-a.mp4",
  "C:/clips/round2.mp4": "/clip-a.mp4",
  "C:/clips/kill-cam.mp4": "/clip-b.mp4",
  "C:/clips/facecam.mp4": "/clip-b.mp4",
};

export function convertFileSrc(path: string): string {
  return REAL_CLIP_BY_PATH[path] ?? path;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function listen<T = unknown>(event: string, cb: (e: { payload: T }) => void): Promise<() => void> {
  const wrapped: Listener = (payload) => cb({ payload: payload as T });
  let set = listeners.get(event);
  if (!set) {
    set = new Set();
    listeners.set(event, set);
  }
  set.add(wrapped);
  return Promise.resolve(() => {
    set!.delete(wrapped);
  });
}

const noop = () => Promise.resolve();
const noopUnlisten = () => Promise.resolve(() => {});

export function getCurrentWindow() {
  return {
    minimize: noop,
    maximize: noop,
    unmaximize: noop,
    toggleMaximize: noop,
    close: noop,
    startDragging: noop,
    isMaximized: () => Promise.resolve(false),
    scaleFactor: () => Promise.resolve(1),
    innerPosition: () => Promise.resolve({ x: 0, y: 0 }),
    innerSize: () => Promise.resolve({ width: 1600, height: 950 }),
    onResized: noopUnlisten,
    onMoved: noopUnlisten,
    onScaleChanged: noopUnlisten,
  };
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function open(_opts?: any): Promise<string | null> {
  return Promise.resolve(null);
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function save(_opts?: any): Promise<string | null> {
  return Promise.resolve(null);
}

// ---- Preview host-asset fixtures (see the read_preview_* cases in `invoke`) ----

/// 2x2x2 .cube rotating channels (r,g,b) -> (g,b,r): visibly obvious, parse-valid.
const MOCK_ROTATE_CUBE = (() => {
  let text = 'TITLE "mock-rotate"\nLUT_3D_SIZE 2\n';
  for (let b = 0; b < 2; b++)
    for (let g = 0; g < 2; g++) for (let r = 0; r < 2; r++) text += `${g} ${b} ${r}\n`;
  return text;
})();

/// 64x36 horizontal black->white ramp as PNG bytes (what the real backend returns for a
/// Raster/Generated matte file).
async function mockGradientMattePng(): Promise<ArrayBuffer> {
  const canvas = new OffscreenCanvas(64, 36);
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("2d context unavailable");
  const grad = ctx.createLinearGradient(0, 0, 64, 0);
  grad.addColorStop(0, "#000");
  grad.addColorStop(1, "#fff");
  ctx.fillStyle = grad;
  ctx.fillRect(0, 0, 64, 36);
  const blob = await canvas.convertToBlob({ type: "image/png" });
  return blob.arrayBuffer();
}
