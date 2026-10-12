//! YuNet face detector (OpenCV Zoo `face_detection_yunet_2026may.onnx`, MIT) running on tract.
//!
//! This is a faithful port of OpenCV's `FaceDetectorYN` pre/post-processing for this model:
//!
//! * input is the image in **BGR order with raw 0..255 values** (no mean/std), as `1x3xHxW`,
//!   with `H` and `W` padded up to multiples of 32 by zero-filling the bottom/right edge;
//! * the graph has three heads at strides 8, 16 and 32, each producing `cls`, `obj`, `bbox` and
//!   `kps` tensors of shape `1 x (rows*cols) x {1,1,4,10}`; `cls`/`obj` are already
//!   probabilities;
//! * per cell `(r, c)`: `score = sqrt(clamp(cls) * clamp(obj))`,
//!   `cx = (c + bbox[0]) * stride`, `cy = (r + bbox[1]) * stride`,
//!   `w = exp(bbox[2]) * stride`, `h = exp(bbox[3]) * stride`, landmark `n` at
//!   `((kps[2n] + c) * stride, (kps[2n+1] + r) * stride)`;
//! * candidates below the score threshold are dropped, then greedy IoU non-maximum suppression
//!   keeps the best `top_k`.
//!
//! The pinned model has symbolic height/width; tract cannot plan symbolic spatial dims for this
//! graph's reshapes usefully, so a plan is built (and cached) per padded input size.

use super::runtime::{load_cached_plan, F32Tensor, InputFact, MlError};

/// Manifest id of the bundled detector (see `models.rs`).
pub const YUNET_MODEL_ID: &str = "yunet-face-2026may";

const STRIDES: [usize; 3] = [8, 16, 32];

/// One detected face. Coordinates are pixels of the image passed to
/// [`FaceDetector::detect_rgba`], origin top-left, +y down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Right eye, left eye (as seen in the image: the subject's right eye is on the image's
    /// left), nose tip, right and left mouth corners.
    pub landmarks: [[f32; 2]; 5],
    /// Confidence `sqrt(cls * obj)` in 0..1.
    pub score: f32,
}

impl Face {
    pub fn centre(&self) -> (f32, f32) {
        (self.x + self.width * 0.5, self.y + self.height * 0.5)
    }

    pub fn area(&self) -> f32 {
        self.width.max(0.0) * self.height.max(0.0)
    }

    pub fn iou(&self, other: &Face) -> f32 {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = (self.x + self.width).min(other.x + other.width);
        let y2 = (self.y + self.height).min(other.y + other.height);
        let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
        let union = self.area() + other.area() - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }
}

/// Detector configuration; construct with [`FaceDetector::default`] for OpenCV's defaults
/// (`score 0.6`, `nms 0.3`, `top_k 5000`).
#[derive(Debug, Clone, Copy)]
pub struct FaceDetector {
    pub score_threshold: f32,
    pub nms_threshold: f32,
    pub top_k: usize,
}

impl Default for FaceDetector {
    fn default() -> Self {
        Self {
            score_threshold: 0.6,
            nms_threshold: 0.3,
            top_k: 5000,
        }
    }
}

impl FaceDetector {
    /// Detect faces in a tightly packed RGBA8 image, best score first.
    pub fn detect_rgba(&self, width: u32, height: u32, rgba: &[u8]) -> Result<Vec<Face>, MlError> {
        let (w, h) = (width as usize, height as usize);
        if w == 0 || h == 0 || rgba.len() != w * h * 4 {
            return Err(MlError::TensorSize {
                shape: vec![h, w, 4],
                expected: w * h * 4,
                got: rgba.len(),
            });
        }
        let pw = w.div_ceil(32) * 32;
        let ph = h.div_ceil(32) * 32;
        let mut data = vec![0.0f32; 3 * pw * ph];
        let plane = pw * ph;
        for y in 0..h {
            for x in 0..w {
                let p = (y * w + x) * 4;
                let o = y * pw + x;
                data[o] = f32::from(rgba[p + 2]); // B
                data[plane + o] = f32::from(rgba[p + 1]); // G
                data[2 * plane + o] = f32::from(rgba[p]); // R
            }
        }
        let input = F32Tensor::new(vec![1, 3, ph, pw], data)?;
        let plan = load_cached_plan(YUNET_MODEL_ID, &[InputFact::Shape(vec![1, 3, ph, pw])])?;
        let outs = plan.run(&[input])?;
        self.decode(&outs, pw, ph)
    }

    /// Turn the 12 raw head outputs (`cls_8/16/32, obj_8/16/32, bbox_8/16/32, kps_8/16/32`) into
    /// faces. `pw`/`ph` are the padded network input size.
    fn decode(&self, outs: &[F32Tensor], pw: usize, ph: usize) -> Result<Vec<Face>, MlError> {
        if outs.len() != 12 {
            return Err(MlError::Run(format!(
                "YuNet returned {} outputs, expected 12",
                outs.len()
            )));
        }
        let mut cands: Vec<Face> = Vec::new();
        for (i, &stride) in STRIDES.iter().enumerate() {
            let cols = pw / stride;
            let rows = ph / stride;
            let cells = rows * cols;
            let (cls, obj, bbox, kps) = (&outs[i], &outs[3 + i], &outs[6 + i], &outs[9 + i]);
            if cls.data.len() != cells
                || obj.data.len() != cells
                || bbox.data.len() != cells * 4
                || kps.data.len() != cells * 10
            {
                return Err(MlError::Run(format!(
                    "YuNet head {stride} has unexpected size for a {pw}x{ph} input"
                )));
            }
            let s = stride as f32;
            for r in 0..rows {
                for c in 0..cols {
                    let idx = r * cols + c;
                    let score =
                        (cls.data[idx].clamp(0.0, 1.0) * obj.data[idx].clamp(0.0, 1.0)).sqrt();
                    if score < self.score_threshold {
                        continue;
                    }
                    let b = &bbox.data[idx * 4..idx * 4 + 4];
                    let cx = (c as f32 + b[0]) * s;
                    let cy = (r as f32 + b[1]) * s;
                    let w = b[2].exp() * s;
                    let h = b[3].exp() * s;
                    let k = &kps.data[idx * 10..idx * 10 + 10];
                    let mut landmarks = [[0.0f32; 2]; 5];
                    for (n, lm) in landmarks.iter_mut().enumerate() {
                        *lm = [(k[2 * n] + c as f32) * s, (k[2 * n + 1] + r as f32) * s];
                    }
                    cands.push(Face {
                        x: cx - w * 0.5,
                        y: cy - h * 0.5,
                        width: w,
                        height: h,
                        landmarks,
                        score,
                    });
                }
            }
        }
        Ok(nms(cands, self.nms_threshold, self.top_k))
    }
}

/// Greedy non-maximum suppression (best score first), keeping at most `top_k`.
fn nms(mut faces: Vec<Face>, iou_threshold: f32, top_k: usize) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut kept: Vec<Face> = Vec::new();
    for f in faces {
        if kept.len() >= top_k {
            break;
        }
        if kept.iter().all(|k| k.iou(&f) <= iou_threshold) {
            kept.push(f);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference detection from OpenCV 4.13 `FaceDetectorYN` (same model file, score 0.6, nms 0.3)
    /// on `assets/test-images/nasa_astronaut_portrait_400x500.jpg`.
    const REF_BOX: [f32; 4] = [157.34, 75.28, 68.70, 90.83];
    const REF_LANDMARKS: [[f32; 2]; 5] = [
        [175.15, 108.47],
        [207.12, 108.35],
        [192.19, 127.72],
        [180.15, 142.96],
        [207.75, 140.84],
    ];

    fn load_portrait() -> (u32, u32, Vec<u8>) {
        let bytes = include_bytes!("../../assets/test-images/nasa_astronaut_portrait_400x500.jpg");
        let img = image::load_from_memory(bytes).unwrap().to_rgba8();
        (img.width(), img.height(), img.into_raw())
    }

    #[test]
    fn synthetic_non_face_images_yield_no_detections() {
        let det = FaceDetector::default();
        // Smooth gradient, hard-edged shapes and noise: none of it is a face.
        let (w, h) = (320usize, 240usize);
        let mut rgba = vec![255u8; w * h * 4];
        let mut seed = 12345u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let noise = (seed >> 24) as u8 / 8;
                let base = if (x / 40 + y / 40) % 2 == 0 { 40 } else { 200 };
                let p = (y * w + x) * 4;
                rgba[p] = (x * 255 / w) as u8;
                rgba[p + 1] = base + noise;
                rgba[p + 2] = (y * 255 / h) as u8;
            }
        }
        let faces = det.detect_rgba(w as u32, h as u32, &rgba).unwrap();
        assert!(faces.is_empty(), "unexpected detections: {faces:?}");
        // Flat mid-grey too (odd size, exercises the 32-multiple padding).
        let flat = vec![128u8; 101 * 67 * 4];
        assert!(det.detect_rgba(101, 67, &flat).unwrap().is_empty());
    }

    #[test]
    fn detects_public_domain_portrait_matching_opencv() {
        let (w, h, rgba) = load_portrait();
        assert_eq!((w, h), (400, 500));
        let faces = FaceDetector::default().detect_rgba(w, h, &rgba).unwrap();
        assert_eq!(faces.len(), 1, "{faces:?}");
        let f = faces[0];
        let reference = Face {
            x: REF_BOX[0],
            y: REF_BOX[1],
            width: REF_BOX[2],
            height: REF_BOX[3],
            landmarks: REF_LANDMARKS,
            score: 0.94,
        };
        assert!(f.score > 0.85, "score {}", f.score);
        assert!(
            f.iou(&reference) > 0.95,
            "iou {} box {f:?}",
            f.iou(&reference)
        );
        // Known face region: the head sits in the upper-middle of the portrait.
        let (cx, cy) = f.centre();
        assert!(
            (150.0..250.0).contains(&cx) && (80.0..180.0).contains(&cy),
            "{cx},{cy}"
        );
        // Landmarks agree with OpenCV to a couple of pixels and are anatomically plausible.
        for (got, want) in f.landmarks.iter().zip(&REF_LANDMARKS) {
            assert!(
                (got[0] - want[0]).abs() < 2.0 && (got[1] - want[1]).abs() < 2.0,
                "{got:?} vs {want:?}"
            );
        }
        let [re, le, nose, rm, lm] = f.landmarks;
        assert!(re[0] < le[0], "eyes ordered left-to-right in the image");
        assert!(nose[1] > re[1].max(le[1]), "nose below the eyes");
        assert!(rm[1] > nose[1] && lm[1] > nose[1], "mouth below the nose");
        assert!(rm[0] < lm[0]);
        for p in f.landmarks {
            assert!(p[0] >= f.x && p[0] <= f.x + f.width && p[1] >= f.y && p[1] <= f.y + f.height);
        }
    }

    #[test]
    fn detection_is_position_equivariant_under_padding() {
        // Same face embedded in a larger odd-sized canvas is found at the shifted position.
        let (w, h, rgba) = load_portrait();
        let (ox, oy, cw, ch) = (37usize, 21usize, 497usize, 541usize);
        let mut canvas = vec![90u8; cw * ch * 4];
        for y in 0..h as usize {
            let src = y * w as usize * 4;
            let dst = ((y + oy) * cw + ox) * 4;
            canvas[dst..dst + w as usize * 4].copy_from_slice(&rgba[src..src + w as usize * 4]);
        }
        let faces = FaceDetector::default()
            .detect_rgba(cw as u32, ch as u32, &canvas)
            .unwrap();
        assert!(!faces.is_empty());
        let (cx, cy) = faces[0].centre();
        assert!(
            (cx - (REF_BOX[0] + REF_BOX[2] / 2.0 + ox as f32)).abs() < 6.0,
            "{cx}"
        );
        assert!(
            (cy - (REF_BOX[1] + REF_BOX[3] / 2.0 + oy as f32)).abs() < 6.0,
            "{cy}"
        );
    }

    #[test]
    fn nms_suppresses_overlaps_and_respects_top_k() {
        let mk = |x: f32, s: f32| Face {
            x,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            landmarks: [[0.0; 2]; 5],
            score: s,
        };
        let out = nms(vec![mk(0.0, 0.7), mk(1.0, 0.9), mk(50.0, 0.8)], 0.3, 10);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].score, 0.9);
        assert_eq!(out[1].score, 0.8);
        assert_eq!(nms(vec![mk(0.0, 0.7), mk(50.0, 0.8)], 0.3, 1).len(), 1);
    }

    #[test]
    fn rejects_wrong_buffer_size() {
        assert!(FaceDetector::default()
            .detect_rgba(10, 10, &[0u8; 12])
            .is_err());
    }
}
