//! Read-only perception helpers for MCP and CLI (Phase 1): transcript, frame preview.

mod analysis;

pub use analysis::{
    audio_peaks, detect_scenes, detect_silence, AnalysisError, AudioPeaks, SceneCut, SilenceSpan,
};

use crate::commands::ExportPreset;
use crate::export::{render_frame_at, ExportError, ExportSettings};
use crate::media::{ffmpeg_available, ffmpeg_path, FfmpegCliError};
use crate::proc::SerialSpawn;
use crate::project::{Clip, MediaKind, Project, TrackKind};
use image::ImageEncoder;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum PerceiveError {
    #[error("{0}")]
    Export(#[from] ExportError),
    #[error("{0}")]
    Ffmpeg(#[from] FfmpegCliError),
    #[error("media not found: {0}")]
    MediaNotFound(Uuid),
    #[error("whisper not available; install whisper.cpp CLI and set RENDERLY_WHISPER_MODEL")]
    WhisperNotAvailable,
    #[error("whisper failed: {0}")]
    WhisperFailed(String),
    #[error("png encode failed: {0}")]
    PngEncode(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start_secs: f64,
    pub end_secs: f64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transcript {
    pub media_id: Uuid,
    pub segments: Vec<TranscriptSegment>,
}

/// Encode an RGBA8 buffer as PNG bytes (shared by headless perceive and live app preview).
pub fn encode_rgba_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, PerceiveError> {
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png);
    encoder
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| PerceiveError::PngEncode(e.to_string()))?;
    Ok(png)
}

/// Pick a representative timeline time for a project thumbnail/gallery card.
///
/// `t=0` is frequently black (empty leader, fade-in), so this finds the earliest-starting
/// enabled video/image clip on a visible video track and samples a bit *into* it —
/// `min(clip_start + 1.0s, clip_start + clip_duration / 2)` — so short clips still land
/// inside their own span and longer ones skip past a fade-from-black at the cut. Falls back
/// to `0.0` for a project with no content clips (an empty project card still renders the
/// black frame rather than erroring).
///
/// Decode-free and pure — reads only clip timing/media-kind, mirrors the "no re-derivation"
/// rule in this module's doc comment, and is deliberately kept fs/render-free for unit
/// testing without a real project directory.
pub fn representative_frame_time(project: &Project) -> f64 {
    let mut earliest: Option<(f64, f64)> = None; // (start, timeline_duration)
    for track in &project.tracks {
        if track.kind != TrackKind::Video || track.hidden {
            continue;
        }
        for clip in &track.clips {
            let Clip::Video(v) = clip else { continue };
            if !v.enabled {
                continue;
            }
            let is_content = project
                .find_media(v.media_id)
                .map(|m| matches!(m.kind, MediaKind::Video | MediaKind::Image))
                .unwrap_or(false);
            if !is_content {
                continue;
            }
            let start = v.position_secs;
            if earliest.is_none_or(|(cur, _)| start < cur) {
                earliest = Some((start, v.timeline_duration_secs()));
            }
        }
    }
    let Some((start, duration)) = earliest else {
        return 0.0;
    };
    let midpoint = start + duration / 2.0;
    (start + 1.0).min(midpoint).max(start)
}

/// Render the composited timeline frame at `time_secs` as PNG bytes.
pub fn render_frame_png(
    project: &Project,
    time_secs: f64,
    preset: ExportPreset,
) -> Result<Vec<u8>, PerceiveError> {
    let settings = ExportSettings::from_preset(&preset, project);
    let rgba = render_frame_at(project, time_secs, settings)?;
    encode_rgba_png(&rgba, settings.width, settings.height)
}

/// Transcribe a media item with whisper.cpp CLI (local STT).
pub fn transcribe_media(project: &Project, media_id: Uuid) -> Result<Transcript, PerceiveError> {
    if !ffmpeg_available() {
        return Err(FfmpegCliError::NotFound.into());
    }
    let media = project
        .find_media(media_id)
        .ok_or(PerceiveError::MediaNotFound(media_id))?;
    if media.kind != MediaKind::Video && media.kind != MediaKind::Audio {
        return Err(PerceiveError::WhisperFailed(
            "transcription requires video or audio media".into(),
        ));
    }

    let model =
        std::env::var("RENDERLY_WHISPER_MODEL").map_err(|_| PerceiveError::WhisperNotAvailable)?;
    let whisper = find_whisper_cli().ok_or(PerceiveError::WhisperNotAvailable)?;

    let dir = std::env::temp_dir().join(format!("renderly-whisper-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(FfmpegCliError::Io)?;
    let wav = dir.join("audio.wav");
    let out_prefix = dir.join("out");

    let status = crate::proc::command(ffmpeg_path()?)
        .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(&media.path)
        .args(["-vn", "-ar", "16000", "-ac", "1", "-f", "wav"])
        .arg(&wav)
        .status_serial()
        .map_err(|e| FfmpegCliError::SpawnFailed {
            tool: "ffmpeg",
            message: e.to_string(),
        })?;
    if !status.success() {
        return Err(PerceiveError::WhisperFailed(
            "ffmpeg audio extract failed".into(),
        ));
    }

    let status = crate::proc::command(&whisper)
        .args(["-m", &model, "-f"])
        .arg(&wav)
        .args(["-oj", "-of"])
        .arg(&out_prefix)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status_serial()
        .map_err(|e| PerceiveError::WhisperFailed(e.to_string()))?;
    if !status.success() {
        return Err(PerceiveError::WhisperFailed(
            "whisper-cli exited with error".into(),
        ));
    }

    let json_path = dir.join("out.json");
    let data = std::fs::read_to_string(&json_path)
        .map_err(|e| PerceiveError::WhisperFailed(e.to_string()))?;
    let segments =
        parse_whisper_json(&data).map_err(|e| PerceiveError::WhisperFailed(e.to_string()))?;

    std::fs::remove_dir_all(&dir).ok();

    Ok(Transcript { media_id, segments })
}

/// Parse whisper.cpp's `-oj` JSON output into transcript segments.
///
/// whisper.cpp emits both `timestamps` (human-readable `"HH:MM:SS,mmm"` strings, for
/// display) and `offsets` (integer milliseconds, for machine use) per segment. Parsing
/// `timestamps` as a float — as earlier code here did — always fails to parse and silently
/// drops every segment via `filter_map`, so `offsets` (ms) is the field to use.
fn parse_whisper_json(data: &str) -> Result<Vec<TranscriptSegment>, String> {
    let parsed: WhisperJson = serde_json::from_str(data).map_err(|e| e.to_string())?;
    let raw_segments = parsed.transcription.unwrap_or_default();
    let raw_count = raw_segments.len();

    let segments: Vec<TranscriptSegment> = raw_segments
        .into_iter()
        .filter_map(|s| {
            let offsets = s.offsets?;
            Some(TranscriptSegment {
                start_secs: offsets.from as f64 / 1000.0,
                end_secs: offsets.to as f64 / 1000.0,
                text: s.text.trim().to_string(),
            })
        })
        .collect();

    // whisper.cpp produced raw entries but every single one failed to parse into a
    // segment (each was missing `offsets`) — almost certainly a field rename/schema drift
    // in a future whisper.cpp version (this has already happened once for this exact
    // codebase — see the `timestamps`-vs-`offsets` note above), not a legitimately empty
    // transcript. Fail loudly instead of silently returning an empty transcript that's
    // indistinguishable from "no speech detected."
    if raw_count > 0 && segments.is_empty() {
        return Err(format!(
            "whisper produced {raw_count} segment(s) but none had a parseable `offsets` \
             field — whisper.cpp's JSON schema may have changed"
        ));
    }

    Ok(segments)
}

fn find_whisper_cli() -> Option<PathBuf> {
    for name in ["whisper-cli", "whisper", "main"] {
        if let Some(path) = which_tool(name) {
            return Some(path);
        }
    }
    None
}

fn which_tool(name: &str) -> Option<PathBuf> {
    let with_exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).find_map(|dir| {
            let candidate = dir.join(&with_exe);
            candidate.is_file().then_some(candidate)
        })
    })
}

#[derive(Debug, Deserialize)]
struct WhisperJson {
    transcription: Option<Vec<WhisperSegment>>,
}

#[derive(Debug, Deserialize)]
struct WhisperSegment {
    offsets: Option<WhisperOffsets>,
    text: String,
}

#[derive(Debug, Deserialize)]
struct WhisperOffsets {
    from: u64,
    to: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_whisper_json_uses_offsets_not_display_timestamps() {
        // Real whisper.cpp `-oj` shape: `timestamps` are SRT-style display strings that
        // must NOT be parsed as floats; `offsets` (ms) are the numeric source of truth.
        let data = r#"{
            "transcription": [
                {
                    "timestamps": { "from": "00:00:00,000", "to": "00:00:02,500" },
                    "offsets": { "from": 0, "to": 2500 },
                    "text": " he just does NOT miss"
                },
                {
                    "timestamps": { "from": "00:00:02,500", "to": "00:00:04,000" },
                    "offsets": { "from": 2500, "to": 4000 },
                    "text": " unbelievable"
                }
            ]
        }"#;

        let segments = parse_whisper_json(data).unwrap();
        assert_eq!(segments.len(), 2);
        assert!((segments[0].start_secs - 0.0).abs() < 1e-9);
        assert!((segments[0].end_secs - 2.5).abs() < 1e-9);
        assert_eq!(segments[0].text, "he just does NOT miss");
        assert!((segments[1].start_secs - 2.5).abs() < 1e-9);
        assert!((segments[1].end_secs - 4.0).abs() < 1e-9);
    }

    #[test]
    fn parse_whisper_json_errors_when_all_segments_lack_offsets() {
        // Simulates a hypothetical future whisper.cpp schema change that renames/removes
        // `offsets` — every entry fails to parse into a segment. This must surface as an
        // error, not silently return an empty (and indistinguishable from "no speech")
        // transcript.
        let data = r#"{
            "transcription": [
                { "timestamps": { "from": "00:00:00,000", "to": "00:00:02,500" }, "text": "hi" }
            ]
        }"#;
        let err = parse_whisper_json(data).unwrap_err();
        assert!(err.contains("offsets"));
    }

    #[test]
    fn parse_whisper_json_empty_transcription_is_not_an_error() {
        // A genuinely silent clip: whisper.cpp returns an empty `transcription` array,
        // not entries that failed to parse — this is legitimately empty, not schema
        // drift, and must not be treated as a failure.
        let data = r#"{ "transcription": [] }"#;
        let segments = parse_whisper_json(data).unwrap();
        assert!(segments.is_empty());
    }

    use crate::project::{MediaClip, MediaItem, Settings, Track};

    fn video_media(project: &mut Project, duration_secs: f64) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        project.media.push(MediaItem {
            id,
            path: PathBuf::from("clip.mp4"),
            kind: MediaKind::Video,
            duration_secs: Some(duration_secs),
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
        });
        id
    }

    /// Adds a video track with one enabled clip starting at `position_secs` and spanning
    /// `duration_secs` of timeline (via a 1x-speed in/out source range), wired to a fresh
    /// video media item.
    fn track_with_clip(project: &mut Project, position_secs: f64, duration_secs: f64) {
        let media_id = video_media(project, position_secs + duration_secs);
        let mut track = Track::new(TrackKind::Video, "V1");
        let clip = MediaClip {
            media_id,
            position_secs,
            source_in_secs: 0.0,
            source_out_secs: duration_secs,
            ..Default::default()
        };
        track.clips.push(Clip::Video(clip));
        project.tracks.push(track);
    }

    #[test]
    fn representative_frame_time_empty_project_is_zero() {
        let project = Project::new("empty", Settings::default());
        assert_eq!(representative_frame_time(&project), 0.0);
    }

    #[test]
    fn representative_frame_time_samples_a_second_into_first_clip() {
        let mut project = Project::new("p", Settings::default());
        track_with_clip(&mut project, 3.0, 10.0);
        // Inside the clip, 1s past its start — clear of a fade-from-black at the cut.
        assert!((representative_frame_time(&project) - 4.0).abs() < 1e-9);
    }

    #[test]
    fn representative_frame_time_short_clip_uses_midpoint() {
        let mut project = Project::new("p", Settings::default());
        // 1.5s clip: start + 1.0 would overshoot past the midpoint, so the midpoint wins.
        track_with_clip(&mut project, 2.0, 1.5);
        assert!((representative_frame_time(&project) - 2.75).abs() < 1e-9);
    }

    #[test]
    fn representative_frame_time_ignores_hidden_tracks_and_disabled_clips() {
        let mut project = Project::new("p", Settings::default());
        track_with_clip(&mut project, 0.5, 5.0);
        project.tracks[0].hidden = true;
        // Second, later, visible track with a disabled clip — should be skipped too.
        track_with_clip(&mut project, 8.0, 5.0);
        if let Clip::Video(c) = &mut project.tracks[1].clips[0] {
            c.enabled = false;
        }
        assert_eq!(representative_frame_time(&project), 0.0);
    }
}
