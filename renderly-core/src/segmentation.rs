//! Local segmentation / background-removal helpers (Phase 4).
//!
//! Heavy ONNX inference is intentionally not linked yet — the design gate chose a
//! Whisper-style local CLI or a built-in heuristic for v1. Set `RENDERLY_SEG_CLI` to a
//! binary that reads an input PNG path (argv1) and writes an alpha PNG (argv2).

use crate::mask::generate_heuristic_matte;
use crate::media::RgbaFrame;
use crate::proc::SerialSpawn;
use crate::project::BackgroundRemoval;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SegmentationError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("segmentation CLI failed: {0}")]
    Cli(String),
    #[error("background removal is disabled")]
    Disabled,
}

pub fn model_available(model_id: &str) -> bool {
    match model_id {
        "heuristic" | "builtin:heuristic" => true,
        "cli" | "rvm" | "birefnet" => std::env::var_os("RENDERLY_SEG_CLI").is_some(),
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
    if model == "cli" || model == "rvm" || model == "birefnet" {
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
