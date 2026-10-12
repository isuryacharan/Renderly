//! Clip mask / matte application (Phase 4). CPU path shared by preview and export.
//!
//! Also home to the cross-target matte plumbing the wasm preview uses: [`matte_cache_key`] /
//! [`matte_file_path`] / [`project_matte_requests`] tell the host which matte files a project
//! needs, and [`luma_from_rgba`] is the exact RGBA->luma8 conversion `image`'s `to_luma8`
//! performs, so a matte decoded by the browser lands in the compositor as the same bytes the
//! native `image::open(..).to_luma8()` path produces.

use std::path::PathBuf;

use crate::frame::RgbaFrame;
use crate::project::{ClipMask, ClipMaskKind, MaskShape, Project};

/// File name of the baked matte inside a `Generated` mask's / background-removal's cache dir.
pub const GENERATED_MATTE_FILE: &str = "matte.png";

/// Compositor matte-cache key for a file-backed matte kind (`None` for shape / `Luma` masks,
/// which need no external data). The wasm host registers decoded mattes under this key
/// (`Compositor::set_matte`); native export derives the same key when it loads from disk.
pub fn matte_cache_key(kind: &ClipMaskKind) -> Option<String> {
    match kind {
        ClipMaskKind::Raster { path } => Some(format!("raster:{}", path.display())),
        ClipMaskKind::Generated { cache_dir } => Some(format!("gen:{}", cache_dir.display())),
        _ => None,
    }
}

/// The image file backing a file-backed matte kind.
pub fn matte_file_path(kind: &ClipMaskKind) -> Option<PathBuf> {
    match kind {
        ClipMaskKind::Raster { path } => Some(path.clone()),
        ClipMaskKind::Generated { cache_dir } => Some(cache_dir.join(GENERATED_MATTE_FILE)),
        _ => None,
    }
}

/// One matte a host must supply to the compositor before the project's file-backed masks
/// render: decode `path`, then hand RGBA to `Compositor::set_matte(key, ..)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatteRequest {
    pub key: String,
    pub path: PathBuf,
}

/// The mask a media clip is composited with when it has a *file-backed* matte, i.e. exactly
/// what export's background-removal resolution picks when a baked matte cache exists:
/// `Generated` from `matte_cache_dir`. Shared by export and the preview so they cannot differ.
pub fn cached_background_mask(cfg: &crate::project::BackgroundRemoval) -> Option<ClipMask> {
    cfg.matte_cache_dir.as_ref().map(|dir| ClipMask {
        enabled: true,
        invert: false,
        feather: cfg.feather,
        kind: ClipMaskKind::Generated {
            cache_dir: dir.clone(),
        },
    })
}

/// Every file-backed matte referenced by `project` (clip masks and baked background-removal
/// mattes), deduplicated by key, in track/clip order. Pure data walk - no filesystem access.
pub fn project_matte_requests(project: &Project) -> Vec<MatteRequest> {
    let mut out: Vec<MatteRequest> = Vec::new();
    let mut push = |kind: &ClipMaskKind| {
        if let (Some(key), Some(path)) = (matte_cache_key(kind), matte_file_path(kind)) {
            if !out.iter().any(|r| r.key == key) {
                out.push(MatteRequest { key, path });
            }
        }
    };
    // Root timeline first, then every sequence (nested clips and adjustment layers carry
    // masks too).
    for track in project.all_tracks() {
        for clip in &track.clips {
            if let Some(adjustment) = clip.as_adjustment() {
                if let Some(mask) = adjustment.mask.as_ref() {
                    push(&mask.kind);
                }
                continue;
            }
            let Some(media) = clip.as_media() else {
                continue;
            };
            if let Some(bg) = media.background_removal.as_ref().filter(|b| b.enabled) {
                if let Some(mask) = cached_background_mask(bg) {
                    push(&mask.kind);
                    continue;
                }
            }
            if let Some(mask) = media.mask.as_ref() {
                push(&mask.kind);
            }
        }
    }
    out
}

/// RGBA8 -> single-channel luma8 with the same integer coefficients and truncation as
/// `image::DynamicImage::to_luma8` (ITU-R BT.709 weights 2126/7152/722 over 10000; alpha
/// ignored). `rgba.len()` must be a multiple of 4 (a trailing partial pixel is dropped).
pub fn luma_from_rgba(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .map(|p| ((2126 * p[0] as u32 + 7152 * p[1] as u32 + 722 * p[2] as u32) / 10000) as u8)
        .collect()
}

/// Apply a clip mask to an RGBA frame in place (modulates alpha).
///
/// `Raster`/`Generated` (filesystem-backed) mattes are only applied on native builds —
/// they go through the `image` crate's file decode, which is unavailable on wasm32. `Luma`
/// (raw in-memory pixels) works everywhere. (This CPU path is not used by the GPU
/// compositor; the wasm preview feeds decoded file mattes through `Compositor::set_matte`.)
pub fn apply_clip_mask(frame: &mut RgbaFrame, mask: &ClipMask) {
    if !mask.enabled {
        return;
    }
    match &mask.kind {
        ClipMaskKind::None => {}
        ClipMaskKind::Rect { .. } | ClipMaskKind::Ellipse { .. } => {
            if let Some(shape) = mask.kind.as_shape() {
                apply_shape_mask(frame, &shape, mask);
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        ClipMaskKind::Raster { path } => {
            if let Ok(matte) = image::open(path) {
                let gray = matte.to_luma8();
                apply_raster_matte(
                    frame,
                    gray.as_raw(),
                    gray.width(),
                    gray.height(),
                    mask.invert,
                    mask.feather,
                );
            }
        }
        #[cfg(target_arch = "wasm32")]
        ClipMaskKind::Raster { .. } => {}
        #[cfg(not(target_arch = "wasm32"))]
        ClipMaskKind::Generated { cache_dir } => {
            let matte_path = std::path::Path::new(cache_dir).join(GENERATED_MATTE_FILE);
            if let Ok(matte) = image::open(&matte_path) {
                let gray = matte.to_luma8();
                apply_raster_matte(
                    frame,
                    gray.as_raw(),
                    gray.width(),
                    gray.height(),
                    mask.invert,
                    mask.feather,
                );
            }
        }
        #[cfg(target_arch = "wasm32")]
        ClipMaskKind::Generated { .. } => {}
        ClipMaskKind::Luma {
            pixels,
            width,
            height,
        } => {
            apply_raster_matte(frame, pixels, *width, *height, mask.invert, mask.feather);
        }
    }
}

fn apply_shape_mask(frame: &mut RgbaFrame, shape: &MaskShape, mask: &ClipMask) {
    let w = frame.width as f32;
    let h = frame.height as f32;
    let feather = mask.feather.clamp(0.0, 1.0) as f32;
    for y in 0..frame.height {
        for x in 0..frame.width {
            let u = (x as f32 + 0.5) / w;
            let v = (y as f32 + 0.5) / h;
            let mut a = shape_alpha(shape, u, v, feather);
            if mask.invert {
                a = 1.0 - a;
            }
            let idx = ((y * frame.width + x) * 4) as usize;
            let src_a = frame.pixels[idx + 3] as f32 / 255.0;
            frame.pixels[idx + 3] = ((src_a * a) * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
}

fn shape_alpha(shape: &MaskShape, u: f32, v: f32, feather: f32) -> f32 {
    match shape {
        MaskShape::Rect {
            x,
            y,
            width,
            height,
        } => {
            let x0 = *x as f32;
            let y0 = *y as f32;
            let x1 = x0 + (*width as f32).max(0.0);
            let y1 = y0 + (*height as f32).max(0.0);
            let dx = if u < x0 {
                x0 - u
            } else if u > x1 {
                u - x1
            } else {
                0.0
            };
            let dy = if v < y0 {
                y0 - v
            } else if v > y1 {
                v - y1
            } else {
                0.0
            };
            let dist = (dx * dx + dy * dy).sqrt();
            soft_edge(dist, feather * 0.15)
        }
        MaskShape::Ellipse { cx, cy, rx, ry } => {
            let rx = (*rx as f32).max(1e-4);
            let ry = (*ry as f32).max(1e-4);
            let nx = (u - *cx as f32) / rx;
            let ny = (v - *cy as f32) / ry;
            let d = (nx * nx + ny * ny).sqrt();
            soft_edge((d - 1.0).max(0.0), feather.max(0.001))
        }
    }
}

fn soft_edge(dist_outside: f32, feather: f32) -> f32 {
    if dist_outside <= 0.0 {
        1.0
    } else if feather <= 1e-6 {
        0.0
    } else {
        (1.0 - dist_outside / feather).clamp(0.0, 1.0)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn apply_luma_matte(
    frame: &mut RgbaFrame,
    matte: &image::GrayImage,
    invert: bool,
    feather: f64,
) {
    apply_raster_matte(
        frame,
        matte.as_raw(),
        matte.width(),
        matte.height(),
        invert,
        feather,
    );
}

/// Modulate `frame`'s alpha by a single-channel (luma) matte given as raw, row-major,
/// unpadded bytes — no `image` crate dependency, so this is usable on wasm32.
fn apply_raster_matte(
    frame: &mut RgbaFrame,
    matte: &[u8],
    mw: u32,
    mh: u32,
    invert: bool,
    feather: f64,
) {
    let feather = feather.clamp(0.0, 1.0) as f32;
    for y in 0..frame.height {
        for x in 0..frame.width {
            let mx = (x as u64 * mw as u64 / frame.width.max(1) as u64) as u32;
            let my = (y as u64 * mh as u64 / frame.height.max(1) as u64) as u32;
            let midx = (my.min(mh.saturating_sub(1)) * mw + mx.min(mw.saturating_sub(1))) as usize;
            let mut a = matte.get(midx).copied().unwrap_or(255) as f32 / 255.0;
            if invert {
                a = 1.0 - a;
            }
            // Light feather as contrast curve around mid-gray.
            if feather > 0.0 {
                let t = feather * 0.5;
                a = smoothstep(0.5 - t, 0.5 + t, a);
            }
            let idx = ((y * frame.width + x) * 4) as usize;
            let src_a = frame.pixels[idx + 3] as f32 / 255.0;
            frame.pixels[idx + 3] = ((src_a * a) * 255.0).clamp(0.0, 255.0) as u8;
        }
    }
}

fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0).max(1e-6)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Heuristic / CLI segmentation matte used when no ONNX runtime is linked. Native-only
/// (used by `segmentation`, which is excluded on wasm32).
#[cfg(not(target_arch = "wasm32"))]
pub fn generate_heuristic_matte(frame: &RgbaFrame, threshold: f64) -> image::GrayImage {
    let threshold = (threshold.clamp(0.0, 1.0) * 255.0) as u8;
    let mut out = image::GrayImage::new(frame.width, frame.height);
    for y in 0..frame.height {
        for x in 0..frame.width {
            let i = ((y * frame.width + x) * 4) as usize;
            let r = frame.pixels[i] as i32;
            let g = frame.pixels[i + 1] as i32;
            let b = frame.pixels[i + 2] as i32;
            // Subject = not green-screen-ish and above luma threshold.
            let luma = ((r * 30 + g * 59 + b * 11) / 100) as u8;
            let green_spill = g > r + 20 && g > b + 20;
            let alpha = if green_spill || luma < threshold {
                0
            } else {
                255
            };
            out.put_pixel(x, y, image::Luma([alpha]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{ClipMask, ClipMaskKind};

    #[test]
    fn rect_mask_zeros_outside_alpha() {
        let mut frame = RgbaFrame {
            width: 4,
            height: 4,
            pixels: vec![255u8; 4 * 4 * 4],
        };
        let mask = ClipMask {
            enabled: true,
            invert: false,
            feather: 0.0,
            kind: ClipMaskKind::Rect {
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            },
        };
        apply_clip_mask(&mut frame, &mask);
        // Corner pixel should be transparent.
        assert_eq!(frame.pixels[3], 0);
        // Center-ish pixel (2,2) should stay opaque.
        let idx = ((2 * 4 + 2) * 4 + 3) as usize;
        assert_eq!(frame.pixels[idx], 255);
    }

    #[test]
    fn luma_from_rgba_matches_bt709_truncation() {
        // gray stays exact; pure channels use the 2126/7152/722 weights, alpha ignored.
        let px = [
            200, 200, 200, 7, //
            255, 0, 0, 255, //
            0, 255, 0, 0, //
            0, 0, 255, 128,
        ];
        assert_eq!(luma_from_rgba(&px), vec![200, 54, 182, 18]);
    }

    #[test]
    fn matte_requests_cover_masks_and_baked_background() {
        use crate::project::{BackgroundRemoval, Settings, Track, TrackKind};
        let mut project = Project::new("t", Settings::default());
        let mut track = Track::new(TrackKind::Video, "V");
        let a = crate::project::MediaClip {
            mask: Some(ClipMask {
                enabled: true,
                invert: false,
                feather: 0.0,
                kind: ClipMaskKind::Raster {
                    path: "m.png".into(),
                },
            }),
            ..Default::default()
        };
        let b = crate::project::MediaClip {
            background_removal: Some(BackgroundRemoval {
                enabled: true,
                model_id: "heuristic".into(),
                threshold: 0.5,
                feather: 0.1,
                temporal: false,
                matte_cache_dir: Some("cache".into()),
            }),
            ..Default::default()
        };
        track.clips.push(crate::project::Clip::Video(a));
        track.clips.push(crate::project::Clip::Video(b));
        project.tracks.push(track);
        let reqs = project_matte_requests(&project);
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].key, "raster:m.png");
        assert_eq!(reqs[1].key, "gen:cache");
        assert_eq!(reqs[1].path, PathBuf::from("cache").join("matte.png"));
    }
}
