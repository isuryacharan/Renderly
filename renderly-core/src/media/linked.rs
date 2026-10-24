//! In-process probe through libav, via `ffmpeg-the-third`. Compiled only with the
//! `linked-ffmpeg` feature and used only when `RENDERLY_MEDIA_BACKEND=linked` is set.
//!
//! Probe only. Frame decode still runs on the CLI reader. This module mirrors what
//! `ffmpeg_cli::probe_video` and `ffmpeg_cli::has_audio_stream` return (see that file for the
//! ffprobe fields each value comes from).
//!
//! NOT COMPILED OR TESTED ON THE DEVELOPMENT MACHINE: it has no FFmpeg dev libraries or
//! libclang. The first real build is the informational `linked-ffmpeg` CI job.

use super::ffmpeg_cli::ProbedVideo;
use ffmpeg_the_third as ffmpeg;
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
    let duration_s = if stream_ticks > 0 && time_base.denominator() != 0 {
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
        duration_us: crate::time::seconds_to_us(duration_s),
    })
}

/// True if the input has an audio stream. Same answer as `ffmpeg_cli::has_audio_stream`.
pub fn has_audio_stream(path: &Path) -> Result<bool, LinkedError> {
    initialise()?;
    let input = ffmpeg::format::input(path)?;
    Ok(input.streams().best(ffmpeg::media::Type::Audio).is_some())
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
            (linked.duration_us - cli.duration_us).abs() < 50_000,
            "duration: linked {} vs cli {}",
            linked.duration_us,
            cli.duration_us
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
