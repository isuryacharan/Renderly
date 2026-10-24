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

/// One recognised word with its time in the media.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimedWord {
    pub text: String,
    pub start_us: i64,
    pub end_us: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub start_us: i64,
    pub end_us: i64,
    pub text: String,
    /// Word timings inside the segment (media time). Empty when the recogniser reported none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<TimedWord>,
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
pub fn representative_frame_time(project: &Project) -> i64 {
    let mut earliest: Option<(i64, i64)> = None; // (start, timeline_duration)
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
                .map(|m| {
                    matches!(
                        m.kind,
                        MediaKind::Video | MediaKind::Image | MediaKind::Sequence
                    )
                })
                .unwrap_or(false);
            if !is_content {
                continue;
            }
            let start = v.start_us;
            if earliest.is_none_or(|(cur, _)| start < cur) {
                earliest = Some((start, v.timeline_duration_us()));
            }
        }
    }
    let Some((start, duration)) = earliest else {
        return 0;
    };
    let midpoint = start + duration / 2;
    (start + 1_000_000).min(midpoint).max(start)
}

/// Render the composited timeline frame at `time_us` as PNG bytes.
pub fn render_frame_png(
    project: &Project,
    time_us: i64,
    preset: ExportPreset,
) -> Result<Vec<u8>, PerceiveError> {
    let settings = ExportSettings::from_preset(&preset, project);
    let rgba = render_frame_at(project, time_us, settings)?;
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

    // Word timings: `-ojf` writes the full JSON (every token with its offsets), and `-ml 60
    // -sow` turns token timestamps on and splits segments at word boundaries into lines of at
    // most 60 characters. A whisper.cpp build that does not know those flags gets the plain
    // `-oj` call it always had: sentence-level segments, no words.
    let run = |flags: &[&str]| -> Result<bool, PerceiveError> {
        crate::proc::command(&whisper)
            .args(["-m", &model, "-f"])
            .arg(&wav)
            .args(flags)
            .arg("-of")
            .arg(&out_prefix)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status_serial()
            .map(|s| s.success())
            .map_err(|e| PerceiveError::WhisperFailed(e.to_string()))
    };
    if !run(&["-ojf", "-ml", "60", "-sow"])? && !run(&["-oj"])? {
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
            let start_us = offsets.from as i64 * 1000;
            let end_us = offsets.to as i64 * 1000;
            Some(TranscriptSegment {
                start_us,
                end_us,
                text: s.text.trim().to_string(),
                words: words_from_tokens(s.tokens.as_deref().unwrap_or_default()),
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
    /// Present in the full JSON (`-ojf`).
    tokens: Option<Vec<WhisperToken>>,
}

#[derive(Debug, Deserialize)]
struct WhisperOffsets {
    from: u64,
    to: u64,
}

#[derive(Debug, Deserialize)]
struct WhisperToken {
    text: String,
    offsets: Option<WhisperTokenOffsets>,
}

/// Token offsets can be negative when whisper has no timestamp for the token.
#[derive(Debug, Deserialize)]
struct WhisperTokenOffsets {
    from: i64,
    to: i64,
}

/// Group a segment's tokens into words. A token starting with whitespace starts a word; other
/// tokens (sub-word pieces, punctuation) extend the current one. Special tokens (`[_BEG_]`,
/// `[_TT_n]`) and tokens without a timestamp are skipped. Times are made monotonic: a word never
/// starts before the previous one ends.
fn words_from_tokens(tokens: &[WhisperToken]) -> Vec<TimedWord> {
    let mut words: Vec<TimedWord> = Vec::new();
    let mut open = false;
    for token in tokens {
        let raw = token.text.as_str();
        if raw.trim().is_empty() || raw.trim_start().starts_with("[_") {
            continue;
        }
        let Some(o) = token
            .offsets
            .as_ref()
            .filter(|o| o.from >= 0 && o.to >= o.from)
        else {
            continue;
        };
        let (from, to) = (o.from * 1000, o.to * 1000);
        if !open || raw.starts_with(char::is_whitespace) {
            words.push(TimedWord {
                text: raw.trim().to_string(),
                start_us: from,
                end_us: to,
            });
            open = true;
        } else if let Some(w) = words.last_mut() {
            w.text.push_str(raw.trim());
            w.end_us = w.end_us.max(to);
        }
    }
    let mut prev_end = 0;
    for w in &mut words {
        w.start_us = w.start_us.max(prev_end);
        w.end_us = w.end_us.max(w.start_us);
        prev_end = w.end_us;
    }
    // A segment whose tokens all share one timestamp carries no word timing at all.
    if words.len() > 1 && words.iter().all(|w| w.start_us == words[0].start_us) {
        return Vec::new();
    }
    words
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
        assert_eq!(segments[0].start_us, 0);
        assert!((segments[0].end_us - 2500000).abs() < 1);
        assert_eq!(segments[0].text, "he just does NOT miss");
        assert!((segments[1].start_us - 2500000).abs() < 1);
        assert!((segments[1].end_us - 4000000).abs() < 1);
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

    /// `whisper-cli -ojf -ml 60 -sow`: each segment lists its tokens with offsets in ms.
    const FULL_JSON: &str = r#"{
        "transcription": [
            {
                "offsets": { "from": 0, "to": 2000 },
                "text": " Hello, world!",
                "tokens": [
                    { "text": "[_BEG_]", "offsets": { "from": 0, "to": 0 } },
                    { "text": " Hello", "offsets": { "from": 100, "to": 600 } },
                    { "text": ",", "offsets": { "from": 600, "to": 700 } },
                    { "text": " wor", "offsets": { "from": 800, "to": 1100 } },
                    { "text": "ld", "offsets": { "from": 1100, "to": 1500 } },
                    { "text": "!", "offsets": { "from": 1500, "to": 1600 } },
                    { "text": "[_TT_100]", "offsets": { "from": 2000, "to": 2000 } }
                ]
            },
            {
                "offsets": { "from": 2000, "to": 3000 },
                "text": " Again",
                "tokens": [
                    { "text": " Again", "offsets": { "from": 2100, "to": 2900 } }
                ]
            }
        ]
    }"#;

    #[test]
    fn parse_whisper_json_groups_tokens_into_timed_words() {
        let segments = parse_whisper_json(FULL_JSON).unwrap();
        assert_eq!(segments.len(), 2);
        let words: Vec<(&str, i64, i64)> = segments[0]
            .words
            .iter()
            .map(|w| (w.text.as_str(), w.start_us, w.end_us))
            .collect();
        // Punctuation and word pieces extend the word they follow; special tokens are skipped.
        assert_eq!(
            words,
            vec![("Hello,", 100_000, 700_000), ("world!", 800_000, 1_600_000)]
        );
        assert_eq!(segments[1].words.len(), 1);
        assert_eq!(segments[1].words[0].text, "Again");
        // The segment itself is unchanged.
        assert_eq!(segments[0].text, "Hello, world!");
        assert_eq!(segments[0].end_us, 2_000_000);
    }

    #[test]
    fn plain_oj_output_has_no_words_and_untimed_tokens_give_none() {
        // The old `-oj` shape: no tokens at all.
        let plain = r#"{ "transcription": [
            { "offsets": { "from": 0, "to": 1000 }, "text": " hi there" } ] }"#;
        let segments = parse_whisper_json(plain).unwrap();
        assert!(segments[0].words.is_empty());
        // Tokens whose timestamps are all missing (negative) or identical carry no timing.
        let untimed = r#"{ "transcription": [
            { "offsets": { "from": 0, "to": 1000 }, "text": " hi there",
              "tokens": [
                { "text": " hi", "offsets": { "from": -1, "to": -1 } },
                { "text": " there", "offsets": { "from": -1, "to": -1 } } ] } ] }"#;
        assert!(parse_whisper_json(untimed).unwrap()[0].words.is_empty());
        let flat = r#"{ "transcription": [
            { "offsets": { "from": 0, "to": 1000 }, "text": " hi there",
              "tokens": [
                { "text": " hi", "offsets": { "from": 0, "to": 0 } },
                { "text": " there", "offsets": { "from": 0, "to": 0 } } ] } ] }"#;
        assert!(parse_whisper_json(flat).unwrap()[0].words.is_empty());
    }

    use crate::project::{MediaClip, MediaItem, Settings, Track};

    fn video_media(project: &mut Project, duration_us: i64) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        project.media.push(MediaItem {
            id,
            path: PathBuf::from("clip.mp4"),
            kind: MediaKind::Video,
            duration_us: Some(duration_us),
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            sequence_id: None,
        });
        id
    }

    /// Adds a video track with one enabled clip starting at `start_us` and spanning
    /// `duration_us` of timeline (via a 1x-speed in/out source range), wired to a fresh
    /// video media item.
    fn track_with_clip(project: &mut Project, start_us: i64, duration_us: i64) {
        let media_id = video_media(project, start_us + duration_us);
        let mut track = Track::new(TrackKind::Video, "V1");
        let clip = MediaClip {
            media_id,
            start_us,
            source_in_us: 0,
            source_out_us: duration_us,
            ..Default::default()
        };
        track.clips.push(Clip::Video(clip));
        project.tracks.push(track);
    }

    #[test]
    fn representative_frame_time_empty_project_is_zero() {
        let project = Project::new("empty", Settings::default());
        assert_eq!(representative_frame_time(&project), 0);
    }

    #[test]
    fn representative_frame_time_samples_a_second_into_first_clip() {
        let mut project = Project::new("p", Settings::default());
        track_with_clip(&mut project, 3000000, 10000000);
        // Inside the clip, 1s past its start — clear of a fade-from-black at the cut.
        assert!((representative_frame_time(&project) - 4000000).abs() < 1);
    }

    #[test]
    fn representative_frame_time_short_clip_uses_midpoint() {
        let mut project = Project::new("p", Settings::default());
        // 1.5s clip: start + 1.0 would overshoot past the midpoint, so the midpoint wins.
        track_with_clip(&mut project, 2000000, 1500000);
        assert!((representative_frame_time(&project) - 2750000).abs() < 1);
    }

    #[test]
    fn representative_frame_time_ignores_hidden_tracks_and_disabled_clips() {
        let mut project = Project::new("p", Settings::default());
        track_with_clip(&mut project, 500000, 5000000);
        project.tracks[0].hidden = true;
        // Second, later, visible track with a disabled clip — should be skipped too.
        track_with_clip(&mut project, 8000000, 5000000);
        if let Clip::Video(c) = &mut project.tracks[1].clips[0] {
            c.enabled = false;
        }
        assert_eq!(representative_frame_time(&project), 0);
    }
}
