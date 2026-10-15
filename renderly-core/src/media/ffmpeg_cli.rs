//! Invoke `ffmpeg` / `ffprobe` as subprocesses. Phase 0 uses the user's installed FFmpeg
//! binaries (no link-time dependency on libav); linked decode/encode via `ffmpeg-the-third`
//! lands once vcpkg/FFMPEG_DIR is wired up for all dev/CI environments.

use crate::proc::SerialSpawn;
use std::io::{BufReader, Read, Write};

use crate::project::TrackAudioRole;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum FfmpegCliError {
    #[error("ffmpeg/ffprobe not found on PATH; install FFmpeg to use media I/O")]
    NotFound,
    #[error("failed to run {tool}: {message}")]
    SpawnFailed { tool: &'static str, message: String },
    #[error("ffmpeg exited with status {0}")]
    NonZeroExit(i32),
    #[error("unexpected ffmpeg output: {0}")]
    BadOutput(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

static FFMPEG: OnceLock<PathBuf> = OnceLock::new();
static FFPROBE: OnceLock<PathBuf> = OnceLock::new();

fn resolve_tool(name: &str, cache: &OnceLock<PathBuf>) -> Result<PathBuf, FfmpegCliError> {
    if let Some(path) = cache.get() {
        return Ok(path.clone());
    }
    let found = which_tool(name).ok_or(FfmpegCliError::NotFound)?;
    let _ = cache.set(found.clone());
    Ok(found)
}

fn which_tool(name: &str) -> Option<PathBuf> {
    let with_exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    // A bundled build ships ffmpeg/ffprobe beside the app and points `RENDERLY_FFMPEG_DIR`
    // at them (see the desktop app's startup), so a user needs nothing on PATH. Checked
    // first; PATH is the fallback for dev and for CLI/MCP use outside the packaged app.
    if let Some(dir) = std::env::var_os("RENDERLY_FFMPEG_DIR") {
        let candidate = PathBuf::from(dir).join(&with_exe);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(&with_exe);
            candidate.is_file().then_some(candidate)
        })
    })
}

/// Keep ffmpeg's noisy-but-benign decoder chatter (e.g. H.264 `mmco: unref short failure`
/// on keyframe seeks) off the inherited console. Callers that need stderr for parsing
/// (silencedetect, etc.) must not use this — they should `.output()` / pipe instead.
fn silence_stderr(cmd: &mut Command) {
    cmd.stderr(Stdio::null());
}

pub fn ffmpeg_path() -> Result<PathBuf, FfmpegCliError> {
    resolve_tool("ffmpeg", &FFMPEG)
}

pub fn ffprobe_path() -> Result<PathBuf, FfmpegCliError> {
    resolve_tool("ffprobe", &FFPROBE)
}

/// Returns true when both `ffmpeg` and `ffprobe` are discoverable on PATH.
pub fn is_available() -> bool {
    ffmpeg_path().is_ok() && ffprobe_path().is_ok()
}

#[derive(Debug, Clone)]
pub struct ProbedVideo {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration_secs: f64,
}

pub fn probe_video(path: &Path) -> Result<ProbedVideo, FfmpegCliError> {
    let output = crate::proc::command(ffprobe_path()?)
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,r_frame_rate,duration",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
        ])
        .arg(path)
        .output_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffprobe",
            message: e.to_string(),
        })?;

    if !output.status.success() {
        return Err(FfmpegCliError::NonZeroExit(
            output.status.code().unwrap_or(-1),
        ));
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| FfmpegCliError::BadOutput(e.to_string()))?;

    let stream = json
        .get("streams")
        .and_then(|s| s.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| FfmpegCliError::BadOutput("no video stream".into()))?;

    let width = stream
        .get("width")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| FfmpegCliError::BadOutput("missing width".into()))? as u32;
    let height = stream
        .get("height")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| FfmpegCliError::BadOutput("missing height".into()))? as u32;

    let fps = stream
        .get("r_frame_rate")
        .and_then(|v| v.as_str())
        .map(parse_rational)
        .transpose()?
        .unwrap_or(30.0);

    let duration_secs = stream
        .get("duration")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .or_else(|| {
            json.get("format")
                .and_then(|f| f.get("duration"))
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
        })
        .ok_or_else(|| FfmpegCliError::BadOutput("missing duration".into()))?;

    Ok(ProbedVideo {
        width,
        height,
        fps,
        duration_secs,
    })
}

/// True if `path` has at least one audio stream (cheap ffprobe). Used to skip waveform
/// generation for silent videos instead of letting ffmpeg fail with a cryptic exit code.
pub fn has_audio_stream(path: &Path) -> Result<bool, FfmpegCliError> {
    let output = crate::proc::command(ffprobe_path()?)
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_type",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffprobe",
            message: e.to_string(),
        })?;

    // ffprobe returns success with empty stdout when no matching stream exists.
    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .contains("audio"))
}

fn parse_rational(s: &str) -> Result<f64, FfmpegCliError> {
    if let Some((num, den)) = s.split_once('/') {
        let num: f64 = num
            .parse()
            .map_err(|e: std::num::ParseFloatError| FfmpegCliError::BadOutput(e.to_string()))?;
        let den: f64 = den
            .parse()
            .map_err(|e: std::num::ParseFloatError| FfmpegCliError::BadOutput(e.to_string()))?;
        if den == 0.0 {
            return Err(FfmpegCliError::BadOutput(format!(
                "zero denominator in {s}"
            )));
        }
        Ok(num / den)
    } else {
        s.parse::<f64>()
            .map_err(|e: std::num::ParseFloatError| FfmpegCliError::BadOutput(e.to_string()))
    }
}

/// Continuously drains a child process's stderr pipe on a background thread for the life
/// of the process, discarding the bytes. `CliReader`/`VideoEncoder` pipe stderr *and*
/// separately block synchronously reading stdout / writing stdin — an unread stderr pipe
/// fills its OS buffer once ffmpeg writes enough (a handful of KB of warnings is plenty),
/// at which point ffmpeg blocks trying to write more, while we're blocked reading/writing
/// the other pipe, deadlocking the whole decode/encode. Draining it independently makes
/// that structurally impossible.
fn drain_stderr(stderr: std::process::ChildStderr) {
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = BufReader::new(stderr).read_to_end(&mut sink);
    });
}

pub use crate::frame::RgbaFrame;

/// Decode-time scaling/pacing knobs for `CliReader`. Used by the playback engine to
/// decode at panel resolution (instead of full source resolution) and at the project's
/// output fps (instead of the source's native fps), rather than downscaling/retiming
/// full-resolution frames after the fact.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReaderOptions {
    /// Decode scaled so the output height matches this (even-rounded); width follows the
    /// source aspect ratio (also even-rounded). `None` or `>=` source height keeps native
    /// resolution.
    pub target_height: Option<u32>,
    /// Force this output frame rate (ffmpeg duplicates/drops frames to match). `None`
    /// keeps the source's native frame rate.
    pub output_fps: Option<f64>,
}

/// Round to the nearest integer, then align *up* to the next even number — mirrors
/// ffmpeg's `FFALIGN(lrint(x), 2)`, which is what libavfilter's `scale` filter actually
/// uses to resolve a `-2` dimension. Our previous implementation rounded down (integer
/// division truncation) instead of up, which mismatched ffmpeg's real output dimensions
/// and desynced the raw RGBA byte stream from our declared frame size.
fn ffalign_even(x: f64) -> u32 {
    let rounded = x.round() as i64;
    (((rounded + 1) & !1).max(2)) as u32
}

/// Scaled (width, height) for `target_height`, matching ffmpeg's `scale=-2:h` behavior:
/// height is even-aligned, width follows source aspect ratio and is also even-aligned
/// (both rounded up, not down — see `ffalign_even`).
pub(super) fn scaled_dimensions(
    src_width: u32,
    src_height: u32,
    target_height: Option<u32>,
) -> (u32, u32) {
    match target_height {
        Some(h) if h > 0 && h < src_height => {
            let height = ffalign_even(h as f64);
            let width_f = src_width as f64 * height as f64 / src_height as f64;
            let width = ffalign_even(width_f);
            (width, height)
        }
        _ => (src_width, src_height),
    }
}

/// Tiled thumbnail strip: one PNG containing `cols × rows` evenly time-sampled frames,
/// laid out left-to-right, top-to-bottom.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThumbnailStrip {
    pub cols: u32,
    pub rows: u32,
    pub tile_width: u32,
    pub tile_height: u32,
    pub tile_count: u32,
    pub interval_secs: f64,
}

/// Generate a tiled thumbnail-strip PNG for `path`, sampling roughly one frame every 2
/// seconds of source duration (capped to `max_tiles`, and always at least one).
///
/// Uses sparse **input seeks** (`-ss` before `-i`) rather than an `fps=` filter over the
/// whole file — decoding every frame of a long clip is the dominant cost of the old path.
/// Seeks land on nearby keyframes (fine for a filmstrip) and finish in roughly
/// `O(tile_count)` keyframe decodes instead of `O(duration)`.
pub fn generate_thumbnail_strip(
    path: &Path,
    out_path: &Path,
    max_tiles: u32,
    tile_height: u32,
) -> Result<ThumbnailStrip, FfmpegCliError> {
    let probed = probe_video(path)?;
    let duration = probed.duration_secs.max(0.1);

    let ideal = (duration / 2.0).ceil() as u32;
    let tile_count = ideal.clamp(1, max_tiles.max(1));
    let interval_secs = duration / tile_count as f64;

    let cols = (tile_count as f64).sqrt().ceil().max(1.0) as u32;
    let rows = tile_count.div_ceil(cols);

    let (width, height) = scaled_dimensions(probed.width, probed.height, Some(tile_height));

    let tmp = std::env::temp_dir().join(format!("renderly-strip-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&tmp).map_err(FfmpegCliError::Io)?;

    let ffmpeg = ffmpeg_path()?;
    const MAX_PARALLEL: usize = 4;

    // Grab frames in parallel batches. Seek-before-input lands on nearby keyframes
    // (fine for a filmstrip) and avoids decoding the whole file.
    for chunk_start in (0..tile_count).step_by(MAX_PARALLEL) {
        let chunk_end = (chunk_start + MAX_PARALLEL as u32).min(tile_count);
        let extract_result = std::thread::scope(|s| {
            let handles: Vec<_> = (chunk_start..chunk_end)
                .map(|i| {
                    let frame_path = tmp.join(format!("f{i:03}.png"));
                    let t = ((i as f64 + 0.5) * interval_secs)
                        .min((duration - 0.05).max(0.0))
                        .max(0.0);
                    let ffmpeg = &ffmpeg;
                    s.spawn(move || {
                        // Keyframe seeks into H.264 often emit `mmco: unref short failure`
                        // at loglevel=error even when the extracted frame is fine — silence
                        // stderr so import doesn't spam the app console.
                        let mut cmd = crate::proc::command(ffmpeg);
                        cmd.args([
                            "-hide_banner",
                            "-loglevel",
                            "fatal",
                            "-ss",
                            &format!("{t:.3}"),
                            "-i",
                        ])
                        .arg(path)
                        .args([
                            "-an",
                            "-sn",
                            "-frames:v",
                            "1",
                            "-vf",
                            &format!("scale=-2:{height}:flags=fast_bilinear"),
                            "-threads",
                            "1",
                            "-y",
                        ])
                        .arg(&frame_path);
                        silence_stderr(&mut cmd);
                        let status =
                            cmd.status_serial()
                                .map_err(|e| FfmpegCliError::SpawnFailed {
                                    tool: "ffmpeg",
                                    message: e.to_string(),
                                })?;
                        if !status.success() {
                            return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)));
                        }
                        if !frame_path.is_file() {
                            return Err(FfmpegCliError::BadOutput(format!(
                                "seek extract produced no file at t={t:.3}"
                            )));
                        }
                        Ok(())
                    })
                })
                .collect();
            for h in handles {
                h.join().unwrap_or_else(|_| {
                    Err(FfmpegCliError::BadOutput("frame worker panicked".into()))
                })?;
            }
            Ok::<(), FfmpegCliError>(())
        });
        if let Err(e) = extract_result {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    }

    // Assemble the grid from the numbered frame sequence.
    let mut tile_cmd = crate::proc::command(&ffmpeg);
    tile_cmd
        .args([
            "-hide_banner",
            "-loglevel",
            "fatal",
            "-y",
            "-framerate",
            "1",
            "-i",
        ])
        .arg(tmp.join("f%03d.png"))
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("tile={cols}x{rows}"),
            "-an",
        ])
        .arg(out_path);
    silence_stderr(&mut tile_cmd);
    let status = tile_cmd
        .status_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;

    let _ = std::fs::remove_dir_all(&tmp);

    if !status.success() {
        return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)));
    }

    Ok(ThumbnailStrip {
        cols,
        rows,
        tile_width: width,
        tile_height: height,
        tile_count,
        interval_secs,
    })
}

/// Hardware decode backends to probe for, in priority order. Vendor-specific backends
/// (`cuda`, `qsv`) come first only because they're typically faster when their GPU is
/// actually present; `d3d11va` works across GPU vendors on Windows via Direct3D 11 and is
/// the broad fallback before giving up on a *named* backend.
const HWACCEL_CANDIDATES: &[&str] = &["cuda", "qsv", "d3d11va"];

static COMPILED_HWACCELS: OnceLock<Vec<String>> = OnceLock::new();
/// Whether a probed hwaccel actually produced a usable frame, cached for the rest of the
/// process after the first `CliReader::open_with` call — see its doc comment.
static HWACCEL_WORKS: OnceLock<bool> = OnceLock::new();

/// Hardware accel names this `ffmpeg` binary was compiled with (`ffmpeg -hwaccels`).
/// Compile-time support only — doesn't mean a usable GPU/driver is actually present;
/// `open_with`'s warm-up read is what actually verifies a candidate works at runtime.
fn compiled_hwaccels() -> &'static [String] {
    COMPILED_HWACCELS.get_or_init(|| {
        let Ok(ffmpeg) = ffmpeg_path() else {
            return Vec::new();
        };
        let Ok(output) = crate::proc::command(ffmpeg)
            .args(["-hide_banner", "-hwaccels"])
            .output_serial()
        else {
            return Vec::new();
        };
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .skip(1) // "Hardware acceleration methods:" header line.
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    })
}

/// First `HWACCEL_CANDIDATES` entry this ffmpeg was compiled with, if any.
///
/// Returns `None` (software decode) when no named candidate is present — we deliberately
/// do **not** fall back to `-hwaccel auto`. On Linux CI and many desktop installs, `auto`
/// can resolve to VAAPI (or similar) with a download/color path that drifts from our
/// explicit BT.709 swscale chain, which would break preview↔software parity.
fn preferred_hwaccel() -> Option<&'static str> {
    let compiled = compiled_hwaccels();
    HWACCEL_CANDIDATES
        .iter()
        .find(|c| compiled.iter().any(|line| line == *c))
        .copied()
}

/// Sequential RGBA frame reader backed by a long-lived `ffmpeg` decode pipe. Wrapped by the
/// public `media::VideoReader` (see `media/backend.rs`); not exposed directly.
pub(super) struct CliReader {
    child: Child,
    stdout: BufReader<ChildStdout>,
    width: u32,
    height: u32,
    frame_bytes: usize,
    /// A3: the warm-up frame `open_with` already decoded to confirm a hwaccel attempt
    /// actually works, handed back on the caller's first `read_frame()` instead of being
    /// discarded and re-decoded.
    pending_first_frame: Option<RgbaFrame>,
    /// Set while this reader decodes with `-hwaccel` and has not produced a frame yet: what
    /// a software retry needs if the hardware decoder exits without one (see `read_frame`).
    hw_retry: Option<HwRetry>,
}

/// The arguments of a hardware-decode spawn, kept so it can be redone in software.
struct HwRetry {
    path: std::path::PathBuf,
    start_secs: f64,
    opts: ReaderOptions,
}

type ChildStdout = std::process::ChildStdout;

impl CliReader {
    /// A3: hardware decode via `-hwaccel`, probed once per process and cached.
    ///
    /// The *first* `CliReader` opened in the process's lifetime pays the cost of
    /// actually trying hardware decode: spawn ffmpeg with the best compiled-in *named*
    /// candidate (`cuda` / `qsv` / `d3d11va`), decode one real frame as a warm-up/health
    /// check, and remember the outcome in `HWACCEL_WORKS`. Every subsequent open in this
    /// process just replays that cached decision instead of re-probing — hwaccel failures
    /// are systemic (missing driver/codec support), not per-file, so re-attempting per
    /// open would only add latency to every single decoder spawn (every seek, scrub, and
    /// clip change) for no benefit.
    ///
    /// If no named candidate is compiled in, or the cached/attempted hwaccel run fails to
    /// produce a first frame, this falls back to a fresh software-only spawn transparently
    /// — callers never see the difference beyond decode running on the CPU instead of the
    /// GPU. We never pass `-hwaccel auto` (see [`preferred_hwaccel`]).
    pub fn open_with(
        path: &Path,
        start_secs: f64,
        opts: &ReaderOptions,
    ) -> Result<Self, FfmpegCliError> {
        match HWACCEL_WORKS.get().copied() {
            // Already confirmed working earlier this process — use it directly, no
            // per-open warm-up/verification cost.
            Some(true) => {
                return Self::spawn(path, start_secs, opts, preferred_hwaccel());
            }
            // Already confirmed broken earlier this process — don't pay to re-try it.
            Some(false) => return Self::spawn(path, start_secs, opts, None),
            // First open this process: probe it once, verified by an actual decode.
            None => {}
        }

        let Some(accel) = preferred_hwaccel() else {
            // No named backend compiled in — stay on software; do not try `-hwaccel auto`.
            let _ = HWACCEL_WORKS.set(false);
            return Self::spawn(path, start_secs, opts, None);
        };

        match Self::spawn(path, start_secs, opts, Some(accel)) {
            Ok(mut reader) => match reader.read_frame_from_pipe() {
                Ok(Some(frame)) => {
                    let _ = HWACCEL_WORKS.set(true);
                    reader.pending_first_frame = Some(frame);
                    return Ok(reader);
                }
                _ => {
                    let _ = HWACCEL_WORKS.set(false);
                    // `reader` drops here — its `Drop` impl kills the failed child.
                }
            },
            Err(_) => {
                let _ = HWACCEL_WORKS.set(false);
            }
        }
        Self::spawn(path, start_secs, opts, None)
    }

    fn spawn(
        path: &Path,
        start_secs: f64,
        opts: &ReaderOptions,
        hwaccel: Option<&str>,
    ) -> Result<Self, FfmpegCliError> {
        let probed = probe_video(path)?;
        let (width, height) = scaled_dimensions(probed.width, probed.height, opts.target_height);

        let mut cmd = crate::proc::command(ffmpeg_path()?);
        cmd.args([
            "-hide_banner",
            // `error` still prints H.264 `mmco: unref short failure` on many game
            // captures after a keyframe seek — those are benign. `fatal` keeps real
            // process failures while stderr is drained to avoid console spam / pipe
            // deadlock (see `drain_stderr`).
            "-loglevel",
            "fatal",
        ]);
        // Must precede `-i` — ffmpeg only applies `-hwaccel` to inputs opened after it.
        if let Some(accel) = hwaccel {
            cmd.args(["-hwaccel", accel]);
        }
        cmd.args(["-ss", &format!("{start_secs:.6}"), "-i"])
            .arg(path);
        if let Some(fps) = opts.output_fps {
            cmd.args(["-r", &format!("{fps:.6}")]);
        }
        // Always run through `scale` so swscale applies an explicit BT.709 limited→full
        // RGB conversion. Auto matrix/range often mis-guesses gameplay captures and
        // yields flat, washed, or oversaturated colours relative to desktop players.
        let scale = if width != probed.width || height != probed.height {
            format!("scale=-2:{height}")
        } else {
            format!("scale={width}:{height}")
        };
        let vf = format!(
            "{scale}:flags=bicubic+accurate_rnd+full_chroma_int:\
             in_color_matrix=bt709:out_color_matrix=bt709:\
             in_range=tv:out_range=pc,format=rgba"
        );
        cmd.args(["-vf", &vf])
            .args(["-an", "-f", "rawvideo", "-pix_fmt", "rgba", "pipe:1"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn_serial()
            .map_err(|e| FfmpegCliError::SpawnFailed {
                tool: "ffmpeg",
                message: e.to_string(),
            })?;

        if let Some(stderr) = child.stderr.take() {
            drain_stderr(stderr);
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| FfmpegCliError::BadOutput("no stdout".into()))?;
        let frame_bytes = (width * height * 4) as usize;

        Ok(Self {
            child,
            stdout: BufReader::new(stdout),
            width,
            height,
            frame_bytes,
            pending_first_frame: None,
            hw_retry: hwaccel.map(|_| HwRetry {
                path: path.to_path_buf(),
                start_secs,
                opts: *opts,
            }),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn read_frame(&mut self) -> Result<Option<RgbaFrame>, FfmpegCliError> {
        if let Some(frame) = self.pending_first_frame.take() {
            self.hw_retry = None;
            return Ok(Some(frame));
        }
        let frame = self.read_frame_from_pipe()?;
        if frame.is_some() {
            self.hw_retry = None;
            return Ok(frame);
        }
        // A hardware decoder that ends before its first frame did not reach end of media:
        // it failed to start (a busy or exhausted GPU does this even after the per-process
        // probe succeeded). Reporting end-of-stream here made export silently drop the
        // layer, so redo this open in software instead.
        if let Some(retry) = self.hw_retry.take() {
            *self = Self::spawn(&retry.path, retry.start_secs, &retry.opts, None)?;
            return self.read_frame_from_pipe();
        }
        Ok(None)
    }

    fn read_frame_from_pipe(&mut self) -> Result<Option<RgbaFrame>, FfmpegCliError> {
        let mut buf = vec![0u8; self.frame_bytes];
        match self.stdout.read_exact(&mut buf) {
            Ok(()) => Ok(Some(RgbaFrame {
                width: self.width,
                height: self.height,
                pixels: buf,
            })),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for CliReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A8: which H.264 encoder to try for export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VideoEncoderPreference {
    /// Probe NVENC/QSV at runtime (one-frame encode), then fall back to libx264.
    #[default]
    Auto,
    /// Force software H.264 (`libx264`).
    Software,
    /// Prefer `h264_nvenc` (falls back to software if unavailable or probe fails).
    Nvenc,
    /// Prefer `h264_qsv` (falls back to software if unavailable or probe fails).
    Qsv,
}

/// Parameters for [`VideoEncoder::open`].
#[derive(Debug, Clone, Copy)]
pub struct VideoEncodeConfig {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub preference: VideoEncoderPreference,
    pub crf: u8,
}

static COMPILED_ENCODERS: OnceLock<Vec<String>> = OnceLock::new();
/// Whether a one-frame probe of `h264_nvenc` actually produced a usable MP4. Compile-time
/// listing (`ffmpeg -encoders`) is not enough — BtbN/static builds often advertise NVENC
/// even on GPU-less CI runners, and picking it then yields BrokenPipe mid-write.
static NVENC_WORKS: OnceLock<bool> = OnceLock::new();
/// Same as [`NVENC_WORKS`] for `h264_qsv`.
static QSV_WORKS: OnceLock<bool> = OnceLock::new();

fn compiled_encoders() -> &'static [String] {
    COMPILED_ENCODERS.get_or_init(|| {
        let Ok(ffmpeg) = ffmpeg_path() else {
            return Vec::new();
        };
        let Ok(output) = crate::proc::command(ffmpeg)
            .args(["-hide_banner", "-encoders"])
            .output_serial()
        else {
            return Vec::new();
        };
        // Lines look like: " V..... h264_nvenc           NVIDIA NVENC H.264 encoder"
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let first = line.chars().next()?;
                if !matches!(first, 'V' | 'A' | 'S') {
                    return None;
                }
                // Six-char capability flags, then whitespace, then encoder name.
                let rest = line.get(7..)?.trim();
                rest.split_whitespace().next().map(str::to_string)
            })
            .collect()
    })
}

fn encoder_available(name: &str) -> bool {
    compiled_encoders().iter().any(|e| e == name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResolvedVideoCodec {
    Libx264,
    H264Nvenc,
    H264Qsv,
}

/// Encode a single 16×16 black frame with `codec` to verify the hardware path works.
/// Cached per process via [`NVENC_WORKS`] / [`QSV_WORKS`] — failures are systemic
/// (missing GPU/driver), not per-export.
fn probe_hw_encoder(codec: ResolvedVideoCodec) -> bool {
    let path = std::env::temp_dir().join(format!(
        "renderly-enc-probe-{}-{}.mp4",
        std::process::id(),
        match codec {
            ResolvedVideoCodec::H264Nvenc => "nvenc",
            ResolvedVideoCodec::H264Qsv => "qsv",
            ResolvedVideoCodec::Libx264 => return true,
        }
    ));
    let config = VideoEncodeConfig {
        width: 16,
        height: 16,
        fps: 30.0,
        // Unused by `spawn` — codec is passed explicitly.
        preference: VideoEncoderPreference::Software,
        crf: 23,
    };
    let Ok(mut enc) = VideoEncoder::spawn(&path, &config, codec) else {
        let _ = std::fs::remove_file(&path);
        return false;
    };
    let black = vec![0u8; 16 * 16 * 4];
    if enc.write_frame(&black).is_err() {
        let _ = std::fs::remove_file(&path);
        return false;
    }
    let ok = enc.finish().is_ok();
    let _ = std::fs::remove_file(&path);
    ok
}

fn hw_encoder_works(codec: ResolvedVideoCodec) -> bool {
    match codec {
        ResolvedVideoCodec::Libx264 => true,
        ResolvedVideoCodec::H264Nvenc => *NVENC_WORKS.get_or_init(|| probe_hw_encoder(codec)),
        ResolvedVideoCodec::H264Qsv => *QSV_WORKS.get_or_init(|| probe_hw_encoder(codec)),
    }
}

/// Pick a codec that is both compiled in *and* verified to work at runtime.
fn resolve_video_encoder(pref: VideoEncoderPreference) -> ResolvedVideoCodec {
    match pref {
        VideoEncoderPreference::Software => ResolvedVideoCodec::Libx264,
        VideoEncoderPreference::Nvenc => {
            if encoder_available("h264_nvenc") && hw_encoder_works(ResolvedVideoCodec::H264Nvenc) {
                ResolvedVideoCodec::H264Nvenc
            } else {
                ResolvedVideoCodec::Libx264
            }
        }
        VideoEncoderPreference::Qsv => {
            if encoder_available("h264_qsv") && hw_encoder_works(ResolvedVideoCodec::H264Qsv) {
                ResolvedVideoCodec::H264Qsv
            } else {
                ResolvedVideoCodec::Libx264
            }
        }
        VideoEncoderPreference::Auto => {
            if encoder_available("h264_nvenc") && hw_encoder_works(ResolvedVideoCodec::H264Nvenc) {
                ResolvedVideoCodec::H264Nvenc
            } else if encoder_available("h264_qsv") && hw_encoder_works(ResolvedVideoCodec::H264Qsv)
            {
                ResolvedVideoCodec::H264Qsv
            } else {
                ResolvedVideoCodec::Libx264
            }
        }
    }
}

fn video_encoder_args(codec: &ResolvedVideoCodec, crf: u8) -> Vec<String> {
    match codec {
        ResolvedVideoCodec::Libx264 => vec![
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "medium".into(),
            "-crf".into(),
            crf.to_string(),
        ],
        ResolvedVideoCodec::H264Nvenc => {
            // Map CRF (~18 default) into NVENC CQ roughly 0–51.
            let cq = crf.clamp(0, 51);
            vec![
                "-c:v".into(),
                "h264_nvenc".into(),
                "-preset".into(),
                "p4".into(),
                "-rc".into(),
                "vbr".into(),
                "-cq".into(),
                cq.to_string(),
                "-b:v".into(),
                "0".into(),
            ]
        }
        ResolvedVideoCodec::H264Qsv => {
            let global_quality = crf.clamp(1, 51);
            vec![
                "-c:v".into(),
                "h264_qsv".into(),
                "-global_quality".into(),
                global_quality.to_string(),
            ]
        }
    }
}

/// H.264 MP4 encoder that accepts raw RGBA frames on stdin.
pub struct VideoEncoder {
    child: Child,
    frame_bytes: usize,
}

impl VideoEncoder {
    /// Open an encoder, probing NVENC/QSV at runtime on first use when preference is
    /// Auto/Nvenc/Qsv and falling back to libx264 if the hardware path fails.
    pub fn open(output_path: &Path, config: &VideoEncodeConfig) -> Result<Self, FfmpegCliError> {
        let codec = resolve_video_encoder(config.preference);
        Self::spawn(output_path, config, codec)
    }

    fn spawn(
        output_path: &Path,
        config: &VideoEncodeConfig,
        codec: ResolvedVideoCodec,
    ) -> Result<Self, FfmpegCliError> {
        let width = config.width;
        let height = config.height;
        let fps = config.fps;
        let mut args: Vec<String> = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "-s",
            &format!("{width}x{height}"),
            "-r",
            &format!("{fps:.6}"),
            "-i",
            "pipe:0",
            "-an",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();

        args.extend(video_encoder_args(&codec, config.crf));

        args.extend(
            [
                "-pix_fmt",
                "yuv420p",
                // Tag the bitstream so players treat it as BT.709 limited (matches the
                // decode path's explicit matrix/range). Without this, some players guess
                // BT.601 and the export looks differently saturated than the preview.
                "-colorspace",
                "bt709",
                "-color_primaries",
                "bt709",
                "-color_trc",
                "bt709",
                "-color_range",
                "tv",
                "-movflags",
                "+faststart",
            ]
            .iter()
            .map(|s| (*s).to_string()),
        );

        let mut child = crate::proc::command(ffmpeg_path()?)
            .args(&args)
            .arg(output_path)
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn_serial()
            .map_err(|e| FfmpegCliError::SpawnFailed {
                tool: "ffmpeg",
                message: e.to_string(),
            })?;

        if let Some(stderr) = child.stderr.take() {
            drain_stderr(stderr);
        }

        Ok(Self {
            child,
            frame_bytes: (width * height * 4) as usize,
        })
    }

    pub fn write_frame(&mut self, pixels: &[u8]) -> Result<(), FfmpegCliError> {
        if pixels.len() != self.frame_bytes {
            return Err(FfmpegCliError::BadOutput(format!(
                "expected {} bytes, got {}",
                self.frame_bytes,
                pixels.len()
            )));
        }
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| FfmpegCliError::BadOutput("encoder stdin closed".into()))?;
        stdin.write_all(pixels)?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<(), FfmpegCliError> {
        drop(self.child.stdin.take());
        let status = self.child.wait()?;
        if !status.success() {
            return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)));
        }
        Ok(())
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Mux a video-only MP4 with a WAV/MP4 audio track.
pub fn mux_video_audio(
    video_path: &Path,
    audio_path: &Path,
    output_path: &Path,
    audio_bitrate_k: u32,
) -> Result<(), FfmpegCliError> {
    let bitrate = format!("{}k", audio_bitrate_k.max(32));
    let status = crate::proc::command(ffmpeg_path()?)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(video_path)
        .args(["-i"])
        .arg(audio_path)
        .args([
            "-c:v",
            "copy",
            "-c:a",
            "aac",
            "-b:a",
            &bitrate,
            "-shortest",
            "-movflags",
            "+faststart",
        ])
        .arg(output_path)
        .status_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;
    if !status.success() {
        return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// Sidechain ducking applied when voice and music buses are both present.
#[derive(Debug, Clone, Copy)]
pub struct DuckSettings {
    pub duck_db: f64,
}

/// Mix enabled audio clips onto a timeline-length WAV file.
pub fn mix_timeline_audio(
    clips: &[AudioMixClip],
    sample_rate: u32,
    duration_secs: f64,
    output_wav: &Path,
    duck: Option<DuckSettings>,
) -> Result<(), FfmpegCliError> {
    if clips.is_empty() {
        return Err(FfmpegCliError::BadOutput("no audio clips".into()));
    }

    if let Some(duck_cfg) = duck {
        let (voice, music, other) = partition_clips(clips);
        if !voice.is_empty() && !music.is_empty() {
            let temp_dir =
                std::env::temp_dir().join(format!("renderly-audio-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&temp_dir).map_err(FfmpegCliError::Io)?;

            let voice_wav = temp_dir.join("voice.wav");
            let music_wav = temp_dir.join("music.wav");
            let ducked_wav = temp_dir.join("ducked_music.wav");

            let voice_clips: Vec<AudioMixClip> = voice.into_iter().cloned().collect();
            let music_clips: Vec<AudioMixClip> = music.into_iter().cloned().collect();
            // `?` alone would leak `temp_dir` on failure (it's already been created above,
            // unlike the later ffmpeg-status and `mix_clip_bus` calls further down, which
            // already clean up on their own error paths).
            if let Err(e) = mix_clip_bus(&voice_clips, sample_rate, duration_secs, &voice_wav) {
                std::fs::remove_dir_all(&temp_dir).ok();
                return Err(e);
            }
            if let Err(e) = mix_clip_bus(&music_clips, sample_rate, duration_secs, &music_wav) {
                std::fs::remove_dir_all(&temp_dir).ok();
                return Err(e);
            }

            // `sidechaincompress` alone already produces the gain reduction ("ducking")
            // when the voice/dialog sidechain is active; no extra gain stage is applied
            // after it. An earlier version chained `volume={db_to_linear(-duck_db)}` here,
            // which for the default duck_db=-12 computed a +12dB *boost* on the whole
            // compressed signal (right sign flipped: -(-12) = +12) — that boost applied
            // uniformly to both ducked and non-ducked passages, undoing the compressor's
            // reduction during dialogue and amplifying the music beyond its original level
            // the rest of the time. `duck_cfg.duck_db`'s magnitude isn't yet mapped to a
            // precise output dB target (only its sign gates ducking on/off, see
            // `duck_settings` in export/mod.rs) — a follow-up could tune ratio/threshold
            // from it, but leaving gain at unity here is the safe, clearly-correct default.
            let _ = duck_cfg.duck_db;
            let filter =
                "[0:a][1:a]sidechaincompress=threshold=0.02:ratio=8:attack=200:release=1000[out]"
                    .to_string();
            let status = crate::proc::command(ffmpeg_path()?)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
                .arg(&music_wav)
                .args(["-i"])
                .arg(&voice_wav)
                .args(["-filter_complex", &filter, "-map", "[out]"])
                .arg(&ducked_wav)
                .status_serial()
                .map_err(|e| FfmpegCliError::SpawnFailed {
                    tool: "ffmpeg",
                    message: e.to_string(),
                })?;
            if !status.success() {
                std::fs::remove_dir_all(&temp_dir).ok();
                return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)));
            }

            let mut final_clips = Vec::new();
            final_clips.push(AudioMixClip {
                path: voice_wav.clone(),
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: duration_secs,
                gain_db: 0.0,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
                role: None,
                speed: 1.0,
                effects: Vec::new(),
                denoise: None,
            });
            final_clips.push(AudioMixClip {
                path: ducked_wav.clone(),
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: duration_secs,
                gain_db: 0.0,
                fade_in_secs: 0.0,
                fade_out_secs: 0.0,
                role: None,
                speed: 1.0,
                effects: Vec::new(),
                denoise: None,
            });
            for c in other {
                final_clips.push(c.clone());
            }

            let result = mix_clip_bus(&final_clips, sample_rate, duration_secs, output_wav);
            std::fs::remove_dir_all(&temp_dir).ok();
            return result;
        }
    }

    mix_clip_bus(clips, sample_rate, duration_secs, output_wav)
}

fn mix_clip_bus(
    clips: &[AudioMixClip],
    sample_rate: u32,
    duration_secs: f64,
    output_wav: &Path,
) -> Result<(), FfmpegCliError> {
    if clips.is_empty() {
        return Err(FfmpegCliError::BadOutput("no audio clips".into()));
    }

    let temp_dir = std::env::temp_dir().join(format!("renderly-audio-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).map_err(FfmpegCliError::Io)?;

    let mut segment_paths = Vec::new();
    let mut filter_parts = Vec::new();

    for (i, clip) in clips.iter().enumerate() {
        let seg = temp_dir.join(format!("seg_{i}.wav"));
        let seg_duration = clip.source_out_secs - clip.source_in_secs;
        if seg_duration <= 0.0 {
            std::fs::remove_dir_all(&temp_dir).ok();
            return Err(FfmpegCliError::BadOutput(format!(
                "invalid audio clip range: source_out_secs ({}) <= source_in_secs ({})",
                clip.source_out_secs, clip.source_in_secs
            )));
        }
        let af = build_audio_filter(clip, seg_duration);
        // RNNoise denoise is materialised as a cached 48 kHz WAV of the whole source
        // (timeline-aligned with it), substituted for the clip's input; `build_audio_filter`
        // emits no denoise filter for that backend.
        let input_path = match rnnoise_input(clip) {
            Ok(p) => p,
            Err(e) => {
                std::fs::remove_dir_all(&temp_dir).ok();
                return Err(e);
            }
        };

        let status = crate::proc::command(ffmpeg_path()?)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-ss",
                &format!("{:.6}", clip.source_in_secs),
                "-i",
            ])
            .arg(&input_path)
            .args([
                "-t",
                &format!("{seg_duration:.6}"),
                "-af",
                &af,
                "-ar",
                &sample_rate.to_string(),
                "-ac",
                "2",
            ])
            .arg(&seg)
            .status_serial()
            .map_err(|e| FfmpegCliError::SpawnFailed {
                tool: "ffmpeg",
                message: e.to_string(),
            })?;
        if !status.success() {
            std::fs::remove_dir_all(&temp_dir).ok();
            return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)));
        }
        segment_paths.push(seg);

        // `as u64` on a negative float saturates to 0 rather than panicking — relying on
        // that implicitly would silently turn a negative position into "starts at 0"
        // instead of a clearly-intentional clamp. `.max(0.0)` makes the clamp explicit.
        let delay_ms = (clip.position_secs.max(0.0) * 1000.0).round() as u64;
        filter_parts.push(format!("[{i}:a]adelay={delay_ms}|{delay_ms}[a{i}]"));
    }

    let n = clips.len();
    let mix_labels = (0..n).map(|i| format!("[a{i}]")).collect::<String>();
    let filter = format!(
        "{};{}amix=inputs={n}:duration=longest:dropout_transition=0,apad=whole_dur={duration_secs:.6}[out]",
        filter_parts.join(";"),
        mix_labels,
    );

    let mut cmd = crate::proc::command(ffmpeg_path()?);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y"]);
    for seg in &segment_paths {
        cmd.args(["-i"]).arg(seg);
    }
    cmd.args(["-filter_complex", &filter, "-map", "[out]", "-ar"])
        .arg(sample_rate.to_string())
        .args(["-ac", "2"])
        .arg(output_wav);

    let status = cmd
        .status_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;
    std::fs::remove_dir_all(&temp_dir).ok();
    if !status.success() {
        return Err(FfmpegCliError::NonZeroExit(status.code().unwrap_or(-1)));
    }
    Ok(())
}

/// The file the mix should read for `clip`: the cached RNNoise-denoised copy of its source
/// when the clip has an enabled `"rnnoise"` denoise, otherwise the source itself.
fn rnnoise_input(clip: &AudioMixClip) -> Result<PathBuf, FfmpegCliError> {
    use crate::audio::denoise::{denoised_wav_cached, DenoiseError};
    match clip.denoise.as_ref() {
        Some(d) if d.enabled && d.backend == "rnnoise" => {
            denoised_wav_cached(&clip.path, d.strength).map_err(|e| match e {
                DenoiseError::Ffmpeg(e) => e,
                DenoiseError::Io(e) => FfmpegCliError::Io(e),
                other => FfmpegCliError::BadOutput(other.to_string()),
            })
        }
        _ => Ok(clip.path.clone()),
    }
}

fn build_audio_filter(clip: &AudioMixClip, seg_duration: f64) -> String {
    let volume = db_to_linear(clip.gain_db);
    let mut parts = Vec::new();
    if let Some(denoise) = clip.denoise.as_ref() {
        if denoise.enabled && denoise.backend == "afftdn" {
            let nr = (denoise.strength.clamp(0.0, 1.0) * 20.0).max(0.01);
            parts.push(format!("afftdn=nr={nr:.3}"));
        }
    }
    parts.push(format!("volume={volume:.6}"));
    let speed = if clip.speed.is_finite() && clip.speed > 0.0 {
        clip.speed.clamp(0.25, 4.0)
    } else {
        1.0
    };
    // FFmpeg atempo accepts ~0.5..2.0 per filter; chain for out-of-range speeds.
    if (speed - 1.0).abs() > 1e-6 {
        let mut remaining = speed;
        while remaining > 2.0 + 1e-9 {
            parts.push("atempo=2.0".into());
            remaining /= 2.0;
        }
        while remaining < 0.5 - 1e-9 {
            parts.push("atempo=0.5".into());
            remaining /= 0.5;
        }
        parts.push(format!("atempo={remaining:.6}"));
    }
    // Fades are in timeline seconds after tempo change.
    let timeline_dur = seg_duration / speed;
    if clip.fade_in_secs > 0.0 {
        parts.push(format!("afade=t=in:st=0:d={:.6}", clip.fade_in_secs));
    }
    if clip.fade_out_secs > 0.0 {
        let st = (timeline_dur - clip.fade_out_secs).max(0.0);
        parts.push(format!(
            "afade=t=out:st={st:.6}:d={:.6}",
            clip.fade_out_secs
        ));
    }
    parts.join(",")
}

fn partition_clips(
    clips: &[AudioMixClip],
) -> (Vec<&AudioMixClip>, Vec<&AudioMixClip>, Vec<&AudioMixClip>) {
    let mut voice = Vec::new();
    let mut music = Vec::new();
    let mut other = Vec::new();
    for c in clips {
        match c.role {
            Some(TrackAudioRole::Voiceover) | Some(TrackAudioRole::Dialog) => voice.push(c),
            Some(TrackAudioRole::Music) => music.push(c),
            _ => other.push(c),
        }
    }
    (voice, music, other)
}

fn db_to_linear(gain_db: f64) -> f64 {
    10_f64.powf(gain_db / 20.0)
}

#[derive(Debug, Clone)]
pub struct AudioMixClip {
    pub path: PathBuf,
    pub position_secs: f64,
    pub source_in_secs: f64,
    pub source_out_secs: f64,
    pub gain_db: f64,
    pub fade_in_secs: f64,
    pub fade_out_secs: f64,
    pub role: Option<TrackAudioRole>,
    /// Timeline playback rate (pitch-preserving via atempo). Default 1.0.
    pub speed: f64,
    /// Optional WASM audio effects applied after decode, before fades/volume in the bus.
    pub effects: Vec<crate::project::EffectInstance>,
    /// Optional Phase 4 audio denoise (`afftdn` filter, or `rnnoise` via a cached WAV)
    /// applied before volume/fades.
    pub denoise: Option<crate::project::AudioDenoise>,
}

#[cfg(test)]
mod scaling_tests {
    use super::scaled_dimensions;

    #[test]
    fn scaled_dimensions_keeps_native_size_when_target_is_none_or_larger() {
        assert_eq!(scaled_dimensions(1920, 1080, None), (1920, 1080));
        assert_eq!(scaled_dimensions(1920, 1080, Some(1080)), (1920, 1080));
        assert_eq!(scaled_dimensions(1920, 1080, Some(2000)), (1920, 1080));
    }

    #[test]
    fn scaled_dimensions_downscales_matching_source_aspect_ratio() {
        // 16:9 source scaled to 720p height should land on the standard 1280x720.
        let (w, h) = scaled_dimensions(1920, 1080, Some(720));
        assert_eq!(h, 720);
        assert_eq!(w, 1280);
    }

    #[test]
    fn scaled_dimensions_always_even() {
        let (w, h) = scaled_dimensions(1081, 721, Some(361));
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
    }

    #[test]
    fn scaled_dimensions_rounds_up_to_even_like_ffmpeg_ffalign() {
        // ffmpeg's `scale=-2:h` resolves a `-2` dimension via FFALIGN(lrint(x), 2), which
        // rounds UP to the next even number, not down. An odd target height of 361 must
        // become 362, matching real ffmpeg output exactly (a prior version floored this to
        // 360 via integer-division truncation, desyncing our declared frame size from
        // ffmpeg's actual raw byte stream).
        let (_, h) = scaled_dimensions(1920, 1080, Some(361));
        assert_eq!(h, 362);
    }

    #[test]
    fn scaled_dimensions_width_rounds_up_like_ffmpeg_ffalign() {
        let (w, _) = scaled_dimensions(1921, 1081, Some(721));
        assert_eq!(w, 1284);
    }
}

#[cfg(test)]
mod video_reader_tests {
    use super::{ffmpeg_path, CliReader, ReaderOptions, SerialSpawn};

    fn ffmpeg_available() -> bool {
        super::is_available()
    }

    /// A2: end-to-end guard for the preview-resolution decode path (`playback.rs` now
    /// passes `target_height` into the *continuous* playback decoder, not just paused
    /// scrub). `scaled_dimensions_*` in `scaling_tests` only checks the predicted (w, h)
    /// against the formula on paper; this test decodes a real odd-dimension source
    /// through a live `CliReader` (exactly like the playback loop does) and reads every
    /// frame to EOF. If our predicted `frame_bytes` ever drifted from what ffmpeg's
    /// `scale=-2:h` actually emits, `read_frame` would desync partway through and either
    /// error out or silently yield the wrong frame count — both are caught below.
    #[test]
    fn reads_exact_frame_count_from_odd_dimension_source_at_downscaled_target() {
        if !ffmpeg_available() {
            eprintln!("skipping video reader test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-reader-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");

        let duration_secs = 2u32;
        let rate = 10u32;
        let status = crate::proc::command(ffmpeg_path().unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=duration={duration_secs}:size=322x240:rate={rate}"),
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&video_path)
            .status_serial()
            .expect("ffmpeg encode");
        assert!(status.success());

        let mut reader = CliReader::open_with(
            &video_path,
            0.0,
            &ReaderOptions {
                // Odd, unaligned target on purpose — forces both the height align-up and
                // the `-2` width resolution ffmpeg does internally.
                target_height: Some(111),
                output_fps: Some(rate as f64),
            },
        )
        .expect("open reader");

        assert_eq!(reader.height() % 2, 0, "height must be even-aligned");
        assert_eq!(reader.width() % 2, 0, "width must be even-aligned");
        assert!(reader.height() < 240, "must actually be downscaled");

        let mut frame_count = 0u32;
        while let Some(frame) = reader.read_frame().expect("read_frame should not error") {
            assert_eq!(frame.width, reader.width());
            assert_eq!(frame.height, reader.height());
            assert_eq!(
                frame.pixels.len(),
                (frame.width * frame.height * 4) as usize
            );
            frame_count += 1;
            // A misaligned byte stream would run away well past the real frame count
            // before finally erroring or exhausting the pipe; bail out early with a
            // clear failure rather than looping until some large default timeout.
            assert!(
                frame_count <= duration_secs * rate + 2,
                "read far more frames than the source contains — byte stream is desynced"
            );
        }

        assert_eq!(
            frame_count,
            duration_secs * rate,
            "frame count must exactly match source duration*rate — a mismatch means our \
             predicted frame_bytes disagreed with ffmpeg's actual scale=-2:h output at some \
             point and the raw pipe desynced"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A3: hardware decode must not change decoded pixel content. `CliReader::spawn`
    /// (private, called directly here to bypass the process-wide `HWACCEL_WORKS` cache
    /// and force each path deterministically) runs the exact same `-vf` color-conversion
    /// chain regardless of decode backend, so a software decode and a hwaccel decode of
    /// the same source should agree almost exactly. Skips the comparison (not the whole
    /// test) if no working hwaccel is available on this machine — this is a correctness
    /// guard for when hwaccel *is* available, not a guarantee that it always is.
    #[test]
    fn hardware_decoder_that_yields_no_frame_falls_back_to_software() {
        if !crate::media::ffmpeg_available() {
            eprintln!("skipping hw fallback test: ffmpeg not on PATH");
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("renderly-hwfallback-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("src.mp4");
        let status = crate::proc::command(ffmpeg_path().unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
            ])
            .arg("testsrc=duration=1:size=160x120:rate=10")
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .status_serial()
            .unwrap();
        assert!(status.success());
        // An unknown -hwaccel makes ffmpeg exit before decoding anything — the same shape as
        // a CUDA decoder that fails to start on a busy GPU. Before the fallback this read
        // returned Ok(None) and export silently dropped the layer.
        let mut reader = super::CliReader::spawn(
            &video,
            0.0,
            &ReaderOptions::default(),
            Some("renderly-no-such-hwaccel"),
        )
        .expect("spawn");
        let frame = reader
            .read_frame()
            .expect("read")
            .expect("software fallback frame");
        assert_eq!((frame.width, frame.height), (160, 120));
        assert!(
            reader.read_frame().expect("read").is_some(),
            "keeps decoding after fallback"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hwaccel_decode_matches_software_decode_pixels() {
        if !ffmpeg_available() {
            eprintln!("skipping hwaccel comparison test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-hwaccel-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        let status = crate::proc::command(ffmpeg_path().unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1:size=320x240:rate=5",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&video_path)
            .status_serial()
            .expect("ffmpeg encode");
        assert!(status.success());

        let opts = ReaderOptions::default();
        let mut sw = CliReader::spawn(&video_path, 0.0, &opts, None).expect("software spawn");
        let sw_frame = sw
            .read_frame_from_pipe()
            .expect("software read")
            .expect("software frame");

        let Some(accel) = super::preferred_hwaccel() else {
            eprintln!("skipping: no named hwaccel (cuda/qsv/d3d11va) compiled into this ffmpeg");
            std::fs::remove_dir_all(&dir).ok();
            return;
        };
        let hw_frame = match CliReader::spawn(&video_path, 0.0, &opts, Some(accel)) {
            Ok(mut hw) => match hw.read_frame_from_pipe() {
                Ok(Some(frame)) => frame,
                _ => {
                    eprintln!("skipping: hwaccel '{accel}' unavailable on this machine");
                    std::fs::remove_dir_all(&dir).ok();
                    return;
                }
            },
            Err(_) => {
                eprintln!("skipping: hwaccel '{accel}' spawn failed on this machine");
                std::fs::remove_dir_all(&dir).ok();
                return;
            }
        };

        assert_eq!(sw_frame.width, hw_frame.width);
        assert_eq!(sw_frame.height, hw_frame.height);
        assert_eq!(sw_frame.pixels.len(), hw_frame.pixels.len());
        let max_diff = sw_frame
            .pixels
            .iter()
            .zip(hw_frame.pixels.iter())
            .map(|(a, b)| (*a as i32 - *b as i32).unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 2,
            "hwaccel ('{accel}') decode pixels drifted from software by {max_diff} (>2 per channel)"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod thumbnail_strip_tests {
    use super::{ffmpeg_path, generate_thumbnail_strip, SerialSpawn};

    fn ffmpeg_available() -> bool {
        super::is_available()
    }

    fn generate_test_video(path: &std::path::Path, duration_secs: u32) {
        let status = crate::proc::command(ffmpeg_path().unwrap())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc=duration={duration_secs}:size=320x240:rate=10"),
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
    fn generate_thumbnail_strip_produces_a_grid_covering_the_duration() {
        if !ffmpeg_available() {
            eprintln!("skipping thumbnail strip test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-thumbs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        let out_path = dir.join("strip.png");
        generate_test_video(&video_path, 6);

        let strip = generate_thumbnail_strip(&video_path, &out_path, 10, 90).unwrap();

        assert!(out_path.is_file());
        assert!(std::fs::metadata(&out_path).unwrap().len() > 0);
        // ~1 tile per 2s of a 6s clip.
        assert_eq!(strip.tile_count, 3);
        assert!(strip.cols * strip.rows >= strip.tile_count);
        assert_eq!(strip.tile_height % 2, 0);
        assert_eq!(strip.tile_width % 2, 0);
        assert!((strip.interval_secs - 2.0).abs() < 1e-6);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn generate_thumbnail_strip_caps_tile_count_for_long_videos() {
        if !ffmpeg_available() {
            eprintln!("skipping thumbnail strip cap test: ffmpeg not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-thumbs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let video_path = dir.join("src.mp4");
        let out_path = dir.join("strip.png");
        // A ~1-per-2s rate would want 5 tiles for 10s; cap forces 3.
        generate_test_video(&video_path, 10);

        let strip = generate_thumbnail_strip(&video_path, &out_path, 3, 90).unwrap();
        assert_eq!(strip.tile_count, 3);

        std::fs::remove_dir_all(&dir).ok();
    }
}
