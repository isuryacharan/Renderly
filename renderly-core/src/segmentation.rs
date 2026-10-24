//! Local segmentation / background-removal helpers (Phase 4).
//!
//! Three matte sources, selected by `BackgroundRemoval::model_id`:
//!
//! * `heuristic` — built-in luma/green threshold, one image for the whole clip;
//! * `cli` / `birefnet` — an external binary named by `RENDERLY_SEG_CLI` (reads an input PNG
//!   path as argv1, writes an alpha PNG as argv2), one image for the whole clip;
//! * `rvm` — Robust Video Matting run in-process ([`crate::ml::rvm`]), producing a **matte per
//!   source frame** ([`bake_rvm_matte`]) that export and the preview index by source time.
//!   The weights are never bundled or fetched implicitly: when they are not installed the bake
//!   fails with [`SegmentationError::ModelNotAvailable`].

use crate::mask::generate_heuristic_matte;
use crate::media::RgbaFrame;
use crate::ml::models;
use crate::ml::rvm::{RvmSession, RVM_MODEL_ID};
use crate::proc::SerialSpawn;
use crate::project::{BackgroundRemoval, Id, MatteFrames, MediaClip, MediaKind, Project};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SegmentationError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("segmentation CLI failed: {0}")]
    Cli(String),
    #[error("background removal is disabled")]
    Disabled,
    /// The matting weights are not installed. Never downloaded implicitly: fetch them with
    /// `ml::download_model` / `renderly models download <id>` after the user consented.
    #[error(
        "model {model_id} is not installed; download it first (renderly models download {model_id})"
    )]
    ModelNotAvailable { model_id: String },
    #[error("matte generation cancelled")]
    Cancelled,
    #[error("matting failed: {0}")]
    Ml(String),
    #[error("{0}")]
    Source(String),
}

/// `model_id`s that select the in-process Robust Video Matting backend.
pub fn is_rvm_model_id(model_id: &str) -> bool {
    matches!(model_id, "rvm" | RVM_MODEL_ID)
}

pub fn model_available(model_id: &str) -> bool {
    match model_id {
        "heuristic" | "builtin:heuristic" => true,
        id if is_rvm_model_id(id) => models::is_available(RVM_MODEL_ID),
        "cli" | "birefnet" => std::env::var_os("RENDERLY_SEG_CLI").is_some(),
        _ => false,
    }
}

/// Generate a matte for one RGBA frame and write `cache_dir/matte.png`.
pub fn bake_frame_matte(
    frame: &RgbaFrame,
    cfg: &BackgroundRemoval,
    cache_dir: &Path,
) -> Result<PathBuf, SegmentationError> {
    if !cfg.enabled {
        return Err(SegmentationError::Disabled);
    }
    std::fs::create_dir_all(cache_dir)?;
    let out = cache_dir.join("matte.png");
    let model = cfg.model_id.as_str();
    if model == "cli" || model == "birefnet" {
        if let Some(cli) = std::env::var_os("RENDERLY_SEG_CLI") {
            let input = cache_dir.join("input.png");
            image::save_buffer(
                &input,
                &frame.pixels,
                frame.width,
                frame.height,
                image::ColorType::Rgba8,
            )
            .map_err(|e| SegmentationError::Cli(e.to_string()))?;
            let status = crate::proc::command(cli)
                .arg(&input)
                .arg(&out)
                .status_serial()
                .map_err(|e| SegmentationError::Cli(e.to_string()))?;
            if !status.success() {
                return Err(SegmentationError::Cli(format!(
                    "exit {}",
                    status.code().unwrap_or(-1)
                )));
            }
            return Ok(out);
        }
    }
    let matte = generate_heuristic_matte(frame, cfg.threshold);
    matte
        .save(&out)
        .map_err(|e| SegmentationError::Io(std::io::Error::other(e.to_string())))?;
    Ok(out)
}

/// Cache dir of the per-frame matte sequence for one clip (a sibling of the single-image caches
/// in the temp dir; keyed by clip so two clips of one source never overwrite each other).
pub fn rvm_cache_dir(media_id: Id, clip_id: Id) -> PathBuf {
    std::env::temp_dir().join(format!("renderly-matte-rvm-{media_id}-{clip_id}"))
}

pub fn cache_key(media_id: uuid::Uuid, cfg: &BackgroundRemoval) -> String {
    format!(
        "{}-{}-{:.3}-{:.3}-{}",
        media_id,
        cfg.model_id,
        cfg.threshold,
        cfg.feather,
        if cfg.temporal { "t" } else { "s" }
    )
}

// ---------------------------------------------------------------------------------------------
// Per-frame matte sequences (in-process RVM)
// ---------------------------------------------------------------------------------------------

/// Provenance manifest written next to the frames; decides whether a baked sequence is still
/// valid for a clip.
pub const SIDECAR_FILE: &str = "matte.json";
const SIDECAR_VERSION: u32 = 1;
/// Mattes are decoded and stored at most this tall (the network only sees ~288 px anyway; the
/// compositor samples the matte bilinearly, so it is stretched to the picture).
pub const MATTE_MAX_HEIGHT: u32 = 720;
/// Highest matte frame rate; faster sources are decimated (the matte is looked up by nearest
/// source time).
pub const MATTE_MAX_FPS: f64 = 30.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatteSidecar {
    pub version: u32,
    /// Manifest id of the weights used.
    pub model_id: String,
    /// SHA-256 of the weights file actually loaded.
    pub model_sha256: String,
    pub source_path: String,
    pub source_mtime_ms: u64,
    pub source_size: u64,
    /// Source time of matte frame 0 and of the last frame.
    pub range_start_us: i64,
    pub range_end_us: i64,
    pub fps: f64,
    pub frame_count: u32,
    pub width: u32,
    pub height: u32,
    /// Milliseconds since the epoch at which the sequence was completed.
    pub stamp: u64,
}

impl MatteSidecar {
    pub fn frames(&self) -> MatteFrames {
        MatteFrames {
            start_us: self.range_start_us,
            fps: crate::time::Fps::from_f64(self.fps),
            count: self.frame_count,
            width: self.width,
            height: self.height,
            stamp: self.stamp,
        }
    }
}

/// Read `cache_dir/matte.json`.
pub fn read_sidecar(cache_dir: &Path) -> Option<MatteSidecar> {
    let text = std::fs::read_to_string(cache_dir.join(SIDECAR_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Everything [`bake_rvm_matte`] needs, captured from a project snapshot so the (long) bake can
/// run without holding the project.
#[derive(Debug, Clone, PartialEq)]
pub struct MatteJob {
    pub source_path: PathBuf,
    pub is_image: bool,
    /// Used source range of the clip (seconds).
    pub source_in_us: i64,
    pub source_out_us: i64,
    /// Native fps of the source (the bake caps it at [`MATTE_MAX_FPS`]).
    pub source_fps: f64,
    pub cache_dir: PathBuf,
}

/// Capture the bake inputs for `clip` (a media clip of `project`).
pub fn prepare_rvm_job(
    project: &Project,
    clip_id: Id,
    clip: &MediaClip,
) -> Result<MatteJob, SegmentationError> {
    let media = project
        .find_media(clip.media_id)
        .ok_or_else(|| SegmentationError::Source(format!("media not found: {}", clip.media_id)))?;
    let is_image = match media.kind {
        MediaKind::Video => false,
        MediaKind::Image => true,
        MediaKind::Audio => {
            return Err(SegmentationError::Source(
                "clip media has no video frames".into(),
            ))
        }
        MediaKind::Sequence => {
            return Err(SegmentationError::Source(
                "background removal is not supported on nested sequences".into(),
            ))
        }
    };
    Ok(MatteJob {
        source_path: media.path.clone(),
        is_image,
        source_in_us: clip.source_in_us.max(0),
        source_out_us: clip.source_out_us.max(clip.source_in_us),
        source_fps: media
            .fps
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(30.0),
        cache_dir: rvm_cache_dir(clip.media_id, clip_id),
    })
}

/// Progress of a running bake: `frame` of `total` (an upper bound; the source may end sooner).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct MatteProgress {
    pub frame: u32,
    pub total: u32,
}

fn file_identity(path: &Path) -> (u64, u64) {
    match std::fs::metadata(path) {
        Ok(m) => (
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_millis() as u64),
            m.len(),
        ),
        Err(_) => (0, 0),
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn write_gray_png(
    path: &Path,
    width: u32,
    height: u32,
    luma: &[u8],
) -> Result<(), SegmentationError> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::{ExtendedColorType, ImageEncoder};
    let file = std::fs::File::create(path)?;
    let enc = PngEncoder::new_with_quality(
        std::io::BufWriter::new(file),
        CompressionType::Fast,
        FilterType::Sub,
    );
    enc.write_image(luma, width, height, ExtendedColorType::L8)
        .map_err(|e| SegmentationError::Io(std::io::Error::other(e.to_string())))
}

/// Number of matte frames planned for a source span at `fps`: one per frame plus the closing
/// frame, so the clip's last source time maps to a real matte frame.
pub fn planned_frame_count(span_s: f64, fps: f64) -> u32 {
    ((span_s.max(0.0) * fps).round() as u32).saturating_add(1)
}

/// Locate the matting weights without touching the network: `Err(ModelNotAvailable)` when
/// neither the override dir nor the cache holds them. Returns the file and its SHA-256.
pub fn locate_rvm_model(
    override_dir: Option<&Path>,
    cache_dir: &Path,
) -> Result<(PathBuf, String), SegmentationError> {
    let spec = models::spec(RVM_MODEL_ID).map_err(|e| SegmentationError::Ml(e.to_string()))?;
    match models::resolve_in(spec, override_dir, cache_dir) {
        Ok(models::ModelSource::File(p)) => {
            let sha = models::sha256_hex(&std::fs::read(&p)?);
            Ok((p, sha))
        }
        Ok(models::ModelSource::Bundled(_)) | Err(models::ModelError::NotInstalled { .. }) => {
            Err(SegmentationError::ModelNotAvailable {
                model_id: RVM_MODEL_ID.into(),
            })
        }
        Err(e) => Err(SegmentationError::Ml(e.to_string())),
    }
}

/// Run Robust Video Matting over the job's source range and write the matte sequence
/// (`f000000.png`, ..., a representative `matte.png`, and `matte.json`) into `job.cache_dir`.
///
/// Frames are decoded sequentially, matted with the recurrent state carried from frame to frame
/// (reset at the start of the range), and stored as 8-bit grayscale PNGs. The sequence is built
/// in a sibling directory and swapped in only on success, so a failed or cancelled run leaves any
/// previous matte untouched. `on_progress` is called after each frame; return `false` to cancel
/// ([`SegmentationError::Cancelled`]).
///
/// Never downloads: without the weights this returns [`SegmentationError::ModelNotAvailable`].
pub fn bake_rvm_matte(
    job: &MatteJob,
    on_progress: &mut dyn FnMut(MatteProgress) -> bool,
) -> Result<MatteSidecar, SegmentationError> {
    let override_dir = std::env::var_os(models::DEFAULT_MODEL_DIR_ENV).map(PathBuf::from);
    let (_, model_sha256) = locate_rvm_model(override_dir.as_deref(), &models::model_cache_dir())?;

    let parent = job
        .cache_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let part_dir = parent.join(format!(
        "{}.part-{}",
        job.cache_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "matte".into()),
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&part_dir)?;
    let result = bake_into(job, &part_dir, &model_sha256, on_progress).and_then(|sidecar| {
        // Swap the finished sequence in. Removing the old dir first keeps stale frames from a
        // longer earlier run out of the new sequence.
        if job.cache_dir.exists() {
            std::fs::remove_dir_all(&job.cache_dir)?;
        }
        std::fs::rename(&part_dir, &job.cache_dir)?;
        Ok(sidecar)
    });
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&part_dir);
    }
    result
}

fn bake_into(
    job: &MatteJob,
    dir: &Path,
    model_sha256: &str,
    on_progress: &mut dyn FnMut(MatteProgress) -> bool,
) -> Result<MatteSidecar, SegmentationError> {
    let fps = if job.is_image {
        1.0
    } else {
        job.source_fps.clamp(1.0, MATTE_MAX_FPS)
    };
    let start = if job.is_image { 0 } else { job.source_in_us };
    let span = if job.is_image {
        0
    } else {
        job.source_out_us - job.source_in_us
    };
    let total = planned_frame_count(crate::time::us_to_seconds(span), fps);

    // Frame source: one decoder for the whole range, or the still image once.
    let mut still: Option<RgbaFrame> = None;
    let mut reader = if job.is_image {
        let img = image::open(&job.source_path)
            .map_err(|e| SegmentationError::Source(e.to_string()))?
            .to_rgba8();
        still = Some(RgbaFrame {
            width: img.width(),
            height: img.height(),
            pixels: img.into_raw(),
        });
        None
    } else {
        Some(
            crate::media::VideoReader::open_with(
                &job.source_path,
                crate::time::us_to_seconds(start),
                &crate::media::ReaderOptions {
                    target_height: Some(MATTE_MAX_HEIGHT),
                    output_fps: Some(fps),
                },
            )
            .map_err(|e| SegmentationError::Source(e.to_string()))?,
        )
    };

    let mut session: Option<RvmSession> = None;
    let mut size = (0u32, 0u32);
    let mut written = 0u32;
    while written < total {
        let frame = match (&mut reader, &still) {
            (Some(r), _) => r
                .read_frame()
                .map_err(|e| SegmentationError::Source(e.to_string()))?,
            (None, Some(f)) => Some(f.clone()),
            (None, None) => None,
        };
        let Some(frame) = frame else {
            break;
        };
        if session.is_none() {
            size = (frame.width, frame.height);
            session = Some(map_ml(RvmSession::new(frame.width, frame.height))?);
        }
        let Some(session) = session.as_mut() else {
            break;
        };
        if (frame.width, frame.height) != size {
            return Err(SegmentationError::Source(
                "source frame size changed mid-clip".into(),
            ));
        }
        let alpha = map_ml(session.process(&frame.pixels))?;
        write_gray_png(
            &dir.join(MatteFrames::file_name(written)),
            size.0,
            size.1,
            &alpha,
        )?;
        written += 1;
        if !on_progress(MatteProgress {
            frame: written,
            total,
        }) {
            return Err(SegmentationError::Cancelled);
        }
    }
    if written == 0 {
        return Err(SegmentationError::Source(
            "no frames could be decoded from the source range".into(),
        ));
    }

    // A single representative frame keeps single-image consumers (older builds, the preview
    // before it has fetched frames) working: the middle of the sequence.
    std::fs::copy(
        dir.join(MatteFrames::file_name(written / 2)),
        dir.join(crate::mask::GENERATED_MATTE_FILE),
    )?;

    let (mtime, len) = file_identity(&job.source_path);
    let sidecar = MatteSidecar {
        version: SIDECAR_VERSION,
        model_id: RVM_MODEL_ID.into(),
        model_sha256: model_sha256.into(),
        source_path: job.source_path.to_string_lossy().into_owned(),
        source_mtime_ms: mtime,
        source_size: len,
        range_start_us: start,
        range_end_us: start + crate::time::seconds_to_us(f64::from(written - 1) / fps),
        fps,
        frame_count: written,
        width: size.0,
        height: size.1,
        stamp: now_ms(),
    };
    let json = serde_json::to_string_pretty(&sidecar)
        .map_err(|e| SegmentationError::Io(std::io::Error::other(e.to_string())))?;
    std::fs::write(dir.join(SIDECAR_FILE), json)?;
    Ok(sidecar)
}

fn map_ml<T>(r: Result<T, crate::ml::MlError>) -> Result<T, SegmentationError> {
    r.map_err(|e| match e {
        crate::ml::MlError::Model(models::ModelError::NotInstalled { .. }) => {
            SegmentationError::ModelNotAvailable {
                model_id: RVM_MODEL_ID.into(),
            }
        }
        other => SegmentationError::Ml(other.to_string()),
    })
}

/// State of a clip's baked matte, for UIs and agents.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MatteStatus {
    /// No matte has been generated (or background removal is off / uses the live heuristic).
    NotGenerated,
    /// A matte exists and matches the clip's source and used range.
    Ready { frames: Option<u32> },
    /// A matte exists but no longer matches: the source file changed, the clip now uses source
    /// time the matte does not cover, the model changed, or the cache files are gone.
    Stale { reason: String },
}

/// Compare a sidecar against the clip's current source and used range.
pub fn sidecar_staleness(
    sidecar: &MatteSidecar,
    source_path: &Path,
    source_in_us: i64,
    source_out_us: i64,
) -> Option<String> {
    if sidecar.version != SIDECAR_VERSION {
        return Some("matte was written by an incompatible version".into());
    }
    if sidecar.model_id != RVM_MODEL_ID {
        return Some("matte was made with a different model".into());
    }
    if Path::new(&sidecar.source_path) != source_path {
        return Some("source file path changed".into());
    }
    let (mtime, len) = file_identity(source_path);
    if mtime != sidecar.source_mtime_ms || len != sidecar.source_size {
        return Some("source file changed on disk".into());
    }
    let frame = (1_000_000.0 / sidecar.fps.max(1.0)).round() as i64;
    if source_in_us < sidecar.range_start_us - frame / 2
        || source_out_us > sidecar.range_end_us + frame * 3 / 2
    {
        return Some("clip uses source time outside the generated range".into());
    }
    None
}

/// Status of `clip`'s background-removal matte.
pub fn matte_status(project: &Project, clip: &MediaClip) -> MatteStatus {
    let Some(cfg) = clip.background_removal.as_ref() else {
        return MatteStatus::NotGenerated;
    };
    let Some(dir) = cfg.matte_cache_dir.as_ref() else {
        return MatteStatus::NotGenerated;
    };
    if !is_rvm_model_id(&cfg.model_id) {
        return if dir.join(crate::mask::GENERATED_MATTE_FILE).is_file() {
            MatteStatus::Ready { frames: None }
        } else {
            MatteStatus::Stale {
                reason: "matte file is missing".into(),
            }
        };
    }
    let Some(sidecar) = read_sidecar(dir) else {
        return MatteStatus::Stale {
            reason: "matte cache is missing".into(),
        };
    };
    let Some(media) = project.find_media(clip.media_id) else {
        return MatteStatus::Stale {
            reason: "source media not found".into(),
        };
    };
    match sidecar_staleness(&sidecar, &media.path, clip.source_in_us, clip.source_out_us) {
        Some(reason) => MatteStatus::Stale { reason },
        None => MatteStatus::Ready {
            frames: Some(sidecar.frame_count),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("renderly-seg-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn sidecar_for(path: &Path) -> MatteSidecar {
        let (mtime, len) = file_identity(path);
        MatteSidecar {
            version: SIDECAR_VERSION,
            model_id: RVM_MODEL_ID.into(),
            model_sha256: "x".into(),
            source_path: path.to_string_lossy().into_owned(),
            source_mtime_ms: mtime,
            source_size: len,
            range_start_us: 2000000,
            range_end_us: 4000000,
            fps: 30.0,
            frame_count: 61,
            width: 8,
            height: 8,
            stamp: 1,
        }
    }

    #[test]
    fn planned_frames_cover_the_closing_frame() {
        assert_eq!(planned_frame_count(2.0, 30.0), 61);
        assert_eq!(planned_frame_count(0.0, 30.0), 1);
        assert_eq!(planned_frame_count(-1.0, 30.0), 1);
    }

    #[test]
    fn staleness_tracks_source_and_range() {
        let dir = tmp("stale");
        let src = dir.join("src.bin");
        std::fs::write(&src, b"abc").unwrap();
        let sc = sidecar_for(&src);

        // Used range inside the baked one: ready (split / trimmed-down clips stay valid).
        assert_eq!(sidecar_staleness(&sc, &src, 2000000, 4000000), None);
        assert_eq!(sidecar_staleness(&sc, &src, 2500000, 3000000), None);
        // Extending the trim past the baked range makes it stale.
        assert!(sidecar_staleness(&sc, &src, 1000000, 4000000)
            .unwrap()
            .contains("outside"));
        assert!(sidecar_staleness(&sc, &src, 2000000, 5000000).is_some());
        // Different path / model / version.
        let other = dir.join("other.bin");
        std::fs::write(&other, b"abc").unwrap();
        assert!(sidecar_staleness(&sc, &other, 2000000, 4000000)
            .unwrap()
            .contains("path"));
        let mut m = sidecar_for(&src);
        m.model_id = "other".into();
        assert!(sidecar_staleness(&m, &src, 2000000, 4000000)
            .unwrap()
            .contains("model"));
        let mut v = sidecar_for(&src);
        v.version = 99;
        assert!(sidecar_staleness(&v, &src, 2000000, 4000000).is_some());
        // Source changed on disk (size differs).
        std::fs::write(&src, b"abcdef").unwrap();
        assert!(sidecar_staleness(&sc, &src, 2000000, 4000000)
            .unwrap()
            .contains("changed on disk"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sidecar_round_trips_and_maps_to_frames() {
        let dir = tmp("sidecar");
        let sc = sidecar_for(&dir.join("src.bin"));
        std::fs::write(dir.join(SIDECAR_FILE), serde_json::to_string(&sc).unwrap()).unwrap();
        assert_eq!(read_sidecar(&dir), Some(sc.clone()));
        let f = sc.frames();
        assert_eq!(
            (f.count, f.fps, f.start_us),
            (61, crate::time::Fps::new(30, 1), 2000000)
        );
        // Frame lookup: nearest, clamped at both ends.
        assert_eq!(f.index_at(2000000), 0);
        assert_eq!(f.index_at(2_000_000 + 33_333), 1);
        assert_eq!(f.index_at(2_000_000 + 13_333), 0);
        assert_eq!(f.index_at(2_000_000 + 20_000), 1);
        assert_eq!(f.index_at(100000000), 60);
        assert_eq!(f.index_at(-5000000), 0);
        assert_eq!(MatteFrames::file_name(7), "f000007.png");
        assert_eq!(read_sidecar(&dir.join("nope")), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_weights_are_model_not_available_without_any_download() {
        // Empty override dir and empty cache dir: resolution is a pure filesystem check, so
        // this can only fail with the typed error - it has no way to reach the network.
        let empty = tmp("nomodel");
        let r = locate_rvm_model(Some(&empty), &empty);
        match r {
            Err(SegmentationError::ModelNotAvailable { model_id }) => {
                assert_eq!(model_id, RVM_MODEL_ID);
            }
            other => panic!("unexpected: {other:?}"),
        }
        // A wrong-sized file in the cache is not the model either.
        let spec = models::spec(RVM_MODEL_ID).unwrap();
        std::fs::write(empty.join(spec.file_name), b"truncated").unwrap();
        assert!(matches!(
            locate_rvm_model(None, &empty),
            Err(SegmentationError::ModelNotAvailable { .. })
        ));
        std::fs::remove_dir_all(&empty).ok();
    }
}
