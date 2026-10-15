//! Backend seam for media probing and frame decode.
//!
//! The FFmpeg CLI (`ffmpeg_cli`) serves every request in a default build. With the
//! `linked-ffmpeg` cargo feature, probing can also go through in-process libav (`linked`),
//! but only when `RENDERLY_MEDIA_BACKEND=linked` is set. Frame decode always runs on the CLI.
//! Encode, mux, audio mixing, thumbnails and analysis do not go through this seam.

use super::ffmpeg_cli::{self, CliReader, FfmpegCliError, ProbedVideo, ReaderOptions, RgbaFrame};
use std::path::Path;

/// Which implementation serves probe and decode requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaBackend {
    /// `ffmpeg` / `ffprobe` subprocesses. Selected in every default build.
    Cli,
    /// In-process libav via `ffmpeg-the-third`. Only exists with `linked-ffmpeg`.
    #[cfg(feature = "linked-ffmpeg")]
    Linked,
}

/// The backend probe calls use. `Cli` unless the `linked-ffmpeg` feature is compiled in
/// AND `RENDERLY_MEDIA_BACKEND=linked` is set.
pub fn selected_backend() -> MediaBackend {
    #[cfg(feature = "linked-ffmpeg")]
    {
        // Read on every call; the value is only consulted when the feature is compiled in.
        let requested = std::env::var("RENDERLY_MEDIA_BACKEND").unwrap_or_default();
        if requested.trim().eq_ignore_ascii_case("linked") {
            return MediaBackend::Linked;
        }
    }
    MediaBackend::Cli
}

/// Probe the first video stream. A linked-backend failure falls back to the CLI probe, so
/// the result does not depend on the linked code working.
pub fn probe_video(path: &Path) -> Result<ProbedVideo, FfmpegCliError> {
    match selected_backend() {
        #[cfg(feature = "linked-ffmpeg")]
        MediaBackend::Linked => {
            if let Ok(probed) = super::linked::probe_video(path) {
                return Ok(probed);
            }
        }
        MediaBackend::Cli => {}
    }
    ffmpeg_cli::probe_video(path)
}

/// True if `path` has at least one audio stream. Same fallback rule as [`probe_video`].
pub fn has_audio_stream(path: &Path) -> Result<bool, FfmpegCliError> {
    match selected_backend() {
        #[cfg(feature = "linked-ffmpeg")]
        MediaBackend::Linked => {
            if let Ok(found) = super::linked::has_audio_stream(path) {
                return Ok(found);
            }
        }
        MediaBackend::Cli => {}
    }
    ffmpeg_cli::has_audio_stream(path)
}

/// Sequential RGBA frame reader. Decodes with the in-process libav reader when the
/// `linked-ffmpeg` feature is compiled in and `RENDERLY_MEDIA_BACKEND=linked` is set, and
/// falls back to the CLI reader if that fails to open. Otherwise it uses the CLI reader.
pub struct VideoReader {
    reader: ReaderBackend,
}

enum ReaderBackend {
    Cli(CliReader),
    #[cfg(feature = "linked-ffmpeg")]
    Linked(super::linked::LinkedReader),
}

impl VideoReader {
    pub fn open(path: &Path, start_secs: f64) -> Result<Self, FfmpegCliError> {
        Self::open_with(path, start_secs, &ReaderOptions::default())
    }

    pub fn open_with(
        path: &Path,
        start_secs: f64,
        opts: &ReaderOptions,
    ) -> Result<Self, FfmpegCliError> {
        #[cfg(feature = "linked-ffmpeg")]
        {
            if selected_backend() == MediaBackend::Linked {
                if let Ok(reader) = super::linked::LinkedReader::open(path, start_secs, opts) {
                    return Ok(Self {
                        reader: ReaderBackend::Linked(reader),
                    });
                }
            }
        }
        Ok(Self {
            reader: ReaderBackend::Cli(CliReader::open_with(path, start_secs, opts)?),
        })
    }

    pub fn width(&self) -> u32 {
        match &self.reader {
            ReaderBackend::Cli(reader) => reader.width(),
            #[cfg(feature = "linked-ffmpeg")]
            ReaderBackend::Linked(reader) => reader.width(),
        }
    }

    pub fn height(&self) -> u32 {
        match &self.reader {
            ReaderBackend::Cli(reader) => reader.height(),
            #[cfg(feature = "linked-ffmpeg")]
            ReaderBackend::Linked(reader) => reader.height(),
        }
    }

    pub fn read_frame(&mut self) -> Result<Option<RgbaFrame>, FfmpegCliError> {
        match &mut self.reader {
            ReaderBackend::Cli(reader) => reader.read_frame(),
            #[cfg(feature = "linked-ffmpeg")]
            ReaderBackend::Linked(reader) => reader.read_frame(),
        }
    }
}
