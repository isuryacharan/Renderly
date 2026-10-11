// Browser dev harness ONLY. Aliased in place of the four @tauri-apps modules by
// vite.harness.config.ts so the real app (which needs a running Tauri backend) can be
// rendered and iterated on in a plain browser. Never imported by the production build.

import { compare } from "fast-json-patch";
import type { Clip, MediaClip, Project, Track } from "../lib/types";

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
} as const;

const sampleProject: Project = {
  schema_version: 6,
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
  ],
  tracks: [
    {
      id: MOCK_IDS["t-v2"], kind: "video", name: "Overlay", muted: false, locked: false, hidden: false,
      clips: [
        { type: "video", id: MOCK_IDS["c-facecam"], media_id: MOCK_IDS["m-facecam"], position_secs: 10, source_in_secs: 0, source_out_secs: 6.2, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0, transform: { x: 0, y: -0.55, scale_x: 0.32, scale_y: 0.32, rotation_deg: 0, opacity: 1 } },
      ],
    },
    {
      id: MOCK_IDS["t-v1"], kind: "video", name: "Gameplay", muted: false, locked: false, hidden: false,
      clips: [
        { type: "video", id: MOCK_IDS["c-round1"], media_id: MOCK_IDS["m-round1"], position_secs: 0, source_in_secs: 0, source_out_secs: 9.4, gain_db: 0, enabled: true, fade_in_secs: 0, fade_out_secs: 0, outgoing_transition: { kind: "crossfade", duration_secs: 0.5 } },
        { type: "video", id: MOCK_IDS["c-killcam"], media_id: MOCK_IDS["m-killcam"], position_secs: 9.4, source_in_secs: 2, source_out_secs: 10.3, gain_db: -3, enabled: true, fade_in_secs: 0, fade_out_secs: 0, transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0, opacity: 1 }, effects: [{ id: MOCK_IDS["e1"], effect_id: "builtin:color_adjust", enabled: true, params: { contrast: 1.1 } }] },
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

function mockFindTrack(project: Project, trackId: string): Track | undefined {
  return project.tracks.find((t) => t.id === trackId);
}

function mockFindClip(track: Track | undefined, clipId: string): Clip | undefined {
  return track?.clips.find((c) => c.id === clipId);
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function applyMockCommand(project: Project, cmd: any): void {
  switch (cmd.command) {
    case "AddTrack": {
      const track: Track = {
        id: cmd.id ?? crypto.randomUUID(),
        kind: cmd.kind,
        name: cmd.name,
        muted: false,
        locked: false,
        hidden: false,
        clips: [],
      };
      project.tracks.push(track);
      break;
    }
    case "AddClip": {
      const track = mockFindTrack(project, cmd.track_id);
      const media = project.media.find((m) => m.id === cmd.media_id);
      if (!track || !media) break;
      const clip: MediaClip = {
        type: media.kind === "audio" ? "audio" : "video",
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
      project.tracks = project.tracks.filter((t) => t.id !== cmd.track_id);
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
      const idx = project.tracks.findIndex((t) => t.id === cmd.track_id);
      if (idx === -1) break;
      const clamped = Math.min(cmd.new_index, project.tracks.length - 1);
      if (clamped === idx) break;
      const [track] = project.tracks.splice(idx, 1);
      project.tracks.splice(clamped, 0, track);
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
      if (clip && clip.type !== "caption") clip.transform = cmd.transform;
      break;
    }
    case "SetClipKeyframes": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.keyframes = cmd.keyframes;
      break;
    }
    case "SetClipEffects": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.effects = cmd.effects;
      break;
    }
    case "SetClipTransition": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.outgoing_transition = cmd.transition;
      break;
    }
    case "SetClipSpeed": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.speed = cmd.speed;
      break;
    }
    case "SetAudioGain": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.gain_db = cmd.gain_db;
      break;
    }
    case "SetAudioFade": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") {
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
    case "MoveClip": {
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      if (!track || !clip) break;
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
      if (clip && clip.type !== "caption") {
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
      return reply({ project: currentProject, revision });
    case "open_project":
    case "quick_start_project":
    case "new_project":
      // Real backend behavior: (re)opening resets session state. Reset the mock's mutable
      // project back to the pristine sample too, so re-running a harness script against a
      // freshly "opened" project starts from known state instead of carrying over the
      // previous script's edits.
      currentProject = structuredClone(sampleProject);
      revision = 1;
      return reply("C:/proj/ep12.renderly.json");
    case "list_extensions":
      return reply(extensionCatalog);
    case "list_registry":
      return reply([
        { id: "retro-pack", kind: "pack", summary: "Retro CRT filters and stickers", schema_version: 1, resolved_path: "C:/reg/retro" },
        { id: "denoise-pro", kind: "plugin", summary: "Studio-grade audio denoise", schema_version: 1, resolved_path: "C:/reg/denoise" },
      ]);
    case "apply_command": {
      const before = structuredClone(currentProject);
      applyMockCommand(currentProject, args?.command);
      const patch = compare(before as unknown as object, currentProject as unknown as object);
      revision += 1;
      return reply({ revision, patch, outcome: "ok" });
    }
    case "apply_commands": {
      const before = structuredClone(currentProject);
      const cmds = (args?.commands as unknown[]) ?? [];
      for (const c of cmds) applyMockCommand(currentProject, c);
      const patch = compare(before as unknown as object, currentProject as unknown as object);
      revision += 1;
      return reply({ revision, patch, outcomes: cmds.map(() => "ok") });
    }
    case "undo":
    case "redo":
      revision += 1;
      return reply({ can_undo: true, can_redo: true, revision, patch: [] });
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
    case "project_thumbnail":
      return reply(null);
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
