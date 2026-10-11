//! Media probing and FFmpeg-backed I/O for export.
//!
//! `probe()` works without FFmpeg for a narrow set of formats (WAV). When FFmpeg is on
//! PATH, video probing uses `ffprobe` during import and export.

mod ffmpeg_cli;

use crate::project::MediaKind;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use thiserror::Error;

pub use ffmpeg_cli::{
    ffmpeg_path, ffprobe_path, generate_thumbnail_strip, has_audio_stream,
    is_available as ffmpeg_available, mix_timeline_audio, mux_video_audio, probe_video,
    AudioMixClip, DuckSettings, FfmpegCliError, ReaderOptions, RgbaFrame, ThumbnailStrip,
    VideoEncodeConfig, VideoEncoder, VideoEncoderPreference, VideoReader,
};

#[derive(Debug, Error)]
pub enum MediaError {
    #[error("media file not found: {0}")]
    NotFound(String),
    #[error("unsupported media format: .{0}")]
    UnsupportedFormat(String),
    #[error("failed to read media file: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProbedMedia {
    pub kind: Option<MediaKind>,
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<f64>,
}

const VIDEO_EXTS: &[&str] = &["mp4", "mov", "mkv", "avi", "webm"];
const AUDIO_EXTS: &[&str] = &["wav", "mp3", "aac", "flac", "ogg", "m4a"];
const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp"];

/// True when `path` has a still-image extension. Stills are decoded once with the `image`
/// crate rather than through the ffmpeg video reader (they have no duration to probe and
/// no frame to seek to).
pub fn is_still_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

pub fn probe(path: &Path) -> Result<ProbedMedia, MediaError> {
    if !path.is_file() {
        return Err(MediaError::NotFound(path.display().to_string()));
    }

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    let kind = if VIDEO_EXTS.contains(&ext.as_str()) {
        MediaKind::Video
    } else if AUDIO_EXTS.contains(&ext.as_str()) {
        MediaKind::Audio
    } else if IMAGE_EXTS.contains(&ext.as_str()) {
        MediaKind::Image
    } else {
        return Err(MediaError::UnsupportedFormat(ext));
    };

    let mut probed = ProbedMedia {
        kind: Some(kind),
        ..Default::default()
    };

    if kind == MediaKind::Audio && ext == "wav" {
        if let Some(duration) = probe_wav_duration(path)? {
            probed.duration_secs = Some(duration);
        }
    }

    if kind == MediaKind::Video && ffmpeg_cli::is_available() {
        if let Ok(video) = ffmpeg_cli::probe_video(path) {
            probed.duration_secs = Some(video.duration_secs);
            probed.width = Some(video.width);
            probed.height = Some(video.height);
            probed.fps = Some(video.fps);
        }
    }

    Ok(probed)
}

/// Minimal RIFF/WAVE header parse: finds the `fmt ` chunk for byte rate and the `data`
/// chunk for size, giving duration without decoding any samples.
fn probe_wav_duration(path: &Path) -> Result<Option<f64>, MediaError> {
    let mut file = File::open(path)?;
    let mut riff_header = [0u8; 12];
    if file.read_exact(&mut riff_header).is_err() {
        return Ok(None);
    }
    if &riff_header[0..4] != b"RIFF" || &riff_header[8..12] != b"WAVE" {
        return Ok(None);
    }

    let mut byte_rate: Option<u32> = None;
    let mut data_size: Option<u32> = None;

    loop {
        let mut chunk_header = [0u8; 8];
        if file.read_exact(&mut chunk_header).is_err() {
            break;
        }
        let chunk_id = &chunk_header[0..4];
        let chunk_size = u32::from_le_bytes(chunk_header[4..8].try_into().unwrap());

        if chunk_id == b"fmt " {
            let mut fmt_body = vec![0u8; chunk_size as usize];
            file.read_exact(&mut fmt_body)?;
            if fmt_body.len() >= 12 {
                byte_rate = Some(u32::from_le_bytes(fmt_body[8..12].try_into().unwrap()));
            }
        } else if chunk_id == b"data" {
            data_size = Some(chunk_size);
            // No need to read the sample data itself.
            file.seek(SeekFrom::Current(chunk_size as i64))?;
        } else {
            file.seek(SeekFrom::Current(chunk_size as i64))?;
        }

        // RIFF chunks are word-aligned; skip a pad byte on odd-sized chunks.
        if chunk_size % 2 == 1 {
            file.seek(SeekFrom::Current(1))?;
        }

        if byte_rate.is_some() && data_size.is_some() {
            break;
        }
    }

    match (byte_rate, data_size) {
        (Some(rate), Some(size)) if rate > 0 => Ok(Some(size as f64 / rate as f64)),
        _ => Ok(None),
    }
}
