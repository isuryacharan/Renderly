//! Keyframe evaluation: interpolates clip transform / volume / speed. Times are integer µs
//! (schema v8); interpolation weights are `f64` (dimensionless ratios).

use super::keys::{default_handles, interpolate_value, span_scale};
use super::{
    clamp_clip_speed, AdjustmentClip, AnimProperty, ClipTransform, Easing, EffectInstance,
    EffectParam, KeyValue, Keyframe, KeyframeTrack, MediaClip, TextClip, TrackProperty,
};

/// Evaluate the effective transform at `timeline_us` (absolute on the project timeline).
pub fn evaluate_transform(clip: &MediaClip, timeline_us: i64) -> ClipTransform {
    evaluate_transform_of(
        &clip.transform,
        &clip.keyframes,
        (timeline_us - clip.start_us).max(0),
    )
}

/// [`evaluate_transform`] for a text clip.
pub fn evaluate_text_transform(clip: &TextClip, timeline_us: i64) -> ClipTransform {
    evaluate_transform_of(
        &clip.transform,
        &clip.keyframes,
        (timeline_us - clip.start_us).max(0),
    )
}

/// The transform `base` with the keyframe tracks in `keyframes` sampled at clip-local `local`.
fn evaluate_transform_of(
    base: &ClipTransform,
    keyframes: &[KeyframeTrack],
    local: i64,
) -> ClipTransform {
    let mut t = base.clamp_opacity().clamp_fit();
    for track in keyframes {
        let Some(property) = track.property.anim() else {
            continue;
        };
        let Some(v) = sample_track(track, local) else {
            continue;
        };
        match property {
            AnimProperty::PosX => t.x = v,
            AnimProperty::PosY => t.y = v,
            AnimProperty::ScaleX => t.scale_x = v,
            AnimProperty::ScaleY => t.scale_y = v,
            AnimProperty::Rotation => t.rotation_deg = v,
            AnimProperty::Opacity => t.opacity = v.clamp(0.0, 1.0),
            AnimProperty::FitX => t.fit_x = v.clamp(-1.0, 1.0),
            AnimProperty::FitY => t.fit_y = v.clamp(-1.0, 1.0),
            AnimProperty::Volume | AnimProperty::Speed => {}
        }
    }
    t
}

/// Effective opacity of an adjustment layer at absolute timeline time `timeline_us`.
pub fn evaluate_adjustment_opacity(clip: &AdjustmentClip, timeline_us: i64) -> f64 {
    let local = (timeline_us - clip.start_us).max(0);
    for track in &clip.keyframes {
        if track.property == AnimProperty::Opacity {
            if let Some(v) = sample_track(track, local) {
                return v.clamp(0.0, 1.0);
            }
        }
    }
    clip.opacity.clamp(0.0, 1.0)
}

/// Effective gain in dB: base `gain_db` plus optional `Volume` keyframe track
/// (keyframe values are absolute dB when present, replacing the static gain).
pub fn evaluate_volume_db(clip: &MediaClip, timeline_us: i64) -> f64 {
    let local = (timeline_us - clip.start_us).max(0);
    for track in &clip.keyframes {
        if track.property == AnimProperty::Volume {
            if let Some(v) = sample_track(track, local) {
                return v;
            }
        }
    }
    clip.gain_db
}

/// Instantaneous playback speed at absolute timeline time (Speed keys or base `speed`).
pub fn evaluate_speed(clip: &MediaClip, timeline_us: i64) -> f64 {
    let local = (timeline_us - clip.start_us).max(0);
    for track in &clip.keyframes {
        if track.property == AnimProperty::Speed {
            if let Some(v) = sample_track(track, local) {
                return clamp_clip_speed(v);
            }
        }
    }
    clip.speed_factor()
}

fn has_speed_keys(clip: &MediaClip) -> bool {
    clip.keyframes
        .iter()
        .any(|t| t.property == AnimProperty::Speed && !t.keys.is_empty())
}

/// Integration step for keyframed speed: 1/120 s in µs (rounded).
const INTEGRATE_DT_US: i64 = 8_333;

/// ∫ speed(τ) dτ over clip-local `[local_from_us, local_to_us]`, in source µs (Riemann sum
/// with integer steps; the speed weight is `f64`, each step is rounded to µs).
pub fn integrate_speed(clip: &MediaClip, local_from_us: i64, local_to_us: i64) -> i64 {
    let from = local_from_us.max(0);
    let to = local_to_us.max(from);
    if to == from {
        return 0;
    }
    if !has_speed_keys(clip) {
        return ((to - from) as f64 * clip.speed_factor()).round() as i64;
    }
    // Accumulate in f64 and round once: rounding each step would drift by up to half a µs
    // per step over a long clip.
    let mut acc = 0.0_f64;
    let mut t = from;
    while t < to {
        let step = (to - t).min(INTEGRATE_DT_US);
        let mid = t + step / 2;
        let s = evaluate_speed(clip, clip.start_us + mid);
        acc += s * step as f64;
        t += step;
    }
    acc.round() as i64
}

/// Timeline duration such that ∫_0^T speed = source span, in µs.
pub fn timeline_duration_us(clip: &MediaClip) -> i64 {
    let src = (clip.source_out_us - clip.source_in_us).max(0);
    if src == 0 {
        return 0;
    }
    if !has_speed_keys(clip) {
        return (src as f64 / clip.speed_factor()).round() as i64;
    }
    // Walk forward in integer steps until the integral covers the source; after the last
    // key, speed is held. The worst case is the minimum speed (0.25x).
    let max_t = src * 4 + INTEGRATE_DT_US;
    let mut t: i64 = 0;
    let mut covered = 0.0_f64;
    let src_f = src as f64;
    while covered < src_f && t < max_t {
        let step = INTEGRATE_DT_US;
        let s = evaluate_speed(clip, clip.start_us + t + step / 2);
        let remain = src_f - covered;
        let need = (remain / s.max(1e-6)).round() as i64;
        if need <= step {
            return t + need;
        }
        covered += s * step as f64;
        t += step;
    }
    t
}

/// Media source time (µs) corresponding to absolute timeline time `t_us`.
pub fn source_time_at(clip: &MediaClip, t_us: i64) -> i64 {
    let local = (t_us - clip.start_us).max(0);
    clip.source_in_us + integrate_speed(clip, 0, local)
}

/// Break a clip into constant-speed audio segments for atempo chaining.
/// Returns `(timeline_offset_from_clip_start_us, duration_timeline_us, speed)` triples.
pub fn speed_segments(clip: &MediaClip) -> Vec<(i64, i64, f64)> {
    let dur = timeline_duration_us(clip);
    if dur <= 0 {
        return Vec::new();
    }
    if !has_speed_keys(clip) {
        return vec![(0, dur, clip.speed_factor())];
    }
    let mut out = Vec::new();
    let mut t: i64 = 0;
    const SEG_US: i64 = 50_000; // 50ms windows
    while t < dur {
        let len = (dur - t).min(SEG_US);
        let s = evaluate_speed(clip, clip.start_us + t + len / 2);
        out.push((t, len, s));
        t += len;
    }
    out
}

/// Scalar value of a track at clip-local `local_us` (first component for array-valued tracks).
fn sample_track(track: &KeyframeTrack, local_us: i64) -> Option<f64> {
    sample_track_value(track, local_us).map(|v| v.num())
}

/// Value of a keyframe track at clip-local `local_us`: before the first key and after the last
/// the end values hold; between keys the span after the earlier key is interpolated by that
/// key's easing. Array values interpolate per component.
pub fn sample_track_value(track: &KeyframeTrack, local_us: i64) -> Option<KeyValue> {
    if track.keys.is_empty() {
        return None;
    }
    let mut sorted: Vec<&Keyframe> = track.keys.iter().collect();
    sorted.sort_by_key(|k| k.time_us);
    let keys = sorted;

    if local_us <= keys[0].time_us {
        return Some(keys[0].value.clone());
    }
    if local_us >= keys[keys.len() - 1].time_us {
        return Some(keys[keys.len() - 1].value.clone());
    }

    for i in 0..keys.len() - 1 {
        let a = keys[i];
        let b = keys[i + 1];
        // Half-open, so a key's own time takes that key's value (matters for `hold`).
        if local_us >= a.time_us && local_us < b.time_us {
            let span_us = (b.time_us - a.time_us).max(1);
            let span = span_us as f64;
            let u = (((local_us - a.time_us) as f64) / span).clamp(0.0, 1.0);
            return Some(match a.easing {
                Easing::Hold => a.value.clone(),
                Easing::Bezier => {
                    // P0 = (0, 0), P3 = (1, 1) in normalised (time, value) space; P1 and P2 are
                    // the handles divided by the span's time and value change.
                    let (hr, hl) = default_handles(a, b);
                    let scale = span_scale(a, b);
                    let p1x = hr.0 as f64 / span;
                    let p1y = if scale.abs() < 1e-9 {
                        0.0
                    } else {
                        hr.1 / scale
                    };
                    let p2x = 1.0 + hl.0 as f64 / span;
                    let p2y = if scale.abs() < 1e-9 {
                        1.0
                    } else {
                        1.0 + hl.1 / scale
                    };
                    let eased = solve_cubic_bezier(u, p1x, p1y, p2x, p2y);
                    interpolate_value(&a.value, &b.value, eased)
                }
                Easing::Auto => sample_auto(&keys, i, u),
                Easing::Linear => interpolate_value(&a.value, &b.value, u),
                Easing::EaseInOut => interpolate_value(&a.value, &b.value, legacy_ease_in_out(u)),
            });
        }
    }
    None
}

/// The v7 ease-in-out: a piecewise quadratic, kept so migrated projects render identically.
fn legacy_ease_in_out(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        2.0 * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
    }
}

/// Monotone cubic (Fritsch-Carlson / PCHIP) interpolation across the span `i -> i + 1`, with
/// tangents derived from the neighbouring keys. Never overshoots the keys' values.
fn sample_auto(keys: &[&Keyframe], i: usize, u: f64) -> KeyValue {
    let a = keys[i];
    let b = keys[i + 1];
    let h = (b.time_us - a.time_us).max(1) as f64;
    let comps = a.value.components().max(b.value.components());
    let mut out = Vec::with_capacity(comps);
    for c in 0..comps {
        let m0 = auto_tangent(keys, i, c);
        let m1 = auto_tangent(keys, i + 1, c);
        let (v0, v1) = (a.value.component(c), b.value.component(c));
        let u2 = u * u;
        let u3 = u2 * u;
        let h00 = 2.0 * u3 - 3.0 * u2 + 1.0;
        let h10 = u3 - 2.0 * u2 + u;
        let h01 = -2.0 * u3 + 3.0 * u2;
        let h11 = u3 - u2;
        out.push(h00 * v0 + h10 * h * m0 + h01 * v1 + h11 * h * m1);
    }
    if a.value.is_vector() || b.value.is_vector() {
        KeyValue::Vector(out)
    } else {
        KeyValue::Number(out[0])
    }
}

/// Tangent (value per µs) at key `i` for component `c`: weighted harmonic mean of the adjacent
/// secants, zero at local extrema; one-sided three-point estimate at the ends.
fn auto_tangent(keys: &[&Keyframe], i: usize, c: usize) -> f64 {
    let n = keys.len();
    let secant = |j: usize| {
        let dt = (keys[j + 1].time_us - keys[j].time_us).max(1) as f64;
        (keys[j + 1].value.component(c) - keys[j].value.component(c)) / dt
    };
    let width = |j: usize| (keys[j + 1].time_us - keys[j].time_us).max(1) as f64;
    if n < 2 {
        return 0.0;
    }
    if n == 2 {
        return secant(0);
    }
    if i == 0 || i == n - 1 {
        // End: three-point formula on the two nearest secants, clamped to stay monotone.
        let (j, k) = if i == 0 { (0, 1) } else { (n - 2, n - 3) };
        let (d0, d1) = (secant(j), secant(k));
        let (h0, h1) = (width(j), width(k));
        let mut m = ((2.0 * h0 + h1) * d0 - h0 * d1) / (h0 + h1);
        if m.signum() != d0.signum() {
            m = 0.0;
        } else if d0.signum() != d1.signum() && m.abs() > 3.0 * d0.abs() {
            m = 3.0 * d0;
        }
        return m;
    }
    let (d0, d1) = (secant(i - 1), secant(i));
    if d0 * d1 <= 0.0 {
        return 0.0;
    }
    let (h0, h1) = (width(i - 1), width(i));
    let w1 = 2.0 * h1 + h0;
    let w2 = h1 + 2.0 * h0;
    (w1 + w2) / (w1 / d0 + w2 / d1)
}

/// Parameter `s` of the cubic Bezier with x control points `(0, x1, x2, 1)` at which
/// `x(s) == target_x`. The control x values are clamped to 0..1 (as CSS does), so x is
/// monotone and the solution unique.
pub fn solve_cubic_param(target_x: f64, x1: f64, x2: f64) -> f64 {
    let (x1, x2) = (x1.clamp(0.0, 1.0), x2.clamp(0.0, 1.0));
    let target = target_x.clamp(0.0, 1.0);
    // Newton from the linear guess.
    let mut s = target;
    for _ in 0..8 {
        let err = sample_bezier_1d(s, x1, x2) - target;
        if err.abs() < 1e-9 {
            return s;
        }
        let dx = derivative_bezier_1d(s, x1, x2);
        if dx.abs() < 1e-6 {
            break;
        }
        s = (s - err / dx).clamp(0.0, 1.0);
    }
    if (sample_bezier_1d(s, x1, x2) - target).abs() < 1e-7 {
        return s;
    }
    // Bisection fallback (x is monotone in s).
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if sample_bezier_1d(mid, x1, x2) < target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// `y` of the cubic Bezier `(0,0), (x1,y1), (x2,y2), (1,1)` at `x == target_x` (CSS
/// `cubic-bezier` semantics).
pub fn solve_cubic_bezier(target_x: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let s = solve_cubic_param(target_x, x1, x2);
    sample_bezier_1d(s, y1, y2)
}

fn sample_bezier_1d(t: f64, p1: f64, p2: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * t * p1 + 3.0 * mt * t * t * p2 + t * t * t
}

fn derivative_bezier_1d(t: f64, p1: f64, p2: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * p1 + 6.0 * mt * t * (p2 - p1) + 3.0 * t * t * (1.0 - p2)
}

/// The effect stack with effect-parameter keyframes evaluated at clip-local `local_us`: the
/// instances in the order given, with each animated param replaced by its interpolated value.
/// Params without a track keep their stored value. A track whose instance or param is gone is
/// ignored.
pub fn evaluate_effect_stack(
    stack: &[&EffectInstance],
    keyframes: &[KeyframeTrack],
    local_us: i64,
) -> Vec<EffectInstance> {
    let mut out: Vec<EffectInstance> = stack.iter().map(|e| (*e).clone()).collect();
    for track in keyframes {
        let TrackProperty::Effect { instance, param } = &track.property else {
            continue;
        };
        let Some(effect) = out.iter_mut().find(|e| e.id == *instance) else {
            continue;
        };
        let Some(value) = sample_track_value(track, local_us) else {
            continue;
        };
        effect.params.insert(
            param.clone(),
            match value {
                KeyValue::Number(n) => EffectParam::Number(n),
                KeyValue::Vector(v) => EffectParam::Vector(v),
            },
        );
    }
    out
}

#[cfg(test)]
mod fit_tests {
    use super::*;
    use crate::project::{Id, Keyframe};

    fn key(time_us: i64, value: f64) -> Keyframe {
        Keyframe::new(time_us, value)
    }

    #[test]
    fn fit_x_keyframes_interpolate_and_clamp() {
        let clip = MediaClip {
            id: Id::new_v4(),
            media_id: Id::new_v4(),
            start_us: 0,
            source_in_us: 0,
            source_out_us: 4_000_000,
            keyframes: vec![KeyframeTrack {
                property: AnimProperty::FitX.into(),
                keys: vec![key(0, -1.0), key(2_000_000, 1.0)],
            }],
            ..MediaClip::default()
        };
        assert!((evaluate_transform(&clip, 0).fit_x + 1.0).abs() < 1e-9);
        assert!(evaluate_transform(&clip, 1_000_000).fit_x.abs() < 1e-9);
        assert!((evaluate_transform(&clip, 2_000_000).fit_x - 1.0).abs() < 1e-9);
        assert_eq!(evaluate_transform(&clip, 1_000_000).fit_y, 0.0);
    }

    #[test]
    fn fit_y_keyframes_are_clamped_to_unit_range() {
        let clip = MediaClip {
            id: Id::new_v4(),
            media_id: Id::new_v4(),
            start_us: 0,
            source_in_us: 0,
            source_out_us: 4_000_000,
            keyframes: vec![KeyframeTrack {
                property: AnimProperty::FitY.into(),
                keys: vec![key(0, 5.0)],
            }],
            ..MediaClip::default()
        };
        assert_eq!(evaluate_transform(&clip, 0).fit_y, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Id, Keyframe};

    fn lin(time_us: i64, value: f64) -> Keyframe {
        Keyframe::new(time_us, value)
    }

    fn clip_with_opacity_keys() -> MediaClip {
        MediaClip {
            id: Id::new_v4(),
            media_id: Id::new_v4(),
            start_us: 10_000_000,
            source_in_us: 0,
            source_out_us: 5_000_000,
            transform: ClipTransform {
                opacity: 1.0,
                ..ClipTransform::default()
            },
            keyframes: vec![KeyframeTrack {
                property: AnimProperty::Opacity.into(),
                keys: vec![lin(0, 0.0), lin(2_000_000, 1.0)],
            }],
            ..MediaClip::default()
        }
    }

    #[test]
    fn interpolates_opacity_midpoint() {
        let clip = clip_with_opacity_keys();
        let t = evaluate_transform(&clip, 11_000_000); // local = 1.0 s → halfway
        assert!((t.opacity - 0.5).abs() < 1e-6);
    }

    #[test]
    fn volume_keyframe_overrides_gain() {
        let mut clip = MediaClip {
            gain_db: -6.0,
            ..MediaClip::default()
        };
        clip.keyframes.push(KeyframeTrack {
            property: AnimProperty::Volume.into(),
            keys: vec![lin(0, -12.0)],
        });
        assert!((evaluate_volume_db(&clip, 0) + 12.0).abs() < 1e-9);
    }

    #[test]
    fn bezier_interpolation_matches_expected() {
        let track = KeyframeTrack {
            property: AnimProperty::Opacity.into(),
            keys: vec![
                Keyframe {
                    easing: Easing::Bezier,
                    handle_right: Some((1_000_000, 0.0)), // Flat out
                    ..Keyframe::new(0, 0.0)
                },
                Keyframe {
                    // Easing on B doesn't matter for the A-B segment.
                    handle_left: Some((-1_000_000, 0.0)), // Flat in
                    ..Keyframe::new(2_000_000, 1.0)
                },
            ],
        };
        // Normalized handles are P1=(0.5, 0.0) and P2=(0.5, 1.0): a classic "S" curve.
        let v_mid = sample_track(&track, 1_000_000).unwrap();
        assert!((v_mid - 0.5).abs() < 1e-6);

        let v_early = sample_track(&track, 100_000).unwrap();
        assert!(v_early < 0.05); // Should stay low longer due to flat start
    }

    #[test]
    fn constant_speed_matches_legacy() {
        let clip = MediaClip {
            source_in_us: 0,
            source_out_us: 4_000_000,
            speed: 2.0,
            ..MediaClip::default()
        };
        assert_eq!(timeline_duration_us(&clip), 2_000_000);
        assert_eq!(source_time_at(&clip, 1_000_000), 2_000_000);
    }

    #[test]
    fn speed_ramp_slows_timeline() {
        let clip = MediaClip {
            source_in_us: 0,
            source_out_us: 2_000_000,
            speed: 1.0,
            keyframes: vec![KeyframeTrack {
                property: AnimProperty::Speed.into(),
                keys: vec![lin(0, 0.5), lin(4_000_000, 0.5)],
            }],
            ..MediaClip::default()
        };
        let dur = timeline_duration_us(&clip);
        assert!(
            (dur - 4_000_000).abs() < 50_000,
            "expected ~4s timeline at 0.5x for 2s source, got {dur} us"
        );
    }
}
