//! Robust Video Matting (MobileNetV3, fp32 ONNX) running on tract.
//!
//! Written from the model's documented input/output contract only (no upstream source is
//! used): the graph takes
//!
//! * `src`: `1x3xHxW` f32, **RGB in 0..1** (no mean/std);
//! * `r1i..r4i`: the recurrent state, `1x1x1x1` zeros for the first frame and afterwards the
//!   previous frame's `r1o..r4o` (whose shapes depend on the input size);
//! * `downsample_ratio`: a `[1]` f32 scalar, the factor by which the backbone sees a smaller copy
//!   of `src` (guidance: pick it so the downsampled long edge is 256..512 px);
//!
//! and returns `fgr`, `pha`, `r1o..r4o`. Only `pha` (the alpha matte, `1x1xHxW`, 0..1) is used.
//!
//! tract needs concrete shapes, so two plans exist per working resolution: one for the first
//! frame (`1x1x1x1` states) and one for every later frame (the real state shapes, learnt from the
//! first run's outputs). Both are cached for the process lifetime by `load_cached_plan`.
//!
//! The model is a heavy network; to stay practical on a CPU the frame is shrunk to a *working
//! size* (long edge <= [`WORKING_LONG_EDGE`], multiples of 32, aspect preserved), run there, and
//! the alpha is bilinearly resampled back to the requested size.

use super::runtime::{load_cached_plan, F32Tensor, InputFact, MlError, Plan};
use image::imageops::FilterType;
use std::sync::Arc;

/// Manifest id of the weights (see `models.rs`).
pub const RVM_MODEL_ID: &str = "rvm-mobilenetv3-fp32";

/// Longest side of the network input.
pub const WORKING_LONG_EDGE: u32 = 512;

/// Network input size and backbone downsample ratio chosen for a source size.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkingSize {
    pub width: u32,
    pub height: u32,
    pub downsample_ratio: f32,
}

/// Pick the working resolution for a `src_w x src_h` frame: scale down so the long edge is at
/// most [`WORKING_LONG_EDGE`] (never up), round each side to the nearest multiple of 32 (the
/// backbone's strides need `size * ratio` divisible by 16), and choose `downsample_ratio` so the
/// backbone's long edge lands in the documented 256..512 range: `0.5` for a full-size 512 px
/// input, `1.0` for smaller ones.
pub fn working_size(src_w: u32, src_h: u32) -> WorkingSize {
    let (sw, sh) = (src_w.max(1) as f64, src_h.max(1) as f64);
    let scale = (f64::from(WORKING_LONG_EDGE) / sw.max(sh)).min(1.0);
    let snap = |v: f64| (((v * scale) / 32.0).round() as u32).max(1) * 32;
    let (width, height) = (snap(sw), snap(sh));
    let downsample_ratio = if width.max(height) >= WORKING_LONG_EDGE {
        0.5
    } else {
        1.0
    };
    WorkingSize {
        width,
        height,
        downsample_ratio,
    }
}

/// RGBA8 (`src_w x src_h`, alpha ignored) -> `1x3xHxW` f32 RGB in 0..1 at the working size
/// (antialiased triangle-filter resize when the sizes differ).
pub fn preprocess_rgba(
    rgba: &[u8],
    src_w: u32,
    src_h: u32,
    work: &WorkingSize,
) -> Result<F32Tensor, MlError> {
    let (sw, sh) = (src_w as usize, src_h as usize);
    if sw == 0 || sh == 0 || rgba.len() != sw * sh * 4 {
        return Err(MlError::TensorSize {
            shape: vec![sh, sw, 4],
            expected: sw * sh * 4,
            got: rgba.len(),
        });
    }
    let (ww, wh) = (work.width as usize, work.height as usize);
    let resized;
    let pixels: &[u8] = if (src_w, src_h) == (work.width, work.height) {
        rgba
    } else {
        let img = image::RgbaImage::from_raw(src_w, src_h, rgba.to_vec()).ok_or_else(|| {
            MlError::TensorSize {
                shape: vec![sh, sw, 4],
                expected: sw * sh * 4,
                got: rgba.len(),
            }
        })?;
        resized = image::imageops::resize(&img, work.width, work.height, FilterType::Triangle);
        resized.as_raw()
    };
    let plane = ww * wh;
    let mut data = vec![0.0f32; 3 * plane];
    for (i, px) in pixels.as_chunks::<4>().0.iter().enumerate() {
        data[i] = f32::from(px[0]) / 255.0;
        data[plane + i] = f32::from(px[1]) / 255.0;
        data[2 * plane + i] = f32::from(px[2]) / 255.0;
    }
    F32Tensor::new(vec![1, 3, wh, ww], data)
}

/// Bilinear resample (half-pixel centres, edge clamped) of a `sw x sh` f32 alpha plane in 0..1
/// to a `dw x dh` 8-bit matte. Pure and deterministic.
pub fn upscale_alpha(alpha: &[f32], sw: usize, sh: usize, dw: usize, dh: usize) -> Vec<u8> {
    let mut out = vec![0u8; dw * dh];
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || alpha.len() < sw * sh {
        return out;
    }
    let fx = sw as f32 / dw as f32;
    let fy = sh as f32 / dh as f32;
    // Per-column source taps are the same for every row.
    let cols: Vec<(usize, usize, f32)> = (0..dw)
        .map(|x| {
            let sx = ((x as f32 + 0.5) * fx - 0.5).clamp(0.0, (sw - 1) as f32);
            let x0 = sx.floor() as usize;
            (x0, (x0 + 1).min(sw - 1), sx - x0 as f32)
        })
        .collect();
    for y in 0..dh {
        let sy = ((y as f32 + 0.5) * fy - 0.5).clamp(0.0, (sh - 1) as f32);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(sh - 1);
        let ty = sy - y0 as f32;
        let (r0, r1) = (
            &alpha[y0 * sw..(y0 + 1) * sw],
            &alpha[y1 * sw..(y1 + 1) * sw],
        );
        for (x, &(x0, x1, tx)) in cols.iter().enumerate() {
            let top = r0[x0] + (r0[x1] - r0[x0]) * tx;
            let bot = r1[x0] + (r1[x1] - r1[x0]) * tx;
            let v = top + (bot - top) * ty;
            out[y * dw + x] = (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    out
}

/// One matting pass over consecutive frames of a single clip: carries the recurrent state from
/// frame to frame. Create a fresh session (or call [`Self::reset`]) at every clip start.
pub struct RvmSession {
    src_w: u32,
    src_h: u32,
    work: WorkingSize,
    first_plan: Arc<Plan>,
    step_plan: Option<Arc<Plan>>,
    state: Option<[F32Tensor; 4]>,
}

impl RvmSession {
    /// Resolve the model (never downloads: [`MlError::Model`] with `NotInstalled` when absent)
    /// and prepare the first-frame plan for `src_w x src_h` input frames.
    pub fn new(src_w: u32, src_h: u32) -> Result<Self, MlError> {
        let work = working_size(src_w, src_h);
        let first_plan = load_cached_plan(RVM_MODEL_ID, &Self::facts(&work, None))?;
        Ok(Self {
            src_w,
            src_h,
            work,
            first_plan,
            step_plan: None,
            state: None,
        })
    }

    pub fn working_size(&self) -> WorkingSize {
        self.work
    }

    /// Forget the recurrent state (next frame is a cold start).
    pub fn reset(&mut self) {
        self.state = None;
    }

    fn facts(work: &WorkingSize, state_shapes: Option<&[Vec<usize>; 4]>) -> Vec<InputFact> {
        let mut facts = vec![InputFact::Shape(vec![
            1,
            3,
            work.height as usize,
            work.width as usize,
        ])];
        for i in 0..4 {
            facts.push(InputFact::Shape(
                state_shapes.map_or_else(|| vec![1, 1, 1, 1], |s| s[i].clone()),
            ));
        }
        facts.push(InputFact::Const(F32Tensor {
            shape: vec![1],
            data: vec![work.downsample_ratio],
        }));
        facts
    }

    /// Matte one RGBA8 frame of the session's source size; returns a `src_w x src_h` 8-bit alpha
    /// (255 = subject). Frames must be fed in order for the temporal state to help.
    pub fn process(&mut self, rgba: &[u8]) -> Result<Vec<u8>, MlError> {
        let src = preprocess_rgba(rgba, self.src_w, self.src_h, &self.work)?;
        let mut inputs = Vec::with_capacity(5);
        inputs.push(src);
        let plan = match &self.state {
            Some(state) => {
                inputs.extend(state.iter().cloned());
                match &self.step_plan {
                    Some(p) => p.clone(),
                    None => {
                        let shapes = [
                            state[0].shape.clone(),
                            state[1].shape.clone(),
                            state[2].shape.clone(),
                            state[3].shape.clone(),
                        ];
                        let p = load_cached_plan(
                            RVM_MODEL_ID,
                            &Self::facts(&self.work, Some(&shapes)),
                        )?;
                        self.step_plan = Some(p.clone());
                        p
                    }
                }
            }
            None => {
                inputs.extend((0..4).map(|_| F32Tensor::zeros(&[1, 1, 1, 1])));
                self.first_plan.clone()
            }
        };
        let mut outs = plan.run(&inputs)?;
        if outs.len() != 6 {
            return Err(MlError::Run(format!(
                "RVM returned {} outputs, expected 6 (fgr, pha, r1o..r4o)",
                outs.len()
            )));
        }
        let r4 = outs.pop();
        let r3 = outs.pop();
        let r2 = outs.pop();
        let r1 = outs.pop();
        let (Some(r1), Some(r2), Some(r3), Some(r4)) = (r1, r2, r3, r4) else {
            return Err(MlError::Run("RVM state outputs missing".into()));
        };
        let pha = outs
            .pop()
            .ok_or_else(|| MlError::Run("RVM pha output missing".into()))?;
        let (ww, wh) = (self.work.width as usize, self.work.height as usize);
        if pha.data.len() != ww * wh {
            return Err(MlError::Run(format!(
                "pha has {} values, expected {}x{}",
                pha.data.len(),
                ww,
                wh
            )));
        }
        // The first-frame plan's states are 1x1x1x1; later plans are keyed by the real shapes,
        // so a size change mid-session cannot reuse stale state.
        self.state = Some([r1, r2, r3, r4]);
        Ok(upscale_alpha(
            &pha.data,
            ww,
            wh,
            self.src_w as usize,
            self.src_h as usize,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_size_follows_the_documented_guidance() {
        // 1080p -> 512x288 at ratio 0.5 (backbone long edge 256).
        assert_eq!(
            working_size(1920, 1080),
            WorkingSize {
                width: 512,
                height: 288,
                downsample_ratio: 0.5
            }
        );
        // Portrait keeps its aspect and is padded up to a multiple of 32.
        assert_eq!(
            working_size(1080, 1920),
            WorkingSize {
                width: 288,
                height: 512,
                downsample_ratio: 0.5
            }
        );
        // 4K is capped to the same size as 1080p.
        assert_eq!(working_size(3840, 2160), working_size(1920, 1080));
        // Small sources are never upscaled and run with ratio 1 (backbone sees all of it).
        let small = working_size(320, 240);
        assert_eq!((small.width, small.height), (320, 256));
        assert_eq!(small.downsample_ratio, 1.0);
        // Degenerate sizes stay valid.
        let tiny = working_size(1, 1);
        assert_eq!((tiny.width, tiny.height), (32, 32));
        for (w, h) in [(1920, 1080), (1280, 720), (640, 480), (400, 500), (100, 33)] {
            let s = working_size(w, h);
            assert_eq!(s.width % 32, 0);
            assert_eq!(s.height % 32, 0);
            let long = (s.width.max(s.height) as f32 * s.downsample_ratio) as u32;
            assert!(long <= WORKING_LONG_EDGE, "{w}x{h} backbone {long}");
        }
    }

    #[test]
    fn preprocess_is_planar_rgb_in_unit_range() {
        // 2x1 source at a 2x1 "working size": no resize, exact channel split.
        let work = WorkingSize {
            width: 2,
            height: 1,
            downsample_ratio: 1.0,
        };
        let t = preprocess_rgba(&[255, 0, 51, 9, 0, 255, 102, 9], 2, 1, &work).unwrap();
        assert_eq!(t.shape, vec![1, 3, 1, 2]);
        let want = [1.0, 0.0, 0.0, 1.0, 0.2, 0.4];
        for (g, w) in t.data.iter().zip(want) {
            assert!((g - w).abs() < 1e-6, "{g} vs {w}");
        }
        // Wrong buffer length is rejected, not read out of bounds.
        assert!(preprocess_rgba(&[0; 7], 2, 1, &work).is_err());
    }

    #[test]
    fn preprocess_resizes_to_working_size() {
        let (w, h) = (640u32, 360u32);
        let rgba = vec![128u8; (w * h * 4) as usize];
        let work = working_size(w, h);
        let t = preprocess_rgba(&rgba, w, h, &work).unwrap();
        assert_eq!(t.shape, vec![1, 3, 288, 512]);
        // A flat image stays flat through the resample.
        assert!(t.data.iter().all(|v| (v - 128.0 / 255.0).abs() < 1e-3));
    }

    #[test]
    fn upscale_is_exact_for_flat_and_interpolates_ramps() {
        let flat = upscale_alpha(&[1.0; 4], 2, 2, 8, 6);
        assert_eq!(flat.len(), 48);
        assert!(flat.iter().all(|&v| v == 255));
        assert!(upscale_alpha(&[0.0; 4], 2, 2, 5, 5).iter().all(|&v| v == 0));
        // Same size is the identity (up to 8-bit quantisation).
        let ramp = [0.0, 0.5, 1.0, 0.25];
        assert_eq!(upscale_alpha(&ramp, 2, 2, 2, 2), vec![0, 128, 255, 64]);
        // A 2x1 0->1 ramp upscaled 4x is monotone and clamps at the edges.
        let up = upscale_alpha(&[0.0, 1.0], 2, 1, 8, 1);
        assert_eq!((up[0], up[7]), (0, 255));
        assert!(up.windows(2).all(|w| w[0] <= w[1]));
        // Out-of-range model output is clamped.
        assert_eq!(upscale_alpha(&[1.7, -0.3], 2, 1, 2, 1), vec![255, 0]);
        // Degenerate input gives a zero matte of the requested size.
        assert_eq!(upscale_alpha(&[], 0, 0, 3, 1), vec![0, 0, 0]);
    }
}
