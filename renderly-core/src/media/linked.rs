//! In-process probe through libav, via `ffmpeg-the-third`. Compiled only with the
//! `linked-ffmpeg` feature and used only when `RENDERLY_MEDIA_BACKEND=linked` is set.
//!
//! Probe: mirrors what `ffmpeg_cli::probe_video` and `ffmpeg_cli::has_audio_stream` return
//! (see that file for the ffprobe fields each value comes from).
//!
//! Frame decode (`LinkedReader`): software decode in-process, with the same contract as
//! `ffmpeg_cli::CliReader`. Output pixels are meant to match the CLI's
//! `scale=...:flags=bicubic+accurate_rnd+full_chroma_int:...,format=rgba` chain within 1 LSB.
//! See docs/linked-ffmpeg.md for the measured differences.
//!
//! NOT COMPILED OR TESTED ON THE DEVELOPMENT MACHINE: it has no FFmpeg dev libraries or
//! libclang. Builds and tests run in the informational `linked-ffmpeg` CI job.

use super::ffmpeg_cli::{scaled_dimensions, FfmpegCliError, ProbedVideo, ReaderOptions, RgbaFrame};
use ffmpeg::format::Pixel;
use ffmpeg::software::scaling;
use ffmpeg_the_third as ffmpeg;
use std::collections::VecDeque;
use std::ffi::c_int;
use std::path::Path;
use std::sync::OnceLock;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LinkedError {
    #[error("libav initialisation failed")]
    Init,
    #[error("libav: {0}")]
    Ffmpeg(#[from] ffmpeg::Error),
    #[error("no video stream")]
    NoVideoStream,
    #[error("invalid frame rate {0}/{1}")]
    BadFrameRate(i32, i32),
    #[error("no usable duration")]
    MissingDuration,
    #[error("invalid stream time base")]
    BadTimeBase,
    #[error("colour conversion setup failed")]
    Colorspace,
}

static INIT: OnceLock<bool> = OnceLock::new();

/// `ffmpeg::init` once per process. Its result is cached, so a failure is not retried.
fn initialise() -> Result<(), LinkedError> {
    let ok = *INIT.get_or_init(|| ffmpeg::init().is_ok());
    if ok {
        Ok(())
    } else {
        Err(LinkedError::Init)
    }
}

/// Same fields as `ffmpeg_cli::probe_video`:
/// - width/height: the video decoder parameters (`stream=width,height`);
/// - fps: `r_frame_rate` (`Stream::rate`), as num/den;
/// - duration: the stream duration in stream time-base units, falling back to the container
///   duration in microseconds when the stream value is unknown (ffprobe's `stream=duration`
///   then `format=duration`).
pub fn probe_video(path: &Path) -> Result<ProbedVideo, LinkedError> {
    initialise()?;
    let input = ffmpeg::format::input(path)?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or(LinkedError::NoVideoStream)?;

    let rate = stream.rate();
    let (num, den) = (rate.numerator(), rate.denominator());
    if den == 0 {
        return Err(LinkedError::BadFrameRate(num, den));
    }
    let fps = num as f64 / den as f64;

    let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
        .decoder()
        .video()?;
    let width = decoder.width();
    let height = decoder.height();

    // AV_NOPTS_VALUE is i64::MIN, so `> 0` also rejects "unknown".
    let stream_ticks = stream.duration();
    let time_base = stream.time_base();
    let duration_secs = if stream_ticks > 0 && time_base.denominator() != 0 {
        stream_ticks as f64 * time_base.numerator() as f64 / time_base.denominator() as f64
    } else {
        // Container duration is in AV_TIME_BASE (1 / 1_000_000 s) units.
        let container_us = input.duration();
        if container_us > 0 {
            container_us as f64 / 1_000_000.0
        } else {
            return Err(LinkedError::MissingDuration);
        }
    };

    Ok(ProbedVideo {
        width,
        height,
        fps,
        duration_secs,
    })
}

/// True if the input has an audio stream. Same answer as `ffmpeg_cli::has_audio_stream`.
pub fn has_audio_stream(path: &Path) -> Result<bool, LinkedError> {
    initialise()?;
    let input = ffmpeg::format::input(path)?;
    Ok(input.streams().best(ffmpeg::media::Type::Audio).is_some())
}

/// `SWS_CS_ITU709` from libswscale/swscale.h (value 1). Spelled out here so the code does not
/// depend on the bindings exposing the macro.
const SWS_CS_ITU709: c_int = 1;

/// Scaler for one source format and size, cached on `LinkedReader` and rebuilt if the decoded
/// frame's format or size changes.
struct Converter {
    key: (Pixel, u32, u32),
    scaler: scaling::Context,
}

/// Software frame decode through libav. Same contract as `ffmpeg_cli::CliReader`: `open`
/// seeks to the keyframe at or before `start_secs`, drops frames earlier than
/// `start_secs - half a frame`, and yields RGBA frames at the scaled size from
/// `ffmpeg_cli::scaled_dimensions`.
pub struct LinkedReader {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    stream_index: usize,
    time_base_secs: f64,
    /// `AVFormatContext.start_time` in seconds. `start_secs` is measured from the file's
    /// start, as ffmpeg's `-ss` is, so frame times are shifted by this value.
    input_start_secs: f64,
    start_secs: f64,
    /// Duration of one source frame from the stream's frame rate; 0 when unknown.
    frame_dur: f64,
    width: u32,
    height: u32,
    /// Output frame rate: `output_fps` when set, else the stream's frame rate. The CLI's
    /// rawvideo output is constant-rate at that rate, so variable-rate sources are resampled
    /// to it too. `None` only when neither is known.
    out_fps: Option<f64>,
    converter: Option<Converter>,
    ready: VecDeque<RgbaFrame>,
    /// Time of the last decoded frame (file-relative), for frames without a timestamp.
    last_t: f64,
    decoder_eof: bool,
    finished: bool,
    /// Resampling state (see `accept`): the latest decoded frame with its output slot, the
    /// next slot to emit, and the last frame's time (relative to the seek point) and duration.
    latest: Option<(i64, RgbaFrame)>,
    next_slot: i64,
    last_rel: f64,
    last_gap: f64,
}

impl LinkedReader {
    pub fn open(
        path: &Path,
        start_secs: f64,
        opts: &ReaderOptions,
    ) -> Result<Self, FfmpegCliError> {
        Self::open_inner(path, start_secs, opts).map_err(to_cli_error)
    }

    fn open_inner(path: &Path, start_secs: f64, opts: &ReaderOptions) -> Result<Self, LinkedError> {
        initialise()?;
        let mut input = ffmpeg::format::input(path)?;
        let (stream_index, time_base, rate, decoder) = {
            let stream = input
                .streams()
                .best(ffmpeg::media::Type::Video)
                .ok_or(LinkedError::NoVideoStream)?;
            let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
                .decoder()
                .video()?;
            (stream.index(), stream.time_base(), stream.rate(), decoder)
        };

        if time_base.denominator() == 0 {
            return Err(LinkedError::BadTimeBase);
        }
        let time_base_secs = time_base.numerator() as f64 / time_base.denominator() as f64;
        let (frame_dur, rate_fps) = if rate.numerator() > 0 && rate.denominator() > 0 {
            (
                rate.denominator() as f64 / rate.numerator() as f64,
                Some(rate.numerator() as f64 / rate.denominator() as f64),
            )
        } else {
            (0.0, None)
        };
        let (width, height) =
            scaled_dimensions(decoder.width(), decoder.height(), opts.target_height);

        // SAFETY: `input` is a live format context owned by this function; only a read of
        // the `start_time` field happens here.
        let raw_start_us = unsafe { (*input.as_ptr()).start_time };
        let input_start_us = if raw_start_us == i64::MIN {
            0
        } else {
            raw_start_us
        };
        if start_secs > 0.0 {
            // `avformat_seek_file` takes absolute AV_TIME_BASE timestamps, so add the container
            // start. The range `[i64::MIN, target]` makes the demuxer choose the keyframe at or
            // before the target, never one after it.
            let target_us = (start_secs * 1_000_000.0).round() as i64 + input_start_us;
            input.seek(target_us, ..=target_us)?;
        }

        Ok(Self {
            input,
            decoder,
            stream_index,
            time_base_secs,
            input_start_secs: input_start_us as f64 / 1_000_000.0,
            start_secs,
            frame_dur,
            width,
            height,
            out_fps: opts
                .output_fps
                .filter(|f| f.is_finite() && *f > 0.0)
                .or(rate_fps),
            converter: None,
            ready: VecDeque::new(),
            last_t: start_secs,
            decoder_eof: false,
            finished: false,
            latest: None,
            next_slot: 0,
            last_rel: 0.0,
            last_gap: frame_dur,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn read_frame(&mut self) -> Result<Option<RgbaFrame>, FfmpegCliError> {
        self.read_frame_inner().map_err(to_cli_error)
    }

    fn read_frame_inner(&mut self) -> Result<Option<RgbaFrame>, LinkedError> {
        loop {
            if let Some(frame) = self.ready.pop_front() {
                return Ok(Some(frame));
            }
            if self.finished {
                return Ok(None);
            }
            match self.next_decoded()? {
                Some((t, decoded)) => {
                    let rgba = self.convert(&decoded)?;
                    self.accept(t, rgba);
                }
                None => self.finish(),
            }
        }
    }

    /// Next decoded frame at or after `start_secs - half a frame`, with its file-relative
    /// time. `None` once the decoder has been drained at end of stream.
    fn next_decoded(&mut self) -> Result<Option<(f64, ffmpeg::frame::Video)>, LinkedError> {
        loop {
            let mut frame = ffmpeg::frame::Video::empty();
            if self.decoder.receive_frame(&mut frame).is_ok() {
                let t = self.frame_time(&frame);
                if t < self.start_secs - self.frame_dur / 2.0 {
                    continue;
                }
                return Ok(Some((t, frame)));
            }
            if self.decoder_eof {
                return Ok(None);
            }
            // `receive_frame` reports both "needs more input" and end of stream as errors, so
            // any error here means the decoder wants a packet. A packet the decoder refuses is
            // skipped, as the CLI's decoder does with corrupt data.
            match self.next_video_packet() {
                Some(packet) => {
                    let _ = self.decoder.send_packet(&packet);
                }
                None => {
                    let _ = self.decoder.send_eof();
                    self.decoder_eof = true;
                }
            }
        }
    }

    fn next_video_packet(&mut self) -> Option<ffmpeg::Packet> {
        loop {
            let mut packet = ffmpeg::Packet::empty();
            // Any read error, end of file included, ends the input.
            if packet.read(&mut self.input).is_err() {
                return None;
            }
            if packet.stream() == self.stream_index {
                return Some(packet);
            }
        }
    }

    fn frame_time(&mut self, frame: &ffmpeg::frame::Video) -> f64 {
        let t = match frame.timestamp().or_else(|| frame.pts()) {
            Some(ts) => ts as f64 * self.time_base_secs - self.input_start_secs,
            None => self.last_t + self.frame_dur,
        };
        self.last_t = t;
        t
    }

    /// Hands one decoded frame to the output. Without an output rate it is emitted as-is.
    /// Otherwise frames are placed on the output slot grid the way ffmpeg's `fps` filter places
    /// them: a frame at `rel` seconds after the seek point goes to slot `round(rel * fps)`, and
    /// each slot shows the latest frame placed at or before it. That duplicates frames when the
    /// output rate is higher and drops them when it is lower. Rounding to the slot grid keeps
    /// millisecond timestamp jitter from changing which frame a slot shows.
    fn accept(&mut self, t: f64, rgba: RgbaFrame) {
        let Some(fps) = self.out_fps else {
            self.ready.push_back(rgba);
            return;
        };
        let rel = t - self.start_secs;
        let slot = (rel * fps).round() as i64;
        // Slots before this frame's slot show the frame held so far.
        while self.next_slot < slot {
            let held = match &self.latest {
                Some((_, held)) => held.clone(),
                None => rgba.clone(),
            };
            self.ready.push_back(held);
            self.next_slot += 1;
        }
        let gap = rel - self.last_rel;
        if self.latest.is_some() && gap > 0.0 {
            self.last_gap = gap;
        } else {
            self.last_gap = self.frame_dur;
        }
        self.last_rel = rel;
        self.latest = Some((slot, rgba));
    }

    /// End of stream. The last frame lasts as long as the gap before it, so the output slots
    /// up to the end of the stream take it.
    fn finish(&mut self) {
        self.finished = true;
        let Some(fps) = self.out_fps else {
            return;
        };
        if let Some((_, last)) = self.latest.take() {
            // Work in whole slots, as the slot placement above does, so timestamp jitter cannot
            // add or remove a tail frame: the end is the last frame's slot plus its gap in slots.
            let end = (self.last_rel * fps).round() as i64
                + ((self.last_gap * fps).round() as i64).max(1);
            while self.next_slot < end {
                self.ready.push_back(last.clone());
                self.next_slot += 1;
            }
        }
    }

    fn convert(&mut self, frame: &ffmpeg::frame::Video) -> Result<RgbaFrame, LinkedError> {
        let key = (frame.format(), frame.width(), frame.height());
        if self.converter.as_ref().map(|c| c.key) != Some(key) {
            self.converter = Some(build_converter(key, self.width, self.height)?);
        }
        let converter = self.converter.as_mut().ok_or(LinkedError::Colorspace)?;

        let mut out = ffmpeg::frame::Video::new(Pixel::RGBA, self.width, self.height);
        converter.scaler.run(frame, &mut out)?;

        // Copy rows out of the frame's padded buffer into a packed RGBA buffer.
        let row = self.width as usize * 4;
        let stride = out.stride(0);
        let data = out.data(0);
        let mut pixels = vec![0u8; row * self.height as usize];
        for (dst, src) in pixels.chunks_exact_mut(row).zip(data.chunks(stride)) {
            if let Some(src) = src.get(..row) {
                dst.copy_from_slice(src);
            }
        }
        Ok(RgbaFrame {
            width: self.width,
            height: self.height,
            pixels,
        })
    }
}

/// Builds the one-step conversion the CLI chain performs: `scale` with
/// `bicubic+accurate_rnd+full_chroma_int`, BT.709 input matrix with limited input range, and
/// full-range RGB output (what `format=rgba` gets from `out_range=pc`).
fn build_converter(
    key: (Pixel, u32, u32),
    out_width: u32,
    out_height: u32,
) -> Result<Converter, LinkedError> {
    let (src_format, src_width, src_height) = key;
    let flags =
        scaling::Flags::BICUBIC | scaling::Flags::ACCURATE_RND | scaling::Flags::FULL_CHR_H_INT;
    let mut scaler = scaling::Context::get(
        src_format,
        src_width,
        src_height,
        Pixel::RGBA,
        out_width,
        out_height,
        flags,
    )?;
    // SAFETY: `scaler` owns a live, initialised SwsContext, and `as_mut_ptr` is only used for
    // this call. `sws_getCoefficients` returns a pointer to a static table that libswscale
    // never frees, and both table pointers are used only inside `sws_setColorspaceDetails`.
    // The remaining arguments are plain integers: brightness 0, contrast and saturation 1.0
    // in 16.16 fixed point, as ffmpeg's `scale` filter sets them.
    let rc = unsafe {
        let coefficients = ffmpeg::ffi::sws_getCoefficients(SWS_CS_ITU709);
        ffmpeg::ffi::sws_setColorspaceDetails(
            scaler.as_mut_ptr(),
            coefficients,
            0,
            coefficients,
            1,
            0,
            1 << 16,
            1 << 16,
        )
    };
    if rc < 0 {
        return Err(LinkedError::Colorspace);
    }
    Ok(Converter { key, scaler })
}

fn to_cli_error(err: LinkedError) -> FfmpegCliError {
    FfmpegCliError::BadOutput(format!("linked decode: {err}"))
}

#[cfg(all(test, feature = "linked-ffmpeg"))]
mod decode_tests {
    //! Dual-backend tests: decode the same fixtures with `CliReader` and `LinkedReader` and
    //! compare. Fixtures are generated with the ffmpeg CLI. The tests skip when the CLI is not
    //! on PATH.

    use super::LinkedReader;
    use crate::media::ffmpeg_cli::{self, CliReader, ReaderOptions, RgbaFrame};
    use crate::proc::SerialSpawn;
    use std::path::{Path, PathBuf};

    struct Fixture {
        name: &'static str,
        path: PathBuf,
        /// Maximum allowed per-channel difference.
        tolerance: u8,
        starts: &'static [f64],
    }

    fn encode(ffmpeg: &Path, input: &[&str], codec: &[&str], out: &Path) {
        let status = crate::proc::command(ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(input)
            .args(codec)
            .arg(out)
            .status_serial()
            .expect("ffmpeg fixture encode");
        assert!(status.success(), "ffmpeg fixture encode failed: {out:?}");
    }

    fn fixtures(dir: &Path, ffmpeg: &Path) -> Vec<Fixture> {
        let testsrc = [
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=322x240:rate=30:duration=3",
        ];
        let ffv1 = dir.join("ffv1_322x240.mkv");
        encode(
            ffmpeg,
            &testsrc,
            &["-c:v", "ffv1", "-pix_fmt", "yuv420p"],
            &ffv1,
        );

        let h264 = dir.join("h264_bframes_322x240.mp4");
        encode(
            ffmpeg,
            &testsrc,
            &[
                "-c:v", "libx264", "-preset", "veryfast", "-crf", "20", "-bf", "2", "-g", "30",
                "-pix_fmt", "yuv420p",
            ],
            &h264,
        );

        // Variable frame rate: 1 s at 30 fps, then 1 s at 15 fps, joined without re-encoding so
        // the timestamps keep their gaps.
        let seg_a = dir.join("seg_a.mkv");
        let seg_b = dir.join("seg_b.mkv");
        encode(
            ffmpeg,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=322x240:rate=30:duration=1",
            ],
            &["-c:v", "ffv1", "-pix_fmt", "yuv420p"],
            &seg_a,
        );
        encode(
            ffmpeg,
            &[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=322x240:rate=15:duration=1",
            ],
            &["-c:v", "ffv1", "-pix_fmt", "yuv420p"],
            &seg_b,
        );
        let list = dir.join("segments.txt");
        std::fs::write(&list, "file 'seg_a.mkv'\nfile 'seg_b.mkv'\n").expect("write list");
        let vfr = dir.join("vfr_322x240.mkv");
        let status = crate::proc::command(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "concat",
                "-safe",
                "0",
                "-i",
            ])
            .arg(&list)
            .args(["-c", "copy"])
            .arg(&vfr)
            .status_serial()
            .expect("ffmpeg concat");
        assert!(status.success(), "ffmpeg concat failed");

        vec![
            Fixture {
                name: "ffv1",
                path: ffv1,
                tolerance: 1,
                starts: &[0.0, 1.5],
            },
            Fixture {
                name: "h264_bframes",
                path: h264,
                tolerance: 2,
                starts: &[0.0, 1.5],
            },
            Fixture {
                name: "vfr_ffv1",
                path: vfr,
                tolerance: 1,
                starts: &[0.0, 0.5],
            },
        ]
    }

    fn decode_cli(path: &Path, start: f64, opts: &ReaderOptions) -> Vec<RgbaFrame> {
        let mut reader = CliReader::open_with(path, start, opts).expect("cli open");
        let mut frames = Vec::new();
        while let Some(frame) = reader.read_frame().expect("cli read") {
            frames.push(frame);
        }
        frames
    }

    fn decode_linked(path: &Path, start: f64, opts: &ReaderOptions) -> Vec<RgbaFrame> {
        let mut reader = LinkedReader::open(path, start, opts).expect("linked open");
        let mut frames = Vec::new();
        while let Some(frame) = reader.read_frame().expect("linked read") {
            frames.push(frame);
        }
        frames
    }

    /// Compares one case and returns the largest per-channel difference.
    fn compare(fixture: &Fixture, start: f64, opts: &ReaderOptions) -> u8 {
        let cli = decode_cli(&fixture.path, start, opts);
        let linked = decode_linked(&fixture.path, start, opts);
        let (w, h) = cli.first().map(|f| (f.width, f.height)).unwrap_or((0, 0));

        let mut max_diff = 0u8;
        let mut dims_ok = true;
        for (a, b) in cli.iter().zip(linked.iter()) {
            dims_ok &= a.width == b.width && a.height == b.height;
            for (x, y) in a.pixels.iter().zip(b.pixels.iter()) {
                max_diff = max_diff.max(x.abs_diff(*y));
            }
        }
        eprintln!(
            "dual-backend {} start={start:.2} target_height={:?} output_fps={:?}: \
             cli_frames={} linked_frames={} size={w}x{h} max_abs_diff={max_diff} (tolerance {})",
            fixture.name,
            opts.target_height,
            opts.output_fps,
            cli.len(),
            linked.len(),
            fixture.tolerance,
        );

        assert_eq!(cli.len(), linked.len(), "{} frame count", fixture.name);
        assert!(dims_ok, "{} frame dimensions differ", fixture.name);
        assert!(
            max_diff <= fixture.tolerance,
            "{} max abs diff {max_diff} exceeds {}",
            fixture.name,
            fixture.tolerance
        );
        max_diff
    }

    #[test]
    fn linked_decode_matches_cli_decode() {
        if !ffmpeg_cli::is_available() {
            eprintln!("skipping dual-backend decode test: ffmpeg/ffprobe not on PATH");
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("renderly-linked-decode-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let ffmpeg = ffmpeg_cli::ffmpeg_path().expect("ffmpeg path");

        let option_sets = [
            ReaderOptions {
                target_height: None,
                output_fps: None,
            },
            ReaderOptions {
                target_height: Some(120),
                output_fps: None,
            },
            ReaderOptions {
                target_height: None,
                output_fps: Some(15.0),
            },
            ReaderOptions {
                target_height: None,
                output_fps: Some(60.0),
            },
            ReaderOptions {
                target_height: Some(120),
                output_fps: Some(24.0),
            },
        ];

        let mut failures = Vec::new();
        for fixture in fixtures(&dir, &ffmpeg) {
            for &start in fixture.starts {
                for opts in &option_sets {
                    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        compare(&fixture, start, opts)
                    }));
                    if outcome.is_err() {
                        failures.push(format!(
                            "{} start={start} target={:?} fps={:?}",
                            fixture.name, opts.target_height, opts.output_fps
                        ));
                    }
                }
            }
        }

        std::fs::remove_dir_all(&dir).ok();
        assert!(
            failures.is_empty(),
            "failing dual-backend cases: {failures:#?}"
        );
    }
}

#[cfg(all(test, feature = "linked-ffmpeg"))]
mod tests {
    use super::{has_audio_stream, probe_video};
    use crate::media::ffmpeg_cli;
    use crate::proc::SerialSpawn;

    /// Generates two clips with the ffmpeg CLI, then checks the linked probe against the CLI
    /// probe. Skips (does not fail) when the ffmpeg CLI is not on PATH.
    #[test]
    fn linked_probe_agrees_with_cli_probe() {
        if !ffmpeg_cli::is_available() {
            eprintln!("skipping linked probe test: ffmpeg/ffprobe not on PATH");
            return;
        }

        let dir = std::env::temp_dir().join(format!("renderly-linked-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let with_audio = dir.join("with_audio.mp4");
        let silent = dir.join("silent.mp4");
        let ffmpeg = ffmpeg_cli::ffmpeg_path().expect("ffmpeg path");

        let status = crate::proc::command(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x240:rate=25",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=2",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
            ])
            .arg(&with_audio)
            .status_serial()
            .expect("ffmpeg encode with audio");
        assert!(status.success());

        let status = crate::proc::command(&ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=2:size=320x240:rate=25",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-an",
            ])
            .arg(&silent)
            .status_serial()
            .expect("ffmpeg encode silent");
        assert!(status.success());

        let cli = ffmpeg_cli::probe_video(&with_audio).expect("cli probe");
        let linked = probe_video(&with_audio).expect("linked probe");
        assert_eq!(linked.width, cli.width, "width");
        assert_eq!(linked.height, cli.height, "height");
        assert!(
            (linked.fps - cli.fps).abs() < 1e-3,
            "fps: linked {} vs cli {}",
            linked.fps,
            cli.fps
        );
        assert!(
            (linked.duration_secs - cli.duration_secs).abs() < 0.05,
            "duration: linked {} vs cli {}",
            linked.duration_secs,
            cli.duration_secs
        );

        assert_eq!(
            has_audio_stream(&with_audio).expect("linked audio check"),
            ffmpeg_cli::has_audio_stream(&with_audio).expect("cli audio check"),
        );
        assert!(has_audio_stream(&with_audio).expect("linked audio check"));
        assert!(!has_audio_stream(&silent).expect("linked silent check"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
