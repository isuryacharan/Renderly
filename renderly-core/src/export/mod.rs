//! Timeline → decode → wgpu composite → encode export pipeline.

use crate::captions::CaptionError;
use crate::commands::ExportPreset;
use crate::compose::eval::{self, EvalError};
use crate::compose::{ComposeError, ComposeLayer, Compositor, LayerSource};
use crate::media::{
    mix_timeline_audio, mux_video_audio, AudioMixClip, DuckSettings, FfmpegCliError, ReaderOptions,
    RgbaFrame, VideoEncoder, VideoReader,
};
#[cfg(test)]
use crate::project::MediaKind;
use crate::project::{evaluate_volume_db, Clip, Project, TrackAudioRole, TrackKind};
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
    /// Returned when `export_project_with_progress`'s callback returns `false`.
    #[error("export cancelled")]
    Cancelled,
}

impl From<EvalError> for ExportError {
    fn from(e: EvalError) -> Self {
        match e {
            EvalError::MediaNotFound(id) => ExportError::MediaNotFound(id),
            EvalError::NotVideo(id) => ExportError::NotVideo(id),
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
            ExportPreset::TikTok9x16 => (1080, 1920, project.settings.fps),
            ExportPreset::Youtube16x9 => (1920, 1080, project.settings.fps),
            ExportPreset::Custom { width, height, fps } => (*width, *height, *fps),
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
    last_source_time: f64,
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

fn quantize_source_time_ms(source_time: f64) -> i64 {
    (source_time * 1000.0).round() as i64
}

impl DecoderState {
    fn new(path: PathBuf, reader_opts: ReaderOptions) -> Self {
        Self {
            path,
            reader_opts,
            reader: None,
            still: None,
            last_source_time: f64::NAN,
            cache: FrameLru::default(),
        }
    }

    fn frame_at(&mut self, source_time: f64) -> Result<Option<RgbaFrame>, FfmpegCliError> {
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
            || source_time + 1e-6 < self.last_source_time
            || (source_time - self.last_source_time).abs() > 0.5;

        if needs_reopen {
            self.reader = Some(VideoReader::open_with(
                &self.path,
                source_time,
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
    decoders: HashMap<uuid::Uuid, DecoderState>,
}

impl FrameRenderer {
    pub fn new(settings: ExportSettings, decode_opts: DecodeOptions) -> Result<Self, ExportError> {
        let compositor = Compositor::new(settings.width, settings.height)?;
        Ok(Self {
            settings,
            decode_opts,
            compositor,
            decoders: HashMap::new(),
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
        })
    }

    pub fn output_view(&self) -> &wgpu::TextureView {
        self.compositor.output_view()
    }

    pub fn output_size(&self) -> (u32, u32) {
        (self.compositor.width(), self.compositor.height())
    }

    /// A5: warm the per-track decode LRU around `time_secs` so scrubbing nearby
    /// (especially backward) hits cache instead of respawning FFmpeg. Decodes only —
    /// does not composite or present. Safe to call from the scrub worker after a present;
    /// respects the same decoder reopen rules as [`Self::render`].
    pub fn prefetch_around(
        &mut self,
        project: &Project,
        time_secs: f64,
        window_secs: f64,
    ) -> Result<(), ExportError> {
        let fps = self.settings.fps.max(1.0);
        let step = 1.0 / fps;
        let window = window_secs.max(0.0);
        let start = (time_secs - window).max(0.0);
        let end = time_secs + window;
        let reader_opts = ReaderOptions {
            target_height: self.decode_opts.target_height,
            output_fps: self.decode_opts.output_fps,
        };

        let mut t = start;
        while t <= end + 1e-9 {
            let layers = eval::active_layers(project, t)?;
            for layer in &layers {
                let decoder = self
                    .decoders
                    .entry(layer.decoder_key)
                    .or_insert_with(|| DecoderState::new(layer.path.clone(), reader_opts));
                if decoder.path != layer.path {
                    *decoder = DecoderState::new(layer.path.clone(), reader_opts);
                }
                let _ = decoder.frame_at(layer.source_time)?;
            }
            t += step;
        }
        Ok(())
    }

    pub fn render(&mut self, project: &Project, time_secs: f64) -> Result<Vec<u8>, ExportError> {
        self.render_inner(project, time_secs, true)
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
        time_secs: f64,
    ) -> Result<(Vec<u8>, FrameTiming), ExportError> {
        self.render_inner(project, time_secs, true)
            .map(|(pixels, timing)| (pixels.expect("readback requested"), timing))
    }

    /// A4: decode + GPU compose with no CPU readback. Result lives in
    /// [`Self::output_view`] for the preview blit path.
    pub fn render_to_texture(
        &mut self,
        project: &Project,
        time_secs: f64,
    ) -> Result<FrameTiming, ExportError> {
        self.render_inner(project, time_secs, false)
            .map(|(_, timing)| timing)
    }

    fn render_inner(
        &mut self,
        project: &Project,
        time_secs: f64,
        readback: bool,
    ) -> Result<(Option<Vec<u8>>, FrameTiming), ExportError> {
        let layers = eval::active_layers(project, time_secs)?;
        let packs = crate::packs::load_project_packs(project);
        let mut compose_layers = Vec::with_capacity(layers.len() + 2);
        let plugin_host = crate::plugins::PluginHost::for_project(project).ok();

        let reader_opts = ReaderOptions {
            target_height: self.decode_opts.target_height,
            output_fps: self.decode_opts.output_fps,
        };
        let mut decode_secs = 0.0;
        for layer in &layers {
            let decoder = self
                .decoders
                .entry(layer.decoder_key)
                .or_insert_with(|| DecoderState::new(layer.path.clone(), reader_opts));
            if decoder.path != layer.path {
                *decoder = DecoderState::new(layer.path.clone(), reader_opts);
            }
            let decode_start = Instant::now();
            let frame = decoder.frame_at(layer.source_time)?;
            decode_secs += decode_start.elapsed().as_secs_f64();
            if let Some(mut frame) = frame {
                if let Some(host) = plugin_host.as_ref() {
                    let _ = host.apply_effects(&mut frame, &layer.effects);
                }

                let mask = if let Some(bg) = layer.background_removal.as_ref().filter(|b| b.enabled)
                {
                    resolve_background_removal_mask(&frame, bg)
                } else {
                    layer.mask.clone()
                };

                compose_layers.push(ComposeLayer::from_active(
                    layer,
                    LayerSource::Frame(frame),
                    mask,
                ));
            }
        }

        for cap in eval::active_captions(project, time_secs) {
            compose_layers.push(ComposeLayer::caption(LayerSource::Frame(
                crate::captions::render_caption_for_project(
                    project,
                    &cap.text,
                    &cap.style_id,
                    self.settings.width,
                    self.settings.height,
                )?,
            )));
        }

        let compose_start = Instant::now();
        let pixels = if readback {
            Some(self.compositor.composite(&packs, &compose_layers)?)
        } else {
            self.compositor
                .compose_to_texture(&packs, &compose_layers)?;
            None
        };
        let compose_secs = compose_start.elapsed().as_secs_f64();

        Ok((
            pixels,
            FrameTiming {
                decode_ms: decode_secs * 1000.0,
                compose_ms: compose_secs * 1000.0,
            },
        ))
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

/// Render one composited RGBA frame at `time_secs` (video + burned-in captions).
///
/// One-shot convenience wrapper: builds a `FrameRenderer` (fresh GPU device + decoders)
/// for a single frame. Fine for perception/MCP call sites that render one isolated frame;
/// callers rendering a sequence (playback, export) should use `FrameRenderer` directly to
/// avoid rebuilding the compositor and decoders per frame.
pub fn render_frame_at(
    project: &Project,
    time_secs: f64,
    settings: ExportSettings,
) -> Result<Vec<u8>, ExportError> {
    let mut renderer = FrameRenderer::new(settings, DecodeOptions::default())?;
    renderer.render(project, time_secs)
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

    let duration_secs = timeline_duration(project);
    if duration_secs <= 0.0 {
        return Err(ExportError::EmptyTimeline);
    }

    if !has_video_content(project) {
        return Err(ExportError::EmptyTimeline);
    }

    let total_frames = (duration_secs * settings.fps).ceil() as u64;
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
        duration_secs,
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
    duration_secs: f64,
    total_frames: u64,
    temp_dir: &Path,
    temp_video: &Path,
    on_progress: &mut dyn FnMut(ExportProgress) -> bool,
) -> Result<(), ExportError> {
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
        let t = frame_idx as f64 / settings.fps;
        let pixels = renderer.render(project, t)?;
        encoder.write_frame(&pixels)?;
    }
    encoder.finish()?;
    drop(renderer);

    let mut audio_clips = collect_audio_clips(project);
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
        duration_secs,
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

fn collect_audio_clips(project: &Project) -> Vec<AudioMixClip> {
    let mut clips = Vec::new();
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
            for (local_off, tl_dur, speed) in crate::project::anim::speed_segments(a) {
                let src_in = a.source_time_at(a.position_secs + local_off);
                let src_out = a.source_time_at(a.position_secs + local_off + tl_dur);
                clips.push(AudioMixClip {
                    path: media.path.clone(),
                    position_secs: a.position_secs + local_off,
                    source_in_secs: src_in,
                    source_out_secs: src_out.max(src_in + 1e-4),
                    gain_db: evaluate_volume_db(a, a.position_secs + local_off),
                    fade_in_secs: if local_off < 1e-6 {
                        a.fade_in_secs
                    } else {
                        0.0
                    },
                    fade_out_secs: if (local_off + tl_dur - a.timeline_duration_secs()).abs() < 0.06
                    {
                        a.fade_out_secs
                    } else {
                        0.0
                    },
                    role: track.audio_role,
                    speed,
                    effects: a.effects.clone(),
                    denoise: a.audio_denoise.clone(),
                });
            }
        }
    }
    clips
}

/// Decode clip source ranges, run audio WASM effects, rewrite clip paths to processed WAVs.
fn bake_wasm_audio_effects(
    project: &Project,
    clips: &mut [AudioMixClip],
) -> Result<Option<PathBuf>, ExportError> {
    let needs = clips.iter().any(|c| {
        c.effects
            .iter()
            .any(|e| e.enabled && e.effect_id.starts_with("wasm:"))
    });
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
            .filter(|e| e.enabled && e.effect_id.starts_with("wasm:"))
            .cloned()
            .collect();
        if wasm_fx.is_empty() {
            continue;
        }
        let has_audio_plugin = wasm_fx.iter().any(|e| {
            e.effect_id
                .strip_prefix("wasm:")
                .map(|id| host.plugin_is_audio(id))
                .unwrap_or(false)
        });
        if !has_audio_plugin {
            continue;
        }

        let seg_dur = (clip.source_out_secs - clip.source_in_secs).max(1e-4);
        let raw_path = temp_dir.join(format!("raw_{i}.f32"));
        let wav_path = temp_dir.join(format!("proc_{i}.wav"));

        let status = crate::proc::command(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-ss",
                &format!("{:.6}", clip.source_in_secs),
                "-i",
            ])
            .arg(&clip.path)
            .args([
                "-t",
                &format!("{seg_dur:.6}"),
                "-f",
                "f32le",
                "-ac",
                "2",
                "-ar",
                &sample_rate.to_string(),
            ])
            .arg(&raw_path)
            .status()
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
            .status()
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
        clip.source_in_secs = 0.0;
        clip.source_out_secs = samples.len() as f64 / 2.0 / sample_rate as f64;
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

pub fn timeline_duration(project: &Project) -> f64 {
    project
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(|c| c.end_secs())
        .fold(0.0_f64, f64::max)
}

/// Mix timeline audio in `[start_secs, start_secs + duration_secs)` to a WAV file,
/// shifted so the file's own t=0 is `start_secs`. Returns `false` (and writes nothing) if
/// no enabled audio clip overlaps the range. File-backed rather than in-memory so the
/// playback engine can pre-mix ranges spanning minutes, not just short scrub segments.
pub fn mix_timeline_audio_range_to_file(
    project: &Project,
    start_secs: f64,
    duration_secs: f64,
    out_path: &Path,
) -> Result<bool, ExportError> {
    if duration_secs <= 0.0 {
        return Ok(false);
    }
    if !crate::media::ffmpeg_available() {
        return Err(FfmpegCliError::NotFound.into());
    }

    let end_secs = start_secs + duration_secs;
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
            let clip_len = a.timeline_duration_secs();
            let clip_end = a.position_secs + clip_len;
            let overlap_start = start_secs.max(a.position_secs);
            let overlap_end = end_secs.min(clip_end);
            if overlap_end <= overlap_start {
                continue;
            }
            let offset = (overlap_start - a.position_secs) * a.speed_factor();
            let overlap_src = (overlap_end - overlap_start) * a.speed_factor();
            shifted.push(AudioMixClip {
                path: media.path.clone(),
                position_secs: overlap_start - start_secs,
                source_in_secs: a.source_in_secs + offset,
                source_out_secs: a.source_in_secs + offset + overlap_src,
                gain_db: evaluate_volume_db(a, overlap_start),
                fade_in_secs: a.fade_in_secs,
                fade_out_secs: a.fade_out_secs,
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
        duration_secs,
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
    start_secs: f64,
    duration_secs: f64,
) -> Result<Vec<u8>, ExportError> {
    let temp_dir = std::env::temp_dir().join(format!("renderly-scrub-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(FfmpegCliError::Io)?;
    let temp_wav = temp_dir.join("segment.wav");

    let wrote = mix_timeline_audio_range_to_file(project, start_secs, duration_secs, &temp_wav)?;
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
            .status()
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
            duration_secs: Some(1.0),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 0.5,
                gain_db: 0.0,
                enabled: true,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
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
            duration_secs: Some(1.0),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 0.5,
                gain_db: 0.0,
                enabled: true,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
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
                position_secs: 1.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
                gain_db: 0.0,
                enabled: true,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
                ..Default::default()
            }));
        assert!((timeline_duration(&project) - 3.0).abs() < 1e-9);
    }

    #[test]
    fn mix_timeline_audio_segment_empty_without_audio() {
        let project = Project::new("t", Settings::default());
        let bytes = mix_timeline_audio_segment(&project, 0.0, 0.5).unwrap();
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
            duration_secs: Some(5.0),
            width: None,
            height: None,
            fps: None,
        });
        let mut track = crate::project::Track::new(TrackKind::Audio, "A1");
        track.muted = true;
        track.clips.push(Clip::Audio(crate::project::MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            position_secs: 0.0,
            source_in_secs: 0.0,
            source_out_secs: 5.0,
            gain_db: 0.0,
            enabled: true,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            ..Default::default()
        }));
        project.tracks.push(track);

        assert!(collect_audio_clips(&project).is_empty());
    }

    #[test]
    fn active_layers_and_has_video_content_skip_hidden_tracks() {
        let mut project = Project::new("t", Settings::default());
        let media_id = uuid::Uuid::new_v4();
        project.media.push(crate::project::MediaItem {
            id: media_id,
            path: "clip.mp4".into(),
            kind: MediaKind::Video,
            duration_secs: Some(5.0),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
        });
        let mut track = crate::project::Track::new(TrackKind::Video, "V1");
        track.hidden = true;
        track.clips.push(Clip::Video(crate::project::MediaClip {
            id: uuid::Uuid::new_v4(),
            media_id,
            position_secs: 0.0,
            source_in_secs: 0.0,
            source_out_secs: 5.0,
            gain_db: 0.0,
            enabled: true,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            ..Default::default()
        }));
        project.tracks.push(track);

        assert!(!has_video_content(&project));
        assert!(eval::active_layers(&project, 0.0).unwrap().is_empty());
    }

    #[test]
    fn mix_timeline_audio_range_to_file_returns_false_without_audio() {
        let project = Project::new("t", Settings::default());
        let dir = std::env::temp_dir().join(format!("renderly-mixtest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.wav");
        let wrote = mix_timeline_audio_range_to_file(&project, 0.0, 0.5, &out).unwrap();
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
            duration_secs: Some(1.0),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 1.0,
                gain_db: 0.0,
                enabled: true,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
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
        let frame_a = renderer.render(&project, 0.0).expect("render frame 0");
        let frame_b = renderer
            .render(&project, 1.0 / 30.0)
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
            duration_secs: Some(1.0),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
        });
        let track = crate::project::Track::new(TrackKind::Video, "V1");
        project.tracks.push(track);
        project.tracks[0]
            .clips
            .push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 1.0,
                gain_db: 0.0,
                enabled: true,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
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
            .render_timed(&project, 0.0)
            .expect("render_timed");

        let mut plain_renderer =
            FrameRenderer::new(settings, DecodeOptions::default()).expect("new");
        let plain_pixels = plain_renderer.render(&project, 0.0).expect("render");

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
            duration_secs: Some(1.0),
            width: Some(320),
            height: Some(240),
            fps: Some(30.0),
        });

        // Two video tracks both showing the SAME media at the same time — a minimal PIP
        // setup. Before the fix, both layers shared one decoder keyed by media_id and
        // fought over that single ffmpeg process's playback position.
        for _ in 0..2 {
            let mut track = crate::project::Track::new(TrackKind::Video, "V");
            track.clips.push(Clip::Video(crate::project::MediaClip {
                id: uuid::Uuid::new_v4(),
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 1.0,
                gain_db: 0.0,
                enabled: true,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
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
        renderer.render(&project, 0.0).expect("render");

        assert_eq!(
            renderer.decoders.len(),
            2,
            "each PIP layer's track should get its own decoder, not share one keyed by media_id"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
