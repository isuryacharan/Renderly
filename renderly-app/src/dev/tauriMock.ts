// Browser dev harness ONLY. Aliased in place of the four @tauri-apps modules by
// vite.harness.config.ts so the real app (which needs a running Tauri backend) can be
// rendered and iterated on in a plain browser. Never imported by the production build.

import { compare } from "fast-json-patch";
import { fpsFromNumber, frameStart, normalizeFps, secsToUs as S, snapToFrameUs } from "../lib/time";
import type { CatalogueEntry } from "../lib/ipc";
import type {
  AdjustmentClip,
  BodyWord,
  CaptionClip,
  Clip,
  EffectClip,
  EffectInstance,
  EffectParams,
  KeyframeTrack,
  MediaClip,
  Project,
  TextClip,
  TextStyleRef,
  Track,
} from "../lib/types";
import { effectStack } from "../lib/types";

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
  // Schema v8 stage 3: a title track + title, markers, one transcript.
  "t-title": "bbbbbbbb-0000-4000-8000-00000000000a",
  "c-title": "cccccccc-0000-4000-8000-000000000011",
  "mk1": "ffffffff-0000-4000-8000-000000000001",
  "mk2": "ffffffff-0000-4000-8000-000000000002",
  "mk3": "ffffffff-0000-4000-8000-000000000003",
  "tr1": "99999999-0000-4000-8000-000000000001",
} as const;

const styleRef = (id: string): TextStyleRef => ({ ref: `builtin:${id}@1`, params: {}, overrides: {} });

/// Caption words from "word start end" triples (seconds, relative to the clip start).
function sampleWords(items: [string, number, number][]): BodyWord[] {
  return items.map(([text, a, b], i) => ({ text, start_us: S(a), end_us: S(b), word_id: `w-${i + 1}` }));
}

/// A sample speech transcript of the facecam clip (source media time), with fillers.
function sampleTranscript(): NonNullable<Project["transcripts"]>[number] {
  const raw: [string, number, number, string[]?][] = [
    ["So", 0.4, 0.6], ["um", 0.7, 0.95, ["filler"]], ["this", 1.0, 1.2], ["run", 1.25, 1.55],
    ["was", 1.6, 1.8], ["like", 1.85, 2.0, ["filler"]], ["absolutely", 2.1, 2.7], ["cursed", 2.75, 3.3],
    ["but", 3.6, 3.8], ["uh", 3.85, 4.05, ["filler"]], ["we", 4.1, 4.2], ["clutched", 4.25, 4.9],
    ["it", 4.95, 5.1], ["anyway", 5.15, 5.8],
  ];
  return {
    id: MOCK_IDS.tr1,
    media_id: MOCK_IDS["m-facecam"],
    language: "en",
    model: "whisper-base.en",
    words: raw.map(([text, a, b, tags], i) => ({
      id: `w-${i + 1}`,
      text,
      start_us: S(a),
      end_us: S(b),
      deleted: false,
      tags: tags ?? [],
    })),
  };
}

const sampleProject: Project = {
  schema_version: 8,
  id: MOCK_IDS["mock-project"],
  name: "Ultra Bruno ep.12",
  markers: [
    { id: MOCK_IDS.mk1, time_us: S(5), label: "intro ends", color: "", kind: "user" },
    { id: MOCK_IDS.mk2, time_us: S(9.4), label: "kill cam", color: "#E5484D", kind: "user" },
    { id: MOCK_IDS.mk3, time_us: S(14), label: "", color: "", kind: "beat" },
  ],
  transcripts: [sampleTranscript()],
  settings: { fps: { num: 60, den: 1 }, width: 1080, height: 1920, sample_rate: 48000, duck_db: -12 },
  // NOTE: paths here are only ever resolved through `convertFileSrc` below (the harness
  // maps them to the real generated clips in dev-assets/ served at the site root by
  // vite.harness.config.ts's `publicDir`) — durations/dims match those real files
  // (1920x1080@60, 12s) so timeline eval (source_in/out) lines up with what actually
  // decodes. See docs/preview-webview.md "Harness verification".
  media: [
    { id: MOCK_IDS["m-round1"], path: "C:/clips/round1.mp4", kind: "video", duration_us: S(12), width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-killcam"], path: "C:/clips/kill-cam.mp4", kind: "video", duration_us: S(12), width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-round2"], path: "C:/clips/round2.mp4", kind: "video", duration_us: S(12), width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-facecam"], path: "C:/clips/facecam.mp4", kind: "video", duration_us: S(12), width: 1920, height: 1080, fps: 60 },
    { id: MOCK_IDS["m-vo"], path: "C:/audio/vo-take2.wav", kind: "audio", duration_us: S(65), width: null, height: null, fps: null },
    { id: MOCK_IDS["m-bgm"], path: "C:/audio/bgm-loop.mp3", kind: "audio", duration_us: S(90), width: null, height: null, fps: null },
    { id: MOCK_IDS["m-logo"], path: "C:/img/logo.png", kind: "image", duration_us: null, width: 512, height: 512, fps: null },
    // Sequence media: path empty, duration/size derived from the inner timeline.
    { id: MOCK_IDS["m-intro"], path: "", kind: "sequence", sequence_id: MOCK_IDS["s-intro"], duration_us: null, width: null, height: null, fps: null },
  ],
  sequences: [
    {
      id: MOCK_IDS["s-intro"], name: "Intro", width: null, height: null,
      tracks: [
        {
          id: MOCK_IDS["t-intro-v"], kind: "video", name: "Video 1", muted: false, locked: false, hidden: false,
          clips: [
            { type: "video", id: MOCK_IDS["c-intro-a"], media_id: MOCK_IDS["m-round1"], start_us: S(0), source_in_us: S(1), source_out_us: S(4), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0) },
            { type: "video", id: MOCK_IDS["c-intro-b"], media_id: MOCK_IDS["m-killcam"], start_us: S(3), source_in_us: S(2), source_out_us: S(5), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0) },
          ],
        },
        {
          id: MOCK_IDS["t-intro-adj"], kind: "video", name: "Grade", muted: false, locked: false, hidden: false,
          clips: [
            { type: "adjustment", id: MOCK_IDS["c-intro-adj"], start_us: S(1), duration_us: S(4), enabled: true, opacity: 1, adjust: { id: MOCK_IDS["e3"], ref: "builtin:color_adjust@1", enabled: true, params: { saturation: 0 } } },
          ],
        },
      ],
    },
  ],
  tracks: [
    {
      id: MOCK_IDS["t-v2"], kind: "video", name: "Overlay", muted: false, locked: false, hidden: false,
      clips: [
        { type: "video", id: MOCK_IDS["c-facecam"], media_id: MOCK_IDS["m-facecam"], start_us: S(10), source_in_us: S(0), source_out_us: S(6.2), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0), transform: { x: 0, y: -0.55, scale_x: 0.32, scale_y: 0.32, rotation_deg: 0, opacity: 1, fit_x: 0, fit_y: 0 } },
      ],
    },
    {
      id: MOCK_IDS["t-v1"], kind: "video", name: "Gameplay", muted: false, locked: false, hidden: false, magnetic: true,
      clips: [
        { type: "video", id: MOCK_IDS["c-round1"], media_id: MOCK_IDS["m-round1"], start_us: S(0), source_in_us: S(0), source_out_us: S(9.4), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0), outgoing_transition: { ref: "builtin:crossfade@1", duration_us: S(0.5), params: {}, easing: null } },
        { type: "video", id: MOCK_IDS["c-killcam"], media_id: MOCK_IDS["m-killcam"], start_us: S(9.4), source_in_us: S(2), source_out_us: S(10.3), gain_db: -3, enabled: true, fade_in_us: S(0), fade_out_us: S(0), transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0, opacity: 1, fit_x: 0, fit_y: 0 }, adjust: { id: MOCK_IDS["e1"], ref: "builtin:color_adjust@1", enabled: true, params: { contrast: 1.1 } } },
        { type: "video", id: MOCK_IDS["c-round2"], media_id: MOCK_IDS["m-round2"], start_us: S(17.7), source_in_us: S(0), source_out_us: S(11.1), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0) },
      ],
    },
    {
      id: MOCK_IDS["t-a1"], kind: "audio", name: "Voiceover", audio_role: "voiceover", muted: false, locked: false, hidden: false,
      clips: [
        { type: "audio", id: MOCK_IDS["c-vo"], media_id: MOCK_IDS["m-vo"], start_us: S(2.5), source_in_us: S(0), source_out_us: S(18), gain_db: 0, enabled: true, fade_in_us: S(0.2), fade_out_us: S(0.4) },
        { type: "audio", id: MOCK_IDS["c-bgm"], media_id: MOCK_IDS["m-bgm"], start_us: S(22), source_in_us: S(0), source_out_us: S(12), gain_db: -12, enabled: true, fade_in_us: S(0), fade_out_us: S(1) },
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
        { type: "video", id: MOCK_IDS["c-round1a"], media_id: MOCK_IDS["m-round1"], start_us: S(40), source_in_us: S(0), source_out_us: S(5.5), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0), outgoing_transition: { ref: "builtin:crossfade@1", duration_us: S(0.5), params: {}, easing: null } },
        // source_in deliberately far from the outgoing side's 0..5.5 range so the two
        // sides sample visibly different frames of m-round1 during the blend window —
        // makes a genuine crossfade distinguishable from "both sides show the same frame"
        // (the pre-fix symptom) when pixel-sampled.
        { type: "video", id: MOCK_IDS["c-round1b"], media_id: MOCK_IDS["m-round1"], start_us: S(45.5), source_in_us: S(8.0), source_out_us: S(11.0), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0) },
      ],
    },
    {
      id: MOCK_IDS["t-nest"], kind: "video", name: "Nested", muted: false, locked: false, hidden: false,
      clips: [
        // Intro is 6 s long (c-intro-b ends at 3 + 3), placed untrimmed at speed 1.
        { type: "video", id: MOCK_IDS["c-nested"], media_id: MOCK_IDS["m-intro"], start_us: S(52), source_in_us: S(0), source_out_us: S(6), gain_db: 0, enabled: true, fade_in_us: S(0), fade_out_us: S(0) },
      ],
    },
    {
      id: MOCK_IDS["t-adj"], kind: "video", name: "Adjustments", muted: false, locked: false, hidden: false,
      clips: [
        { type: "adjustment", id: MOCK_IDS["c-adj"], start_us: S(20), duration_us: S(6), enabled: true, opacity: 0.8, effects: [{ id: MOCK_IDS["e2"], ref: "builtin:blur@1", enabled: true, params: { radius: 4 } }] },
      ],
    },
    {
      id: MOCK_IDS["t-title"], kind: "video", name: "Titles", muted: false, locked: false, hidden: false,
      clips: [
        {
          type: "text", id: MOCK_IDS["c-title"], start_us: S(0.5), duration_us: S(3), enabled: true,
          body: { text: "EPISODE 12", runs: [], words: [] },
          style: { ref: "builtin:tiktok-minimal@1", params: {}, overrides: { anchor: "center" } },
          animations: { in: { ref: "builtin:fade-up@1", params: {} } },
          transform: { x: 0, y: 0, scale_x: 1, scale_y: 1, rotation_deg: 0, opacity: 1, fit_x: 0, fit_y: 0 },
        },
      ],
    },
    {
      id: MOCK_IDS["t-c1"], kind: "caption", name: "Captions", muted: false, locked: false, hidden: false,
      caption_style: styleRef("tiktok-bold-yellow"),
      clips: [
        {
          type: "caption", id: MOCK_IDS["c-cap1"], start_us: S(3), duration_us: S(2.4),
          body: {
            text: "okay so THIS run", runs: [],
            words: sampleWords([["okay", 0.1, 0.4], ["so", 0.45, 0.6], ["THIS", 0.7, 1.2], ["run", 1.3, 1.8]]),
          },
          style: null, animations: {},
        },
        {
          type: "caption", id: MOCK_IDS["c-cap2"], start_us: S(6), duration_us: S(3),
          body: {
            text: "was absolutely cursed", runs: [],
            words: sampleWords([["was", 0.1, 0.4], ["absolutely", 0.5, 1.4], ["cursed", 1.5, 2.2]]).map((w, i) => ({ ...w, word_id: `w-${i + 5}` })),
          },
          style: null, animations: {},
        },
        { type: "caption", id: MOCK_IDS["c-cap3"], start_us: S(11), duration_us: S(2.2), body: { text: "watch the corner", runs: [], words: [] }, style: styleRef("tiktok-box"), animations: {} },
      ],
    },
  ],
};

/// The engine's effect catalogue (`list_extensions().effects`): the builtins as the engine
/// reports them (ref, kind, label, slot, params with type / range / default / initial), plus a
/// mock pack. `animatable: false` marks params that cannot be keyframed.
const mockCatalogue: CatalogueEntry[] = [
  {
    "ref": "builtin:blur@1",
    "kind": "effect",
    "label": "Blur",
    "source": "builtin",
    "state": "ok",
    "slot": "effects",
    "params": [
      {
        "key": "radius",
        "type": "float",
        "min": 0.0,
        "max": 64.0,
        "default": 0.0,
        "initial": 8.0,
        "animatable": true
      }
    ]
  },
  {
    "ref": "builtin:color_adjust@1",
    "kind": "effect",
    "label": "Color adjust",
    "source": "builtin",
    "state": "ok",
    "slot": "adjust",
    "params": [
      {
        "key": "exposure",
        "type": "float",
        "min": -5.0,
        "max": 5.0,
        "default": 0.0,
        "initial": 0.0,
        "animatable": true
      },
      {
        "key": "contrast",
        "type": "float",
        "min": 0.0,
        "max": 4.0,
        "default": 1.0,
        "initial": 1.0,
        "animatable": true
      },
      {
        "key": "saturation",
        "type": "float",
        "min": 0.0,
        "max": 4.0,
        "default": 1.0,
        "initial": 1.0,
        "animatable": true
      }
    ]
  },
  {
    "ref": "builtin:lut_contrast@1",
    "kind": "effect",
    "label": "LUT Contrast",
    "source": "builtin",
    "state": "ok",
    "slot": "effects",
    "params": [
      {
        "key": "intensity",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 1.0,
        "initial": 1.0,
        "animatable": true
      }
    ]
  },
  {
    "ref": "builtin:lut_warm@1",
    "kind": "effect",
    "label": "LUT Warm",
    "source": "builtin",
    "state": "ok",
    "slot": "effects",
    "params": [
      {
        "key": "intensity",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 1.0,
        "initial": 1.0,
        "animatable": true
      }
    ]
  },
  {
    "ref": "builtin:glitch@1",
    "kind": "effect",
    "label": "Glitch",
    "source": "builtin",
    "state": "ok",
    "slot": "effects",
    "params": [
      {
        "key": "intensity",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 0.5,
        "initial": 0.5,
        "animatable": true
      },
      {
        "key": "slice",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 0.5,
        "initial": 0.5,
        "animatable": true
      },
      {
        "key": "seed",
        "type": "float",
        "min": -1000000000.0,
        "max": 1000000000.0,
        "default": 0.0,
        "initial": 0.0,
        "animatable": false
      }
    ]
  },
  {
    "ref": "builtin:chroma_key@1",
    "kind": "effect",
    "label": "Chroma key",
    "source": "builtin",
    "state": "ok",
    "slot": "key",
    "params": [
      {
        "key": "key_r",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 0.0,
        "initial": 0.0,
        "animatable": true
      },
      {
        "key": "key_g",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 1.0,
        "initial": 1.0,
        "animatable": true
      },
      {
        "key": "key_b",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 0.0,
        "initial": 0.0,
        "animatable": true
      },
      {
        "key": "tolerance",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 0.3,
        "initial": 0.3,
        "animatable": true
      },
      {
        "key": "softness",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 0.1,
        "initial": 0.1,
        "animatable": true
      },
      {
        "key": "spill",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 0.0,
        "initial": 0.0,
        "animatable": true
      }
    ]
  },
  {
    "ref": "builtin:lut_filter@1",
    "kind": "filter",
    "label": "LUT",
    "source": "builtin",
    "state": "ok",
    "slot": "filter",
    "params": [
      {
        "key": "intensity",
        "type": "float",
        "min": 0.0,
        "max": 1.0,
        "default": 1.0,
        "initial": 1.0,
        "animatable": true
      },
      {
        "key": "lut",
        "type": "lut",
        "default": "",
        "initial": "",
        "animatable": false
      }
    ]
  },
  {
    "ref": "builtin:crossfade@1",
    "kind": "transition",
    "label": "Crossfade",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:fade_black@1",
    "kind": "transition",
    "label": "Fade to black",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:wipe_left@1",
    "kind": "transition",
    "label": "Wipe left",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:wipe_right@1",
    "kind": "transition",
    "label": "Wipe right",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:wipe_up@1",
    "kind": "transition",
    "label": "Wipe up",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:wipe_down@1",
    "kind": "transition",
    "label": "Wipe down",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:slide_left@1",
    "kind": "transition",
    "label": "Slide left",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:slide_right@1",
    "kind": "transition",
    "label": "Slide right",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:iris@1",
    "kind": "transition",
    "label": "Iris",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "builtin:blur_dissolve@1",
    "kind": "transition",
    "label": "Blur dissolve",
    "source": "builtin",
    "state": "ok",
    "slot": "transition",
    "params": []
  },
  {
    "ref": "pack:local.neon/neon_night@1",
    "kind": "lut",
    "label": "Neon Night",
    "source": "local.neon",
    "state": "ok",
    "slot": "filter",
    "params": []
  },
  {
    "ref": "pack:local.neon/spin_wipe@1",
    "kind": "transition",
    "label": "Spin wipe",
    "source": "local.neon",
    "state": "ok",
    "slot": "transition",
    "params": [
      {
        "key": "angle",
        "type": "float",
        "min": 0,
        "max": 360,
        "default": 90,
        "initial": 90,
        "animatable": false
      },
      {
        "key": "soft",
        "type": "bool",
        "default": false,
        "initial": true,
        "animatable": false
      }
    ]
  },
  {
    "ref": "pack:local.neon/vignette_tint@1",
    "kind": "effect",
    "label": "Vignette tint",
    "source": "local.neon",
    "state": "ok",
    "slot": "effects",
    "params": [
      {
        "key": "strength",
        "type": "float",
        "min": 0,
        "max": 1,
        "default": 0,
        "initial": 0.6,
        "animatable": true
      },
      {
        "key": "tint",
        "type": "color",
        "default": [
          0,
          0,
          0,
          1
        ],
        "initial": [
          0.1,
          0.0,
          0.3,
          1
        ],
        "animatable": true
      },
      {
        "key": "center",
        "type": "vec2",
        "min": 0,
        "max": 1,
        "default": [
          0.5,
          0.5
        ],
        "initial": [
          0.5,
          0.5
        ],
        "animatable": true
      },
      {
        "key": "shape",
        "type": "choice",
        "min": 0,
        "max": 2,
        "default": 0,
        "initial": 0,
        "options": [
          "round",
          "square",
          "diamond"
        ],
        "animatable": false
      },
      {
        "key": "invert",
        "type": "bool",
        "default": false,
        "initial": false,
        "animatable": false
      }
    ]
  }
];

const extensionCatalog = {
  packs: [
    { id: "local.neon", name: "Neon Gamer Pack", path: "C:/packs/neon", stickers: [{ id: "s1", label: "Boom", default_duration_us: S(1) }, { id: "s2", label: "GG", default_duration_us: S(1.5) }], sfx: [{ id: "sfx1", label: "Whoosh" }], luts: [{ id: "neon_night", label: "Neon Night", ref: "pack:local.neon/neon_night@1" }], transitions: [{ id: "spin_wipe", label: "Spin wipe", kind: "wipe_left", default_duration_us: S(0.6), ref: "pack:local.neon/spin_wipe@1" }], templates: [{ id: "tpl1", label: "Montage Intro" }] },
  ],
  plugins: [
    { id: "glow", name: "Glow FX", path: "C:/plugins/glow", has_frame: true, has_audio: false },
    { id: "bass", name: "Bass Boost", path: "C:/plugins/bass", has_frame: false, has_audio: true },
  ],
  effects: mockCatalogue,
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
// Undo/redo for the mock, mirroring renderly_core::history: records hold the pre-change project,
// consecutive changes with the same merge key within 1 s amend the top record, and an open
// transaction folds its changes into one record (docs/architecture.md "History, transactions").
interface MockRecord {
  before: Project;
  key: string | null;
  touched: number | null;
}
const MOCK_HISTORY_CAP = 100;
const MOCK_COALESCE_MS = 1000;
const undoStack: MockRecord[] = [];
const redoStack: MockRecord[] = [];
let mockTx: { depth: number; base: Project | null } | null = null;

function recordMock(before: Project, mergeKey: unknown): void {
  if (mockTx) {
    if (!mockTx.base) mockTx.base = before;
    return;
  }
  redoStack.length = 0;
  const key = typeof mergeKey === "string" && mergeKey.length > 0 ? mergeKey : null;
  const now = Date.now();
  const top = undoStack[undoStack.length - 1];
  if (key && top && top.key === key && top.touched !== null && now - top.touched <= MOCK_COALESCE_MS) {
    top.touched = now;
    return;
  }
  undoStack.push({ before, key, touched: key ? now : null });
  if (undoStack.length > MOCK_HISTORY_CAP) undoStack.shift();
}

function mockHistoryFlags() {
  return { can_undo: undoStack.length > 0, can_redo: redoStack.length > 0 };
}

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

// All mock times are integer µs, like the real core (schema v8).
function mockClipDuration(clip: Clip): number {
  if (clip.type === "caption" || clip.type === "adjustment" || clip.type === "text") return clip.duration_us;
  const speed = clip.speed && clip.speed > 0 ? clip.speed : 1;
  return Math.round((clip.source_out_us - clip.source_in_us) / speed);
}

/// A command `Time` parameter: integer µs, or a string such as "1.5s", "250ms", "90f" (frames at
/// the project rate). The app only sends integers; the string forms keep scripted calls honest.
function mockTime(project: Project, v: unknown): number {
  if (typeof v === "number") return Math.round(v);
  if (typeof v === "string") {
    const m = /^(-?\d+(?:\.\d+)?)(s|ms|f)?$/.exec(v.trim());
    if (m) {
      const n = Number(m[1]);
      if (m[2] === "f") return frameStart(normalizeFps(project.settings.fps), Math.round(n));
      if (m[2] === "ms") return Math.round(n * 1000);
      if (m[2] === "s") return S(n);
      return Math.round(n);
    }
  }
  throw new Error(`invalid time: ${String(v)}`);
}

/// Position-like times snap to the project's frame grid (the core's frame-boundary rule).
function mockFrame(project: Project, v: unknown): number {
  return snapToFrameUs(normalizeFps(project.settings.fps), mockTime(project, v));
}

function mockSequenceDuration(project: Project, sequenceId: string): number {
  const seq = project.sequences?.find((s) => s.id === sequenceId);
  if (!seq) return 0;
  let end = 0;
  for (const t of seq.tracks)
    for (const c of t.clips) if (c.type !== "caption" && c.enabled) end = Math.max(end, c.start_us + mockClipDuration(c));
  return end;
}

function mockOverlaps(track: Track, pos: number, dur: number, ignoreId?: string): boolean {
  // Magnetic tracks have no overlap: core inserts and ripples instead.
  if (track.magnetic) return false;
  return track.clips.some(
    (c) => c.id !== ignoreId && pos < c.start_us + mockClipDuration(c) && c.start_us < pos + dur,
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

// ---- Schema v8 effect commands (mirror renderly-core/src/commands/effects.rs) ----

function mockRefBase(ref: string): string {
  const at = ref.lastIndexOf("@");
  return at > 0 ? ref.slice(0, at) : ref;
}

function mockEntry(ref: string): CatalogueEntry | undefined {
  return mockCatalogue.find((e) => e.ref === ref) ?? mockCatalogue.find((e) => mockRefBase(e.ref) === mockRefBase(ref));
}

interface MockSpec {
  id?: string;
  ref: string;
  hash?: string;
  enabled?: boolean;
  params?: EffectParams;
}

/// A spec as an instance: an existing id keeps its params when none are given; a new instance
/// without params gets the effect's `initial` values (so a new blur is visible).
function mockBuildInstance(spec: MockSpec, current: EffectInstance[]): EffectInstance {
  const entry = spec.ref.startsWith("wasm:") ? undefined : mockEntry(spec.ref);
  if (!spec.ref.startsWith("wasm:") && !entry) throw new Error(`unknown effect '${spec.ref}' (see list_extensions)`);
  const existing = spec.id ? current.find((e) => e.id === spec.id) : undefined;
  const initial: EffectParams = {};
  for (const p of entry?.params ?? []) initial[p.key] = Array.isArray(p.initial) ? [...p.initial] : p.initial;
  const instance: EffectInstance = {
    id: spec.id ?? crypto.randomUUID(),
    ref: spec.ref,
    enabled: spec.enabled ?? true,
    params: spec.params ?? (existing && existing.ref === spec.ref ? existing.params : initial),
  };
  if (spec.ref.startsWith("pack:")) instance.hash = "sha256:mock-" + mockRefBase(spec.ref).slice(5);
  return instance;
}

/// Deleting an effect instance deletes its keyframe tracks.
function mockDropOrphanTracks(clip: EffectClip): void {
  const ids = new Set(effectStack(clip).map((e) => e.id));
  const kept = (clip.keyframes ?? []).filter((t) => {
    if (!t.property.startsWith("effect:")) return true;
    return ids.has(t.property.slice("effect:".length).split("/")[0]);
  });
  if (kept.length !== (clip.keyframes ?? []).length) clip.keyframes = kept;
}

function mockSetClipEffects(project: Project, cmd: Record<string, unknown>): void {
  const clip = mockFindClip(mockFindTrack(project, cmd.track_id as string), cmd.clip_id as string);
  if (!clip || clip.type === "caption") return;
  const hasAny = ["filter", "key", "adjust", "effects", "blend_mode"].some((k) => k in cmd);
  if (!hasAny) throw new Error("invalid effects: SetClipEffects needs at least one of filter, key, adjust, effects, blend_mode");
  const adjustment = clip.type === "adjustment";
  const noKey = adjustment || clip.type === "text";
  const current = effectStack(clip);
  const mk = (spec: MockSpec) => {
    if (adjustment && spec.ref.startsWith("wasm:"))
      throw new Error("invalid adjustment layer: WASM plugin effects cannot run on an adjustment layer");
    if (clip.type === "text" && spec.ref.startsWith("wasm:"))
      throw new Error("invalid effects: WASM plugin effects cannot run on a text clip");
    return mockBuildInstance(spec, current);
  };
  const slot = (name: "filter" | "key" | "adjust") => {
    if (!(name in cmd)) return;
    if (name === "key" && noKey) throw new Error("invalid effects: this clip has no key slot");
    const spec = cmd[name] as MockSpec | null;
    const record = clip as unknown as Record<string, unknown>;
    if (spec) record[name] = mk(spec);
    else delete record[name];
  };
  slot("filter");
  slot("key");
  slot("adjust");
  if ("effects" in cmd) clip.effects = (cmd.effects as MockSpec[]).map(mk);
  if ("blend_mode" in cmd) {
    if (cmd.blend_mode === "normal") delete clip.blend_mode;
    else clip.blend_mode = cmd.blend_mode as MediaClip["blend_mode"];
  }
  mockDropOrphanTracks(clip);
}

const MOCK_EASING_PRESETS: Record<string, [number, number, number, number]> = {
  ease_in: [0.42, 0, 1, 1],
  ease_out: [0, 0, 0.58, 1],
  ease_in_out: [0.42, 0, 0.58, 1],
};

function mockSetClipTransition(project: Project, cmd: Record<string, unknown>): void {
  const clip = mockFindClip(mockFindTrack(project, cmd.track_id as string), cmd.clip_id as string);
  if (!clip || (clip.type !== "video" && clip.type !== "audio" && clip.type !== "text")) return;
  const ref = cmd.ref as string | null | undefined;
  if (ref === undefined) throw new Error("SetClipTransition needs `ref` (null clears)");
  if (ref === null) {
    delete clip.outgoing_transition;
    return;
  }
  const entry = mockEntry(ref);
  if (!entry || entry.kind !== "transition") throw new Error(`'${ref}' is not a transition`);
  const existing = clip.outgoing_transition ?? null;
  const duration = (cmd.duration as number | undefined) ?? existing?.duration_us;
  if (duration === undefined) throw new Error("setting a new transition needs a duration");
  let easing = existing && mockRefBase(existing.ref) === mockRefBase(ref) ? existing.easing : null;
  if ("easing" in cmd) {
    const e = cmd.easing as string | { bezier: [number, number, number, number] } | null;
    easing = e == null ? null : typeof e === "string" ? (e === "linear" ? "linear" : { bezier: MOCK_EASING_PRESETS[e] }) : e;
  }
  const sameRef = existing && existing.ref === ref;
  const initial: EffectParams = {};
  for (const p of entry.params) initial[p.key] = Array.isArray(p.initial) ? [...p.initial] : p.initial;
  clip.outgoing_transition = {
    ref,
    ...(ref.startsWith("pack:") ? { hash: "sha256:mock-" + mockRefBase(ref).slice(5) } : {}),
    duration_us: Math.round(duration),
    params: (cmd.params as EffectParams | undefined) ?? (sameRef ? existing.params : initial),
    easing,
  };
}

function mockSetKeyframeEasing(project: Project, cmd: Record<string, unknown>): void {
  const clip = mockFindClip(mockFindTrack(project, cmd.track_id as string), cmd.clip_id as string);
  if (!clip || clip.type === "caption") return;
  const tracks: KeyframeTrack[] = clip.keyframes ?? [];
  const track = tracks.find((t) => t.property === cmd.property);
  if (!track) throw new Error(`invalid keyframes: the clip has no keyframe track '${String(cmd.property)}'`);
  track.keys.sort((a, b) => a.time_us - b.time_us);
  const at = cmd.time as number;
  const idx = track.keys.findIndex((k) => Math.abs(k.time_us - at) <= 8_000);
  if (idx < 0) throw new Error(`invalid keyframes: no key of '${String(cmd.property)}' at ${at} us`);
  if (idx + 1 >= track.keys.length)
    throw new Error("invalid keyframes: that is the last key: easing describes the span after a key, and there is none");
  const a = track.keys[idx];
  const b = track.keys[idx + 1];
  const choice = cmd.easing as string | { bezier: [number, number, number, number] };
  const pts = typeof choice === "string" ? MOCK_EASING_PRESETS[choice] : choice.bezier;
  if (pts) {
    const [x1, y1, x2, y2] = pts;
    const span = b.time_us - a.time_us;
    const vector = Array.isArray(a.value) || Array.isArray(b.value);
    const scale = vector ? 1 : (b.value as number) - (a.value as number);
    a.easing = "bezier";
    a.handle_right = [Math.round(x1 * span), y1 * scale];
    b.handle_left = [Math.round((x2 - 1) * span), (y2 - 1) * scale];
  } else {
    a.easing = choice as "linear" | "hold" | "auto";
    delete a.handle_right;
    delete b.handle_left;
  }
}

/// Effects and transitions whose ref the mock catalogue does not know (what `get_effect_status`
/// reports as missing).
function mockEffectStatus(project: Project): { clip_id: string; instance_id?: string; ref: string; status: string }[] {
  const out: { clip_id: string; instance_id?: string; ref: string; status: string }[] = [];
  const containers = [project.tracks, ...(project.sequences ?? []).map((s) => s.tracks)];
  for (const tracks of containers) {
    for (const t of tracks) {
      for (const c of t.clips) {
        if (c.type === "caption") continue;
        for (const e of effectStack(c)) {
          if (!e.ref.startsWith("wasm:") && !mockEntry(e.ref)) out.push({ clip_id: c.id, instance_id: e.id, ref: e.ref, status: "missing" });
        }
        if (c.type !== "adjustment" && c.outgoing_transition && !mockEntry(c.outgoing_transition.ref))
          out.push({ clip_id: c.id, ref: c.outgoing_transition.ref, status: "missing" });
      }
    }
  }
  return out;
}

// ---- Magnetic tracks (mirror renderly-core/src/commands/magnetic.rs; simplified: starts are
// frame-snapped, lengths untouched) ----

/** Clips in start order, each starting where the previous one ends, the first at 0. */
function mockPack(project: Project, track: Track): void {
  const fps = normalizeFps(project.settings.fps);
  track.clips.sort((a, b) => a.start_us - b.start_us);
  let cursor = 0;
  for (const c of track.clips) {
    c.start_us = cursor;
    cursor = Math.max(snapToFrameUs(fps, cursor + mockClipDuration(c)), cursor + 1);
  }
}

function mockPackAll(project: Project): void {
  for (const container of mockContainers(project)) for (const t of container.tracks) if (t.magnetic) mockPack(project, t);
}

/** Insert `clip` where its centre falls among the clips already there, then close up. */
function mockInsertMagnetic(project: Project, track: Track, clip: Clip): void {
  const fps = normalizeFps(project.settings.fps);
  const centre = clip.start_us + Math.floor(mockClipDuration(clip) / 2);
  track.clips.sort((a, b) => a.start_us - b.start_us);
  const at = track.clips.filter((c) => c.start_us + Math.floor(mockClipDuration(c) / 2) <= centre).length;
  track.clips.splice(at, 0, clip);
  let cursor = 0;
  for (const c of track.clips) {
    c.start_us = cursor;
    cursor = Math.max(snapToFrameUs(fps, cursor + mockClipDuration(c)), cursor + 1);
  }
}

/** Put a new clip on a track: magnetic tracks insert by centre, others just append. */
function mockPlace(project: Project, track: Track, clip: Clip): void {
  if (track.magnetic) mockInsertMagnetic(project, track, clip);
  else track.clips.push(clip);
}

// ---- Schema v8 text commands (mirror renderly-core/src/commands/text.rs; abridged) ----

function mockStyle(spec: unknown): TextStyleRef | null {
  if (spec == null) return null;
  const s = spec as Partial<TextStyleRef>;
  if (!s.ref || !/^(builtin:[\w-]+@\d+|pack:[\w.-]+\/[\w-]+@\d+)$/.test(s.ref)) throw new Error(`invalid text: malformed style ref '${String(s.ref)}'`);
  return { ref: s.ref, ...(s.hash ? { hash: s.hash } : {}), params: s.params ?? {}, overrides: s.overrides ?? {} };
}

function mockBody(project: Project, input: { text: string; words?: { text: string; start: unknown; end: unknown; emphasis?: boolean; word_id?: string }[] }, duration: number | null, keep?: CaptionClip["body"]) {
  const words: BodyWord[] | undefined = input.words?.map((w) => ({
    text: w.text,
    start_us: mockTime(project, w.start),
    end_us: mockTime(project, w.end),
    ...(w.emphasis ? { emphasis: true } : {}),
    ...(w.word_id ? { word_id: w.word_id } : {}),
  }));
  const body = { text: input.text, runs: keep?.runs ?? [], words: words ?? keep?.words ?? [] };
  if (duration != null && body.words.some((w) => w.end_us > duration)) throw new Error("invalid text: a word ends after the clip ends");
  return body;
}

/** `{track_id, clip_id}` or `targets[]` as clip references to caption / text clips. */
function mockTextTargets(project: Project, cmd: Record<string, unknown>): (CaptionClip | TextClip)[] {
  const refs = (cmd.targets as { track_id: string; clip_id: string }[] | undefined)?.length
    ? (cmd.targets as { track_id: string; clip_id: string }[])
    : [{ track_id: cmd.track_id as string, clip_id: cmd.clip_id as string }];
  return refs.map((r) => {
    const clip = mockFindClip(mockFindTrack(project, r.track_id), r.clip_id);
    if (!clip || (clip.type !== "caption" && clip.type !== "text")) throw new Error(`invalid text: ${r.clip_id} is not a caption or text clip`);
    return clip;
  });
}

function mockSplitCaptions(project: Project, cmd: Record<string, unknown>): void {
  const track = mockFindTrack(project, cmd.track_id as string);
  if (!track || track.kind !== "caption") throw new Error("invalid text: not a caption track");
  const maxChars = cmd.max_chars as number;
  const maxLines = cmd.max_lines as number;
  const maxDur = mockTime(project, cmd.max_duration);
  if (!(maxChars > 0) || !(maxLines > 0) || !(maxDur > 0)) throw new Error("invalid text: limits must be > 0");
  const fps = normalizeFps(project.settings.fps);
  const caps = track.clips.filter((c): c is CaptionClip => c.type === "caption").sort((a, b) => a.start_us - b.start_us);
  const typed = caps.filter((c) => c.body.words.length === 0);
  // Words on the timeline in order, with the style of the caption each came from.
  const words = caps.flatMap((c) =>
    c.body.words.map((w) => ({ ...w, start_us: c.start_us + w.start_us, end_us: c.start_us + w.end_us, style: c.style })),
  );
  const pages: (typeof words)[] = [];
  let page: typeof words = [];
  let lines = 1;
  let lineLen = 0;
  for (const w of words) {
    const len = w.text.length;
    const startNew = () => {
      if (page.length) pages.push(page);
      page = [];
      lines = 1;
      lineLen = 0;
    };
    if (page.length && w.end_us - page[0].start_us > maxDur) startNew();
    if (page.length) {
      if (lineLen + 1 + len <= maxChars) lineLen += 1 + len;
      else if (lines < maxLines) {
        lines += 1;
        lineLen = len;
      } else {
        startNew();
        lineLen = len;
      }
    } else lineLen = len;
    page.push(w);
  }
  if (page.length) pages.push(page);
  const made: CaptionClip[] = [];
  let prevEnd = 0;
  for (const pg of pages) {
    const start = Math.max(snapToFrameUs(fps, pg[0].start_us), prevEnd);
    const end = Math.max(snapToFrameUs(fps, pg[pg.length - 1].end_us), start + 1);
    // Lines greedy-filled with the same limits, joined with a newline.
    const out: string[] = [];
    let cur = "";
    for (const w of pg) {
      if (cur && cur.length + 1 + w.text.length > maxChars) {
        out.push(cur);
        cur = w.text;
      } else cur = cur ? `${cur} ${w.text}` : w.text;
    }
    if (cur) out.push(cur);
    made.push({
      type: "caption",
      id: crypto.randomUUID(),
      start_us: start,
      duration_us: end - start,
      body: {
        text: out.join("\n"),
        runs: [],
        words: pg.map(({ style: _style, ...w }) => ({ ...w, start_us: w.start_us - start, end_us: Math.min(w.end_us - start, end - start) })),
      },
      style: pg[0].style,
      animations: {},
    });
    prevEnd = end;
  }
  for (const t of typed) if (made.some((m) => t.start_us < m.start_us + m.duration_us && m.start_us < t.start_us + t.duration_us)) throw new Error("overlap: a typed caption would overlap a page");
  track.clips = [...typed, ...made].sort((a, b) => a.start_us - b.start_us);
}

/** Ranges of source time (µs) made of consecutive deleted words of `transcript`. */
function mockDeletedRanges(words: { start_us: number; end_us: number; deleted: boolean }[]): [number, number][] {
  const out: [number, number][] = [];
  let open: [number, number] | null = null;
  for (const w of words) {
    if (w.deleted) open = open ? [open[0], w.end_us] : [w.start_us, w.end_us];
    else if (open) {
      out.push(open);
      open = null;
    }
  }
  if (open) out.push(open);
  return out;
}

/** Cut `[a, b)` of source time out of every clip of `mediaId`, closing the gaps (ripple). */
function mockCutSource(project: Project, mediaId: string, a: number, b: number): void {
  for (const container of mockContainers(project)) {
    for (const track of container.tracks) {
      if (track.kind === "caption") continue;
      const next: Clip[] = [];
      let shift = 0;
      const ordered = [...track.clips].sort((x, y) => x.start_us - y.start_us);
      let changed = false;
      for (const clip of ordered) {
        if ((clip.type !== "video" && clip.type !== "audio") || clip.media_id !== mediaId) {
          next.push({ ...clip, start_us: clip.start_us - shift });
          continue;
        }
        const lo = Math.max(clip.source_in_us, a);
        const hi = Math.min(clip.source_out_us, b);
        if (hi <= lo) {
          next.push({ ...clip, start_us: clip.start_us - shift });
          continue;
        }
        changed = true;
        const pos = clip.start_us - shift;
        if (lo > clip.source_in_us) next.push({ ...clip, start_us: pos, source_out_us: lo });
        if (hi < clip.source_out_us)
          next.push({ ...structuredClone(clip), id: crypto.randomUUID(), start_us: pos + (lo > clip.source_in_us ? lo - clip.source_in_us : 0), source_in_us: hi });
        shift += hi - lo;
      }
      if (changed) track.clips = next;
    }
  }
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function applyMockCommand(project: Project, cmd: any): void {
  applyMockCommandInner(project, cmd);
  mockPackAll(project);
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function applyMockCommandInner(project: Project, cmd: any): void {
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
      // The first video track of the root timeline is magnetic unless said otherwise.
      if (cmd.magnetic === true && cmd.kind === "caption") throw new Error("invalid text: caption tracks cannot be magnetic");
      const magnetic = cmd.magnetic ?? (cmd.kind === "video" && !cmd.sequence_id && !project.tracks.some((t) => t.kind === "video"));
      if (magnetic) track.magnetic = true;
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
      const start = mockFrame(project, cmd.start);
      const srcIn = mockTime(project, cmd.source_in);
      const srcOut = mockTime(project, cmd.source_out);
      if (mockOverlaps(track, start, srcOut - srcIn))
        throw new Error(`clip range [${start}, ${start + srcOut - srcIn}) overlaps an existing clip on track ${track.id}`);
      const clip: MediaClip = {
        type: track.kind === "audio" ? "audio" : "video",
        id: crypto.randomUUID(),
        media_id: cmd.media_id,
        start_us: start,
        source_in_us: srcIn,
        source_out_us: srcOut,
        gain_db: 0,
        enabled: true,
        fade_in_us: 0,
        fade_out_us: 0,
      };
      mockPlace(project, track, clip);
      break;
    }
    case "AddCaption": {
      const track = mockFindTrack(project, cmd.track_id);
      if (!track) break;
      if (track.kind !== "caption") throw new Error("track kind mismatch");
      const duration = mockTime(project, cmd.duration);
      const start = mockFrame(project, cmd.start);
      if (!(duration > 0)) throw new Error("invalid duration");
      if (mockOverlaps(track, start, duration)) throw new Error(`clip range [${start}, ${start + duration}) overlaps an existing clip on track ${track.id}`);
      track.clips.push({
        type: "caption",
        id: crypto.randomUUID(),
        start_us: start,
        duration_us: duration,
        body: mockBody(project, cmd.body, duration),
        style: mockStyle(cmd.style),
        animations: cmd.animations ?? {},
      });
      break;
    }
    case "AddText": {
      const track = mockFindTrack(project, cmd.track_id);
      if (!track) throw new Error(`track not found: ${cmd.track_id}`);
      if (track.kind !== "video") throw new Error("invalid text: titles go on video tracks");
      const duration = mockTime(project, cmd.duration);
      const start = mockFrame(project, cmd.start);
      if (!(duration > 0)) throw new Error("invalid duration");
      if (mockOverlaps(track, start, duration)) throw new Error(`clip range [${start}, ${start + duration}) overlaps an existing clip on track ${track.id}`);
      const clip: TextClip = {
        type: "text",
        id: crypto.randomUUID(),
        start_us: start,
        duration_us: duration,
        enabled: true,
        body: mockBody(project, cmd.body, duration),
        style: mockStyle(cmd.style) ?? { ref: "builtin:tiktok-minimal@1", params: {}, overrides: { anchor: "center" } },
        animations: cmd.animations ?? {},
      };
      mockPlace(project, track, clip);
      break;
    }
    case "SetTextTiming": {
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      if (!track || !clip || clip.type !== "text") throw new Error("invalid text: not a text clip");
      if (cmd.start == null && cmd.duration == null) throw new Error("invalid text: SetTextTiming needs start and/or duration");
      const start = cmd.start != null ? mockFrame(project, cmd.start) : clip.start_us;
      const duration = cmd.duration != null ? mockTime(project, cmd.duration) : clip.duration_us;
      if (!(duration > 0)) throw new Error("invalid duration");
      if (mockOverlaps(track, start, duration, clip.id)) throw new Error(`clip range [${start}, ${start + duration}) overlaps an existing clip on track ${track.id}`);
      clip.start_us = start;
      clip.duration_us = duration;
      break;
    }
    case "SetTextBody": {
      if (cmd.text == null && cmd.runs == null) throw new Error("invalid text: SetTextBody needs text and/or runs");
      for (const clip of mockTextTargets(project, cmd)) {
        if (cmd.text != null) clip.body.text = cmd.text;
        if (cmd.runs != null) clip.body.runs = cmd.runs;
      }
      break;
    }
    case "SetTextStyle": {
      if (!("ref" in cmd) && cmd.hash == null && cmd.params == null && cmd.overrides == null) throw new Error("invalid text: SetTextStyle needs a field");
      const targets = mockTextTargets(project, cmd);
      const fresh = "ref" in cmd && cmd.ref != null ? mockStyle({ ref: cmd.ref }) : null;
      for (const clip of targets) {
        if ("ref" in cmd) {
          if (cmd.ref == null) {
            if (clip.type === "caption") clip.style = null;
            else clip.style = { ref: "builtin:tiktok-minimal@1", params: {}, overrides: { anchor: "center" } };
            continue;
          }
          const keep = clip.style;
          clip.style = { ref: fresh!.ref, params: keep?.params ?? {}, overrides: keep?.overrides ?? {} } as TextStyleRef;
        }
        if (clip.style) {
          if (cmd.params != null) clip.style.params = cmd.params;
          if (cmd.overrides != null) clip.style.overrides = cmd.overrides;
        }
      }
      break;
    }
    case "SetTextAnimation": {
      for (const clip of mockTextTargets(project, cmd)) {
        if (cmd.ref == null) delete clip.animations[cmd.phase as "in"];
        else clip.animations[cmd.phase as "in"] = { ref: cmd.ref, params: cmd.params ?? {}, ...(cmd.duration != null ? { duration_us: mockTime(project, cmd.duration) } : {}) };
      }
      break;
    }
    case "SetTrackCaptionStyle": {
      const track = mockFindTrack(project, cmd.track_id);
      if (!track || track.kind !== "caption") throw new Error("invalid text: not a caption track");
      const style = mockStyle(cmd.style);
      if (style) track.caption_style = style;
      else delete track.caption_style;
      if (cmd.reset_clips) for (const c of track.clips) if (c.type === "caption") c.style = null;
      break;
    }
    case "SetCaptionWords": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (!clip || clip.type !== "caption") throw new Error("invalid text: not a caption");
      clip.body = mockBody(project, { text: clip.body.text, words: cmd.words }, clip.duration_us);
      break;
    }
    case "SetWordEmphasis": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (!clip || clip.type !== "caption") throw new Error("invalid text: not a caption");
      for (const i of cmd.indices as number[]) {
        const w = clip.body.words[i];
        if (!w) throw new Error(`invalid text: no word at index ${i}`);
        if (cmd.emphasis ?? true) w.emphasis = true;
        else delete w.emphasis;
      }
      break;
    }
    case "SplitCaptions":
      mockSplitCaptions(project, cmd);
      break;
    case "AddMarkers": {
      const input = cmd.markers as { id?: string; time: unknown; label?: string; color?: string; kind?: "user" | "beat" }[];
      if (!input?.length) throw new Error("invalid marker: no markers given");
      const next = [...(project.markers ?? [])];
      for (const m of input) {
        const time = mockTime(project, m.time);
        if (time < 0) throw new Error("invalid marker: negative time");
        if (m.color && !/^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/.test(m.color)) throw new Error(`invalid marker: '${m.color}' is not a colour`);
        const id = m.id ?? crypto.randomUUID();
        if (next.some((x) => x.id === id)) throw new Error(`invalid marker: id ${id} already exists`);
        next.push({ id, time_us: time, label: m.label ?? "", color: m.color ?? "", kind: m.kind ?? "user" });
      }
      project.markers = next.sort((a, b) => a.time_us - b.time_us);
      break;
    }
    case "RemoveMarkers": {
      const ids = (cmd.ids as string[] | undefined) ?? [];
      if (ids.length === 0 && !cmd.kind) throw new Error("invalid marker: give ids and/or a kind");
      for (const id of ids) if (!project.markers?.some((m) => m.id === id)) throw new Error(`invalid marker: no marker ${id}`);
      const left = (project.markers ?? []).filter((m) => !ids.includes(m.id) && m.kind !== cmd.kind);
      if (left.length) project.markers = left;
      else delete project.markers;
      break;
    }
    case "ApplyTranscriptCuts": {
      const tr = project.transcripts?.find((t) => t.id === cmd.transcript_id);
      if (!tr) throw new Error(`invalid transcript: no transcript ${cmd.transcript_id}`);
      const ids = new Set<string>(cmd.word_ids ?? []);
      for (const id of ids) if (!tr.words.some((w) => w.id === id)) throw new Error(`invalid transcript: no word ${id}`);
      const tags = new Set<string>(cmd.tags ?? []);
      const before = mockDeletedRanges(tr.words);
      for (const w of tr.words) if (ids.has(w.id) || w.tags.some((t) => tags.has(t))) w.deleted = true;
      if (!tr.words.some((w) => w.deleted)) throw new Error("invalid transcript: no words selected");
      const fresh = mockDeletedRanges(tr.words).filter(([a, b]) => !before.some(([x, y]) => x === a && y === b));
      for (const [a, b] of fresh) mockCutSource(project, tr.media_id, a, b);
      break;
    }
    case "RestoreTranscriptWords": {
      const tr = project.transcripts?.find((t) => t.id === cmd.transcript_id);
      if (!tr) throw new Error(`invalid transcript: no transcript ${cmd.transcript_id}`);
      const ids = new Set<string>(cmd.word_ids ?? []);
      const targets = tr.words.filter((w) => w.deleted && (ids.size === 0 || ids.has(w.id)));
      if (targets.length === 0) throw new Error("invalid transcript: no deleted words to restore");
      // Simplified: the words come back as a clip of that media after the clip nearest before.
      for (const w of targets) w.deleted = false;
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
      if (cmd.magnetic != null) {
        if (cmd.magnetic && track.kind === "caption") throw new Error("invalid text: caption tracks cannot be magnetic");
        if (cmd.magnetic) track.magnetic = true;
        else delete track.magnetic;
      }
      break;
    }
    case "SetClipEnabled": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type !== "caption") clip.enabled = cmd.enabled;
      break;
    }
    case "SetClipTransform": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio" || clip.type === "text")) clip.transform = cmd.transform;
      break;
    }
    case "SetClipKeyframes": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type === "adjustment") {
        if ((cmd.keyframes as { property: string }[]).some((k) => k.property !== "opacity"))
          throw new Error("invalid adjustment layer: only opacity keyframes apply");
        clip.keyframes = cmd.keyframes;
      } else if (clip && (clip.type === "video" || clip.type === "audio" || clip.type === "text")) clip.keyframes = cmd.keyframes;
      break;
    }
    case "SetClipEffects":
      mockSetClipEffects(project, cmd);
      break;
    case "SetClipTransition":
      mockSetClipTransition(project, cmd);
      break;
    case "SetKeyframeEasing":
      mockSetKeyframeEasing(project, cmd);
      break;
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
        clip.fade_in_us = mockTime(project, cmd.fade_in);
        clip.fade_out_us = mockTime(project, cmd.fade_out);
      }
      break;
    }
    case "SetCaption": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && clip.type === "caption") {
        if (cmd.body == null && cmd.start == null && cmd.duration == null && !("style" in cmd) && cmd.animations == null)
          throw new Error("invalid text: SetCaption needs at least one field");
        if (cmd.start != null) clip.start_us = mockFrame(project, cmd.start);
        if (cmd.duration != null) clip.duration_us = mockTime(project, cmd.duration);
        if (cmd.body != null) clip.body = mockBody(project, cmd.body, clip.duration_us, clip.body);
        if ("style" in cmd) clip.style = mockStyle(cmd.style);
        if (cmd.animations != null) clip.animations = cmd.animations;
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
            start_us: clip.source_in_us,
            fps: { num: 30, den: 1 },
            count: Math.round(((clip.source_out_us - clip.source_in_us) / 1_000_000) * 30) + 1,
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
        if (dest && mockOverlaps(dest, mockFrame(project, cmd.start), mockClipDuration(clip), clip.id))
          throw new Error(`clip range overlaps an existing clip on track ${dest.id}`);
      }
      clip.start_us = mockFrame(project, cmd.start);
      {
        const dest = cmd.new_track_id ? mockFindTrack(project, cmd.new_track_id) : track;
        if (dest?.magnetic) {
          // Reorders by centre among the clips there; the source closes up (global pack).
          track.clips = track.clips.filter((c) => c.id !== cmd.clip_id);
          mockInsertMagnetic(project, dest, clip);
        } else if (dest && dest.id !== track.id) {
          track.clips = track.clips.filter((c) => c.id !== cmd.clip_id);
          dest.clips.push(clip);
        }
      }
      break;
    }
    case "TrimClip": {
      const clip = mockFindClip(mockFindTrack(project, cmd.track_id), cmd.clip_id);
      if (clip && (clip.type === "video" || clip.type === "audio")) {
        if (cmd.source_in != null) clip.source_in_us = mockTime(project, cmd.source_in);
        if (cmd.source_out != null) clip.source_out_us = mockTime(project, cmd.source_out);
      }
      break;
    }
    case "SetProjectSettings": {
      if (cmd.width != null) project.settings.width = cmd.width;
      if (cmd.height != null) project.settings.height = cmd.height;
      if (cmd.fps != null) project.settings.fps = fpsFromNumber(cmd.fps);
      break;
    }
    case "SplitClip": {
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      if (!track || !clip) break;
      const at: number = mockFrame(project, cmd.at);
      const dur = mockClipDuration(clip);
      if (at <= clip.start_us || at >= clip.start_us + dur) throw new Error("invalid split point");
      const left = at - clip.start_us;
      const right = structuredClone(clip) as Clip;
      right.id = crypto.randomUUID();
      right.start_us = at;
      if (clip.type === "adjustment" || clip.type === "caption" || clip.type === "text") {
        clip.duration_us = left;
        (right as AdjustmentClip).duration_us = dur - left;
        if (clip.type === "text") delete (right as TextClip).outgoing_transition;
        if (clip.type === "caption" || clip.type === "text") {
          // Words go to the side holding their midpoint; the right side is rebased.
          const rc = right as CaptionClip | TextClip;
          const mid = (w: BodyWord) => (w.start_us + w.end_us) / 2;
          clip.body = { ...clip.body, words: clip.body.words.filter((w) => mid(w) < left).map((w) => ({ ...w, end_us: Math.min(w.end_us, left) })) };
          rc.body = { ...rc.body, words: rc.body.words.filter((w) => mid(w) >= left).map((w) => ({ ...w, start_us: Math.max(0, w.start_us - left), end_us: Math.max(0, w.end_us - left) })) };
        }
      } else {
        const speed = clip.speed && clip.speed > 0 ? clip.speed : 1;
        const cut = clip.source_in_us + Math.round(left * speed);
        (right as MediaClip).source_in_us = cut;
        clip.source_out_us = cut;
      }
      track.clips.push(right);
      break;
    }
    case "CreateSequence": {
      const id: string = cmd.id ?? crypto.randomUUID();
      if (project.sequences?.some((s) => s.id === id)) throw new Error(`sequence id ${id} already exists`);
      if (cmd.width === 0 || cmd.height === 0) throw new Error("invalid dimensions");
      (project.sequences ??= []).push({ id, name: cmd.name, width: cmd.width ?? null, height: cmd.height ?? null, tracks: [] });
      project.media.push({ id: crypto.randomUUID(), path: "", kind: "sequence", sequence_id: id, duration_us: null, width: null, height: null, fps: null });
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
      const start = Math.min(...picked.map((p) => p.clip.start_us));
      const end = Math.max(...picked.map((p) => p.clip.start_us + mockClipDuration(p.clip)));
      const seqId = crypto.randomUUID();
      const mediaId = crypto.randomUUID();
      // Inner tracks mirror the source tracks (kind/name/flags); clips keep their ids and are
      // shifted so the sequence starts at 0.
      const inner: Track[] = [];
      for (const src of container.tracks) {
        const mine = picked.filter((p) => p.track.id === src.id);
        if (mine.length === 0) continue;
        inner.push({ ...src, id: crypto.randomUUID(), clips: mine.map((p) => ({ ...p.clip, start_us: p.clip.start_us - start })) });
      }
      for (const p of picked) p.track.clips = p.track.clips.filter((c) => c.id !== p.clip.id);
      (project.sequences ??= []).push({ id: seqId, name: cmd.name, width: null, height: null, tracks: inner });
      project.media.push({ id: mediaId, path: "", kind: "sequence", sequence_id: seqId, duration_us: null, width: null, height: null, fps: null });
      const hasVideo = picked.some((p) => p.track.kind === "video");
      const hasAudio = picked.some((p) => p.track.kind === "audio");
      const place = (kind: "video" | "audio") => {
        const candidates = container.tracks.filter((t) => t.kind === kind);
        let target = [...candidates].reverse().find((t) => !mockOverlaps(t, start, end - start));
        if (!target) {
          target = { id: crypto.randomUUID(), kind, name: kind === "video" ? "Nested video" : "Nested audio", muted: false, locked: false, hidden: false, clips: [] };
          container.tracks.push(target);
        }
        target.clips.push({ type: kind, id: crypto.randomUUID(), media_id: mediaId, start_us: start, source_in_us: 0, source_out_us: end - start, gain_db: 0, enabled: true, fade_in_us: 0, fade_out_us: 0 });
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
      if (clip.source_in_us > 0 || Math.abs(clip.source_out_us - fullDur) > 1) throw new Error("cannot unnest: the clip is trimmed");
      if (clip.speed && Math.abs(clip.speed - 1) > 1e-6) throw new Error("cannot unnest: the clip has a speed change");
      if ((clip.effects?.length ?? 0) > 0) throw new Error("cannot unnest: the clip has effects");
      const wantAudio = clip.type === "audio";
      const copied: Track[] = [];
      for (const inner of seq.tracks) {
        if ((inner.kind === "audio") !== wantAudio || inner.kind === "caption") continue;
        copied.push({ ...inner, id: crypto.randomUUID(), clips: inner.clips.map((c) => ({ ...structuredClone(c), id: crypto.randomUUID(), start_us: c.start_us + clip.start_us })) });
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
      const start = mockFrame(project, cmd.start);
      const duration = mockTime(project, cmd.duration);
      if (!(duration > 0)) throw new Error("invalid duration");
      if (mockOverlaps(track, start, duration))
        throw new Error(`clip range [${start}, ${start + duration}) overlaps an existing clip on track ${track.id}`);
      track.clips.push({ type: "adjustment", id: crypto.randomUUID(), start_us: start, duration_us: duration, enabled: true, opacity: 1, effects: [], mask: null, keyframes: [] });
      break;
    }
    case "SetAdjustmentLayer": {
      if (cmd.start == null && cmd.duration == null && cmd.opacity == null)
        throw new Error("SetAdjustmentLayer requires at least one of start/duration/opacity");
      const track = mockFindTrack(project, cmd.track_id);
      const clip = mockFindClip(track, cmd.clip_id);
      if (!track || !clip) throw new Error(`clip not found: ${cmd.clip_id}`);
      if (clip.type !== "adjustment") throw new Error(`clip ${clip.id} is not an adjustment layer`);
      const pos = cmd.start != null ? mockFrame(project, cmd.start) : clip.start_us;
      const dur = cmd.duration != null ? mockTime(project, cmd.duration) : clip.duration_us;
      if (pos < 0 || !(dur > 0)) throw new Error("invalid adjustment layer: bad position or duration");
      if (cmd.opacity != null && (cmd.opacity < 0 || cmd.opacity > 1)) throw new Error("invalid adjustment layer: opacity out of range");
      if (mockOverlaps(track, pos, dur, clip.id)) throw new Error(`clip range overlaps an existing clip on track ${track.id}`);
      clip.start_us = pos;
      clip.duration_us = dur;
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
// Playhead of the mock backend, integer µs (as the real backend reports).
let mockTimeUs = 0;

function stopMockPlayback(): number {
  if (playTimer != null) {
    clearInterval(playTimer);
    playTimer = null;
  }
  return mockTimeUs;
}

function startMockPlayback(fromUs: number): void {
  stopMockPlayback();
  mockTimeUs = fromUs;
  const start = performance.now();
  const base = fromUs;
  playTimer = setInterval(() => {
    mockTimeUs = Math.round(base + (performance.now() - start) * 1000);
    emit("playback:tick", { time_us: mockTimeUs, playing: true });
  }, 33);
  emit("playback:state", { playing: true, time_us: fromUs });
}

// ---- Scripted fake agent (agent chat, docs/agent-chat-design.md). Stands in for the Rust
// agent_chat backend so the Agent tab can be exercised in the browser harness. Prompts with
// "export" script a permission prompt; any other prompt trims the first video clip for real
// through the mock's apply_commands (so the turn's Undo works against the mock's undo stack).
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
    note: "Codex: the handshake and model list were checked against codex-cli 0.163.0. A full turn is not yet verified.",
  },
  {
    id: "ollama",
    name: "Ollama (local)",
    installed: true,
    version: null,
    signIn: "unknown",
    signInCommand: null,
    installHint: "Install Ollama from https://ollama.com, start it, then pull a model with tool support.",
    note: "not running — start Ollama",
  },
];

/** Mock model lists, shaped like the backend's `agent_models` reply. */
const MOCK_MODELS: Record<string, unknown> = {
  "claude-code": {
    agentId: "claude-code",
    status: "ready",
    models: [
      { id: "default", label: "Default (recommended)", description: null, efforts: ["low", "medium", "high", "xhigh", "max"], defaultEffort: null, contextLength: null, vision: false, thinking: false },
      { id: "sonnet", label: "Sonnet 5.5", description: null, efforts: ["low", "medium", "high", "xhigh", "max"], defaultEffort: null, contextLength: null, vision: false, thinking: false },
    ],
    defaultModel: "default",
    message: null,
  },
  codex: {
    agentId: "codex",
    status: "not_installed",
    models: [],
    defaultModel: null,
    message: "Install the Codex CLI from OpenAI, then run `codex login`.",
  },
  ollama: {
    agentId: "ollama",
    status: "not_running",
    models: [],
    defaultModel: null,
    message: "Ollama is not running at http://127.0.0.1:11434. Start Ollama, then refresh.",
  },
};

function agentEmit(event: Record<string, unknown>): void {
  if (mockAgent.sessionId) emit("agent:event", { session_id: mockAgent.sessionId, ...event });
}

function firstVideoClip(): { trackId: string; clipId: string; sourceOut: number } | null {
  for (const track of currentProject.tracks) {
    if (track.kind !== "video") continue;
    const clip = track.clips.find((c) => c.type === "video");
    if (clip && clip.type === "video") {
      return { trackId: track.id, clipId: clip.id, sourceOut: clip.source_out_us };
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
  // The real agent reads the outline first (hidden in the transcript, kept under Details).
  const outlineId = `mock-outline-${turn}`;
  agentEmit({ type: "tool_started", id: outlineId, name: "mcp__renderly__timeline", input: {} });
  await pause(250);
  if (!live()) return;
  agentEmit({ type: "tool_finished", id: outlineId, output_summary: "track 0 video 'V1' ...", is_error: false });

  const command = {
    command: "TrimClip",
    track_id: target.trackId,
    clip_id: target.clipId,
    source_out: Math.min(S(5), target.sourceOut),
  };
  await pause(300);
  if (!live()) return;
  // One `edit` call is one undo step, the same path the GUI uses (apply_commands).
  const toolId = `mock-tu-${turn}`;
  const input = { commands: [command] };
  agentEmit({ type: "tool_started", id: toolId, name: "mcp__renderly__edit", input });
  try {
    await invoke("apply_commands", { commands: [command] });
    await pause(250);
    const clip = currentProject.tracks.flatMap((t) => t.clips).find((c) => c.id === target.clipId);
    const media = clip && clip.type === "video" ? currentProject.media.find((m) => m.id === clip.media_id) : undefined;
    const name = media ? media.path.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, "") ?? "clip" : "clip";
    const summary = [
      "Applied 1 command(s) as one undo step:",
      `- Trimmed '${name}' to 0:00.0-0:0${Math.min(5, target.sourceOut / 1_000_000) | 0}.0`,
    ].join("\n");
    agentEmit({ type: "tool_finished", id: toolId, output_summary: summary, is_error: false });
  } catch (e) {
    agentEmit({ type: "tool_finished", id: toolId, output_summary: String(e), is_error: true });
  }
  if (!live()) return;
  await say("Trimmed it to five seconds.");
  agentEmit({ type: "turn_finished", stop_reason: "end_turn" });
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function invoke<T = unknown>(cmd: string, args?: any): Promise<T> {
  const reply = (v: unknown) => Promise.resolve(v as T);
  switch (cmd) {
    case "play":
      startMockPlayback((args?.timeUs as number) ?? 0);
      return reply(undefined);
    case "pause": {
      const t = stopMockPlayback();
      emit("playback:state", { playing: false, time_us: t });
      return reply(t);
    }
    case "seek": {
      mockTimeUs = (args?.timeUs as number) ?? mockTimeUs;
      return reply(undefined);
    }
    case "list_projects":
      return reply([
        { path: "C:/proj/ep12.renderly.json", name: "Ultra Bruno ep.12", durationUs: S(84), modifiedMs: Date.now() - 7.2e6, width: 1080, height: 1920, fps: 60 },
        { path: "C:/proj/montage.renderly.json", name: "ranked grind montage", durationUs: S(58), modifiedMs: Date.now() - 9e7, width: 1080, height: 1920, fps: 60 },
        { path: "C:/proj/recap.renderly.json", name: "tourney recap", durationUs: S(192), modifiedMs: Date.now() - 3.4e8, width: 1920, height: 1080, fps: 30 },
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
      mockTx = null;
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
    case "get_effect_status":
      return reply(mockEffectStatus(currentProject));
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
      recordMock(before, args?.mergeKey);
      revision += 1;
      emit("project:changed", {
        revision,
        mutation_id: args?.mutationId,
        can_undo: undoStack.length > 0,
        can_redo: redoStack.length > 0,
      });
      // The edit-contract result (command-api.md "Edit contract"), with the fields the app reads.
      const write = {
        committed: true,
        revision,
        changed: { created: [], modified: [], deleted: [] },
        summary: cmds.map((c) => String((c as { command?: string } | undefined)?.command ?? "Command")),
        warnings: [],
        patch,
        dry_run: false,
        replayed: false,
      };
      return reply(
        cmd === "apply_command"
          ? { ...write, outcome: "ok" }
          : { ...write, outcomes: cmds.map(() => "ok") },
      );
    }
    case "begin_transaction": {
      if (mockTx) mockTx.depth += 1;
      else mockTx = { depth: 1, base: null };
      return reply(mockHistoryFlags());
    }
    case "commit_transaction": {
      if (mockTx && mockTx.depth > 1) {
        mockTx.depth -= 1;
      } else if (mockTx) {
        const tx = mockTx;
        mockTx = null;
        if (tx.base) {
          redoStack.length = 0;
          undoStack.push({ before: tx.base, key: null, touched: null });
        }
      }
      return reply(mockHistoryFlags());
    }
    case "rollback_transaction": {
      const base = mockTx?.base ?? null;
      mockTx = null;
      let patch: ReturnType<typeof compare> = [];
      if (base) {
        patch = structuredClone(compare(currentProject as unknown as object, base as unknown as object));
        currentProject = base;
        revision += 1;
        emit("project:changed", { revision, mutation_id: args?.mutationId, ...mockHistoryFlags() });
      }
      return reply({ ...mockHistoryFlags(), revision, patch });
    }
    case "end_gesture": {
      const top = undoStack[undoStack.length - 1];
      if (top) top.key = null;
      return reply(mockHistoryFlags());
    }
    case "set_preview_override":
    case "clear_preview_override":
      // Render-only in the app; the browser harness has no compositor to show it.
      return reply({ ok: true });
    case "undo":
    case "redo": {
      // An open transaction is committed first, as the core does.
      if (mockTx) {
        const tx = mockTx;
        mockTx = null;
        if (tx.base) {
          redoStack.length = 0;
          undoStack.push({ before: tx.base, key: null, touched: null });
        }
      }
      const from = cmd === "undo" ? undoStack : redoStack;
      const to = cmd === "undo" ? redoStack : undoStack;
      const target = from.pop();
      let patch: ReturnType<typeof compare> = [];
      if (target) {
        to.push({ before: structuredClone(currentProject), key: null, touched: null });
        patch = structuredClone(compare(currentProject as unknown as object, target.before as unknown as object));
        currentProject = target.before;
      }
      revision += 1;
      emit("project:changed", {
        revision,
        mutation_id: args?.mutationId,
        ...mockHistoryFlags(),
      });
      return reply({ ...mockHistoryFlags(), revision, patch });
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
    // Pack items by ref. Only LUTs are served (the harness has no effect.json / WGSL files):
    // enough to exercise the fetch -> set_pack_item path; anything else is omitted, which the
    // compositor renders pass-through, as the real backend does for an item it cannot run.
    case "read_preview_pack_items":
      return reply(
        ((args?.wanted as { ref: string; hash?: string }[]) ?? [])
          .filter((w) => mockEntry(w.ref)?.kind === "lut")
          .map((w) => {
            const [pack, rest] = mockRefBase(w.ref).slice(5).split("/");
            return {
              ref: w.ref,
              pack_id: pack,
              item_id: rest,
              version: 1,
              kind: "lut",
              hash: w.hash ?? null,
              cube: MOCK_ROTATE_CUBE,
              shaders: {},
              textures: [],
            };
          }),
      );
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
    case "agent_models":
      return reply(MOCK_MODELS[String(args?.agentId)] ?? null);
    case "agent_start": {
      if (args?.agentId === "codex") {
        return Promise.reject("Codex is not installed (mock). Install it, then start again.");
      }
      if (args?.agentId === "ollama") {
        return Promise.reject("Ollama is not running (mock). Start Ollama, then try again.");
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
