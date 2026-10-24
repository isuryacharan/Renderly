//! Timeline → decode → wgpu composite → encode export pipeline.

use crate::captions::CaptionError;
use crate::commands::ExportPreset;
use crate::compose::eval::{self, ActiveLayer, EvalError, InstanceKey};
use crate::compose::preview_overrides::PreviewOverrides;
use crate::compose::{
    build_scene, ComposeError, ComposeLayer, Compositor, LayerSource, SceneError, SceneHost,
    SceneStep,
};
use crate::media::{
    mix_timeline_audio, mux_video_audio, AudioMixClip, DuckSettings, FfmpegCliError, ReaderOptions,
    RgbaFrame, VideoEncoder, VideoReader,
};
use crate::proc::SerialSpawn;
use crate::project::{
    evaluate_volume_db, Clip, ClipMask, MediaClip, MediaKind, Project, TrackAudioRole, TrackKind,
    MAX_SEQUENCE_DEPTH,
};
use crate::time::us_to_seconds;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExportError {
    #[error("{0}")]
    Ffmpeg(#[from] FfmpegCliError),
    #[error("{0}")]
    Compose(#[from] ComposeError),
    #[error("{0}")]
    Caption(#[from] CaptionError),
    #[error("no enabled video clips on the timeline")]
    EmptyTimeline,
    #[error("media not found: {0}")]
    MediaNotFound(uuid::Uuid),
    #[error("media {0} is not video")]
    NotVideo(uuid::Uuid),
    #[error("sequence not found: {0}")]
    SequenceNotFound(uuid::Uuid),
    #[error("sequence {0} is nested deeper than the limit of {MAX_SEQUENCE_DEPTH}")]
    SequenceTooDeep(uuid::Uuid),
    /// Returned when `export_project_with_progress`'s callback returns `false`.
    #[error("export cancelled")]
    Cancelled,
    /// A baked per-frame background-removal matte could not be read (regenerate it).
    #[error("background-removal matte frame {path}: {message}")]
    Matte { path: String, message: String },
}

impl From<EvalError> for ExportError {
    fn from(e: EvalError) -> Self {
        match e {
            EvalError::MediaNotFound(id) => ExportError::MediaNotFound(id),
            EvalError::NotVideo(id) => ExportError::NotVideo(id),
            EvalError::SequenceNotFound(id) => ExportError::SequenceNotFound(id),
            EvalError::SequenceTooDeep(id) => ExportError::SequenceTooDeep(id),
        }
    }
}

/// Coarse stage of an in-flight export — used by GUI progress UI and CLI status lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPhase {
    Video,
    Audio,
    Mux,
}

/// Progress snapshot passed to `export_project_with_progress`'s callback.
///
/// During `Video`, `frame` is the index about to be rendered (`0..total_frames`).
/// During `Audio` / `Mux`, `frame == total_frames` (video encode is finished).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExportProgress {
    pub phase: ExportPhase,
    pub frame: u64,
    pub total_frames: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportSettings {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    /// A8: which H.264 encoder to try.
    pub video_encoder: crate::media::VideoEncoderPreference,
    /// CRF for libx264 (hardware encoders map this to a CQ/quality value).
    pub crf: u8,
    /// AAC bitrate in kbps for the mux step.
    pub audio_bitrate_k: u32,
}

impl Default for ExportSettings {
    fn default() -> Self {
        Self {
            width: 1080,
            height: 1920,
            fps: 30.0,
            video_encoder: crate::media::VideoEncoderPreference::Auto,
            crf: 18,
            audio_bitrate_k: 192,
        }
    }
}

impl ExportSettings {
    pub fn from_preset(preset: &ExportPreset, project: &Project) -> Self {
        let (width, height, fps) = match preset {
            ExportPreset::TikTok9x16 => (1080, 1920, project.settings.fps.as_f64()),
            ExportPreset::Youtube16x9 => (1920, 1080, project.settings.fps.as_f64()),
            ExportPreset::Custom { width, height, fps } => (*width, *height, fps.as_f64()),
        };
        Self {
            width,
            height,
            fps,
            ..Self::default()
        }
    }

    fn encode_config(&self) -> crate::media::VideoEncodeConfig {
        crate::media::VideoEncodeConfig {
            width: self.width,
            height: self.height,
            fps: self.fps,
            preference: self.video_encoder,
            crf: self.crf,
        }
    }
}

struct DecoderState {
    path: PathBuf,
    reader_opts: ReaderOptions,
    reader: Option<VideoReader>,
    /// A still image (`MediaKind::Image`), decoded once and returned for every source time.
    still: Option<RgbaFrame>,
    last_source_time: i64,
    /// A5: LRU of recently decoded frames keyed by quantized source time. Avoids
    /// respawning FFmpeg on every backward scrub / small seek within the window.
    cache: FrameLru,
}

/// ~60 preview-res frames. At panel height (~540p) that's on the order of ~100 MB;
/// capacity is intentionally modest and shared per decoder (per track).
const FRAME_CACHE_CAPACITY: usize = 60;

#[derive(Default)]
struct FrameLru {
    /// Insertion order (front = oldest).
    order: std::collections::VecDeque<i64>,
    map: HashMap<i64, RgbaFrame>,
}

impl FrameLru {
    fn get(&mut self, key: i64) -> Option<RgbaFrame> {
        let frame = self.map.get(&key)?.clone();
        // Touch: move key to the back (most recently used).
        if let Some(pos) = self.order.iter().position(|&k| k == key) {
            self.order.remove(pos);
            self.order.push_back(key);
        }
        Some(frame)
    }

    fn insert(&mut self, key: i64, frame: RgbaFrame) {
        if let std::collections::hash_map::Entry::Occupied(mut e) = self.map.entry(key) {
            e.insert(frame);
            if let Some(pos) = self.order.iter().position(|&k| k == key) {
                self.order.remove(pos);
                self.order.push_back(key);
            }
            return;
        }
        while self.order.len() >= FRAME_CACHE_CAPACITY {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
        self.order.push_back(key);
        self.map.insert(key, frame);
    }
}

fn quantize_source_time_ms(source_time_us: i64) -> i64 {
    (source_time_us + 500).div_euclid(1000)
}

impl DecoderState {
    fn new(path: PathBuf, reader_opts: ReaderOptions) -> Self {
        Self {
            path,
            reader_opts,
            reader: None,
            still: None,
            last_source_time: i64::MIN,
            cache: FrameLru::default(),
        }
    }

    fn frame_at(&mut self, source_time: i64) -> Result<Option<RgbaFrame>, FfmpegCliError> {
        if crate::media::is_still_image(&self.path) {
            if self.still.is_none() {
                let img = image::open(&self.path)
                    .map_err(|e| {
                        FfmpegCliError::BadOutput(format!(
                            "decoding image {}: {e}",
                            self.path.display()
                        ))
                    })?
                    .to_rgba8();
                self.still = Some(RgbaFrame {
                    width: img.width(),
                    height: img.height(),
                    pixels: img.into_raw(),
                });
            }
            return Ok(self.still.clone());
        }

        let key = quantize_source_time_ms(source_time);
        if let Some(cached) = self.cache.get(key) {
            self.last_source_time = source_time;
            return Ok(Some(cached));
        }

        let needs_reopen = self.reader.is_none()
            || source_time < self.last_source_time
            || (source_time - self.last_source_time).abs() > 500_000;

        if needs_reopen {
            self.reader = Some(VideoReader::open_with(
                &self.path,
                us_to_seconds(source_time),
                &self.reader_opts,
            )?);
        }

        let reader = self.reader.as_mut().expect("reader just opened");
        let frame = reader.read_frame()?;
        self.last_source_time = source_time;
        if let Some(ref f) = frame {
            self.cache.insert(key, f.clone());
        }
        Ok(frame)
    }
}

/// Decode-time knobs for a `FrameRenderer`. `target_height` decodes video layers
/// downscaled (e.g. to preview-panel resolution) instead of full source resolution;
/// `output_fps` paces decode to a fixed frame rate instead of the source's native fps.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct DecodeOptions {
    pub target_height: Option<u32>,
    pub output_fps: Option<f64>,
}

/// Persistent composite-frame renderer: holds one wgpu `Compositor` and one decoder per
/// source media across repeated `render` calls, instead of recreating the GPU device and
/// respawning ffmpeg per video layer on every call (what `render_frame_at` — now a
/// one-shot wrapper around this — used to do, and what made the old per-frame preview
/// path freeze the UI thread). Callers that render a sequence of frames from the same
/// project (playback, export) should keep one `FrameRenderer` alive for the whole run.
pub struct FrameRenderer {
    settings: ExportSettings,
    decode_opts: DecodeOptions,
    compositor: Compositor,
    /// One decoder per layer instance (see [`InstanceKey`]): the same media at two different
    /// times in one frame — a sequence placed twice, or a clip both nested and on the root —
    /// gets two independent readers.
    decoders: HashMap<InstanceKey, DecoderState>,
    /// Per-frame background-removal mattes currently registered with the compositor, as
    /// `(frame index, sequence stamp)` per matte key, so an unchanged frame is not re-read and
    /// re-uploaded.
    frame_mattes: HashMap<String, (u32, u64)>,
    /// Render-only overrides (see [`PreviewOverrides`]). Empty for export; the live app sets
    /// them so `render_frame` and the MCP `look` show what the preview shows. Never saved.
    overrides: PreviewOverrides,
    /// The packs item refs resolve against. Loaded from the user library on the first frame
    /// unless the host supplies them with [`FrameRenderer::set_packs`].
    packs: Option<std::sync::Arc<Vec<crate::packs::LoadedPack>>>,
}

impl FrameRenderer {
    /// Resolve item refs against exactly these packs (hosts and tests that manage their own
    /// set). Without a call, the installed library is loaded once, on the first frame.
    pub fn set_packs(&mut self, packs: Vec<crate::packs::LoadedPack>) {
        self.packs = Some(std::sync::Arc::new(packs));
    }

    /// Forget the loaded packs so the next frame reloads the library (after an install or an
    /// uninstall).
    pub fn reload_packs(&mut self) {
        self.packs = None;
    }

    /// Render with these preview overrides applied (replaces any previous set). Export never
    /// calls this, so exported files always match the saved project.
    pub fn set_preview_overrides(&mut self, overrides: PreviewOverrides) {
        self.overrides = overrides;
    }

    pub fn new(settings: ExportSettings, decode_opts: DecodeOptions) -> Result<Self, ExportError> {
        let compositor = Compositor::new(settings.width, settings.height)?;
        Ok(Self {
            settings,
            decode_opts,
            compositor,
            decoders: HashMap::new(),
            frame_mattes: HashMap::new(),
            overrides: PreviewOverrides::new(),
            packs: None,
        })
    }

    /// A4: share the preview surface's wgpu device so compose stays on the GPU and the
    /// app can blit [`Self::output_view`] without a CPU readback.
    pub fn with_device(
        device: std::sync::Arc<wgpu::Device>,
        queue: std::sync::Arc<wgpu::Queue>,
        settings: ExportSettings,
        decode_opts: DecodeOptions,
    ) -> Result<Self, ExportError> {
        let compositor = Compositor::with_device(device, queue, settings.width, settings.height)?;
        Ok(Self {
            settings,
            decode_opts,
            compositor,
            decoders: HashMap::new(),
            frame_mattes: HashMap::new(),
            overrides: PreviewOverrides::new(),
            packs: None,
        })
    }

    pub fn output_view(&self) -> &wgpu::TextureView {
        self.compositor.output_view()
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.compositor.width(), self.compositor.height())
    }

    /// A5: warm the per-track decode LRU around `time_us` so scrubbing nearby
    /// (especially backward) hits cache instead of respawning FFmpeg. Decodes only —
    /// does not composite or present. Safe to call from the scrub worker after a present;
    /// respects the same decoder reopen rules as [`Self::render`].
    pub fn prefetch_around(
        &mut self,
        project: &Project,
        time_us: i64,
        window_us: i64,
    ) -> Result<(), ExportError> {
        let fps = project.settings.fps;
        let window = window_us.max(0);
        let start = (time_us - window).max(0);
        let end = time_us + window;
        let reader_opts = ReaderOptions {
            target_height: self.decode_opts.target_height,
            output_fps: self.decode_opts.output_fps,
        };

        let mut t = start;
        while t <= end {
            // Includes layers inside nested sequences.
            let layers = eval::leaf_media_layers(project, t)?;
            for layer in &layers {
                let Some((_, path, source_time)) = layer.media() else {
                    continue;
                };
                let decoder = self
                    .decoders
                    .entry(layer.instance.clone())
                    .or_insert_with(|| DecoderState::new(path.to_path_buf(), reader_opts));
                if decoder.path != path {
                    *decoder = DecoderState::new(path.to_path_buf(), reader_opts);
                }
                let _ = decoder.frame_at(source_time)?;
            }
            // Next frame boundary at the project rate.
            t = fps.frame_start(fps.frame_at(t) + 1);
        }
        Ok(())
    }

    pub fn render(&mut self, project: &Project, time_us: i64) -> Result<Vec<u8>, ExportError> {
        self.render_inner(project, time_us, true)
            .map(|(pixels, _)| pixels.expect("readback requested"))
    }

    /// Same as [`Self::render`], plus a coarse decode-vs-compose time breakdown for the
    /// perf HUD (improvement-plan A0 — "measure from day one" before A1-A8 touch this hot
    /// path). Export and MCP perception stay on the untimed `render` — the few
    /// `Instant::now()` calls this adds are negligible next to a decode+composite, but
    /// there's no reason to pay even that for callers nobody's watching a HUD for.
    pub fn render_timed(
        &mut self,
        project: &Project,
        time_us: i64,
    ) -> Result<(Vec<u8>, FrameTiming), ExportError> {
        self.render_inner(project, time_us, true)
            .map(|(pixels, timing)| (pixels.expect("readback requested"), timing))
    }

    /// A4: decode + GPU compose with no CPU readback. Result lives in
    /// [`Self::output_view`] for the preview blit path.
    pub fn render_to_texture(
        &mut self,
        project: &Project,
        time_us: i64,
    ) -> Result<FrameTiming, ExportError> {
        self.render_inner(project, time_us, false)
            .map(|(_, timing)| timing)
    }

    fn render_inner(
        &mut self,
        project: &Project,
        time_us: i64,
        readback: bool,
    ) -> Result<(Option<Vec<u8>>, FrameTiming), ExportError> {
        // Same projected view the wasm preview builds (compose::preview_overrides): overrides
        // are applied before evaluation, so both paths render an override identically.
        let view = self.overrides.apply(project);
        let project: &Project = &view;
        let steps = eval::active_steps(project, time_us)?;
        let packs = self
            .packs
            .get_or_insert_with(|| std::sync::Arc::new(crate::packs::load_installed_packs()))
            .clone();
        let plugin_host = crate::plugins::PluginHost::for_project(project).ok();

        let reader_opts = ReaderOptions {
            target_height: self.decode_opts.target_height,
            output_fps: self.decode_opts.output_fps,
        };
        let mut host = ExportHost {
            decoders: &mut self.decoders,
            plugin_host: plugin_host.as_ref(),
            reader_opts,
            decode_s: 0.0,
            frame_mattes: Vec::new(),
            project,
        };
        let mut scene = build_scene(project, steps, &mut host).map_err(|e| match e {
            SceneError::Eval(e) => ExportError::from(e),
            SceneError::Host(e) => e,
        })?;
        let decode_s = host.decode_s;
        let wanted_mattes = std::mem::take(&mut host.frame_mattes);
        self.register_frame_mattes(&wanted_mattes)?;

        for cap in eval::active_captions(project, time_us) {
            scene.push(SceneStep::Layer(ComposeLayer::caption(LayerSource::Frame(
                crate::captions::render_caption_for_project(
                    project,
                    &cap.text,
                    &cap.style_id,
                    &cap.overrides,
                    self.settings.width,
                    self.settings.height,
                )?,
            ))));
        }

        let compose_start = Instant::now();
        let pixels = if readback {
            Some(self.compositor.composite_steps(&packs, &scene)?)
        } else {
            self.compositor.compose_steps_to_texture(&packs, &scene)?;
            None
        };
        let compose_s = compose_start.elapsed().as_secs_f64();

        Ok((
            pixels,
            FrameTiming {
                decode_ms: decode_s * 1000.0,
                compose_ms: compose_s * 1000.0,
            },
        ))
    }
}

impl FrameRenderer {
    /// Load and register the per-frame mattes this frame's layers need (nearest matte frame to
    /// each layer's source time) under the same compositor keys the single-image path uses.
    fn register_frame_mattes(&mut self, wanted: &[FrameMatte]) -> Result<(), ExportError> {
        for m in wanted {
            if self.frame_mattes.get(&m.key) == Some(&(m.index, m.stamp)) {
                continue;
            }
            let path = m.dir.join(crate::project::MatteFrames::file_name(m.index));
            let matte_err = |message: String| ExportError::Matte {
                path: path.display().to_string(),
                message,
            };
            let gray = image::open(&path)
                .map_err(|e| matte_err(e.to_string()))?
                .to_luma8();
            self.compositor
                .set_matte(&m.key, gray.width(), gray.height(), gray.as_raw())?;
            self.frame_mattes.insert(m.key.clone(), (m.index, m.stamp));
        }
        Ok(())
    }
}

/// A per-frame matte a layer needs this frame (see [`FrameRenderer::register_frame_mattes`]).
struct FrameMatte {
    key: String,
    dir: PathBuf,
    index: u32,
    stamp: u64,
}

/// Native [`SceneHost`]: media layers are decoded through the renderer's per-instance ffmpeg
/// readers.
struct ExportHost<'a> {
    decoders: &'a mut HashMap<InstanceKey, DecoderState>,
    plugin_host: Option<&'a crate::plugins::PluginHost>,
    reader_opts: ReaderOptions,
    decode_s: f64,
    /// Per-frame background-removal mattes the layers built so far need.
    frame_mattes: Vec<FrameMatte>,
    /// The project being rendered (pack caption styles resolve against the library).
    project: &'a Project,
}

impl SceneHost for ExportHost<'_> {
    type Error = ExportError;

    fn media_layer(&mut self, layer: &ActiveLayer) -> Result<Option<ComposeLayer>, ExportError> {
        let Some((_, path, source_time)) = layer.media() else {
            return Ok(None);
        };
        let reader_opts = self.reader_opts;
        let decoder = self
            .decoders
            .entry(layer.instance.clone())
            .or_insert_with(|| DecoderState::new(path.to_path_buf(), reader_opts));
        if decoder.path != path {
            *decoder = DecoderState::new(path.to_path_buf(), reader_opts);
        }
        let decode_start = Instant::now();
        let frame = decoder.frame_at(source_time)?;
        self.decode_s += decode_start.elapsed().as_secs_f64();
        let Some(mut frame) = frame else {
            return Ok(None);
        };
        if let Some(host) = self.plugin_host {
            let _ = host.apply_effects(&mut frame, &layer.effects);
        }

        let mask = if let Some(bg) = layer.background_removal.as_ref().filter(|b| b.enabled) {
            match (&bg.matte_frames, crate::mask::cached_background_mask(bg)) {
                // Baked per-frame matte: the nearest matte frame to this layer's *source* time,
                // composed through the same `Generated` mask (feather, invert) as a still matte.
                (Some(frames), Some(mask)) => {
                    if let (Some(key), Some(dir)) = (
                        crate::mask::matte_cache_key(&mask.kind),
                        bg.matte_cache_dir.as_ref(),
                    ) {
                        self.frame_mattes.push(FrameMatte {
                            key,
                            dir: dir.clone(),
                            index: frames.index_at(source_time),
                            stamp: frames.stamp,
                        });
                    }
                    Some(mask)
                }
                _ => resolve_background_removal_mask(&frame, bg),
            }
        } else {
            layer.mask.clone()
        };

        Ok(Some(ComposeLayer::from_active(
            layer,
            LayerSource::Frame(frame),
            mask,
        )))
    }

    fn text_layer(
        &mut self,
        layer: &ActiveLayer,
        size: (u32, u32),
    ) -> Result<Option<ComposeLayer>, ExportError> {
        let Some(text) = layer.text() else {
            return Ok(None);
        };
        let frame = crate::captions::render_caption_for_project(
            self.project,
            &text.text,
            &text.style_id,
            &text.overrides,
            size.0,
            size.1,
        )?;
        Ok(Some(ComposeLayer::from_active(
            layer,
            LayerSource::Frame(frame),
            layer.mask.clone(),
        )))
    }

    fn sequence_mask(&mut self, layer: &ActiveLayer) -> Option<ClipMask> {
        // Background removal without a baked matte is per inner clip; a baked matte or the
        // clip's own mask applies to the sequence's picture.
        layer
            .background_removal
            .as_ref()
            .filter(|b| b.enabled)
            .and_then(crate::mask::cached_background_mask)
            .or_else(|| layer.mask.clone())
    }
}

/// Coarse per-frame timing breakdown from [`FrameRenderer::render_timed`] — decode is the
/// sum across every active layer's `frame_at` call, compose is the single
/// `Compositor::composite` call. Doesn't cover present (that's on the GPU-present side,
/// e.g. `gfx.rs`'s `present_rgba`, timed by the caller since it's outside `renderly-core`).
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
pub struct FrameTiming {
    pub decode_ms: f64,
    pub compose_ms: f64,
}

/// Render one composited RGBA frame at `time_us` (video + burned-in captions).
///
/// One-shot convenience wrapper: builds a `FrameRenderer` (fresh GPU device + decoders)
/// for a single frame. Fine for perception/MCP call sites that render one isolated frame;
/// callers rendering a sequence (playback, export) should use `FrameRenderer` directly to
/// avoid rebuilding the compositor and decoders per frame.
pub fn render_frame_at(
    project: &Project,
    time_us: i64,
    settings: ExportSettings,
) -> Result<Vec<u8>, ExportError> {
    let mut renderer = FrameRenderer::new(settings, DecodeOptions::default())?;
    renderer.render(project, time_us)
}

/// Render the project's timeline to an MP4 file (video + captions + mixed audio).
pub fn export_project(
    project: &Project,
    output_path: &Path,
    preset: ExportPreset,
) -> Result<(), ExportError> {
    export_project_with_progress(project, output_path, preset, &mut |_| true)
}

/// Like [`export_project`], but reports progress and supports cooperative cancel.
///
/// `on_progress` is called before each video frame and again when entering the audio /
/// mux phases. Return `false` to cancel: the temp working directory is removed and
/// [`ExportError::Cancelled`] is returned. Callers that only need a fire-and-forget
/// export should use [`export_project`].
pub fn export_project_with_progress(
    project: &Project,
    output_path: &Path,
    preset: ExportPreset,
    on_progress: &mut dyn FnMut(ExportProgress) -> bool,
) -> Result<(), ExportError> {
    let settings = ExportSettings::from_preset(&preset, project);
    export_project_with_settings(project, output_path, settings, on_progress)
}

/// Export using an explicit [`ExportSettings`] (A8/C5 — codec, CRF, audio bitrate, size).
pub fn export_project_with_settings(
    project: &Project,
    output_path: &Path,
    settings: ExportSettings,
    on_progress: &mut dyn FnMut(ExportProgress) -> bool,
) -> Result<(), ExportError> {
    if !crate::media::ffmpeg_available() {
        return Err(FfmpegCliError::NotFound.into());
    }

    let duration_us = timeline_duration(project);
    if duration_us <= 0 {
        return Err(ExportError::EmptyTimeline);
    }

    if !has_video_content(project) {
        return Err(ExportError::EmptyTimeline);
    }

    // Frames are paced at the export rate, which may differ from the project rate.
    let total_frames =
        (crate::time::Fps::from_f64(settings.fps).frame_at(duration_us - 1) + 1).max(0) as u64;
    if total_frames == 0 {
        return Err(ExportError::EmptyTimeline);
    }

    let temp_dir = std::env::temp_dir().join(format!("renderly-export-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(FfmpegCliError::Io)?;
    let temp_video = temp_dir.join("video.mp4");

    let result = export_project_with_progress_inner(
        project,
        output_path,
        settings,
        duration_us,
        total_frames,
        &temp_dir,
        &temp_video,
        on_progress,
    );

    // Always drop the working dir — on success the MP4 has already been moved/copied out;
    // on cancel/error this is what kills partial encodes and avoids orphaned temps.
    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

#[allow(clippy::too_many_arguments)]
fn export_project_with_progress_inner(
    project: &Project,
    output_path: &Path,
    settings: ExportSettings,
    duration_us: i64,
    total_frames: u64,
    temp_dir: &Path,
    temp_video: &Path,
    on_progress: &mut dyn FnMut(ExportProgress) -> bool,
) -> Result<(), ExportError> {
    let export_fps = crate::time::Fps::from_f64(settings.fps);
    let mut encoder = VideoEncoder::open(temp_video, &settings.encode_config())?;
    // `output_fps: Some(settings.fps)` paces each source decoder to the export frame rate
    // (ffmpeg duplicates/drops frames to match) rather than the source's native fps —
    // this also fixes frame-pacing drift for sources whose fps doesn't match the export.
    let mut renderer = FrameRenderer::new(
        settings,
        DecodeOptions {
            target_height: None,
            output_fps: Some(settings.fps),
        },
    )?;

    for frame_idx in 0..total_frames {
        if !on_progress(ExportProgress {
            phase: ExportPhase::Video,
            frame: frame_idx,
            total_frames,
        }) {
            return Err(ExportError::Cancelled);
        }
        let t = export_fps.frame_start(frame_idx as i64);
        let pixels = renderer.render(project, t)?;
        encoder.write_frame(&pixels)?;
    }
    encoder.finish()?;
    drop(renderer);

    // Holds intermediate sequence buses (see `collect_audio_clips`); removed on drop.
    let mut audio_temp = AudioTemp::default();
    let mut audio_clips = collect_audio_clips(project, &mut audio_temp)?;
    let wasm_tmp = bake_wasm_audio_effects(project, &mut audio_clips)?;
    if audio_clips.is_empty() {
        let _ = wasm_tmp;
        std::fs::rename(temp_video, output_path).or_else(|_| {
            std::fs::copy(temp_video, output_path).map_err(FfmpegCliError::Io)?;
            Ok::<(), FfmpegCliError>(())
        })?;
        return Ok(());
    }

    if !on_progress(ExportProgress {
        phase: ExportPhase::Audio,
        frame: total_frames,
        total_frames,
    }) {
        if let Some(d) = wasm_tmp {
            std::fs::remove_dir_all(d).ok();
        }
        return Err(ExportError::Cancelled);
    }

    let temp_audio = temp_dir.join("audio.wav");
    let duck = duck_settings(project);
    let mix_result = mix_timeline_audio(
        &audio_clips,
        project.settings.sample_rate,
        duration_us,
        &temp_audio,
        duck,
    );
    if let Some(d) = wasm_tmp {
        std::fs::remove_dir_all(d).ok();
    }
    mix_result?;

    if !on_progress(ExportProgress {
        phase: ExportPhase::Mux,
        frame: total_frames,
        total_frames,
    }) {
        return Err(ExportError::Cancelled);
    }
    mux_video_audio(
        temp_video,
        &temp_audio,
        output_path,
        settings.audio_bitrate_k,
    )?;
    Ok(())
}

/// Temp directories holding intermediate sequence audio buses; removed on drop.
#[derive(Default)]
struct AudioTemp(Vec<PathBuf>);

impl Drop for AudioTemp {
    fn drop(&mut self) {
        for dir in &self.0 {
            std::fs::remove_dir_all(dir).ok();
        }
    }
}

/// Shortest audio piece kept by the mixer (100 µs): shorter pieces are rounding noise.
const MIN_PIECE_US: i64 = 100;

/// `d` µs divided by a playback rate, rounded to µs.
fn scale_us(d: i64, rate: f64) -> i64 {
    (d as f64 / rate).round() as i64
}

/// `d` µs multiplied by a playback rate, rounded to µs.
fn mul_us(d: i64, rate: f64) -> i64 {
    (d as f64 * rate).round() as i64
}

/// Mix pieces for one audio clip on its own timeline: one per constant-speed segment.
fn leaf_audio_pieces(
    a: &MediaClip,
    path: &Path,
    role: Option<TrackAudioRole>,
) -> Vec<AudioMixClip> {
    let mut clips = Vec::new();
    for (local_off, tl_dur, speed) in crate::project::anim::speed_segments(a) {
        let src_in = a.source_time_at(a.start_us + local_off);
        let src_out = a.source_time_at(a.start_us + local_off + tl_dur);
        clips.push(AudioMixClip {
            path: path.to_path_buf(),
            start_us: a.start_us + local_off,
            source_in_us: src_in,
            source_out_us: src_out.max(src_in + MIN_PIECE_US),
            gain_db: evaluate_volume_db(a, a.start_us + local_off),
            fade_in_us: if local_off < 1 { a.fade_in_us } else { 0 },
            fade_out_us: if (local_off + tl_dur - a.timeline_duration_us()).abs() < 60_000 {
                a.fade_out_us
            } else {
                0
            },
            role,
            speed,
            effects: a.effects.clone(),
            denoise: a.audio_denoise.clone(),
        });
    }
    clips
}

/// Every enabled audio clip of a container's tracks as mix pieces on that container's own
/// timeline. A clip whose media is a nested sequence expands recursively into the
/// sequence's leaf pieces (see [`expand_sequence_audio`]).
fn container_audio_pieces(
    project: &Project,
    tracks: &[crate::project::Track],
    depth: usize,
    temp: &mut AudioTemp,
) -> Result<Vec<AudioMixClip>, ExportError> {
    let mut clips = Vec::new();
    for track in tracks {
        if track.kind != TrackKind::Audio || track.muted {
            continue;
        }
        for clip in &track.clips {
            let Clip::Audio(a) = clip else { continue };
            if !a.enabled {
                continue;
            }
            let Some(media) = project.find_media(a.media_id) else {
                continue;
            };
            if let (MediaKind::Sequence, Some(seq_id)) = (media.kind, media.sequence_id) {
                clips.extend(expand_sequence_audio(
                    project,
                    a,
                    track.audio_role,
                    seq_id,
                    depth,
                    temp,
                )?);
            } else {
                clips.extend(leaf_audio_pieces(a, &media.path, track.audio_role));
            }
        }
    }
    Ok(clips)
}

fn collect_audio_clips(
    project: &Project,
    temp: &mut AudioTemp,
) -> Result<Vec<AudioMixClip>, ExportError> {
    container_audio_pieces(project, &project.tracks, 1, temp)
}

/// Expand an audio clip that places a nested sequence into the sequence's own leaf pieces,
/// mapped onto the outer timeline through the outer clip's position, trim, speed (including
/// speed keyframes), gain and fades, and clipped to the outer clip's span. The pieces inherit
/// `outer_role` on the parent mix.
///
/// Ducking is resolved per container: when the sequence itself holds both voice and music
/// tracks, its audio is first mixed (with ducking) into one intermediate bus, which the
/// parent then treats as a single piece. Otherwise no intermediate render happens and the
/// parent mix is a single filter graph over leaf media.
fn expand_sequence_audio(
    project: &Project,
    outer: &MediaClip,
    outer_role: Option<TrackAudioRole>,
    sequence_id: crate::project::Id,
    depth: usize,
    temp: &mut AudioTemp,
) -> Result<Vec<AudioMixClip>, ExportError> {
    if depth > MAX_SEQUENCE_DEPTH {
        return Err(ExportError::SequenceTooDeep(sequence_id));
    }
    let seq = project
        .find_sequence(sequence_id)
        .ok_or(ExportError::SequenceNotFound(sequence_id))?;
    let inner = container_audio_pieces(project, &seq.tracks, depth + 1, temp)?;
    let inner = premix_sequence_bus(project, inner, project.sequence_duration(sequence_id), temp)?;
    Ok(map_through_outer(&inner, outer, outer_role))
}

/// If `pieces` (a sequence's audio, on its own timeline) holds both voice and music roles and
/// ducking is enabled, mix them into one ducked intermediate WAV and return it as a single
/// piece; otherwise return `pieces` unchanged.
fn premix_sequence_bus(
    project: &Project,
    mut pieces: Vec<AudioMixClip>,
    duration_us: i64,
    temp: &mut AudioTemp,
) -> Result<Vec<AudioMixClip>, ExportError> {
    if project.settings.duck_db >= 0.0 || duration_us <= 0 {
        return Ok(pieces);
    }
    let has_voice = pieces.iter().any(|c| {
        matches!(
            c.role,
            Some(TrackAudioRole::Voiceover) | Some(TrackAudioRole::Dialog)
        )
    });
    let has_music = pieces.iter().any(|c| c.role == Some(TrackAudioRole::Music));
    if !(has_voice && has_music) {
        return Ok(pieces);
    }
    if let Some(dir) = bake_wasm_audio_effects(project, &mut pieces)? {
        temp.0.push(dir);
    }
    let dir = std::env::temp_dir().join(format!("renderly-seqmix-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(FfmpegCliError::Io)?;
    temp.0.push(dir.clone());
    let wav = dir.join("bus.wav");
    mix_timeline_audio(
        &pieces,
        project.settings.sample_rate,
        duration_us,
        &wav,
        Some(DuckSettings {
            duck_db: project.settings.duck_db,
        }),
    )?;
    Ok(vec![AudioMixClip {
        path: wav,
        start_us: 0,
        source_in_us: 0,
        source_out_us: duration_us,
        gain_db: 0.0,
        fade_in_us: 0,
        fade_out_us: 0,
        role: None,
        speed: 1.0,
        effects: Vec::new(),
        denoise: None,
    }])
}

/// Map pieces expressed on a sequence's timeline onto the timeline of the audio clip that
/// places it. Per constant-speed segment of `outer`: the window of sequence time it plays
/// is intersected with each piece; position scales by the outer speed, the playback rate
/// multiplies, and the outer volume adds in dB. The outer fade-in/out apply to the pieces
/// that touch the outer clip's start/end (an outer fade longer than one piece is not spread
/// across the following pieces).
fn map_through_outer(
    pieces: &[AudioMixClip],
    outer: &MediaClip,
    outer_role: Option<TrackAudioRole>,
) -> Vec<AudioMixClip> {
    let mut out = Vec::new();
    let outer_dur = outer.timeline_duration_us();
    for (local_off, tl_dur, speed_o) in crate::project::anim::speed_segments(outer) {
        let seg_start = outer.start_us + local_off;
        let w0 = outer.source_time_at(seg_start);
        let w1 = outer.source_time_at(seg_start + tl_dur);
        let outer_gain = evaluate_volume_db(outer, seg_start);
        let first = local_off < 1;
        let last = (local_off + tl_dur - outer_dur).abs() < 60_000;
        for p in pieces {
            let speed_p = p.speed.max(1e-6);
            let p0 = p.start_us;
            let p1 = p0 + scale_us(p.source_out_us - p.source_in_us, speed_p);
            let lo = p0.max(w0);
            let hi = p1.min(w1);
            if hi - lo <= MIN_PIECE_US {
                continue;
            }
            let src_in = p.source_in_us + mul_us(lo - p0, speed_p);
            let src_out = p.source_in_us + mul_us(hi - p0, speed_p);
            let at_piece_start = lo <= p0;
            let at_piece_end = hi >= p1;
            let fade_in = if at_piece_start { p.fade_in_us } else { 0 }.max(if first && lo <= w0 {
                outer.fade_in_us
            } else {
                0
            });
            let fade_out = if at_piece_end { p.fade_out_us } else { 0 }.max(if last && hi >= w1 {
                outer.fade_out_us
            } else {
                0
            });
            out.push(AudioMixClip {
                path: p.path.clone(),
                start_us: seg_start + scale_us(lo - w0, speed_o),
                source_in_us: src_in,
                source_out_us: src_out.max(src_in + MIN_PIECE_US),
                gain_db: p.gain_db + outer_gain,
                fade_in_us: fade_in,
                fade_out_us: fade_out,
                role: outer_role,
                speed: p.speed * speed_o,
                effects: p.effects.clone(),
                denoise: p.denoise.clone(),
            });
        }
    }
    out
}

/// Cut a mix piece to `[start, end)` on its timeline and shift so `start` becomes t=0.
fn clip_piece_to_range(p: &AudioMixClip, start: i64, end: i64) -> Option<AudioMixClip> {
    let speed = p.speed.max(1e-6);
    let p0 = p.start_us;
    let p1 = p0 + scale_us(p.source_out_us - p.source_in_us, speed);
    let lo = p0.max(start);
    let hi = p1.min(end);
    if hi - lo < 1 {
        return None;
    }
    let mut out = p.clone();
    out.start_us = lo - start;
    out.source_in_us = p.source_in_us + mul_us(lo - p0, speed);
    out.source_out_us = p.source_in_us + mul_us(hi - p0, speed);
    if lo > p0 {
        out.fade_in_us = 0;
    }
    if hi < p1 {
        out.fade_out_us = 0;
    }
    Some(out)
}

/// Decode clip source ranges, run audio WASM effects, rewrite clip paths to processed WAVs.
fn bake_wasm_audio_effects(
    project: &Project,
    clips: &mut [AudioMixClip],
) -> Result<Option<PathBuf>, ExportError> {
    let needs = clips
        .iter()
        .any(|c| c.effects.iter().any(|e| e.enabled && e.is_wasm()));
    if !needs {
        return Ok(None);
    }
    let host = crate::plugins::PluginHost::for_project(project)
        .map_err(|e| ExportError::Ffmpeg(FfmpegCliError::BadOutput(e)))?;
    let temp_dir =
        std::env::temp_dir().join(format!("renderly-wasm-audio-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(FfmpegCliError::Io)?;
    let sample_rate = project.settings.sample_rate;
    let ffmpeg = crate::media::ffmpeg_path()?;

    for (i, clip) in clips.iter_mut().enumerate() {
        let wasm_fx: Vec<_> = clip
            .effects
            .iter()
            .filter(|e| e.enabled && e.is_wasm())
            .cloned()
            .collect();
        if wasm_fx.is_empty() {
            continue;
        }
        let has_audio_plugin = wasm_fx.iter().any(|e| {
            e.item_ref
                .strip_prefix("wasm:")
                .map(|id| host.plugin_is_audio(id))
                .unwrap_or(false)
        });
        if !has_audio_plugin {
            continue;
        }

        let seg_dur = (clip.source_out_us - clip.source_in_us).max(MIN_PIECE_US);
        let raw_path = temp_dir.join(format!("raw_{i}.f32"));
        let wav_path = temp_dir.join(format!("proc_{i}.wav"));

        let status = crate::proc::command(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-ss",
                &format!("{:.6}", us_to_seconds(clip.source_in_us)),
                "-i",
            ])
            .arg(&clip.path)
            .args([
                "-t",
                &format!("{:.6}", us_to_seconds(seg_dur)),
                "-f",
                "f32le",
                "-ac",
                "2",
                "-ar",
                &sample_rate.to_string(),
            ])
            .arg(&raw_path)
            .status_serial()
            .map_err(|e| {
                ExportError::Ffmpeg(FfmpegCliError::SpawnFailed {
                    tool: "ffmpeg",
                    message: e.to_string(),
                })
            })?;
        if !status.success() {
            std::fs::remove_dir_all(&temp_dir).ok();
            return Err(ExportError::Ffmpeg(FfmpegCliError::NonZeroExit(
                status.code().unwrap_or(-1),
            )));
        }

        let bytes = std::fs::read(&raw_path).map_err(FfmpegCliError::Io)?;
        if bytes.len() % 4 != 0 {
            std::fs::remove_dir_all(&temp_dir).ok();
            return Err(ExportError::Ffmpeg(FfmpegCliError::BadOutput(
                "odd f32le PCM length".into(),
            )));
        }
        let mut samples: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        host.apply_audio_effects(&mut samples, sample_rate, 2, &wasm_fx)
            .map_err(|e| ExportError::Ffmpeg(FfmpegCliError::BadOutput(e)))?;

        let mut out_bytes = Vec::with_capacity(samples.len() * 4);
        for s in &samples {
            out_bytes.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&raw_path, &out_bytes).map_err(FfmpegCliError::Io)?;

        let status = crate::proc::command(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "f32le",
                "-ar",
                &sample_rate.to_string(),
                "-ac",
                "2",
                "-i",
            ])
            .arg(&raw_path)
            .arg(&wav_path)
            .status_serial()
            .map_err(|e| {
                ExportError::Ffmpeg(FfmpegCliError::SpawnFailed {
                    tool: "ffmpeg",
                    message: e.to_string(),
                })
            })?;
        if !status.success() {
            std::fs::remove_dir_all(&temp_dir).ok();
            return Err(ExportError::Ffmpeg(FfmpegCliError::NonZeroExit(
                status.code().unwrap_or(-1),
            )));
        }

        clip.path = wav_path;
        clip.source_in_us = 0;
        clip.source_out_us =
            (samples.len() as f64 / 2.0 / sample_rate as f64 * 1_000_000.0).round() as i64;
        clip.effects.clear();
    }

    Ok(Some(temp_dir))
}

fn duck_settings(project: &Project) -> Option<DuckSettings> {
    if project.settings.duck_db >= 0.0 {
        return None;
    }
    let has_voice = project.tracks.iter().any(|t| {
        t.kind == TrackKind::Audio
            && matches!(
                t.audio_role,
                Some(TrackAudioRole::Voiceover) | Some(TrackAudioRole::Dialog)
            )
            && t.clips
                .iter()
                .any(|c| matches!(c, Clip::Audio(a) if a.enabled))
    });
    let has_music = project.tracks.iter().any(|t| {
        t.kind == TrackKind::Audio
            && t.audio_role == Some(TrackAudioRole::Music)
            && t.clips
                .iter()
                .any(|c| matches!(c, Clip::Audio(a) if a.enabled))
    });
    if has_voice && has_music {
        Some(DuckSettings {
            duck_db: project.settings.duck_db,
        })
    } else {
        None
    }
}

fn has_video_content(project: &Project) -> bool {
    project.tracks.iter().any(|track| {
        track.kind == TrackKind::Video
            && !track.hidden
            && track.clips.iter().any(|clip| match clip {
                Clip::Video(c) => c.enabled,
                _ => false,
            })
    })
}

pub fn timeline_duration(project: &Project) -> i64 {
    project
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.end_us())
        .fold(0_i64, i64::max)
}

/// Mix timeline audio in `[start_us, start_us + duration_us)` to a WAV file,
/// shifted so the file's own t=0 is `start_us`. Returns `false` (and writes nothing) if
/// no enabled audio clip overlaps the range. File-backed rather than in-memory so the
/// playback engine can pre-mix ranges spanning minutes, not just short scrub segments.
pub fn mix_timeline_audio_range_to_file(
    project: &Project,
    start_us: i64,
    duration_us: i64,
    out_path: &Path,
) -> Result<bool, ExportError> {
    if duration_us <= 0 {
        return Ok(false);
    }
    if !crate::media::ffmpeg_available() {
        return Err(FfmpegCliError::NotFound.into());
    }

    let end_us = start_us + duration_us;
    let mut audio_temp = AudioTemp::default();
    let mut shifted = Vec::new();
    for track in &project.tracks {
        if track.kind != TrackKind::Audio || track.muted {
            continue;
        }
        for clip in &track.clips {
            let Clip::Audio(a) = clip else { continue };
            if !a.enabled {
                continue;
            }
            let Some(media) = project.find_media(a.media_id) else {
                continue;
            };
            if let (MediaKind::Sequence, Some(seq_id)) = (media.kind, media.sequence_id) {
                // A nested sequence's audio: expand to leaf pieces on the outer timeline,
                // then cut each to the requested range.
                let pieces = expand_sequence_audio(
                    project,
                    a,
                    track.audio_role,
                    seq_id,
                    1,
                    &mut audio_temp,
                )?;
                shifted.extend(
                    pieces
                        .iter()
                        .filter_map(|p| clip_piece_to_range(p, start_us, end_us)),
                );
                continue;
            }
            let clip_len = a.timeline_duration_us();
            let clip_end = a.start_us + clip_len;
            let overlap_start = start_us.max(a.start_us);
            let overlap_end = end_us.min(clip_end);
            if overlap_end <= overlap_start {
                continue;
            }
            let offset = mul_us(overlap_start - a.start_us, a.speed_factor());
            let overlap_src = mul_us(overlap_end - overlap_start, a.speed_factor());
            shifted.push(AudioMixClip {
                path: media.path.clone(),
                start_us: overlap_start - start_us,
                source_in_us: a.source_in_us + offset,
                source_out_us: a.source_in_us + offset + overlap_src,
                gain_db: evaluate_volume_db(a, overlap_start),
                fade_in_us: a.fade_in_us,
                fade_out_us: a.fade_out_us,
                role: track.audio_role,
                speed: a.speed_factor(),
                effects: a.effects.clone(),
                denoise: a.audio_denoise.clone(),
            });
        }
    }
    if shifted.is_empty() {
        return Ok(false);
    }

    let wasm_tmp = bake_wasm_audio_effects(project, &mut shifted)?;
    let duck = duck_settings(project);
    let result = mix_timeline_audio(
        &shifted,
        project.settings.sample_rate,
        duration_us,
        out_path,
        duck,
    );
    if let Some(d) = wasm_tmp {
        std::fs::remove_dir_all(d).ok();
    }
    result?;
    Ok(true)
}

/// Mix a slice of timeline audio into an in-memory WAV (for preview scrub).
pub fn mix_timeline_audio_segment(
    project: &Project,
    start_us: i64,
    duration_us: i64,
) -> Result<Vec<u8>, ExportError> {
    let temp_dir = std::env::temp_dir().join(format!("renderly-scrub-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(FfmpegCliError::Io)?;
    let temp_wav = temp_dir.join("segment.wav");

    let wrote = mix_timeline_audio_range_to_file(project, start_us, duration_us, &temp_wav)?;
    let bytes = if wrote {
        std::fs::read(&temp_wav).map_err(FfmpegCliError::Io)?
    } else {
        Vec::new()
    };
    std::fs::remove_dir_all(&temp_dir).ok();
    Ok(bytes)
}

fn resolve_background_removal_mask(
    frame: &RgbaFrame,
    cfg: &crate::project::BackgroundRemoval,
) -> Option<crate::project::ClipMask> {
    use crate::project::{ClipMask, ClipMaskKind};

    if let Some(mask) = crate::mask::cached_background_mask(cfg) {
        return Some(mask);
    }

    // Live heuristic matte when no cache has been baked yet.
    let matte = crate::mask::generate_heuristic_matte(frame, cfg.threshold);
    Some(ClipMask {
        enabled: true,
        invert: false,
        feather: cfg.feather,
        kind: ClipMaskKind::Luma {
            pixels: matte.into_raw(),
            width: frame.width,
            height: frame.height,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Project, Settings};

    fn ffmpeg_available() -> bool {
        crate::media::ffmpeg_available()
    }

    /// Serialises the v7 tests that spawn ffmpeg children (concurrent spawns can deadlock on
    /// Windows when long-lived readers hold inherited pipe handles).
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn generate_test_video(path: &Path) {
        let status = crate::proc::command(crate::media::ffmpeg_path().unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=30",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(path)
            .status_serial()
            .expect("ffmpeg");
        assert!(status.success());
    }

    #[test]
    fn export_cuts_only_timeline_to_mp4() {
        if !ffmpeg_available() {
            eprintln!("skipping export test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-export-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        let output_path = dir.join("out.mp4");
        generate_test_video(&video_path);

        let mut project = Project::new("export-test", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: video_path.clone(),
            kind: MediaKind::Video,
            duration_us: Some(1000000),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
            sequence_id: None,
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                start_us: 0,
                source_in_us: 0,
                source_out_us: 500_000,
                gain_db: 0.0,
                enabled: true,
                fade_in_us: 0,
                fade_out_us: 0,
                ..Default::default()
            }));

        export_project_with_settings(
            &project,
            &output_path,
            ExportSettings {
                width: 320,
                height: 240,
                fps: 30.0,
                // Force software so CI (GPU-less runners with NVENC-advertising FFmpeg
                // builds) doesn't depend on hardware encode probing.
                video_encoder: crate::media::VideoEncoderPreference::Software,
                ..Default::default()
            },
            &mut |_| true,
        )
        .expect("export");

        assert!(output_path.is_file());
        let meta = std::fs::metadata(&output_path).unwrap();
        assert!(meta.len() > 0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_project_with_progress_cancel_returns_cancelled() {
        if !crate::media::ffmpeg_available() {
            eprintln!("skipping cancel export test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-cancel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        let output_path = dir.join("out.mp4");
        generate_test_video(&video_path);

        let mut project = Project::new("t", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: video_path.clone(),
            kind: MediaKind::Video,
            duration_us: Some(1000000),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
            sequence_id: None,
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                start_us: 0,
                source_in_us: 0,
                source_out_us: 500_000,
                gain_db: 0.0,
                enabled: true,
                fade_in_us: 0,
                fade_out_us: 0,
                ..Default::default()
            }));

        let err = export_project_with_settings(
            &project,
            &output_path,
            ExportSettings {
                width: 320,
                height: 240,
                fps: 30.0,
                video_encoder: crate::media::VideoEncoderPreference::Software,
                ..Default::default()
            },
            &mut |_| false,
        )
        .expect_err("cancel");
        assert!(matches!(err, ExportError::Cancelled));
        assert!(!output_path.exists());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frame_lru_evicts_oldest_and_returns_hits() {
        fn tiny(n: u8) -> RgbaFrame {
            RgbaFrame {
                width: 1,
                height: 1,
                pixels: vec![n, 0, 0, 255],
            }
        }
        let mut lru = FrameLru::default();
        assert!(lru.get(0).is_none());
        lru.insert(10, tiny(1));
        lru.insert(20, tiny(2));
        assert_eq!(lru.get(10).unwrap().pixels[0], 1);
        // Touch 10 so 20 becomes oldest once we fill past capacity... for a small
        // capacity check, fill to FRAME_CACHE_CAPACITY and ensure the untouched key goes.
        for i in 0..FRAME_CACHE_CAPACITY {
            lru.insert(100 + i as i64, tiny(3));
        }
        // 20 was never touched after insert; 10 was touched via get — depending on order,
        // after filling with new keys both early keys should be gone.
        assert!(
            lru.get(20).is_none(),
            "untouched early key must be evicted once capacity is exceeded"
        );
        assert_eq!(lru.order.len(), FRAME_CACHE_CAPACITY);
    }

    #[test]
    fn timeline_duration_uses_latest_clip_end() {
        let mut project = Project::new("t", Settings::default());
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id: uuid::Uuid::new_v4(),
                start_us: 1000000,
                source_in_us: 0,
                source_out_us: 2000000,
                gain_db: 0.0,
                enabled: true,
                fade_in_us: 0,
                fade_out_us: 0,
                ..Default::default()
            }));
        assert!((timeline_duration(&project) - 3000000).abs() < 1);
    }

    #[test]
    fn mix_timeline_audio_segment_empty_without_audio() {
        let project = Project::new("t", Settings::default());
        let bytes = mix_timeline_audio_segment(&project, 0, 500000).unwrap();
        assert!(bytes.is_empty());
    }

    #[test]
    fn collect_audio_clips_skips_muted_tracks() {
        let mut project = Project::new("t", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: "voice.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(5000000),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        let mut track = crate::project::Track::new(TrackKind::Audio, "A1");
        track.muted = true;
        track.clips.push(Clip::Audio(crate::project::MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: 0,
            source_in_us: 0,
            source_out_us: 5000000,
            gain_db: 0.0,
            enabled: true,
            fade_in_us: 0,
            fade_out_us: 0,
            ..Default::default()
        }));
        project.tracks.push(track);

        assert!(collect_audio_clips(&project, &mut AudioTemp::default())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn active_layers_and_has_video_content_skip_hidden_tracks() {
        let mut project = Project::new("t", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: "clip.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(5000000),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
            sequence_id: None,
        });
        let mut track = crate::project::Track::new(TrackKind::Video, "V1");
        track.hidden = true;
        track.clips.push(Clip::Video(crate::project::MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: 0,
            source_in_us: 0,
            source_out_us: 5000000,
            gain_db: 0.0,
            enabled: true,
            fade_in_us: 0,
            fade_out_us: 0,
            ..Default::default()
        }));
        project.tracks.push(track);

        assert!(!has_video_content(&project));
        assert!(eval::active_steps(&project, 0).unwrap().is_empty());
    }

    #[test]
    fn mix_timeline_audio_range_to_file_returns_false_without_audio() {
        let project = Project::new("t", Settings::default());
        let dir = std::env::temp_dir().join(format!("renderly-mixtest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.wav");
        let wrote = mix_timeline_audio_range_to_file(&project, 0, 500000, &out).unwrap();
        assert!(!wrote);
        assert!(!out.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frame_renderer_reuses_state_across_render_calls() {
        if !ffmpeg_available() {
            eprintln!("skipping frame renderer test: ffmpeg not on PATH");
            return;
        }

        let dir =
            std::env::temp_dir().join(format!("renderly-framerender-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        generate_test_video(&video_path);

        let mut project = Project::new("frame-renderer-test", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: video_path.clone(),
            kind: MediaKind::Video,
            duration_us: Some(1000000),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
            sequence_id: None,
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                start_us: 0,
                source_in_us: 0,
                source_out_us: 1000000,
                gain_db: 0.0,
                enabled: true,
                fade_in_us: 0,
                fade_out_us: 0,
                ..Default::default()
            }));

        let settings = ExportSettings {
            width: 320,
            height: 240,
            fps: 30.0,
            ..Default::default()
        };
        let mut renderer = FrameRenderer::new(settings, DecodeOptions::default()).expect("new");
        // Sequential monotonic renders reuse the same decoder without reopening ffmpeg.
        let frame_a = renderer.render(&project, 0).expect("render frame 0");
        let frame_b = renderer
            .render(&project, 1000000 / 30000000)
            .expect("render frame 1");
        assert_eq!(frame_a.len(), (320 * 240 * 4) as usize);
        assert_eq!(frame_b.len(), (320 * 240 * 4) as usize);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn render_timed_matches_render_pixels_and_reports_nonzero_decode() {
        if !ffmpeg_available() {
            eprintln!("skipping render_timed test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-timed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        generate_test_video(&video_path);

        let mut project = Project::new("render-timed-test", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: video_path.clone(),
            kind: MediaKind::Video,
            duration_us: Some(1000000),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
            sequence_id: None,
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                start_us: 0,
                source_in_us: 0,
                source_out_us: 1000000,
                gain_db: 0.0,
                enabled: true,
                fade_in_us: 0,
                fade_out_us: 0,
                ..Default::default()
            }));

        let settings = ExportSettings {
            width: 320,
            height: 240,
            fps: 30.0,
            ..Default::default()
        };
        let mut timed_renderer =
            FrameRenderer::new(settings, DecodeOptions::default()).expect("new");
        let (timed_pixels, timing) = timed_renderer
            .render_timed(&project, 0)
            .expect("render_timed");

        let mut plain_renderer =
            FrameRenderer::new(settings, DecodeOptions::default()).expect("new");
        let plain_pixels = plain_renderer.render(&project, 0).expect("render");

        assert_eq!(timed_pixels, plain_pixels);
        // A real decode + composite of an actual video frame never completes in 0ms; a
        // regression that stops timing the decode loop (e.g. measuring outside it) would
        // silently report exactly this.
        assert!(timing.decode_ms > 0.0);
        assert!(timing.compose_ms >= 0.0);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frame_renderer_keys_decoders_by_track_not_media_for_pip() {
        if !ffmpeg_available() {
            eprintln!("skipping PIP decoder test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-pip-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        generate_test_video(&video_path);

        let mut project = Project::new("pip-test", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: video_path.clone(),
            kind: MediaKind::Video,
            duration_us: Some(1000000),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
            sequence_id: None,
        });

        // Two video tracks both showing the SAME media at the same time — a minimal PIP
        // setup. Before the fix, both layers shared one decoder keyed by media_id and
        // fought over that single ffmpeg process's playback position.
        for _ in 0..2 {
            let mut track = crate::project::Track::new(TrackKind::Video, "V");
            track.clips.push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                start_us: 0,
                source_in_us: 0,
                source_out_us: 1000000,
                gain_db: 0.0,
                enabled: true,
                fade_in_us: 0,
                fade_out_us: 0,
                ..Default::default()
            }));
            project.tracks.push(track);
        }

        let settings = ExportSettings {
            width: 320,
            height: 240,
            fps: 30.0,
            ..Default::default()
        };
        let mut renderer = FrameRenderer::new(settings, DecodeOptions::default()).expect("new");
        renderer.render(&project, 0).expect("render");

        assert_eq!(
            renderer.decoders.len(),
            2,
            "each PIP layer's track should get its own decoder, not share one keyed by media_id"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn frame_renderer_opens_one_decoder_per_nested_instance() {
        use crate::project::{MediaItem, Sequence, Track};
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        if !ffmpeg_available() {
            eprintln!("skipping nested decoder test: ffmpeg not on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("renderly-nested-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        generate_test_video(&video_path);

        let mut project = Project::new("nested-decoders", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(MediaItem {
            id: media_id,
            path: video_path,
            kind: MediaKind::Video,
            duration_us: Some(1000000),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
            sequence_id: None,
        });
        let seq_id = uuid::Uuid::new_v4();
        let seq_media = uuid::Uuid::new_v4();
        let mut inner = Track::new(TrackKind::Video, "inner");
        inner.clips.push(Clip::Video(crate::project::MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: 0,
            source_in_us: 0,
            source_out_us: 1000000,
            enabled: true,
            ..Default::default()
        }));
        project.sequences.push(Sequence {
            id: seq_id,
            name: "S".into(),
            width: None,
            height: None,
            tracks: vec![inner],
        });
        project.media.push(MediaItem {
            id: seq_media,
            path: Default::default(),
            kind: MediaKind::Sequence,
            duration_us: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(seq_id),
        });
        // The same sequence on two tracks at once, plus the raw clip on a third.
        for _ in 0..2 {
            let mut t = Track::new(TrackKind::Video, "V");
            t.clips.push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id: seq_media,
                start_us: 0,
                source_in_us: 0,
                source_out_us: 1000000,
                enabled: true,
                ..Default::default()
            }));
            project.tracks.push(t);
        }
        let mut raw = Track::new(TrackKind::Video, "raw");
        raw.clips.push(Clip::Video(crate::project::MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            start_us: 0,
            source_in_us: 0,
            source_out_us: 1000000,
            enabled: true,
            ..Default::default()
        }));
        project.tracks.push(raw);
        assert!(project.validate().is_ok());

        let settings = ExportSettings {
            width: 320,
            height: 240,
            fps: 30.0,
            ..Default::default()
        };
        let mut renderer = FrameRenderer::new(settings, DecodeOptions::default()).expect("new");
        let frame = renderer.render(&project, 200000).expect("render");
        assert_eq!(frame.len(), (320 * 240 * 4) as usize);
        assert_eq!(
            renderer.decoders.len(),
            3,
            "two sequence placements + the raw clip each get their own decoder"
        );
        // timeline_duration stays the root timeline's span.
        assert!((timeline_duration(&project) - 1000000).abs() < 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- schema v7: nested-sequence audio ----------------------------------------------

    fn seq_audio_project(
        outer: MediaClip,
        inner: MediaClip,
        inner_role: Option<TrackAudioRole>,
        outer_role: Option<TrackAudioRole>,
    ) -> Project {
        use crate::project::{MediaItem, Sequence, Track};
        let mut project = Project::new("nested-audio", Settings::default());
        let wav_media = uuid::Uuid::new_v4();
        project.media.push(MediaItem {
            id: wav_media,
            path: "tone.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(20000000),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        let seq_id = uuid::Uuid::new_v4();
        let seq_media = uuid::Uuid::new_v4();
        let mut inner_track = Track::new(TrackKind::Audio, "inner");
        inner_track.audio_role = inner_role;
        let mut inner = inner;
        inner.media_id = wav_media;
        inner_track.clips.push(Clip::Audio(inner));
        project.sequences.push(Sequence {
            id: seq_id,
            name: "S".into(),
            width: None,
            height: None,
            tracks: vec![inner_track],
        });
        project.media.push(MediaItem {
            id: seq_media,
            path: Default::default(),
            kind: MediaKind::Sequence,
            duration_us: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(seq_id),
        });
        let mut outer_track = Track::new(TrackKind::Audio, "outer");
        outer_track.audio_role = outer_role;
        let mut outer = outer;
        outer.media_id = seq_media;
        outer_track.clips.push(Clip::Audio(outer));
        project.tracks.push(outer_track);
        project
    }

    fn audio_clip(position: f64, src_in: f64, src_out: f64) -> MediaClip {
        MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id: uuid::Uuid::new_v4(),
            start_us: (position * 1e6).round() as i64,
            source_in_us: (src_in * 1e6).round() as i64,
            source_out_us: (src_out * 1e6).round() as i64,
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn nested_audio_maps_position_trim_speed_and_gain_through_the_outer_clip() {
        let mut inner = audio_clip(2.0, 0.0, 4.0);
        inner.gain_db = -3.0;
        // The outer clip plays sequence time [1, 5) at 2x, placed at t = 10, gain -6 dB.
        let mut outer = audio_clip(10.0, 1.0, 5.0);
        outer.speed = 2.0;
        outer.gain_db = -6.0;
        let project = seq_audio_project(outer, inner, None, Some(TrackAudioRole::Music));

        let clips = collect_audio_clips(&project, &mut AudioTemp::default()).unwrap();
        assert_eq!(clips.len(), 1);
        let c = &clips[0];
        // The inner clip covers sequence time [2, 6); the outer window [1, 5) keeps [2, 5):
        // source [0, 3), starting 1 sequence-second (0.5 s at 2x) into the outer clip.
        assert_eq!(c.source_in_us, 0);
        assert!((c.source_out_us - 3000000).abs() < 1);
        assert!((c.start_us - 10500000).abs() < 1);
        assert!((c.speed - 2.0).abs() < 1e-9);
        assert!((c.gain_db - (-9.0)).abs() < 1e-9);
        // Ends where the outer clip ends: 10 + 4/2 = 12.
        let end =
            c.start_us as f64 / 1e6 + ((c.source_out_us - c.source_in_us) as f64 / 1e6) / c.speed;
        assert!((end - 12.0).abs() < 1e-9, "{end}");
        // The expanded clip takes the outer track's role on the parent.
        assert_eq!(c.role, Some(TrackAudioRole::Music));
    }

    #[test]
    fn nested_audio_is_clipped_to_the_outer_span_and_keeps_inner_settings() {
        use crate::project::AudioDenoise;
        let mut inner = audio_clip(0.0, 3.0, 7.0);
        inner.fade_in_us = 500000;
        inner.fade_out_us = 250000;
        inner.audio_denoise = Some(AudioDenoise {
            enabled: true,
            backend: "rnnoise".into(),
            strength: 0.7,
        });
        // The outer clip shows only the first 1.5 s of the sequence.
        let outer = audio_clip(5.0, 0.0, 1.5);
        let project = seq_audio_project(outer, inner, Some(TrackAudioRole::Voiceover), None);

        let clips = collect_audio_clips(&project, &mut AudioTemp::default()).unwrap();
        assert_eq!(clips.len(), 1);
        let c = &clips[0];
        assert!((c.source_in_us - 3000000).abs() < 1);
        assert!((c.source_out_us - 4500000).abs() < 1, "{}", c.source_out_us);
        assert!((c.start_us - 5000000).abs() < 1);
        // The inner clip's start fade survives; its end fade was cut off by the outer span.
        assert!((c.fade_in_us - 500000).abs() < 1);
        assert_eq!(c.fade_out_us, 0);
        // RNNoise denoise stays attached to the leaf clip so `mix_clip_bus` still substitutes
        // the denoised input for nested clips.
        let d = c.denoise.as_ref().expect("denoise survives expansion");
        assert_eq!((d.backend.as_str(), d.strength), ("rnnoise", 0.7));
        // The outer track has no role, so the expanded clip has none either.
        assert_eq!(c.role, None);
    }

    #[test]
    fn nested_audio_outer_fades_apply_at_the_outer_clip_edges() {
        let inner = audio_clip(0.0, 0.0, 10.0);
        let mut outer = audio_clip(0.0, 0.0, 4.0);
        outer.fade_in_us = 1_000_000;
        outer.fade_out_us = 2_000_000;
        let project = seq_audio_project(outer, inner, None, None);
        let clips = collect_audio_clips(&project, &mut AudioTemp::default()).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(clips[0].fade_in_us, 1000000);
        assert_eq!(clips[0].fade_out_us, 2000000);
    }

    #[test]
    fn nested_audio_range_mix_pieces_are_cut_to_the_range() {
        let inner = audio_clip(0.0, 0.0, 10.0);
        let outer = audio_clip(0.0, 0.0, 10.0);
        let project = seq_audio_project(outer, inner, None, None);
        let pieces = collect_audio_clips(&project, &mut AudioTemp::default()).unwrap();
        let cut = clip_piece_to_range(&pieces[0], 2000000, 4000000).unwrap();
        assert_eq!(cut.start_us, 0);
        assert!((cut.source_in_us - 2000000).abs() < 1);
        assert!((cut.source_out_us - 4000000).abs() < 1);
        assert!(clip_piece_to_range(&pieces[0], 11000000, 12000000).is_none());
    }

    /// Decode 16-bit PCM WAV samples (the only format `mix_clip_bus` writes).
    fn wav_samples(path: &Path) -> Vec<i16> {
        let bytes = std::fs::read(path).unwrap();
        let mut i = 12;
        while i + 8 <= bytes.len() {
            let id = &bytes[i..i + 4];
            let len = u32::from_le_bytes(bytes[i + 4..i + 8].try_into().unwrap()) as usize;
            if id == b"data" {
                let end = (i + 8 + len).min(bytes.len());
                return bytes[i + 8..end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|b| i16::from_le_bytes(*b))
                    .collect();
            }
            i += 8 + len + (len & 1);
        }
        panic!("no data chunk in {}", path.display());
    }

    #[test]
    fn nested_audio_mix_equals_the_flat_equivalent() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        if !ffmpeg_available() {
            eprintln!("skipping nested audio mix test: ffmpeg not on PATH");
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("renderly-nested-audio-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let tone = dir.join("tone.wav");
        let status = crate::proc::command(crate::media::ffmpeg_path().unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=6:sample_rate=48000",
                "-ac",
                "2",
            ])
            .arg(&tone)
            .status_serial()
            .unwrap();
        assert!(status.success());

        // Flat: the tone's [1, 3) at t = 4 with -6 dB.
        let mut flat_clip = audio_clip(4.0, 1.0, 3.0);
        flat_clip.gain_db = -6.0;
        // Nested: inside the sequence the tone [0.5, 3) sits at sequence time 1 (-3 dB); the
        // outer clip (-3 dB) plays sequence time [1.5, 3) at t = 4, which keeps tone [1, 2.5).
        // Flat equivalent: tone [1, 2.5) at t = 4 with -6 dB.
        let mut inner = audio_clip(1.0, 0.5, 3.0);
        inner.gain_db = -3.0;
        let mut outer = audio_clip(4.0, 1.5, 3.0);
        outer.gain_db = -3.0;
        let mut flat_project = Project::new("flat", Settings::default());
        let mut nested = seq_audio_project(outer, inner, None, None);
        for p in [&mut flat_project, &mut nested] {
            for m in p.media.iter_mut().filter(|m| m.kind == MediaKind::Audio) {
                m.path = tone.clone();
            }
        }
        // Flat equivalent of the nested clip: tone[1, 2.5) = the nested clip's visible span.
        flat_clip.source_out_us = 2500000;
        let flat_media = nested
            .media
            .iter()
            .find(|m| m.kind == MediaKind::Audio)
            .unwrap()
            .clone();
        flat_clip.media_id = flat_media.id;
        flat_clip.gain_db = -6.0;
        flat_project.media.push(flat_media);
        let mut track = crate::project::Track::new(TrackKind::Audio, "A");
        track.clips.push(Clip::Audio(flat_clip));
        flat_project.tracks.push(track);

        let mut temp = AudioTemp::default();
        let flat_clips = collect_audio_clips(&flat_project, &mut temp).unwrap();
        let nested_clips = collect_audio_clips(&nested, &mut temp).unwrap();
        assert_eq!(flat_clips.len(), 1);
        assert_eq!(nested_clips.len(), 1);

        let flat_wav = dir.join("flat.wav");
        let nested_wav = dir.join("nested.wav");
        mix_timeline_audio(&flat_clips, 48_000, 8000000, &flat_wav, None).unwrap();
        mix_timeline_audio(&nested_clips, 48_000, 8000000, &nested_wav, None).unwrap();
        let (a, b) = (wav_samples(&flat_wav), wav_samples(&nested_wav));
        assert_eq!(a.len(), b.len());
        let max_diff = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs())
            .max()
            .unwrap();
        // Same filter graph on the same leaf media; allow a little for -3 dB x2 vs -6 dB
        // rounding.
        assert!(max_diff <= 40, "nested vs flat max sample diff {max_diff}");
        assert!(
            a.iter().any(|s| s.unsigned_abs() > 1000),
            "the mix must not be silent"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sequence_with_voice_and_music_is_premixed_so_ducking_stays_inside_it() {
        let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use crate::project::{MediaItem, Sequence, Track};
        if !ffmpeg_available() {
            eprintln!("skipping sequence ducking test: ffmpeg not on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("renderly-seq-duck-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let tone = dir.join("tone.wav");
        let status = crate::proc::command(crate::media::ffmpeg_path().unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=330:duration=4:sample_rate=48000",
                "-ac",
                "2",
            ])
            .arg(&tone)
            .status_serial()
            .unwrap();
        assert!(status.success());

        let mut project = Project::new("duck", Settings::default());
        let wav = uuid::Uuid::new_v4();
        project.media.push(MediaItem {
            id: wav,
            path: tone.clone(),
            kind: MediaKind::Audio,
            duration_us: Some(4000000),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        let mk_track = |role| {
            let mut t = Track::new(TrackKind::Audio, "t");
            t.audio_role = Some(role);
            let mut c = audio_clip(0.0, 0.0, 4.0);
            c.media_id = wav;
            t.clips.push(Clip::Audio(c));
            t
        };
        let seq_id = uuid::Uuid::new_v4();
        let seq_media = uuid::Uuid::new_v4();
        project.sequences.push(Sequence {
            id: seq_id,
            name: "S".into(),
            width: None,
            height: None,
            tracks: vec![
                mk_track(TrackAudioRole::Voiceover),
                mk_track(TrackAudioRole::Music),
            ],
        });
        project.media.push(MediaItem {
            id: seq_media,
            path: Default::default(),
            kind: MediaKind::Sequence,
            duration_us: None,
            width: None,
            height: None,
            fps: None,
            sequence_id: Some(seq_id),
        });
        let mut outer_track = Track::new(TrackKind::Audio, "outer");
        outer_track.audio_role = Some(TrackAudioRole::Music);
        let mut outer = audio_clip(2.0, 0.0, 4.0);
        outer.media_id = seq_media;
        outer_track.clips.push(Clip::Audio(outer));
        project.tracks.push(outer_track);

        let mut temp = AudioTemp::default();
        let clips = collect_audio_clips(&project, &mut temp).unwrap();
        // One pre-mixed bus (ducked inside the sequence), carrying the outer track's role.
        assert_eq!(clips.len(), 1, "{clips:?}");
        assert!(clips[0].path.extension().is_some_and(|e| e == "wav"));
        assert!(
            clips[0].path.is_file(),
            "the bus exists while the guard lives"
        );
        assert_eq!(clips[0].role, Some(TrackAudioRole::Music));
        assert!((clips[0].start_us - 2000000).abs() < 1);
        let bus_dir = clips[0].path.parent().unwrap().to_path_buf();
        drop(temp);
        assert!(
            !bus_dir.exists(),
            "intermediate buses are cleaned up on drop"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
