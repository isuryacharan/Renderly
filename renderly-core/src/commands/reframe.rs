//! `AutoReframeClip`: subject-aware reframing driven by the YuNet face detector.
//!
//! Frames are sampled sequentially (one ffmpeg process, <= 360 px tall, ~5 fps of timeline
//! time), faces are detected, one subject is followed through the clip and its path becomes
//! `PosX`/`PosY` keyframes plus one constant zoom. When no face is ever found (or the media
//! cannot be decoded) the old centred scale is applied unchanged.
//!
//! ## Geometry (compositor conventions, see `compose/composite.wgsl` and `motion.rs`)
//!
//! The layer is cover-fitted to the canvas first, so with scale 1 the canvas shows only the
//! central `cover_fractions` window of the source. Call a point's position inside that window
//! `p` in -1..1 (x right, y up). A layer scaled by `s` and translated by `(x, y)` NDC puts `p`
//! at `x + s*p`; the canvas stays fully covered iff `|x| <= s - 1` (same for y). Centring a
//! subject at `p` therefore needs `x = -s*p`, which is reachable only while `|p| <= 1 - 1/s`.
//!
//! Consequence worth knowing: the compositor has no pan beyond the cover window (a 16:9 clip in
//! a 9:16 canvas can only follow subjects inside its centre ~32% width strip). The follower
//! therefore never asks for more than the window can give; subjects outside it are kept as
//! close to the edge of the picture as possible. Lifting that needs a per-clip crop offset in
//! the schema, which this command deliberately does not introduce.

use super::motion::{
    analysis_err, cover_fractions, key, keys_simplified, out_size, plan_samples, resolve,
    FrameFeeder, SamplePlan, VideoClip,
};
use super::{media_clip_mut, CommandError, CommandOutcome};
use crate::ml::yunet::{Face, FaceDetector};
use crate::project::{AnimProperty, Id, KeyframeTrack, MediaKind, Project};
use crate::tracking::gaussian_smooth;

/// Timeline-time sampling rate for detection.
const SAMPLE_FPS: f64 = 5.0;
/// Minimum detector confidence for a face to count (stricter than OpenCV's 0.6 default).
const MIN_SCORE: f32 = 0.7;
/// Highest zoom the reframer will add on top of the base cover scale.
const MAX_ZOOM: f64 = 3.0;
/// The face may fill at most 1/FACE_FIT_W of the visible width and 1/FACE_FIT_H of its height.
const FACE_FIT_W: f64 = 2.0;
const FACE_FIT_H: f64 = 1.6;
/// Dead zone half-size as a fraction of the visible half-extent: the camera ignores subject
/// movement inside it.
const DEAD_ZONE: f64 = 0.15;
/// Gaussian smoothing of the raw subject path (detector noise) and of the final camera path.
const SUBJECT_SMOOTH_SECS: f64 = 0.12;
const CAMERA_SMOOTH_SECS: f64 = 0.3;
/// A subject missing for longer than this is forgotten (a new one may then be picked freely).
const HOLD_SECS: f64 = 2.0;
/// Keyframe simplification tolerance in NDC.
const POS_TOLERANCE: f64 = 0.004;
/// Need detections in at least this fraction of the samples (guards against a lone false hit).
const MIN_HIT_FRACTION: f64 = 0.1;

/// A face expressed as fractions of the analysed frame (origin top-left, +y down).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Subject {
    cx: f64,
    cy: f64,
    w: f64,
    h: f64,
}

impl Subject {
    fn from_face(f: &Face, frame_w: f64, frame_h: f64) -> Self {
        let (cx, cy) = f.centre();
        Self {
            cx: f64::from(cx) / frame_w,
            cy: f64::from(cy) / frame_h,
            w: f64::from(f.width) / frame_w,
            h: f64::from(f.height) / frame_h,
        }
    }
}

struct Detections {
    /// Per sample: the chosen subject, if any face was seen (or held) there.
    subjects: Vec<Option<Subject>>,
    /// Number of samples in which a face was actually detected (not held).
    hits: usize,
}

/// Pick the subject for one sample. Largest x confidence, but a face near the previous subject
/// gets a continuity bonus (up to 4x) so the choice does not flip between two similar faces.
fn pick_subject(
    faces: &[Face],
    prev: Option<&Subject>,
    frame_w: f64,
    frame_h: f64,
) -> Option<Subject> {
    faces
        .iter()
        .map(|f| {
            let s = Subject::from_face(f, frame_w, frame_h);
            let mut weight = f64::from(f.score) * s.w * s.h;
            if let Some(p) = prev {
                // Distances in units of the previous subject's height (isotropic in pixels).
                let dx = (s.cx - p.cx) * frame_w;
                let dy = (s.cy - p.cy) * frame_h;
                let sigma = (p.h * frame_h).max(1.0);
                weight *= 1.0 + 3.0 * (-(dx * dx + dy * dy) / (2.0 * sigma * sigma)).exp();
            }
            (weight, s)
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, s)| s)
}

/// Run subject selection over per-sample face lists, holding the last subject through gaps.
fn select_subjects(
    per_sample: &[Vec<Face>],
    times: &[f64],
    frame_w: f64,
    frame_h: f64,
) -> Detections {
    let mut subjects = Vec::with_capacity(per_sample.len());
    let mut prev: Option<(Subject, f64)> = None; // subject + time last seen
    let mut hits = 0;
    for (faces, &t) in per_sample.iter().zip(times) {
        if let Some((_, seen)) = prev {
            if t - seen > HOLD_SECS {
                prev = None;
            }
        }
        match pick_subject(faces, prev.as_ref().map(|(s, _)| s), frame_w, frame_h) {
            Some(s) => {
                hits += 1;
                prev = Some((s, t));
                subjects.push(Some(s));
            }
            None => subjects.push(None),
        }
    }
    Detections { subjects, hits }
}

/// Fill gaps: leading/trailing gaps take the nearest detection, interior gaps interpolate
/// linearly between the surrounding ones. Returns `None` when there is nothing to fill from.
fn fill_gaps(subjects: &[Option<Subject>], times: &[f64]) -> Option<Vec<Subject>> {
    let known: Vec<usize> = (0..subjects.len())
        .filter(|&i| subjects[i].is_some())
        .collect();
    let (&first, &last) = (known.first()?, known.last()?);
    let mut out = Vec::with_capacity(subjects.len());
    for i in 0..subjects.len() {
        let s = match subjects[i] {
            Some(s) => s,
            None if i < first => subjects[first]?,
            None if i > last => subjects[last]?,
            None => {
                let hi = known.partition_point(|&k| k < i);
                let (a, b) = (known[hi - 1], known[hi]);
                let (sa, sb) = (subjects[a]?, subjects[b]?);
                let span = (times[b] - times[a]).max(1e-9);
                let f = ((times[i] - times[a]) / span).clamp(0.0, 1.0);
                let lerp = |x: f64, y: f64| x + (y - x) * f;
                Subject {
                    cx: lerp(sa.cx, sb.cx),
                    cy: lerp(sa.cy, sb.cy),
                    w: lerp(sa.w, sb.w),
                    h: lerp(sa.h, sb.h),
                }
            }
        };
        out.push(s);
    }
    Some(out)
}

/// The framing decided for a clip: one constant scale and a camera path as NDC offsets.
#[derive(Debug, Clone, PartialEq)]
struct Framing {
    scale: f64,
    /// `(PosX, PosY)` per sample time.
    pos: Vec<(f64, f64)>,
}

/// Turn a subject path into a zoom and camera path (see the module docs for the maths).
/// `cover` is `(cs_x, cs_y)` from [`cover_fractions`], `base_scale` the minimum zoom, `dt` the
/// sample spacing in seconds.
fn plan_framing(path: &[Subject], cover: (f64, f64), base_scale: f64, dt: f64) -> Framing {
    let sigma_of = |secs: f64| if dt > 1e-9 { secs / dt } else { 0.0 };
    let (sigma_subject, sigma_camera) =
        (sigma_of(SUBJECT_SMOOTH_SECS), sigma_of(CAMERA_SMOOTH_SECS));
    let to_p = |s: &Subject| ((s.cx - 0.5) / cover.0 * 2.0, -(s.cy - 0.5) / cover.1 * 2.0);
    let raw: (Vec<f64>, Vec<f64>) = path.iter().map(to_p).unzip();
    let (px, py) = (
        gaussian_smooth(&raw.0, sigma_subject),
        gaussian_smooth(&raw.1, sigma_subject),
    );

    // Zoom: enough that the window can reach the farthest subject position, but not so much
    // that the face would overfill the crop, and never below the base cover scale.
    let pmax = px
        .iter()
        .chain(py.iter())
        .fold(0.0f64, |m, v| m.max(v.abs()))
        .min(0.95);
    let reach = if pmax > 1e-3 { 1.0 / (1.0 - pmax) } else { 1.0 };
    let face_w = path.iter().fold(0.0f64, |m, s| m.max(s.w)).max(1e-3);
    let face_h = path.iter().fold(0.0f64, |m, s| m.max(s.h)).max(1e-3);
    let face_cap = (cover.0 / (FACE_FIT_W * face_w)).min(cover.1 / (FACE_FIT_H * face_h));
    let scale = reach
        .min(face_cap)
        .min(MAX_ZOOM * base_scale)
        .max(base_scale)
        .max(1.0);

    // Dead-zone follower on the smoothed subject path, camera confined to the no-border range.
    let limit = (1.0 - 1.0 / scale).max(0.0);
    let dead = DEAD_ZONE / scale;
    let follow = |v: &[f64]| -> Vec<f64> {
        let mut c = v[0].clamp(-limit, limit);
        v.iter()
            .map(|&x| {
                let d = x - c;
                if d.abs() > dead {
                    c += d - dead * d.signum();
                }
                c = c.clamp(-limit, limit);
                c
            })
            .collect()
    };
    let (cx, cy) = (
        gaussian_smooth(&follow(&px), sigma_camera),
        gaussian_smooth(&follow(&py), sigma_camera),
    );
    // Smoothing a clamped series stays inside the clamp, but guard against rounding anyway.
    let bound = scale - 1.0;
    let pos = cx
        .iter()
        .zip(&cy)
        .map(|(&x, &y)| {
            (
                (-scale * x).clamp(-bound, bound),
                (-scale * y).clamp(-bound, bound),
            )
        })
        .collect();
    Framing { scale, pos }
}

/// Minimum zoom so a `target_aspect` window covers the canvas: 1 when the canvas already has
/// that aspect (the usual case), otherwise the ratio between the two aspects.
fn base_scale(target_aspect: f64, out_w: f64, out_h: f64) -> f64 {
    let canvas = out_w / out_h;
    if (canvas / target_aspect - 1.0).abs() < 0.01 {
        1.0
    } else {
        (canvas / target_aspect).max(target_aspect / canvas)
    }
}

pub(super) fn auto_reframe_clip(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    target_aspect: f64,
) -> Result<CommandOutcome, CommandError> {
    if !target_aspect.is_finite() || target_aspect <= 0.0 {
        return Err(CommandError::Analysis(
            "target_aspect must be a finite number > 0".into(),
        ));
    }
    let vc = resolve(project, track_id, clip_id)?;
    let (out_w, out_h) = out_size(project);
    let detector = FaceDetector {
        score_threshold: MIN_SCORE,
        ..FaceDetector::default()
    };
    let analysed = if vc.kind == MediaKind::Image {
        analyse_still(&vc, &detector)?
    } else {
        analyse_video(&vc, &detector)?
    };
    let Some(a) = analysed else {
        return super::centred_reframe(project, track_id, clip_id, target_aspect);
    };

    let cover = cover_fractions(a.frame_w, a.frame_h, out_w, out_h);
    let base = base_scale(target_aspect, out_w, out_h);
    let dt = if a.times.len() > 1 {
        (a.times[a.times.len() - 1] - a.times[0]) / (a.times.len() - 1) as f64
    } else {
        0.0
    };
    let framing = plan_framing(&a.path, cover, base, dt);

    let clip = media_clip_mut(project, track_id, clip_id)?;
    clip.keyframes.retain(|t| {
        !matches!(
            t.property,
            AnimProperty::PosX | AnimProperty::PosY | AnimProperty::ScaleX | AnimProperty::ScaleY
        )
    });
    clip.transform.scale_x = framing.scale;
    clip.transform.scale_y = framing.scale;
    if vc.kind == MediaKind::Image {
        let (x, y) = framing.pos[0];
        clip.transform.x = x;
        clip.transform.y = y;
    } else {
        clip.transform.x = 0.0;
        clip.transform.y = 0.0;
        let xs: Vec<f64> = framing.pos.iter().map(|p| p.0).collect();
        let ys: Vec<f64> = framing.pos.iter().map(|p| p.1).collect();
        for (property, values) in [(AnimProperty::PosX, xs), (AnimProperty::PosY, ys)] {
            let mut keys = keys_simplified(&a.times, &values, POS_TOLERANCE);
            if keys.len() < 2 {
                // Constant path collapsed to one key: keep both ends so the track is explicit.
                keys = vec![
                    key(a.times[0], values[0]),
                    key(a.times[a.times.len() - 1], values[0]),
                ];
            }
            clip.keyframes.push(KeyframeTrack { property, keys });
        }
    }
    Ok(CommandOutcome::Applied)
}

struct Analysis {
    frame_w: f64,
    frame_h: f64,
    /// Clip-local timeline seconds of each sample.
    times: Vec<f64>,
    /// Filled-in subject path, one per sample.
    path: Vec<Subject>,
}

fn ml_err(e: impl std::fmt::Display) -> CommandError {
    CommandError::Analysis(format!("face detection: {e}"))
}

fn analyse_still(
    vc: &VideoClip,
    detector: &FaceDetector,
) -> Result<Option<Analysis>, CommandError> {
    let Ok(img) = image::open(&vc.path) else {
        return Ok(None);
    };
    // Detect on a <= 360 px tall copy (same budget as video analysis).
    let img = if img.height() > 360 {
        let w = ((u64::from(img.width()) * 360) / u64::from(img.height())).max(1) as u32;
        img.resize_exact(w, 360, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = (f64::from(rgba.width()), f64::from(rgba.height()));
    let faces = detector
        .detect_rgba(rgba.width(), rgba.height(), rgba.as_raw())
        .map_err(ml_err)?;
    Ok(pick_subject(&faces, None, w, h).map(|s| Analysis {
        frame_w: w,
        frame_h: h,
        times: vec![0.0],
        path: vec![s],
    }))
}

fn analyse_video(
    vc: &VideoClip,
    detector: &FaceDetector,
) -> Result<Option<Analysis>, CommandError> {
    let dur = vc.clip.timeline_duration_secs();
    let requested = (dur * SAMPLE_FPS).ceil() as usize + 1;
    let plan: SamplePlan = plan_samples(vc, Some(requested))?;
    let Ok(mut feeder) = FrameFeeder::open(&vc.path, vc.clip.source_in_secs, plan.fps) else {
        return Ok(None);
    };
    let mut per_sample: Vec<Vec<Face>> = Vec::with_capacity(plan.idx.len());
    let mut frame_dims: Option<(f64, f64)> = None;
    let mut last_served: Option<i64> = None;
    for &want in &plan.idx {
        let faces = match feeder.rgba_at(want) {
            Ok((frame, served)) => {
                frame_dims = Some((f64::from(frame.width), f64::from(frame.height)));
                if last_served == Some(served) {
                    // Same decoded frame again (stalled source / end of stream): reuse.
                    per_sample.last().cloned().unwrap_or_default()
                } else {
                    last_served = Some(served);
                    detector
                        .detect_rgba(frame.width, frame.height, &frame.pixels)
                        .map_err(ml_err)?
                }
            }
            // Decode failed: nothing for this (and later) samples.
            Err(e) => {
                if frame_dims.is_none() {
                    let _ = analysis_err(e);
                    return Ok(None);
                }
                Vec::new()
            }
        };
        per_sample.push(faces);
    }
    let Some((fw, fh)) = frame_dims else {
        return Ok(None);
    };
    let det = select_subjects(&per_sample, &plan.times, fw, fh);
    let needed = (per_sample.len() as f64 * MIN_HIT_FRACTION).ceil().max(1.0) as usize;
    if det.hits < needed {
        return Ok(None);
    }
    Ok(fill_gaps(&det.subjects, &plan.times).map(|path| Analysis {
        frame_w: fw,
        frame_h: fh,
        times: plan.times,
        path,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proc::SerialSpawn;

    fn face(cx: f32, cy: f32, w: f32, h: f32, score: f32) -> Face {
        Face {
            x: cx - w / 2.0,
            y: cy - h / 2.0,
            width: w,
            height: h,
            landmarks: [[0.0; 2]; 5],
            score,
        }
    }

    fn subj(cx: f64, cy: f64, w: f64, h: f64) -> Subject {
        Subject { cx, cy, w, h }
    }

    #[test]
    fn picks_largest_confident_face_without_history() {
        let faces = [
            face(100.0, 100.0, 40.0, 40.0, 0.9),
            face(300.0, 100.0, 80.0, 80.0, 0.9),
            face(200.0, 200.0, 90.0, 90.0, 0.4),
        ];
        let s = pick_subject(&faces, None, 400.0, 300.0).unwrap();
        assert!((s.cx - 0.75).abs() < 1e-6);
    }

    #[test]
    fn continuity_prevents_flipping_between_similar_faces() {
        let prev = subj(0.25, 0.33, 0.1, 0.13);
        // The right-hand face is slightly larger, but the left one continues the track.
        let faces = [
            face(100.0, 100.0, 40.0, 40.0, 0.9),
            face(300.0, 100.0, 48.0, 48.0, 0.9),
        ];
        let s = pick_subject(&faces, Some(&prev), 400.0, 300.0).unwrap();
        assert!(s.cx < 0.5, "flipped to the other face: {s:?}");
        // A much larger newcomer still wins.
        let faces = [
            face(100.0, 100.0, 20.0, 20.0, 0.9),
            face(300.0, 100.0, 120.0, 120.0, 0.9),
        ];
        let s = pick_subject(&faces, Some(&prev), 400.0, 300.0).unwrap();
        assert!(s.cx > 0.5);
    }

    #[test]
    fn subject_is_held_through_short_gaps_and_dropped_after_long_ones() {
        let times: Vec<f64> = (0..20).map(|i| i as f64 * 0.5).collect();
        let a = vec![face(100.0, 100.0, 40.0, 40.0, 0.9)];
        let b = vec![face(300.0, 100.0, 60.0, 60.0, 0.9)];
        let mut samples: Vec<Vec<Face>> = vec![Vec::new(); 20];
        samples[0] = a.clone();
        samples[1] = a.clone();
        // 1 s gap (held), then a face on the far side that is bigger: held subject keeps
        // continuity only if near; here it is far, so the larger one simply wins.
        samples[4] = b.clone();
        let d = select_subjects(&samples, &times, 400.0, 300.0);
        assert_eq!(d.hits, 3);
        assert!(d.subjects[2].is_none() && d.subjects[3].is_none());
        let filled = fill_gaps(&d.subjects, &times).unwrap();
        // Interior gap interpolates, trailing gap holds the last detection.
        assert!(filled[2].cx > filled[1].cx && filled[2].cx < filled[4].cx);
        assert_eq!(filled[10], filled[4]);
        assert_eq!(filled[0], filled[1]);
    }

    #[test]
    fn fill_gaps_needs_a_detection() {
        assert!(fill_gaps(&[None, None], &[0.0, 1.0]).is_none());
    }

    #[test]
    fn centred_stationary_subject_needs_no_zoom_or_motion() {
        let path = vec![subj(0.5, 0.5, 0.08, 0.12); 30];
        let f = plan_framing(&path, (0.316, 1.0), 1.0, 0.2);
        assert!((f.scale - 1.0).abs() < 1e-9);
        assert!(f.pos.iter().all(|p| p.0.abs() < 1e-9 && p.1.abs() < 1e-9));
    }

    #[test]
    fn framing_never_reveals_borders_and_ignores_small_jitter() {
        // Wide source, 9:16 canvas: cover window is 31.6% of the width. Subject wanders inside
        // it with a small tremor.
        let cover = (0.316, 1.0);
        let path: Vec<Subject> = (0..60)
            .map(|i| {
                let t = i as f64 * 0.2;
                let jitter = if i % 2 == 0 { 0.002 } else { -0.002 };
                subj(
                    0.5 + 0.05 * (t * 0.8).sin() + jitter,
                    0.5 + 0.1 * (t * 0.5).sin(),
                    0.07,
                    0.17,
                )
            })
            .collect();
        let f = plan_framing(&path, cover, 1.0, 0.2);
        assert!(f.scale > 1.0 && f.scale <= MAX_ZOOM);
        for &(x, y) in &f.pos {
            assert!(x.abs() <= f.scale - 1.0 + 1e-9 && y.abs() <= f.scale - 1.0 + 1e-9);
        }
        // Dead zone + smoothing: the camera moves far less than the raw subject path jitters.
        let steps: f64 = f
            .pos
            .windows(2)
            .map(|w| (w[1].0 - w[0].0).abs())
            .fold(0.0, f64::max);
        assert!(steps < 0.12, "camera step {steps}");
    }

    #[test]
    fn face_size_caps_the_zoom() {
        // Big face near the edge would want a large zoom for reach, but the face must fit.
        let path = vec![subj(0.6, 0.5, 0.12, 0.3); 10];
        let f = plan_framing(&path, (0.316, 1.0), 1.0, 0.2);
        assert!(f.scale <= 0.316 / (FACE_FIT_W * 0.12) + 1e-9 || (f.scale - 1.0).abs() < 1e-9);
    }

    #[test]
    fn base_scale_matches_canvas_or_ratio() {
        assert_eq!(base_scale(9.0 / 16.0, 1080.0, 1920.0), 1.0);
        assert!((base_scale(9.0 / 16.0, 1920.0, 1080.0) - 16.0 / 9.0 * 16.0 / 9.0).abs() < 1e-9);
    }

    // ---- command-level tests (need ffmpeg for the video ones) ---------------------------------

    use crate::commands::{apply_command, Command, CommandOutcome};
    use crate::media;
    use crate::project::{evaluate_transform, MediaClip, MediaItem, Settings, TrackKind};
    use std::path::{Path, PathBuf};
    use std::process::Command as Proc;

    const PORTRAIT: &[u8] =
        include_bytes!("../../assets/test-images/nasa_astronaut_portrait_400x500.jpg");
    /// Face centre in the 400x500 portrait (OpenCV reference detection) and the head-and-
    /// shoulders crop used for the moving-face video.
    const FACE_CENTRE: (f64, f64) = (191.7, 120.7);
    const CROP: (u32, u32, u32) = (100, 40, 200); // x0, y0, side
    const OVERLAY: f64 = 160.0; // crop is scaled to this many px
    const SRC: (u32, u32) = (640, 360);
    const FPS: f64 = 15.0;
    const SECS: f64 = 4.0;
    const AMP_X: f64 = 50.0;
    const AMP_Y: f64 = 30.0;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("renderly-{tag}-{}", Id::new_v4()));
        std::fs::create_dir_all(&d).expect("tmpdir");
        d
    }

    /// Face centre in source pixels at clip-local time `t` for the synthetic video.
    fn face_at(t: f64) -> (f64, f64) {
        let k = OVERLAY / f64::from(CROP.2);
        let ox = f64::from(SRC.0) / 2.0 - OVERLAY / 2.0
            + AMP_X * (std::f64::consts::TAU * t / 4.0).sin();
        let oy = f64::from(SRC.1) / 2.0 - OVERLAY / 2.0
            + AMP_Y * (std::f64::consts::TAU * t / 3.0).sin();
        (
            ox + (FACE_CENTRE.0 - f64::from(CROP.0)) * k,
            oy + (FACE_CENTRE.1 - f64::from(CROP.1)) * k,
        )
    }

    /// 640x360 video: flat background with the licensed portrait's head moving on a known path.
    fn synth_face_video(dir: &Path) -> PathBuf {
        let img = image::load_from_memory(PORTRAIT).unwrap().to_rgba8();
        let crop = image::imageops::crop_imm(&img, CROP.0, CROP.1, CROP.2, CROP.2).to_image();
        let crop = image::imageops::resize(
            &crop,
            OVERLAY as u32,
            OVERLAY as u32,
            image::imageops::FilterType::Lanczos3,
        );
        let png = dir.join("head.png");
        crop.save(&png).unwrap();
        let out = dir.join("face.mp4");
        let filter = format!(
            "[0:v][1:v]overlay=x='{}-{}+{AMP_X}*sin(2*PI*t/4)':y='{}-{}+{AMP_Y}*sin(2*PI*t/3)':shortest=1,format=yuv420p",
            SRC.0 / 2,
            OVERLAY / 2.0,
            SRC.1 / 2,
            OVERLAY / 2.0
        );
        let status = Proc::new(media::ffmpeg_path().expect("ffmpeg"))
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!(
                "color=c=0x30506a:s={}x{}:r={FPS}:d={SECS}",
                SRC.0, SRC.1
            ))
            .args(["-loop", "1", "-framerate", &FPS.to_string(), "-i"])
            .arg(&png)
            .args(["-filter_complex", &filter, "-c:v", "libx264", "-crf", "8"])
            .arg(&out)
            .status_serial()
            .expect("spawn ffmpeg");
        assert!(status.success());
        out
    }

    fn synth_blank_video(dir: &Path) -> PathBuf {
        let out = dir.join("blank.mp4");
        let status = Proc::new(media::ffmpeg_path().expect("ffmpeg"))
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
            .arg(format!(
                "gradients=s={}x{}:r={FPS}:d={SECS}:c0=0x203040:c1=0xa08060:speed=0.01",
                SRC.0, SRC.1
            ))
            .args(["-c:v", "libx264", "-crf", "12", "-pix_fmt", "yuv420p"])
            .arg(&out)
            .status_serial()
            .expect("spawn ffmpeg");
        assert!(status.success());
        out
    }

    fn project_with(
        path: &Path,
        kind: MediaKind,
        dims: (u32, u32),
        secs: f64,
    ) -> (Project, Id, Id) {
        let mut project = Project::new(
            "t",
            Settings {
                width: 270,
                height: 480,
                ..Settings::default()
            },
        );
        let media_id = Id::new_v4();
        project.media.push(MediaItem {
            id: media_id,
            path: path.to_path_buf(),
            kind,
            duration_secs: Some(secs),
            width: Some(dims.0),
            height: Some(dims.1),
            fps: (kind == MediaKind::Video).then_some(FPS),
        });
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V".into(),
                id: None,
            },
        )
        .expect("track")
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let clip_id = match apply_command(
            &mut project,
            Command::AddClip {
                track_id,
                media_id,
                position_secs: 1.0,
                source_in_secs: 0.0,
                source_out_secs: secs,
            },
        )
        .expect("clip")
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        (project, track_id, clip_id)
    }

    fn clip_ref(project: &Project, t: Id, c: Id) -> &MediaClip {
        project
            .find_track(t)
            .and_then(|tr| tr.clips.iter().find(|x| x.id() == c))
            .and_then(|c| c.as_media())
            .expect("clip")
    }

    fn reframe(project: &mut Project, track_id: Id, clip_id: Id) {
        apply_command(
            project,
            Command::AutoReframeClip {
                track_id,
                clip_id,
                target_aspect: 9.0 / 16.0,
            },
        )
        .expect("reframe");
    }

    #[test]
    fn reframe_follows_moving_face_inside_the_crop() {
        if !media::ffmpeg_available() {
            eprintln!("skipping reframe_follows_moving_face_inside_the_crop: ffmpeg not available");
            return;
        }
        let dir = tmp_dir("reframe");
        let video = synth_face_video(&dir);
        let (mut project, tid, cid) = project_with(&video, MediaKind::Video, SRC, SECS);
        let started = std::time::Instant::now();
        reframe(&mut project, tid, cid);
        eprintln!(
            "auto-reframe of a {SECS}s clip took {:?}",
            started.elapsed()
        );
        let clip = clip_ref(&project, tid, cid);
        for p in [AnimProperty::PosX, AnimProperty::PosY] {
            assert!(clip.keyframes.iter().any(|k| k.property == p), "{p:?} keys");
        }
        let (cs_x, cs_y) = cover_fractions(f64::from(SRC.0), f64::from(SRC.1), 270.0, 480.0);
        // Face box half-size in cover-window units (p) at scale 1.
        let k = OVERLAY / f64::from(CROP.2);
        let half_w = 34.0 * k / (f64::from(SRC.0) * cs_x) * 2.0;
        let half_h = 45.0 * k / (f64::from(SRC.1) * cs_y) * 2.0;
        let (mut worst_c, mut worst_edge, mut max_border) = (0.0f64, 0.0f64, f64::MIN);
        for j in 0..=16 {
            let local = SECS * f64::from(j) / 16.0;
            let tf = evaluate_transform(clip, clip.position_secs + local);
            let (fx, fy) = face_at(local);
            let p = (
                (fx / f64::from(SRC.0) - 0.5) / cs_x * 2.0,
                -(fy / f64::from(SRC.1) - 0.5) / cs_y * 2.0,
            );
            // Where the face centre lands on the canvas (NDC, y up).
            let (nx, ny) = (tf.x + tf.scale_x * p.0, tf.y + tf.scale_y * p.1);
            worst_c = worst_c.max(nx.abs()).max(ny.abs());
            worst_edge = worst_edge
                .max(nx.abs() + half_w * tf.scale_x)
                .max(ny.abs() + half_h * tf.scale_y);
            // Border check: the scaled layer must cover the whole canvas.
            max_border = max_border
                .max(tf.x.abs() - (tf.scale_x - 1.0))
                .max(tf.y.abs() - (tf.scale_y - 1.0));
            eprintln!(
                "t={local:.2} face ndc=({nx:+.3},{ny:+.3}) scale={:.2} pos=({:+.3},{:+.3})",
                tf.scale_x, tf.x, tf.y
            );
        }
        eprintln!(
            "worst centre offset {worst_c:.3} ndc, worst face edge {worst_edge:.3}, border excess {max_border:.4}"
        );
        assert!(
            max_border <= 1e-6,
            "layer reveals the canvas background by {max_border}"
        );
        assert!(worst_edge < 1.0, "face leaves the crop: {worst_edge}");
        assert!(worst_c < 0.35, "face not near the crop centre: {worst_c}");
        // Constant zoom, no scale keyframes.
        assert!(clip.transform.scale_x > 1.2);
        assert!(clip
            .keyframes
            .iter()
            .all(|k| !matches!(k.property, AnimProperty::ScaleX | AnimProperty::ScaleY)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reframe_without_a_face_keeps_the_old_centred_result() {
        if !media::ffmpeg_available() {
            eprintln!(
                "skipping reframe_without_a_face_keeps_the_old_centred_result: ffmpeg not available"
            );
            return;
        }
        let dir = tmp_dir("reframe-blank");
        let video = synth_blank_video(&dir);
        let (mut project, tid, cid) = project_with(&video, MediaKind::Video, SRC, SECS);
        let mut expected = project.clone();
        super::super::centred_reframe(&mut expected, tid, cid, 9.0 / 16.0).unwrap();
        reframe(&mut project, tid, cid);
        let (got, want) = (clip_ref(&project, tid, cid), clip_ref(&expected, tid, cid));
        assert_eq!(got.transform, want.transform);
        assert!(got.keyframes.is_empty());
        let old_scale = (16.0f64 / 9.0) / (9.0 / 16.0);
        assert!((got.transform.scale_x - old_scale).abs() < 1e-9);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reframe_of_missing_media_falls_back_to_centred() {
        let (mut project, tid, cid) =
            project_with(Path::new("does-not-exist.mp4"), MediaKind::Video, SRC, SECS);
        reframe(&mut project, tid, cid);
        let clip = clip_ref(&project, tid, cid);
        assert!((clip.transform.scale_x - (16.0f64 / 9.0) / (9.0 / 16.0)).abs() < 1e-9);
        assert!(clip.keyframes.is_empty());
    }

    #[test]
    fn reframe_still_image_sets_a_static_transform_without_keyframes() {
        let dir = tmp_dir("reframe-still");
        let path = dir.join("portrait.jpg");
        std::fs::write(&path, PORTRAIT).unwrap();
        let (mut project, tid, cid) = project_with(&path, MediaKind::Image, (400, 500), 5.0);
        reframe(&mut project, tid, cid);
        let clip = clip_ref(&project, tid, cid);
        assert!(clip.keyframes.is_empty());
        let tf = clip.transform;
        assert!(
            tf.scale_x > 1.5 && (tf.scale_x - tf.scale_y).abs() < 1e-12,
            "{tf:?}"
        );
        let (cs_x, cs_y) = cover_fractions(400.0, 500.0, 270.0, 480.0);
        let p = (
            (FACE_CENTRE.0 / 400.0 - 0.5) / cs_x * 2.0,
            -(FACE_CENTRE.1 / 500.0 - 0.5) / cs_y * 2.0,
        );
        let (nx, ny) = (tf.x + tf.scale_x * p.0, tf.y + tf.scale_y * p.1);
        eprintln!(
            "still: scale {:.2} pos ({:+.3},{:+.3}) face ndc ({nx:+.3},{ny:+.3})",
            tf.scale_x, tf.x, tf.y
        );
        assert!(nx.abs() < 0.1 && ny.abs() < 0.1, "face at ({nx},{ny})");
        assert!(tf.x.abs() <= tf.scale_x - 1.0 + 1e-9 && tf.y.abs() <= tf.scale_y - 1.0 + 1e-9);
        std::fs::remove_dir_all(&dir).ok();
    }
}
