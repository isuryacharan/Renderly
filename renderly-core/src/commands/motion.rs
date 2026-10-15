//! `TrackMotion` and `StabilizeClip`: feature-based motion analysis on top of
//! [`crate::tracking`].
//!
//! Frames are decoded sequentially (one ffmpeg process) at <= 360 px tall, converted to
//! grayscale and fed through a [`MotionTracker`]; the per-frame motion estimates are turned
//! into keyframes in the compositor's conventions (see `compose/composite.wgsl`):
//!
//! * `PosX`/`PosY` are NDC offsets from the canvas centre, +x right, +y UP;
//! * `ScaleX`/`ScaleY` scale the cover-fitted layer about the canvas centre;
//! * `Rotation` is degrees, counter-clockwise on screen (y-up), applied after scale and before
//!   translation.
//!
//! Image y runs downwards, so every vertical quantity and every angle is negated on the way out.

use super::{CommandError, CommandOutcome, TrackRegion};
use crate::media::{self, ReaderOptions, VideoReader};
use crate::project::{
    AnimProperty, ClipTransform, Easing, Id, Keyframe, KeyframeTrack, MediaClip, MediaKind,
    Project, TrackKind,
};
use crate::tracking::{
    gaussian_smooth, simplify_polyline, GrayImage, MotionModel, MotionTracker, Rect, Similarity,
    TrackerConfig,
};
use std::path::PathBuf;

/// Analysis frames are decoded at most this tall.
const ANALYSIS_HEIGHT: u32 = 360;
/// Hard cap on analysed frames per command (keeps long clips bounded; frames are then skipped).
const MAX_ANALYSIS_FRAMES: usize = 900;
/// Highest source frame rate analysed; faster sources are decimated by ffmpeg.
const MAX_ANALYSIS_FPS: f64 = 30.0;
/// Upper bound on the constant auto-zoom added by stabilisation.
const MAX_AUTO_ZOOM: f64 = 1.4;
/// Default stabilisation strength.
const DEFAULT_STRENGTH: f64 = 0.6;
/// Smoothing sigma in seconds at strength 0 and the extra added per unit strength.
const SIGMA_BASE_SECS: f64 = 0.05;
const SIGMA_PER_STRENGTH_SECS: f64 = 1.5;
/// Keyframe simplification tolerances (NDC, degrees, scale ratio).
const POS_TOLERANCE: f64 = 0.0004;
const ROT_TOLERANCE: f64 = 0.005;
const SCALE_TOLERANCE: f64 = 0.0003;

pub(super) struct VideoClip {
    pub(super) clip: MediaClip,
    pub(super) path: PathBuf,
    pub(super) kind: MediaKind,
    pub(super) fps: f64,
}

pub(super) fn resolve(
    project: &Project,
    track_id: Id,
    clip_id: Id,
) -> Result<VideoClip, CommandError> {
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Video {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Video,
        ));
    }
    let clip = track
        .clips
        .iter()
        .find(|c| c.id() == clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?
        .as_media()
        .ok_or(CommandError::NotMediaClip(clip_id))?
        .clone();
    let media = project
        .find_media(clip.media_id)
        .ok_or(CommandError::MediaNotFound(clip.media_id))?;
    if media.kind == MediaKind::Audio {
        return Err(CommandError::Analysis(
            "clip media has no video frames".into(),
        ));
    }
    if media.kind == MediaKind::Sequence {
        return Err(CommandError::UnsupportedForSequence(
            "TrackMotion / StabilizeClip",
        ));
    }
    Ok(VideoClip {
        clip,
        path: media.path.clone(),
        kind: media.kind,
        fps: media
            .fps
            .filter(|f| f.is_finite() && *f > 0.0)
            .unwrap_or(30.0),
    })
}

pub(super) fn analysis_err(e: impl std::fmt::Display) -> CommandError {
    CommandError::Analysis(e.to_string())
}

/// Sequential grayscale frame source: skips (decodes and discards) frames as indices advance.
pub(super) struct FrameFeeder {
    reader: VideoReader,
    /// Index of the most recently decoded frame (-1 before the first read).
    cur_idx: i64,
    raw: Option<media::RgbaFrame>,
    gray: Option<GrayImage>,
    eof: bool,
}

impl FrameFeeder {
    pub(super) fn open(path: &std::path::Path, start: f64, fps: f64) -> Result<Self, CommandError> {
        let reader = VideoReader::open_with(
            path,
            start.max(0.0),
            &ReaderOptions {
                target_height: Some(ANALYSIS_HEIGHT),
                output_fps: Some(fps),
            },
        )
        .map_err(analysis_err)?;
        Ok(Self {
            reader,
            cur_idx: -1,
            raw: None,
            gray: None,
            eof: false,
        })
    }

    /// Advance to frame `idx` (holding the last frame once the stream ends) and return the
    /// decoded RGBA frame plus the index actually served.
    pub(super) fn rgba_at(&mut self, idx: i64) -> Result<(&media::RgbaFrame, i64), CommandError> {
        self.advance_to(idx)?;
        match self.raw.as_ref() {
            Some(f) => Ok((f, self.cur_idx)),
            None => Err(CommandError::Analysis("no frame decoded".into())),
        }
    }

    fn advance_to(&mut self, idx: i64) -> Result<(), CommandError> {
        while self.cur_idx < idx && !self.eof {
            match self.reader.read_frame().map_err(analysis_err)? {
                Some(f) => {
                    self.cur_idx += 1;
                    self.raw = Some(f);
                    self.gray = None;
                }
                None => self.eof = true,
            }
        }
        Ok(())
    }

    /// Frame `idx` (holding the last frame once the stream ends) and the index actually served.
    fn gray_at(&mut self, idx: i64) -> Result<(&GrayImage, i64), CommandError> {
        self.advance_to(idx)?;
        if self.gray.is_none() {
            let raw = self
                .raw
                .as_ref()
                .ok_or_else(|| CommandError::Analysis("no frame decoded".into()))?;
            self.gray = Some(
                GrayImage::from_rgba8(raw.width as usize, raw.height as usize, &raw.pixels)
                    .ok_or_else(|| CommandError::Analysis("malformed decoded frame".into()))?,
            );
        }
        match self.gray.as_ref() {
            Some(g) => Ok((g, self.cur_idx)),
            None => Err(CommandError::Analysis("no frame decoded".into())),
        }
    }
}

/// Which frames to analyse: clip-local timeline times and the matching source frame indices.
pub(super) struct SamplePlan {
    pub(super) times: Vec<f64>,
    pub(super) idx: Vec<i64>,
    pub(super) fps: f64,
}

pub(super) fn plan_samples(
    vc: &VideoClip,
    requested: Option<usize>,
) -> Result<SamplePlan, CommandError> {
    let clip = &vc.clip;
    let dur = clip.timeline_duration_secs();
    let span = (clip.source_out_secs - clip.source_in_secs).max(0.0);
    if !(dur.is_finite() && dur > 1e-6 && span > 1e-6) {
        return Err(CommandError::Analysis("clip has zero duration".into()));
    }
    let fps = vc.fps.clamp(1.0, MAX_ANALYSIS_FPS);
    // One analysis frame per native source frame, unless the caller asked for fewer/more.
    let native = (span * fps).round() as usize + 1;
    let n = requested.unwrap_or(native).clamp(2, MAX_ANALYSIS_FRAMES);
    let mut times = Vec::with_capacity(n);
    let mut idx = Vec::with_capacity(n);
    for i in 0..n {
        let t = dur * i as f64 / (n - 1) as f64;
        let src = clip.source_time_at(clip.position_secs + t);
        times.push(t);
        idx.push((((src - clip.source_in_secs) * fps).round() as i64).max(0));
    }
    Ok(SamplePlan { times, idx, fps })
}

/// Frame-to-frame motion for a whole clip.
struct MotionSeries {
    /// Clip-local timeline seconds of each analysed frame.
    times: Vec<f64>,
    /// `steps[i]` maps frame `i-1` to frame `i` in analysis-image pixels (`steps[0]` = identity).
    steps: Vec<Similarity>,
    /// Centre of the tracked region on each frame, analysis-image pixels.
    centres: Vec<(f64, f64)>,
    width: u32,
    height: u32,
}

fn analyse_motion(
    vc: &VideoClip,
    requested_frames: Option<usize>,
    model: MotionModel,
    region: Option<TrackRegion>,
    follow_region: bool,
) -> Result<MotionSeries, CommandError> {
    let plan = plan_samples(vc, requested_frames)?;
    let mut feeder = FrameFeeder::open(&vc.path, vc.clip.source_in_secs, plan.fps)?;
    let (first, first_idx) = {
        let (g, i) = feeder.gray_at(plan.idx[0])?;
        (g.clone(), i)
    };
    let (w, h) = (first.width, first.height);
    let rect = region.map(|r| {
        Rect::new(
            (r.x * w as f64) as f32,
            (r.y * h as f64) as f32,
            ((r.x + r.width) * w as f64) as f32,
            ((r.y + r.height) * h as f64) as f32,
        )
    });
    let cfg = TrackerConfig {
        model,
        follow_region,
        ..TrackerConfig::default()
    };
    let mut tracker = MotionTracker::new(&first, rect, cfg).map_err(analysis_err)?;
    let c0 = tracker.region().centre();
    let mut steps = vec![Similarity::IDENTITY];
    let mut centres = vec![(c0.x as f64, c0.y as f64)];
    let mut prev_served = first_idx;
    for &want in &plan.idx[1..] {
        let (frame, served) = feeder.gray_at(want)?;
        if served == prev_served {
            // Same decoded frame again (end of stream or a stalled source): no motion.
            steps.push(Similarity::IDENTITY);
        } else {
            steps.push(tracker.step(frame).motion);
            prev_served = served;
        }
        let c = tracker.region().centre();
        centres.push((c.x as f64, c.y as f64));
    }
    Ok(MotionSeries {
        times: plan.times,
        steps,
        centres,
        width: w as u32,
        height: h as u32,
    })
}

/// Fraction of the source frame visible on each axis under the compositor's cover fit.
pub(super) fn cover_fractions(src_w: f64, src_h: f64, out_w: f64, out_h: f64) -> (f64, f64) {
    let layer_aspect = src_w / src_h;
    let out_aspect = out_w / out_h;
    if layer_aspect > out_aspect {
        (out_aspect / layer_aspect, 1.0)
    } else {
        (1.0, layer_aspect / out_aspect)
    }
}

pub(super) fn key(time_secs: f64, value: f64) -> Keyframe {
    Keyframe {
        time_secs,
        value,
        easing: Easing::Linear,
        handle_left: None,
        handle_right: None,
    }
}

fn set_tracks(clip: &mut MediaClip, tracks: Vec<(AnimProperty, Vec<Keyframe>)>) {
    clip.keyframes
        .retain(|t| !tracks.iter().any(|(p, _)| *p == t.property));
    for (property, keys) in tracks {
        clip.keyframes.push(KeyframeTrack { property, keys });
    }
}

pub(super) fn keys_simplified(times: &[f64], values: &[f64], tolerance: f64) -> Vec<Keyframe> {
    simplify_polyline(times, values, tolerance)
        .into_iter()
        .map(|i| key(times[i], values[i]))
        .collect()
}

fn lerp_series(times: &[f64], values: &[f64], t: f64) -> f64 {
    if t <= times[0] {
        return values[0];
    }
    let last = times.len() - 1;
    if t >= times[last] {
        return values[last];
    }
    let hi = times.partition_point(|&x| x <= t).clamp(1, last);
    let lo = hi - 1;
    let span = (times[hi] - times[lo]).max(1e-12);
    values[lo] + (values[hi] - values[lo]) * (t - times[lo]) / span
}

pub(super) fn out_size(project: &Project) -> (f64, f64) {
    (
        project.settings.width.max(1) as f64,
        project.settings.height.max(1) as f64,
    )
}

fn validate_region(r: &TrackRegion) -> Result<(), CommandError> {
    let finite = [r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite());
    let inside = r.x >= 0.0
        && r.y >= 0.0
        && r.width > 0.0
        && r.height > 0.0
        && r.x + r.width <= 1.0 + 1e-9
        && r.y + r.height <= 1.0 + 1e-9;
    if finite && inside {
        Ok(())
    } else {
        Err(CommandError::Analysis(
            "region must be a normalised rect inside 0..1 with positive width/height".into(),
        ))
    }
}

pub(super) fn track_motion(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    sample_count: u32,
    region: Option<TrackRegion>,
    track_rotation_scale: bool,
) -> Result<CommandOutcome, CommandError> {
    let vc = resolve(project, track_id, clip_id)?;
    if let Some(r) = &region {
        validate_region(r)?;
    }
    if vc.kind == MediaKind::Image {
        // A still never moves: nothing to track.
        return Ok(CommandOutcome::Applied);
    }
    let region = region.unwrap_or(TrackRegion {
        x: 0.25,
        y: 0.25,
        width: 0.5,
        height: 0.5,
    });
    let model = if track_rotation_scale {
        MotionModel::Similarity
    } else {
        MotionModel::Translation
    };
    let series = analyse_motion(&vc, None, model, Some(region), true)?;
    let (out_w, out_h) = out_size(project);
    let mut curves = tracking_curves(
        &series,
        out_w,
        out_h,
        sample_count as usize,
        track_rotation_scale,
    );
    // `tracking_curves` places the point in the centred cover window. A static cover-fit
    // position on the clip slides that window, so the tracked point sits this far further
    // along the canvas (in NDC, before the clip transform). Zero for fit 0.
    let fit = vc.clip.transform.clamp_fit();
    let (cs_x, cs_y) = cover_fractions(series.width as f64, series.height as f64, out_w, out_h);
    let (shift_x, shift_y) = (
        -(1.0 - cs_x) * fit.fit_x / cs_x,
        (1.0 - cs_y) * fit.fit_y / cs_y,
    );
    for v in &mut curves.pos_x {
        *v += shift_x;
    }
    for v in &mut curves.pos_y {
        *v += shift_y;
    }
    let mut tracks = vec![
        (AnimProperty::PosX, keys_from(&curves.times, &curves.pos_x)),
        (AnimProperty::PosY, keys_from(&curves.times, &curves.pos_y)),
    ];
    if let Some((rot, scale)) = curves.rot_scale {
        tracks.push((AnimProperty::Rotation, keys_from(&curves.times, &rot)));
        tracks.push((AnimProperty::ScaleX, keys_from(&curves.times, &scale)));
        tracks.push((AnimProperty::ScaleY, keys_from(&curves.times, &scale)));
    }
    let clip = super::media_clip_mut(project, track_id, clip_id)?;
    set_tracks(clip, tracks);
    Ok(CommandOutcome::Applied)
}

fn keys_from(times: &[f64], values: &[f64]) -> Vec<Keyframe> {
    times.iter().zip(values).map(|(&t, &v)| key(t, v)).collect()
}

struct TrackingCurves {
    times: Vec<f64>,
    pos_x: Vec<f64>,
    pos_y: Vec<f64>,
    /// (rotation degrees, uniform scale) when requested.
    rot_scale: Option<(Vec<f64>, Vec<f64>)>,
}

/// Tracked point -> NDC keyframe values at `samples` evenly spaced times.
fn tracking_curves(
    series: &MotionSeries,
    out_w: f64,
    out_h: f64,
    samples: usize,
    rot_scale: bool,
) -> TrackingCurves {
    let (w, h) = (series.width as f64, series.height as f64);
    let (cs_x, cs_y) = cover_fractions(w, h, out_w, out_h);
    let n = series.times.len();
    let m = samples.clamp(2, n.max(2));
    let dur = series.times[n - 1];
    let ndc_x: Vec<f64> = series
        .centres
        .iter()
        .map(|c| 2.0 * (0.5 + (c.0 / w - 0.5) / cs_x) - 1.0)
        .collect();
    let ndc_y: Vec<f64> = series
        .centres
        .iter()
        .map(|c| 1.0 - 2.0 * (0.5 + (c.1 / h - 0.5) / cs_y))
        .collect();
    let (mut rot, mut scale) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let mut cum = Similarity::IDENTITY;
    for s in &series.steps {
        cum = cum.then(s);
        rot.push(-cum.angle().to_degrees());
        scale.push(cum.scale());
    }
    let times: Vec<f64> = (0..m).map(|j| dur * j as f64 / (m - 1) as f64).collect();
    let sample = |v: &[f64]| -> Vec<f64> {
        times
            .iter()
            .map(|&t| lerp_series(&series.times, v, t))
            .collect()
    };
    TrackingCurves {
        pos_x: sample(&ndc_x),
        pos_y: sample(&ndc_y),
        rot_scale: rot_scale.then(|| (sample(&rot), sample(&scale))),
        times,
    }
}

pub(super) fn stabilize_clip(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    strength: Option<f64>,
    sample_count: Option<u32>,
) -> Result<CommandOutcome, CommandError> {
    let vc = resolve(project, track_id, clip_id)?;
    let strength = strength.unwrap_or(DEFAULT_STRENGTH);
    if !strength.is_finite() {
        return Err(CommandError::Analysis(
            "strength must be a finite number in 0..1".into(),
        ));
    }
    let strength = strength.clamp(0.0, 1.0);
    if vc.kind == MediaKind::Image {
        // A still has no camera shake: leave the clip untouched.
        return Ok(CommandOutcome::Applied);
    }
    let series = analyse_motion(
        &vc,
        sample_count.map(|n| n as usize),
        MotionModel::Similarity,
        None,
        false,
    )?;
    let (out_w, out_h) = out_size(project);
    let sigma_secs = SIGMA_BASE_SECS + SIGMA_PER_STRENGTH_SECS * strength;
    let curves = stabilization_curves(&series, out_w, out_h, sigma_secs, &vc.clip.transform);
    let tracks = vec![
        (
            AnimProperty::PosX,
            keys_simplified(&curves.times, &curves.pos_x, POS_TOLERANCE),
        ),
        (
            AnimProperty::PosY,
            keys_simplified(&curves.times, &curves.pos_y, POS_TOLERANCE),
        ),
        (
            AnimProperty::Rotation,
            keys_simplified(&curves.times, &curves.rotation_deg, ROT_TOLERANCE),
        ),
        (
            AnimProperty::ScaleX,
            keys_simplified(&curves.times, &curves.scale_x, SCALE_TOLERANCE),
        ),
        (
            AnimProperty::ScaleY,
            keys_simplified(&curves.times, &curves.scale_y, SCALE_TOLERANCE),
        ),
    ];
    let clip = super::media_clip_mut(project, track_id, clip_id)?;
    set_tracks(clip, tracks);
    Ok(CommandOutcome::Applied)
}

struct StabilizationCurves {
    times: Vec<f64>,
    pos_x: Vec<f64>,
    pos_y: Vec<f64>,
    rotation_deg: Vec<f64>,
    scale_x: Vec<f64>,
    scale_y: Vec<f64>,
}

/// Camera path -> smoothed path -> compensating compositor transform per frame.
///
/// With `C_k` the accumulated motion of frame `k` relative to frame 0 and `S_k` its smoothed
/// version, frame `k` must be warped by `S_k * C_k^-1` so the content sits where the smooth
/// camera would have seen it. All maths is done about the frame centre in y-down pixels, then
/// mapped to NDC / y-up angles.
fn stabilization_curves(
    series: &MotionSeries,
    out_w: f64,
    out_h: f64,
    sigma_secs: f64,
    base: &ClipTransform,
) -> StabilizationCurves {
    let n = series.times.len();
    let (w, h) = (series.width as f64, series.height as f64);
    let (cx, cy) = (w * 0.5, h * 0.5);

    // Accumulated path in centred coordinates: translation, angle, log-scale.
    let (mut tx, mut ty) = (vec![0.0; n], vec![0.0; n]);
    let (mut th, mut ls) = (vec![0.0; n], vec![0.0; n]);
    let mut cum = Similarity::IDENTITY;
    for i in 1..n {
        let s = series.steps[i];
        // Re-express the top-left-origin step about the frame centre: z' = m z + (m c + t - c).
        let centred = Similarity {
            a: s.a,
            b: s.b,
            tx: s.a * cx - s.b * cy + s.tx - cx,
            ty: s.b * cx + s.a * cy + s.ty - cy,
        };
        cum = cum.then(&centred);
        tx[i] = cum.tx;
        ty[i] = cum.ty;
        th[i] = th[i - 1] + s.angle();
        ls[i] = ls[i - 1] + s.scale().max(1e-6).ln();
    }

    let dt = series.times[n - 1] / (n - 1).max(1) as f64;
    let sigma = if dt > 1e-9 { sigma_secs / dt } else { 0.0 };
    let (stx, sty) = (gaussian_smooth(&tx, sigma), gaussian_smooth(&ty, sigma));
    let (sth, sls) = (gaussian_smooth(&th, sigma), gaussian_smooth(&ls, sigma));

    let (cs_x, cs_y) = cover_fractions(w, h, out_w, out_h);
    struct Corr {
        ndc_x: f64,
        ndc_y: f64,
        rot_rad: f64,
        rho: f64,
    }
    let corr: Vec<Corr> = (0..n)
        .map(|i| {
            // m_corr = exp(dls + i*dth); t_corr = t_s - m_corr * t_c.
            let (dls, dth) = (sls[i] - ls[i], sth[i] - th[i]);
            let rho = dls.exp();
            let (ma, mb) = (rho * dth.cos(), rho * dth.sin());
            let ux = stx[i] - (ma * tx[i] - mb * ty[i]);
            let uy = sty[i] - (mb * tx[i] + ma * ty[i]);
            Corr {
                ndc_x: 2.0 * ux / (cs_x * w),
                ndc_y: -2.0 * uy / (cs_y * h),
                rot_rad: -dth,
                rho,
            }
        })
        .collect();

    // Constant zoom so the warped layer still covers the canvas in every frame.
    let base_cover = base.scale_x.min(base.scale_y).max(1e-3);
    let mut need = 1.0f64;
    for c in &corr {
        let (sin, cos) = c.rot_rad.sin_cos();
        for (px, py) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
            let (dx, dy) = (px - c.ndc_x, py - c.ndc_y);
            // Inverse-rotate the corner into the layer's frame.
            let (lx, ly) = (dx * cos + dy * sin, -dx * sin + dy * cos);
            need = need.max(lx.abs().max(ly.abs()) / (c.rho * base_cover));
        }
    }
    let zoom = if need > 1.0 {
        (need * 1.005).min(MAX_AUTO_ZOOM)
    } else {
        1.0
    };

    let mut out = StabilizationCurves {
        times: series.times.clone(),
        pos_x: Vec::with_capacity(n),
        pos_y: Vec::with_capacity(n),
        rotation_deg: Vec::with_capacity(n),
        scale_x: Vec::with_capacity(n),
        scale_y: Vec::with_capacity(n),
    };
    for c in &corr {
        // Compose with the clip's static transform: correction acts on the already-placed layer.
        let (sin, cos) = c.rot_rad.sin_cos();
        out.pos_x
            .push(c.rho * (cos * base.x - sin * base.y) + c.ndc_x);
        out.pos_y
            .push(c.rho * (sin * base.x + cos * base.y) + c.ndc_y);
        out.rotation_deg
            .push(base.rotation_deg + c.rot_rad.to_degrees());
        out.scale_x.push(zoom * c.rho * base.scale_x);
        out.scale_y.push(zoom * c.rho * base.scale_y);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{apply_command, Command};
    use crate::proc::SerialSpawn;
    use crate::project::{evaluate_transform, MediaItem, Settings};
    use crate::tracking::test_support::texture;
    use std::io::Write;
    use std::process::{Command as Proc, Stdio};

    fn series_from_pan(offsets: &[(f64, f64)], dt: f64) -> MotionSeries {
        // Content moves by -offset (camera pans by +offset).
        let mut steps = vec![Similarity::IDENTITY];
        for w in offsets.windows(2) {
            steps.push(Similarity::from_translation(
                -(w[1].0 - w[0].0),
                -(w[1].1 - w[0].1),
            ));
        }
        let n = offsets.len();
        MotionSeries {
            times: (0..n).map(|i| i as f64 * dt).collect(),
            steps,
            centres: vec![(0.0, 0.0); n],
            width: 400,
            height: 300,
        }
    }

    fn second_diff_rms(v: &[f64]) -> f64 {
        let s: f64 = v
            .windows(3)
            .map(|w| (w[0] - 2.0 * w[1] + w[2]).powi(2))
            .sum();
        (s / (v.len() - 2) as f64).sqrt()
    }

    #[test]
    fn curves_cancel_translation_jitter_and_keep_the_pan() {
        // Output matches the source aspect so cover is identity: 400x300 px <-> NDC +-1.
        let n = 150;
        let dt = 1.0 / 30.0;
        let offsets: Vec<(f64, f64)> = (0..n)
            .map(|i| {
                let t = i as f64 * dt;
                (
                    30.0 * t
                        + 6.0 * (std::f64::consts::TAU * 3.3 * t).sin()
                        + 2.0 * ((i * 7 % 5) as f64 - 2.0),
                    8.0 * t + 4.0 * (std::f64::consts::TAU * 2.1 * t + 1.0).sin(),
                )
            })
            .collect();
        let series = series_from_pan(&offsets, dt);
        let c = stabilization_curves(&series, 400.0, 300.0, 0.8, &ClipTransform::default());
        // Where the frame centre content ends up (px, y-down): -offset + correction.
        let rx: Vec<f64> = (0..n).map(|i| -offsets[i].0 + c.pos_x[i] * 200.0).collect();
        let ry: Vec<f64> = (0..n).map(|i| -offsets[i].1 - c.pos_y[i] * 150.0).collect();
        let raw_x: Vec<f64> = offsets.iter().map(|o| -o.0).collect();
        let raw_y: Vec<f64> = offsets.iter().map(|o| -o.1).collect();
        assert!(second_diff_rms(&raw_x) / second_diff_rms(&rx) > 8.0);
        assert!(second_diff_rms(&raw_y) / second_diff_rms(&ry) > 8.0);
        // Residual path follows the ideal pan (-30 t, -8 t) closely.
        for i in 0..n {
            let t = i as f64 * dt;
            assert!(
                (rx[i] + 30.0 * t).abs() < 2.0,
                "x[{i}] {} vs {}",
                rx[i],
                -30.0 * t
            );
            assert!((ry[i] + 8.0 * t).abs() < 1.5, "y[{i}]");
        }
        // Pure translation: no rotation, scale differs from the zoom only by the constant.
        assert!(c.rotation_deg.iter().all(|r| r.abs() < 1e-9));
        let z = c.scale_x[0];
        assert!(z > 1.0 && z <= MAX_AUTO_ZOOM && c.scale_x.iter().all(|s| (s - z).abs() < 1e-9));
    }

    /// Rotation by `angle` (y-down, radians) about the centre of a 400x300 frame, expressed in
    /// the top-left-origin pixel frame the tracker reports.
    fn roll_about_centre(angle: f64) -> Similarity {
        let m = Similarity::from_parts(1.0, angle, 0.0, 0.0);
        let (px, py) = m.apply(200.0, 150.0);
        Similarity::from_parts(1.0, angle, 200.0 - px, 150.0 - py)
    }

    #[test]
    fn curves_compensate_rotation_with_compositor_sign_and_zoom_covers_the_frame() {
        // Camera roll flickers between 0 and +theta (y-down) every frame: a pure high-frequency
        // jitter, so the smoothed path sits at theta/2 and the correction is -+theta/2.
        let n = 80;
        let dt = 1.0 / 30.0;
        let mut series = series_from_pan(&vec![(0.0, 0.0); n], dt);
        let theta = 0.06;
        for k in 1..n {
            series.steps[k] = roll_about_centre(if k % 2 == 1 { theta } else { -theta });
        }
        let c = stabilization_curves(&series, 400.0, 300.0, 0.8, &ClipTransform::default());
        let half = (theta * 0.5).to_degrees();
        // Odd frames are rolled +theta in image coordinates; the warp un-rolls them, which is a
        // positive (counter-clockwise on screen) compositor rotation after the y-flip.
        assert!(
            (c.rotation_deg[41] - half).abs() < 0.15,
            "{}",
            c.rotation_deg[41]
        );
        assert!(
            (c.rotation_deg[40] + half).abs() < 0.15,
            "{}",
            c.rotation_deg[40]
        );
        // Rotating about the centre leaves the centre in place.
        assert!(c.pos_x[41].abs() < 0.01 && c.pos_y[41].abs() < 0.01);
        let z = c.scale_x[0];
        let r = c.rotation_deg[41].to_radians();
        let worst = r.cos().abs() + r.sin().abs();
        assert!(
            z >= worst && z <= MAX_AUTO_ZOOM,
            "zoom {z} vs needed {worst}"
        );
    }

    #[test]
    fn zoom_is_capped_and_static_transform_is_composed() {
        let n = 40;
        let dt = 1.0 / 30.0;
        // A violent 380 px excursion (of 400) wants a huge zoom -> capped.
        let offsets: Vec<(f64, f64)> = (0..n)
            .map(|i| (if i == 20 { 380.0 } else { 0.0 }, 0.0))
            .collect();
        let series = series_from_pan(&offsets, dt);
        let c = stabilization_curves(&series, 400.0, 300.0, 0.02, &ClipTransform::default());
        assert!((c.scale_x[0] - MAX_AUTO_ZOOM).abs() < 1e-9);
        // A static offset on the clip survives (keyframes override static values, so it must be
        // folded in): with no motion, position keys equal the static transform.
        let still = series_from_pan(&vec![(0.0, 0.0); n], dt);
        let base = ClipTransform {
            x: 0.25,
            y: -0.1,
            scale_x: 1.5,
            scale_y: 1.5,
            rotation_deg: 10.0,
            ..ClipTransform::default()
        };
        let c = stabilization_curves(&still, 400.0, 300.0, 0.5, &base);
        assert!((c.pos_x[5] - 0.25).abs() < 1e-9 && (c.pos_y[5] + 0.1).abs() < 1e-9);
        assert!((c.scale_x[5] - 1.5).abs() < 1e-9 && (c.rotation_deg[5] - 10.0).abs() < 1e-9);
    }

    #[test]
    fn tracking_curves_use_ndc_with_cover_fit_and_y_up() {
        // Landscape 480x270 analysis frame onto a portrait 1080x1920 canvas: only the centre
        // 270*9/16 = 151.875 source px are visible horizontally.
        let n = 30;
        let series = MotionSeries {
            times: (0..n).map(|i| i as f64 / 30.0).collect(),
            steps: vec![Similarity::IDENTITY; n],
            centres: (0..n)
                .map(|i| (240.0 + 1.0 * i as f64, 135.0 - 2.0 * i as f64))
                .collect(),
            width: 480,
            height: 270,
        };
        let c = tracking_curves(&series, 1080.0, 1920.0, 30, false);
        assert!(c.pos_x[0].abs() < 1e-12 && c.pos_y[0].abs() < 1e-12);
        let vis_w = 270.0 * 1080.0 / 1920.0;
        assert!((c.pos_x[29] - 2.0 * 29.0 / vis_w).abs() < 1e-9);
        // Point moved UP in the image (y decreased) -> +y in NDC.
        assert!((c.pos_y[29] - 2.0 * 58.0 / 270.0).abs() < 1e-9);
        assert!(c.rot_scale.is_none());
        // Fewer keyframes than frames interpolate; sample_count above n is clamped.
        assert_eq!(
            tracking_curves(&series, 1080.0, 1920.0, 5, false)
                .times
                .len(),
            5
        );
        assert_eq!(
            tracking_curves(&series, 1080.0, 1920.0, 500, true)
                .times
                .len(),
            n
        );
    }

    #[test]
    fn region_validation() {
        let ok = TrackRegion {
            x: 0.1,
            y: 0.1,
            width: 0.5,
            height: 0.5,
        };
        assert!(validate_region(&ok).is_ok());
        for bad in [
            TrackRegion { width: 0.0, ..ok },
            TrackRegion { x: 0.8, ..ok },
            TrackRegion { y: -0.1, ..ok },
            TrackRegion { x: f64::NAN, ..ok },
        ] {
            assert!(validate_region(&bad).is_err());
        }
    }

    // ---- command-level tests (need ffmpeg) -----------------------------------------------

    const W: usize = 480;
    const H: usize = 270;
    const FPS: f64 = 30.0;
    const MARGIN: f64 = 80.0;

    /// Encode `frames` (offset of the viewing window into an infinite texture per frame) to an
    /// H.264 mp4. Ground truth is exact: pixel (x, y) of frame k shows texture(x+ox_k, y+oy_k).
    fn synth_video(dir: &std::path::Path, offsets: &[(f64, f64)]) -> PathBuf {
        let max = offsets.iter().fold((0.0f64, 0.0f64), |m, o| {
            (m.0.max(o.0.abs()), m.1.max(o.1.abs()))
        });
        let ww = W + 2 * (max.0.ceil() as usize + MARGIN as usize);
        let wh = H + 2 * (max.1.ceil() as usize + MARGIN as usize);
        let pad = (max.0.ceil() + MARGIN, max.1.ceil() + MARGIN);
        let world = GrayImage::from_fn(ww, wh, |x, y| texture(x as f64, y as f64));
        let path = dir.join("synth.mp4");
        let mut child = Proc::new(media::ffmpeg_path().expect("ffmpeg"))
            .args([
                "-y",
                "-loglevel",
                "error",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "gray",
            ])
            .args([
                "-video_size",
                &format!("{W}x{H}"),
                "-framerate",
                &FPS.to_string(),
            ])
            .args([
                "-i", "-", "-c:v", "libx264", "-crf", "8", "-pix_fmt", "yuv420p",
            ])
            .arg(&path)
            .stdin(Stdio::piped())
            .spawn_serial()
            .expect("spawn ffmpeg");
        {
            let stdin = child.stdin.as_mut().expect("stdin");
            let mut buf = vec![0u8; W * H];
            for &(ox, oy) in offsets {
                for y in 0..H {
                    for x in 0..W {
                        let v = world.sample(
                            (x as f64 + ox + pad.0) as f32,
                            (y as f64 + oy + pad.1) as f32,
                        );
                        buf[y * W + x] = v.round().clamp(0.0, 255.0) as u8;
                    }
                }
                stdin.write_all(&buf).expect("write frame");
            }
        }
        assert!(child.wait().expect("ffmpeg wait").success());
        path
    }

    fn project_with_clip(path: &std::path::Path, secs: f64, out: (u32, u32)) -> (Project, Id, Id) {
        let mut project = Project::new(
            "t",
            Settings {
                width: out.0,
                height: out.1,
                ..Settings::default()
            },
        );
        let media_id = Id::new_v4();
        project.media.push(MediaItem {
            id: media_id,
            path: path.to_path_buf(),
            kind: MediaKind::Video,
            duration_secs: Some(secs),
            width: Some(W as u32),
            height: Some(H as u32),
            fps: Some(FPS),
            sequence_id: None,
        });
        let track_id = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V".into(),
                id: None,
                sequence_id: None,
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
                position_secs: 2.0,
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

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("renderly-{tag}-{}", Id::new_v4()));
        std::fs::create_dir_all(&d).expect("tmpdir");
        d
    }

    #[test]
    fn track_motion_follows_known_pan() {
        if !media::ffmpeg_available() {
            eprintln!("skipping track_motion_follows_known_pan: ffmpeg not available");
            return;
        }
        let dir = tmp_dir("trk");
        let secs = 3.0;
        let (vx, vy) = (42.0, -13.0);
        let n = (secs * FPS) as usize;
        let offsets: Vec<(f64, f64)> = (0..n)
            .map(|k| (vx * k as f64 / FPS, vy * k as f64 / FPS))
            .collect();
        let video = synth_video(&dir, &offsets);
        // 16:9 canvas matching the source aspect: NDC +-1 <-> +-W/2, +-H/2 source px.
        let (mut project, tid, cid) = project_with_clip(&video, (n - 1) as f64 / FPS, (1280, 720));
        let started = std::time::Instant::now();
        apply_command(
            &mut project,
            Command::TrackMotion {
                track_id: tid,
                clip_id: cid,
                sample_count: 12,
                region: None,
                track_rotation_scale: false,
            },
        )
        .expect("track");
        eprintln!("track_motion analysis: {:?}", started.elapsed());
        let clip = clip_ref(&project, tid, cid);
        let mut max_err: f64 = 0.0;
        // Default region centre is the frame centre; content moves by -v*t.
        for j in 0..=24 {
            let local = clip.timeline_duration_secs() * j as f64 / 24.0;
            let tf = evaluate_transform(clip, clip.position_secs + local);
            let (px, py) = (tf.x * W as f64 / 2.0, -tf.y * H as f64 / 2.0);
            let (ex, ey) = (-vx * local, -vy * local);
            max_err = max_err.max((px - ex).abs()).max((py - ey).abs());
        }
        eprintln!("track_motion max error: {max_err:.3} px");
        assert!(max_err < 0.5, "tracking error {max_err} px");
        assert_eq!(
            clip.keyframes
                .iter()
                .filter(|k| k.property == AnimProperty::PosX)
                .count(),
            1
        );
        assert!(clip
            .keyframes
            .iter()
            .all(|k| k.property != AnimProperty::Rotation));

        // Explicit region + similarity mode on the same clip: start point is the region centre,
        // rotation/scale stay ~identity for a pure pan.
        apply_command(
            &mut project,
            Command::TrackMotion {
                track_id: tid,
                clip_id: cid,
                sample_count: 6,
                region: Some(TrackRegion {
                    x: 0.1,
                    y: 0.2,
                    width: 0.3,
                    height: 0.4,
                }),
                track_rotation_scale: true,
            },
        )
        .expect("track region");
        let clip = clip_ref(&project, tid, cid);
        let t0 = evaluate_transform(clip, clip.position_secs);
        let (cx, cy) = ((0.1 + 0.15) * W as f64, (0.2 + 0.2) * H as f64);
        assert!((t0.x * W as f64 / 2.0 - (cx - W as f64 / 2.0)).abs() < 1.5);
        assert!((-t0.y * H as f64 / 2.0 - (cy - H as f64 / 2.0)).abs() < 1.5);
        let tf = evaluate_transform(clip, clip.position_secs + 2.0);
        assert!(
            tf.rotation_deg.abs() < 0.3 && (tf.scale_x - 1.0).abs() < 0.01,
            "{tf:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stabilize_cancels_injected_jitter_and_keeps_the_pan() {
        if !media::ffmpeg_available() {
            eprintln!("skipping stabilize test: ffmpeg not available");
            return;
        }
        let dir = tmp_dir("stab");
        let secs = 4.0;
        let n = (secs * FPS) as usize;
        let mut seed = 12345u64;
        let mut rnd = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0
        };
        let pan = |t: f64| (38.0 * t, 9.0 * t);
        let offsets: Vec<(f64, f64)> = (0..n)
            .map(|k| {
                let t = k as f64 / FPS;
                let (px, py) = pan(t);
                (
                    px + 5.0 * (std::f64::consts::TAU * 3.1 * t).sin() + 2.0 * rnd(),
                    py + 4.0 * (std::f64::consts::TAU * 2.3 * t + 1.0).sin() + 1.5 * rnd(),
                )
            })
            .collect();
        let video = synth_video(&dir, &offsets);
        let (mut project, tid, cid) = project_with_clip(&video, (n - 1) as f64 / FPS, (1280, 720));
        let started = std::time::Instant::now();
        apply_command(
            &mut project,
            Command::StabilizeClip {
                track_id: tid,
                clip_id: cid,
                strength: None,
                sample_count: None,
            },
        )
        .expect("stabilize");
        eprintln!("stabilize analysis: {:?}", started.elapsed());
        let clip = clip_ref(&project, tid, cid);
        assert!(clip.keyframes.len() == 5);
        let keys: usize = clip.keyframes.iter().map(|k| k.keys.len()).sum();
        eprintln!("stabilize keyframes written: {keys}");
        // Frame-centre content path after correction (px, y-down), through the real evaluator.
        let mut rx = Vec::new();
        let mut ry = Vec::new();
        for (k, o) in offsets.iter().enumerate() {
            let tf = evaluate_transform(clip, clip.position_secs + k as f64 / FPS);
            rx.push(-o.0 + tf.x * W as f64 / 2.0);
            ry.push(-o.1 - tf.y * H as f64 / 2.0);
        }
        let raw_x: Vec<f64> = offsets.iter().map(|o| -o.0).collect();
        let raw_y: Vec<f64> = offsets.iter().map(|o| -o.1).collect();
        let fx = second_diff_rms(&raw_x) / second_diff_rms(&rx);
        let fy = second_diff_rms(&raw_y) / second_diff_rms(&ry);
        eprintln!("stabilize jitter reduction: x {fx:.1}x, y {fy:.1}x");
        assert!(fx > 3.0 && fy > 3.0, "reduction x {fx} y {fy}");
        let mut worst: f64 = 0.0;
        for k in 0..n {
            let (px, py) = pan(k as f64 / FPS);
            worst = worst.max((rx[k] + px).abs()).max((ry[k] + py).abs());
        }
        eprintln!("stabilize max deviation from ideal pan: {worst:.2} px");
        assert!(worst < 4.0, "pan not preserved: {worst} px");
        let tf = evaluate_transform(clip, clip.position_secs + 1.0);
        assert!(tf.scale_x >= 1.0 && tf.scale_x <= MAX_AUTO_ZOOM);
        assert!(tf.rotation_deg.abs() < 0.3);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Wall-clock check on a 9 s 1080p clip. Run with
    /// `cargo test --release -p renderly-core --lib analysis_runtime -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn analysis_runtime_on_1080p_clip() {
        if !media::ffmpeg_available() {
            return;
        }
        let dir = tmp_dir("perf");
        let n = 90usize;
        let offsets: Vec<(f64, f64)> = (0..n).map(|k| (1.3 * k as f64, 0.4 * k as f64)).collect();
        let small = synth_video(&dir, &offsets);
        let big = dir.join("big.mp4");
        let ok = Proc::new(media::ffmpeg_path().expect("ffmpeg"))
            .args(["-y", "-loglevel", "error", "-stream_loop", "2", "-i"])
            .arg(&small)
            .args([
                "-vf",
                "scale=1920:1080",
                "-t",
                "9",
                "-c:v",
                "libx264",
                "-crf",
                "14",
            ])
            .arg(&big)
            .status_serial()
            .expect("ffmpeg")
            .success();
        assert!(ok);
        let (mut project, tid, cid) = project_with_clip(&big, 9.0, (1920, 1080));
        for (name, cmd) in [
            (
                "StabilizeClip",
                Command::StabilizeClip {
                    track_id: tid,
                    clip_id: cid,
                    strength: None,
                    sample_count: None,
                },
            ),
            (
                "TrackMotion",
                Command::TrackMotion {
                    track_id: tid,
                    clip_id: cid,
                    sample_count: 12,
                    region: None,
                    track_rotation_scale: true,
                },
            ),
        ] {
            let started = std::time::Instant::now();
            apply_command(&mut project, cmd).expect(name);
            eprintln!(
                "{name} on 9 s 1080p30 (270 frames): {:?}",
                started.elapsed()
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn still_images_are_a_noop_and_audio_is_rejected() {
        let dir = tmp_dir("still");
        let png = dir.join("a.png");
        image::RgbaImage::from_pixel(64, 64, image::Rgba([10, 20, 30, 255]))
            .save(&png)
            .expect("png");
        let mut project = Project::new("t", Settings::default());
        let media_id = Id::new_v4();
        project.media.push(MediaItem {
            id: media_id,
            path: png,
            kind: MediaKind::Image,
            duration_secs: None,
            width: Some(64),
            height: Some(64),
            fps: None,
            sequence_id: None,
        });
        let tid = match apply_command(
            &mut project,
            Command::AddTrack {
                kind: TrackKind::Video,
                name: "V".into(),
                id: None,
                sequence_id: None,
            },
        )
        .expect("track")
        {
            CommandOutcome::TrackAdded { track_id } => track_id,
            _ => unreachable!(),
        };
        let cid = match apply_command(
            &mut project,
            Command::AddClip {
                track_id: tid,
                media_id,
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 3.0,
            },
        )
        .expect("clip")
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => unreachable!(),
        };
        for cmd in [
            Command::TrackMotion {
                track_id: tid,
                clip_id: cid,
                sample_count: 12,
                region: None,
                track_rotation_scale: false,
            },
            Command::StabilizeClip {
                track_id: tid,
                clip_id: cid,
                strength: Some(1.0),
                sample_count: None,
            },
        ] {
            assert!(matches!(
                apply_command(&mut project, cmd),
                Ok(CommandOutcome::Applied)
            ));
        }
        assert!(clip_ref(&project, tid, cid).keyframes.is_empty());
        // Invalid parameters are rejected before any decoding.
        assert!(apply_command(
            &mut project,
            Command::StabilizeClip {
                track_id: tid,
                clip_id: cid,
                strength: Some(f64::NAN),
                sample_count: None,
            }
        )
        .is_err());
        assert!(apply_command(
            &mut project,
            Command::TrackMotion {
                track_id: tid,
                clip_id: cid,
                sample_count: 4,
                region: Some(TrackRegion {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 1.0
                }),
                track_rotation_scale: false,
            }
        )
        .is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn legacy_json_still_deserialises() {
        let track = Id::new_v4();
        let clip = Id::new_v4();
        let old_track: Command = serde_json::from_str(&format!(
            r#"{{"command":"TrackMotion","track_id":"{track}","clip_id":"{clip}","sample_count":12}}"#
        ))
        .expect("old TrackMotion json");
        assert!(matches!(
            old_track,
            Command::TrackMotion {
                region: None,
                track_rotation_scale: false,
                ..
            }
        ));
        let old_stab: Command = serde_json::from_str(&format!(
            r#"{{"command":"StabilizeClip","track_id":"{track}","clip_id":"{clip}"}}"#
        ))
        .expect("old StabilizeClip json");
        assert!(matches!(
            old_stab,
            Command::StabilizeClip {
                strength: None,
                sample_count: None,
                ..
            }
        ));
    }
}
