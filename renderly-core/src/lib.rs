//! Headless engine for Renderly. No UI dependencies — see docs/architecture.md.
//! `project` and `commands` are the contract described in docs/project-schema.md and
//! docs/command-api.md; keep them in sync with those documents.

// Modules below touch processes, the filesystem, native codecs, or sandboxed-wasm plugin
// execution (wasmtime) and are unavailable in a wasm32 (browser) build — see
// docs/preview-webview.md P2 for the wasm-compositor gating rationale. `project`, `compose`,
// and `mask` are pure evaluation/compositing code and must build on both targets: the wasm
// compositor (renderly-wasm) links against them directly for preview-parity with export.
#[cfg(not(target_arch = "wasm32"))]
pub mod audio;
// Pure CPU rasterization (ab_glyph) — no fs/decode deps beyond font loading, which is
// target-gated internally (fs candidates natively, an embedded bundled font on wasm32). Both
// the native export path and the wasm preview compositor link this for caption-burn-in parity
// (docs/preview-webview.md P3).
pub mod captions;
#[cfg(not(target_arch = "wasm32"))]
pub mod commands;
pub mod compose;
// The agent edit contract drives the native command set and the write surfaces; the wasm
// preview compositor does not write projects, so it is native-only.
#[cfg(not(target_arch = "wasm32"))]
pub mod edit_contract;
#[cfg(not(target_arch = "wasm32"))]
pub mod export;
pub mod frame;
pub mod history;
pub mod mask;
#[cfg(not(target_arch = "wasm32"))]
pub mod media;
// Local ML inference (tract) + model manifest/downloads: native only, never in the wasm build.
#[cfg(not(target_arch = "wasm32"))]
pub mod ml;
// OpenTimelineIO interop (docs/otio.md): pure serde_json, builds on both targets (the media
// probe pass inside is native-only).
pub mod otio;
pub mod packs;
#[cfg(not(target_arch = "wasm32"))]
pub mod perceive;
#[cfg(not(target_arch = "wasm32"))]
pub mod plugins;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod proc;
pub mod project;
#[cfg(not(target_arch = "wasm32"))]
pub mod segmentation;
#[cfg(not(target_arch = "wasm32"))]
pub mod tracking;

#[cfg(not(target_arch = "wasm32"))]
pub use audio::{TtsError, VoiceoverProvider};
#[cfg(not(target_arch = "wasm32"))]
pub use commands::{apply_command, Command, CommandError, CommandOutcome};
pub use compose::{builtin_effect_ids, BUILTIN_EFFECT_IDS};
#[cfg(not(target_arch = "wasm32"))]
pub use export::{
    export_project, export_project_with_progress, export_project_with_settings,
    mix_timeline_audio_range_to_file, mix_timeline_audio_segment, render_frame_at,
    timeline_duration, DecodeOptions, ExportError, ExportPhase, ExportProgress, ExportSettings,
    FrameRenderer, FrameTiming,
};
#[cfg(not(target_arch = "wasm32"))]
pub use media::{generate_thumbnail_strip, ReaderOptions, ThumbnailStrip, VideoEncoderPreference};
#[cfg(target_arch = "wasm32")]
pub use packs::LoadedPack;
#[cfg(not(target_arch = "wasm32"))]
pub use packs::{load_pack, LoadedPack};
#[cfg(not(target_arch = "wasm32"))]
pub use perceive::{
    audio_peaks, detect_scenes, detect_silence, encode_rgba_png, transcribe_media, AnalysisError,
    AudioPeaks, PerceiveError, SceneCut, SilenceSpan, Transcript, TranscriptSegment,
};
#[cfg(not(target_arch = "wasm32"))]
pub use plugins::{compile_gain_wasm, compile_invert_wasm, PluginHost};
pub use project::Project;
