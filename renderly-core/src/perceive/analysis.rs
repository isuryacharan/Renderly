//! FFmpeg-backed media analysis: silence, scene cuts, waveform peaks.

use crate::media::{ffmpeg_available, ffmpeg_path, FfmpegCliError};
use crate::proc::SerialSpawn;
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AnalysisError {
    #[error("{0}")]
    Ffmpeg(#[from] FfmpegCliError),
    #[error("analysis parse failed: {0}")]
    Parse(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SilenceSpan {
    pub start_secs: f64,
    pub end_secs: f64,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneCut {
    pub time_secs: f64,
    pub score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioPeaks {
    pub bucket_secs: f64,
    pub peaks: Vec<f32>,
}

/// Detect silent spans using FFmpeg `silencedetect`.
pub fn detect_silence(
    path: &Path,
    noise_db: f64,
    min_duration_secs: f64,
) -> Result<Vec<SilenceSpan>, AnalysisError> {
    if !ffmpeg_available() {
        return Err(FfmpegCliError::NotFound.into());
    }

    let filter = format!("silencedetect=noise={noise_db}dB:d={min_duration_secs}");
    let output = crate::proc::command(ffmpeg_path()?)
        .args(["-hide_banner", "-i"])
        .arg(path)
        .args(["-af", &filter, "-f", "null", "-"])
        .output_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;

    parse_silence(&String::from_utf8_lossy(&output.stderr))
}

fn parse_silence(stderr: &str) -> Result<Vec<SilenceSpan>, AnalysisError> {
    let mut spans = Vec::new();
    let mut start: Option<f64> = None;

    for line in stderr.lines() {
        if let Some(rest) = line.split("silence_start:").nth(1) {
            if let Ok(t) = rest.trim().parse::<f64>() {
                start = Some(t);
            }
        }
        if let Some(rest) = line.split("silence_end:").nth(1) {
            let end_str = rest.split('|').next().unwrap_or(rest).trim();
            if let (Some(s), Ok(end)) = (start.take(), end_str.parse::<f64>()) {
                spans.push(SilenceSpan {
                    start_secs: s,
                    end_secs: end,
                    duration_secs: end - s,
                });
            }
        }
    }
    Ok(spans)
}

/// Detect scene changes using FFmpeg `select=gt(scene,threshold)`.
pub fn detect_scenes(path: &Path, threshold: f64) -> Result<Vec<SceneCut>, AnalysisError> {
    if !ffmpeg_available() {
        return Err(FfmpegCliError::NotFound.into());
    }

    let filter = format!("select='gt(scene,{threshold})',showinfo");
    let output = crate::proc::command(ffmpeg_path()?)
        .args(["-hide_banner", "-i"])
        .arg(path)
        .args(["-vf", &filter, "-f", "null", "-"])
        .output_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;

    parse_scenes(&String::from_utf8_lossy(&output.stderr), threshold)
}

/// `showinfo` prints each frame's `pts_time:` on its own summary line, then — on separate
/// `Metadata:`/`  key=value` lines immediately after — any frame metadata, including
/// `lavfi.scene_score` when the `select` filter attached it. The score is NOT a token on
/// the same line as `pts_time:` (a prior version assumed a same-line `scene:` token, which
/// never actually appears in `showinfo`'s output, silently pinning every cut's score to a
/// hardcoded fake value). Some ffmpeg builds don't emit the metadata lines at all; for
/// those, `threshold` is used as a floor — every frame that reached this parser passed
/// `select='gt(scene,threshold)'`, so it's guaranteed to score above `threshold`, which is
/// at least an honest, internally-consistent value (unlike a disconnected constant that
/// could read below the caller's own requested threshold).
fn parse_scenes(stderr: &str, threshold: f64) -> Result<Vec<SceneCut>, AnalysisError> {
    let mut cuts = Vec::new();
    let mut pending_time: Option<f64> = None;
    let mut pending_score: Option<f64> = None;

    for line in stderr.lines() {
        if line.contains("showinfo") && line.contains("pts_time:") {
            if let Some(t) = pending_time.take() {
                cuts.push(SceneCut {
                    time_secs: t,
                    score: pending_score.take().unwrap_or(threshold),
                });
            }
            for part in line.split_whitespace() {
                if let Some(v) = part.strip_prefix("pts_time:") {
                    pending_time = v.parse().ok();
                }
            }
            continue;
        }
        if let Some(idx) = line.find("lavfi.scene_score=") {
            let val = line[idx + "lavfi.scene_score=".len()..].trim();
            pending_score = val.parse().ok();
        }
    }
    if let Some(t) = pending_time {
        cuts.push(SceneCut {
            time_secs: t,
            score: pending_score.unwrap_or(threshold),
        });
    }

    Ok(cuts)
}

/// Downsampled peak envelope for waveform display / agent perception.
///
/// Files with no audio stream return empty `peaks` (not an error) so callers can skip
/// drawing a waveform without treating silent video as a hard failure.
///
/// Sample rate is chosen so we decode roughly a handful of samples per display bucket
/// instead of a fixed 8 kHz PCM dump of the whole file (which dominates import time on
/// long clips).
pub fn audio_peaks(path: &Path, buckets: u32) -> Result<AudioPeaks, AnalysisError> {
    if !ffmpeg_available() {
        return Err(FfmpegCliError::NotFound.into());
    }
    if buckets == 0 {
        return Err(AnalysisError::Parse("buckets must be > 0".into()));
    }

    let duration = media_duration(path).max(0.1);
    let bucket_secs = duration / buckets as f64;

    // ~6 PCM samples per UI bucket is enough for an envelope. Cap the rate so short clips
    // still get enough resolution and long clips stay cheap to decode. The signal is
    // rectified (`abs`) BEFORE resampling: the resampler low-passes everything above half the
    // target rate, which on its own removed any tone or voice above ~25 Hz for long clips and
    // drew them flat. Rectified, the resampler keeps the amplitude envelope (the mean of
    // |signal| — about 0.64 of the peak for a sine), which is what a waveform view needs.
    let target_samples = (buckets as f64 * 6.0).max(buckets as f64);
    let sample_rate = ((target_samples / duration).ceil() as u32).clamp(50, 4_000);

    let output = crate::proc::command(ffmpeg_path()?)
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(path)
        .args([
            "-vn",
            "-sn",
            "-ac",
            "1",
            "-af",
            "aeval=exprs=abs(val(0))",
            "-ar",
            &sample_rate.to_string(),
            "-f",
            "f32le",
            "pipe:1",
        ])
        .output_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        // Silent / video-only sources: treat as empty peaks rather than a hard error.
        if stderr.contains("does not contain any stream")
            || stderr.contains("Output file does not contain any stream")
            || stderr.contains("matches no streams")
        {
            return Ok(AudioPeaks {
                bucket_secs,
                peaks: Vec::new(),
            });
        }
        if stderr.is_empty() {
            return Err(FfmpegCliError::NonZeroExit(output.status.code().unwrap_or(-1)).into());
        }
        return Err(FfmpegCliError::BadOutput(format!(
            "ffmpeg exited with status {}: {stderr}",
            output.status.code().unwrap_or(-1)
        ))
        .into());
    }

    let samples: &[f32] = bytemuck::cast_slice(&output.stdout);
    let peaks = bucket_peaks(samples, buckets);

    Ok(AudioPeaks { bucket_secs, peaks })
}

/// Downsample `samples` into `buckets` peak values. Boundaries are computed as exact
/// fractions of `samples.len()` (`i * len / buckets`, multiply-then-divide) rather than a
/// fixed per-bucket stride (`len / buckets`, floored) — a fixed stride silently drops the
/// remainder tail off the very end whenever `len` isn't an exact multiple of `buckets`
/// (the common case for real audio), which could hide the loudest peak in a clip if it
/// happened to fall in that dropped remainder. The multiply-then-divide form is
/// monotonic in `i`, so `start <= end` always holds — no separate guard needed against
/// `buckets` exceeding the sample count (very short clips, or a large requested bucket
/// count).
fn bucket_peaks(samples: &[f32], buckets: u32) -> Vec<f32> {
    let buckets = buckets as usize;
    let len = samples.len();
    let mut peaks = Vec::with_capacity(buckets);
    for i in 0..buckets {
        let start = (i * len / buckets).min(len);
        let end = ((i + 1) * len / buckets).min(len);
        let peak = samples[start..end]
            .iter()
            .map(|s| s.abs())
            .fold(0.0_f32, f32::max);
        peaks.push(peak);
    }
    peaks
}

fn media_duration(path: &Path) -> f64 {
    if let Ok(p) = crate::media::probe(path) {
        if let Some(d) = p.duration_secs {
            return d;
        }
    }
    if let Ok(v) = crate::media::probe_video(path) {
        return v.duration_secs;
    }
    60.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_silence_lines() {
        let stderr = "[silencedetect @ 0x1] silence_start: 1.5\n[silencedetect @ 0x1] silence_end: 3.0 | silence_duration: 1.5\n";
        let spans = parse_silence(stderr).unwrap();
        assert_eq!(spans.len(), 1);
        assert!((spans[0].duration_secs - 1.5).abs() < 1e-9);
    }

    /// Regression: a steady 440 Hz tone over a long clip used to come back nearly flat
    /// (~0.005) because the low-rate resample filtered the tone out before the peaks were
    /// measured. The rectified envelope keeps it.
    #[test]
    fn audio_peaks_keep_tones_above_the_decode_rate() {
        if !ffmpeg_available() {
            eprintln!("skipping audio_peaks tone test: ffmpeg not on PATH");
            return;
        }
        let dir = std::env::temp_dir().join(format!("renderly-peaks-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        // 20 s: 440 Hz at amplitude 0.5 for 10 s, then 10 s of silence.
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
            .arg("aevalsrc=exprs='0.5*sin(2*PI*440*t)*lt(t,10)':s=48000:d=20")
            .arg(&wav)
            .status_serial()
            .unwrap();
        assert!(status.success());
        let peaks = audio_peaks(&wav, 64).unwrap();
        assert_eq!(peaks.peaks.len(), 64);
        let (tone, silence) = peaks.peaks.split_at(32);
        let tone_min = tone[1..31].iter().copied().fold(f32::MAX, f32::min);
        let silence_max = silence[1..].iter().copied().fold(0.0_f32, f32::max);
        assert!(tone_min > 0.25, "tone envelope too low: {tone_min}");
        assert!(silence_max < 0.01, "silence not silent: {silence_max}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bucket_peaks_does_not_panic_when_buckets_exceed_samples() {
        // A very short clip (few samples) with many requested buckets used to panic:
        // `i * samples_per_bucket` could exceed `samples.len()` while `end` stayed clamped,
        // producing a `start > end` slice panic.
        let samples = [0.1_f32, 0.5, 0.2];
        let peaks = bucket_peaks(&samples, 256);
        assert_eq!(peaks.len(), 256);
        assert!(peaks.iter().any(|&p| p > 0.0));
    }

    #[test]
    fn bucket_peaks_finds_max_abs_per_bucket() {
        let samples = [0.1_f32, -0.9, 0.2, 0.3];
        let peaks = bucket_peaks(&samples, 2);
        assert_eq!(peaks.len(), 2);
        assert!((peaks[0] - 0.9).abs() < 1e-6);
        assert!((peaks[1] - 0.3).abs() < 1e-6);
    }

    #[test]
    fn bucket_peaks_does_not_drop_trailing_samples_on_non_exact_division() {
        // 10 samples / 3 buckets doesn't divide evenly — a fixed per-bucket stride
        // (floor(10/3)=3) used to only cover indices [0, 9), silently dropping index 9
        // from every bucket. Put the loudest sample at the very end: a dropped tail
        // sample would be invisible in the returned peaks.
        let mut samples = [0.0_f32; 10];
        samples[9] = 0.99;
        let peaks = bucket_peaks(&samples, 3);
        assert_eq!(peaks.len(), 3);
        assert!(
            (peaks[2] - 0.99).abs() < 1e-6,
            "last bucket must include the final sample, got {:?}",
            peaks
        );
    }

    #[test]
    fn parse_scenes_reads_real_lavfi_scene_score_metadata_and_falls_back_to_threshold() {
        // Real `showinfo` output: the score is a `lavfi.scene_score=` metadata line
        // AFTER the frame's `pts_time:` summary line, not a same-line `scene:` token (the
        // previous parser looked for a token that never appears, silently pinning every
        // cut to a hardcoded fake 0.3). The second frame here has no metadata line at all
        // — some ffmpeg builds don't emit it — so it must fall back to `threshold`.
        let stderr = "\
[Parsed_showinfo_1 @ 0x1] n:   0 pts:      0 pts_time:0       pos:      0 fmt:yuv420p type:I\n\
[Parsed_showinfo_1 @ 0x1] Metadata:\n\
[Parsed_showinfo_1 @ 0x1]   lavfi.scene_score=0.812345\n\
[Parsed_showinfo_1 @ 0x1] n:   1 pts:     33 pts_time:1.1     pos:  12345 fmt:yuv420p type:P\n\
";
        let cuts = parse_scenes(stderr, 0.4).unwrap();
        assert_eq!(cuts.len(), 2);
        assert!((cuts[0].time_secs - 0.0).abs() < 1e-9);
        assert!((cuts[0].score - 0.812345).abs() < 1e-6);
        assert!((cuts[1].time_secs - 1.1).abs() < 1e-9);
        assert!((cuts[1].score - 0.4).abs() < 1e-9);
    }
}
