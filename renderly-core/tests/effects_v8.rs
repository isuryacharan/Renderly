//! Schema v8 effects at the pixel and value level: blend modes (checked against an independent
//! CPU implementation of the W3C formulas), `hold` / `bezier` / `auto` easing, keyframes on
//! effect parameters (numbers and colours), and pack effects, filters, LUTs and transitions
//! resolved from a temp library (never the real one), including the pass-through for a missing
//! or changed item.
//!
//! GPU tests skip (return) when no wgpu adapter is available, like the other suites.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use renderly_core::compose::{
    eval, ComposeError, ComposeLayer, ComposeStep, Compositor, FxTiming, LayerSource,
    LayerTransition,
};
use renderly_core::frame::RgbaFrame;
use renderly_core::library::Library;
use renderly_core::packs::status::{effect_status, problem_effects};
use renderly_core::packs::{LoadedItem, LoadedPack};
use renderly_core::project::{
    evaluate_effect_stack, sample_track_value, AnimProperty, BlendMode, Clip, ClipTransform,
    Easing, EffectInstance, EffectParam, EffectStatus, KeyValue, Keyframe, KeyframeTrack,
    MediaClip, MediaItem, MediaKind, Project, Settings, Track, TrackKind, TrackProperty,
};
use uuid::Uuid;

fn compositor(w: u32, h: u32) -> Option<Compositor> {
    match Compositor::new(w, h) {
        Ok(c) => Some(c),
        Err(ComposeError::NoAdapter) => None,
        Err(e) => panic!("{e}"),
    }
}

fn solid(w: u32, h: u32, rgb: [u8; 3]) -> RgbaFrame {
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..w * h {
        pixels.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    RgbaFrame {
        width: w,
        height: h,
        pixels,
    }
}

fn layer(frame: RgbaFrame) -> ComposeLayer {
    ComposeLayer::from(frame)
}

fn px(out: &[u8], x: usize, w: usize) -> [u8; 3] {
    let o = (8 * w + x) * 4;
    [out[o], out[o + 1], out[o + 2]]
}

fn near(a: [u8; 3], b: [u8; 3], tol: i32, what: &str) {
    for c in 0..3 {
        assert!(
            (a[c] as i32 - b[c] as i32).abs() <= tol,
            "{what}: got {a:?}, expected {b:?}"
        );
    }
}

// ---- blend modes ---------------------------------------------------------------------------

/// The W3C separable blend functions (https://www.w3.org/TR/compositing-1/#blendingseparable),
/// written independently of the WGSL.
fn blend_fn(mode: BlendMode, cb: f64, cs: f64) -> f64 {
    let hard_light = |cb: f64, cs: f64| {
        if cs <= 0.5 {
            cb * 2.0 * cs
        } else {
            let s = 2.0 * cs - 1.0;
            cb + s - cb * s
        }
    };
    match mode {
        BlendMode::Normal | BlendMode::Add => cs,
        BlendMode::Multiply => cb * cs,
        BlendMode::Screen => cb + cs - cb * cs,
        BlendMode::Overlay => hard_light(cs, cb),
        BlendMode::Darken => cb.min(cs),
        BlendMode::Lighten => cb.max(cs),
        BlendMode::ColorDodge => {
            if cb == 0.0 {
                0.0
            } else if cs >= 1.0 {
                1.0
            } else {
                (cb / (1.0 - cs)).min(1.0)
            }
        }
        BlendMode::ColorBurn => {
            if cb >= 1.0 {
                1.0
            } else if cs <= 0.0 {
                0.0
            } else {
                1.0 - ((1.0 - cb) / cs).min(1.0)
            }
        }
        BlendMode::HardLight => hard_light(cb, cs),
        BlendMode::SoftLight => {
            if cs <= 0.5 {
                cb - (1.0 - 2.0 * cs) * cb * (1.0 - cb)
            } else {
                let d = if cb <= 0.25 {
                    ((16.0 * cb - 12.0) * cb + 4.0) * cb
                } else {
                    cb.sqrt()
                };
                cb + (2.0 * cs - 1.0) * (d - cb)
            }
        }
        BlendMode::Difference => (cb - cs).abs(),
        BlendMode::Exclusion => cb + cs - 2.0 * cb * cs,
    }
}

/// Opaque backdrop `cb`, source `cs` with alpha `a`: `(1 - a) * cb + a * B(cb, cs)` (plus
/// operator for `add`).
fn expected_blend(mode: BlendMode, cb: [u8; 3], cs: [u8; 3], a: f64) -> [u8; 3] {
    let mut out = [0u8; 3];
    for c in 0..3 {
        let (b, s) = (cb[c] as f64 / 255.0, cs[c] as f64 / 255.0);
        let v = if mode == BlendMode::Add {
            (b + s * a).min(1.0)
        } else {
            (1.0 - a) * b + a * blend_fn(mode, b, s)
        };
        out[c] = (v * 255.0).round() as u8;
    }
    out
}

#[test]
fn all_thirteen_blend_modes_match_the_w3c_formulas() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    // Backdrop and source chosen so every branch (dodge, burn, soft/hard light, both halves)
    // is exercised across the channels.
    let base = [204u8, 102, 26];
    let src = [77u8, 153, 230];
    for opacity in [1.0, 0.5] {
        for mode in BlendMode::ALL {
            let top = ComposeLayer {
                transform: ClipTransform {
                    opacity,
                    ..ClipTransform::default()
                },
                blend_mode: mode,
                ..layer(solid(16, 16, src))
            };
            let out = c
                .composite(&[], &[layer(solid(16, 16, base)), top])
                .unwrap();
            near(
                px(&out, 8, 16),
                expected_blend(mode, base, src, opacity),
                2,
                &format!("{} at opacity {opacity}", mode.as_str()),
            );
        }
    }
}

#[test]
fn blend_modes_only_touch_the_pixels_a_layer_covers() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    // A half-size layer in multiply mode: the corner keeps the backdrop, the centre is
    // darkened.
    let base = [200u8, 200, 200];
    let top = ComposeLayer {
        transform: ClipTransform {
            scale_x: 0.5,
            scale_y: 0.5,
            ..ClipTransform::default()
        },
        blend_mode: BlendMode::Multiply,
        ..layer(solid(16, 16, [128, 128, 128]))
    };
    let out = c
        .composite(&[], &[layer(solid(16, 16, base)), top])
        .unwrap();
    near(px(&out, 8, 16), [100, 100, 100], 2, "centre multiplied");
    let corner = (16 + 1) * 4;
    near(
        [out[corner], out[corner + 1], out[corner + 2]],
        base,
        1,
        "corner untouched",
    );
}

#[test]
fn a_blend_mode_on_an_adjustment_layer_blends_the_adjusted_picture_over_the_original() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    let base = [150u8, 110, 70];
    let adjust = |blend: BlendMode| {
        renderly_core::compose::SceneStep::adjustment(
            vec![EffectInstance::new("builtin:color_adjust@1").with_param("saturation", 0.0)],
            1.0,
            None,
            blend,
            Default::default(),
        )
    };
    let normal = c
        .composite_steps(
            &[],
            &[
                renderly_core::compose::SceneStep::Layer(layer(solid(16, 16, base))),
                adjust(BlendMode::Normal),
            ],
        )
        .unwrap();
    let adjusted = px(&normal, 8, 16);
    assert_ne!(adjusted, base, "the adjustment changes the picture");
    let multiplied = c
        .composite_steps(
            &[],
            &[
                renderly_core::compose::SceneStep::Layer(layer(solid(16, 16, base))),
                adjust(BlendMode::Multiply),
            ],
        )
        .unwrap();
    near(
        px(&multiplied, 8, 16),
        expected_blend(BlendMode::Multiply, base, adjusted, 1.0),
        2,
        "adjusted picture multiplied over the original",
    );
}

// ---- easing --------------------------------------------------------------------------------

fn track(keys: Vec<Keyframe>) -> KeyframeTrack {
    KeyframeTrack::new(AnimProperty::Opacity, keys)
}

fn at(track: &KeyframeTrack, t_us: i64) -> f64 {
    sample_track_value(track, t_us).unwrap().num()
}

#[test]
fn hold_keeps_the_value_until_the_next_key() {
    let t = track(vec![
        Keyframe::new(0, 0.2).with_easing(Easing::Hold),
        Keyframe::new(1_000_000, 0.8).with_easing(Easing::Hold),
        Keyframe::new(2_000_000, 0.4),
    ]);
    assert_eq!(at(&t, 0), 0.2);
    assert_eq!(at(&t, 999_999), 0.2);
    assert_eq!(at(&t, 1_000_000), 0.8);
    assert_eq!(at(&t, 1_999_999), 0.8);
    assert_eq!(at(&t, 2_000_000), 0.4);
    assert_eq!(at(&t, 5_000_000), 0.4);
}

/// CSS `cubic-bezier(x1, y1, x2, y2)` evaluated by plain bisection on x: an independent
/// reference for the solver in core.
fn css_bezier(x: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> f64 {
    let bez = |t: f64, p1: f64, p2: f64| {
        let m = 1.0 - t;
        3.0 * m * m * t * p1 + 3.0 * m * t * t * p2 + t * t * t
    };
    let (mut lo, mut hi) = (0.0, 1.0);
    for _ in 0..80 {
        let mid = 0.5 * (lo + hi);
        if bez(mid, x1, x2) < x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(0.5 * (lo + hi), y1, y2)
}

#[test]
fn bezier_solves_x_for_t_like_css_cubic_bezier() {
    // ease-in-out (0.42, 0, 0.58, 1) over 2 s, value 0 -> 10, as the preset writes it.
    let span: f64 = 2_000_000.0;
    let t = track(vec![
        Keyframe {
            easing: Easing::Bezier,
            handle_right: Some(((0.42 * span).round() as i64, 0.0)),
            ..Keyframe::new(0, 0.0)
        },
        Keyframe {
            handle_left: Some((((0.58 - 1.0) * span).round() as i64, 0.0)),
            ..Keyframe::new(2_000_000, 10.0)
        },
    ]);
    for step in 0..=40 {
        let u = step as f64 / 40.0;
        let got = at(&t, (u * span) as i64);
        let want = 10.0 * css_bezier(u, 0.42, 0.0, 0.58, 1.0);
        assert!(
            (got - want).abs() < 1e-3,
            "u={u}: got {got}, css gives {want}"
        );
    }
    // Control points whose x is outside 0..1 are clamped (CSS semantics): still monotone.
    let wild = track(vec![
        Keyframe {
            easing: Easing::Bezier,
            handle_right: Some((5_000_000, 3.0)),
            ..Keyframe::new(0, 0.0)
        },
        Keyframe {
            handle_left: Some((-5_000_000, -3.0)),
            ..Keyframe::new(2_000_000, 1.0)
        },
    ]);
    let mut prev = f64::NEG_INFINITY;
    for step in 0..=50 {
        let x = at(&wild, step * 40_000);
        assert!(x.is_finite());
        assert!(x >= prev - 1.0, "no wild jumps");
        prev = x;
    }
}

#[test]
fn auto_is_a_monotone_cubic_that_never_overshoots() {
    let ks = [
        (0, 0.0),
        (1_000_000, 1.0),
        (2_000_000, 1.0),
        (3_000_000, 0.0),
    ];
    let t = track(
        ks.iter()
            .map(|&(t, v)| Keyframe::new(t, v).with_easing(Easing::Auto))
            .collect(),
    );
    // Passes through every key.
    for (time, value) in ks {
        assert!((at(&t, time) - value).abs() < 1e-9);
    }
    // Never leaves the range of the neighbouring keys, and the flat top stays flat.
    for step in 0..=300 {
        let v = at(&t, step * 10_000);
        assert!((-1e-9..=1.0 + 1e-9).contains(&v), "overshoot {v} at {step}");
    }
    for step in 100..=200 {
        assert!(
            (at(&t, step * 10_000) - 1.0).abs() < 1e-9,
            "flat span stays flat"
        );
    }
    // Smooth through the key: the slopes either side agree (no corner), unlike linear.
    let h = 1000;
    let left = (at(&t, 1_000_000) - at(&t, 1_000_000 - h)) / h as f64;
    let right = (at(&t, 1_000_000 + h) - at(&t, 1_000_000)) / h as f64;
    assert!(
        (left - right).abs() < 1e-6 + left.abs() * 0.01,
        "{left} vs {right}"
    );
    // Two keys: a straight line.
    let two = track(vec![
        Keyframe::new(0, 0.0).with_easing(Easing::Auto),
        Keyframe::new(1_000_000, 1.0),
    ]);
    assert!((at(&two, 250_000) - 0.25).abs() < 1e-9);
}

#[test]
fn array_values_interpolate_per_component() {
    let t = KeyframeTrack::new(
        TrackProperty::effect_param(Uuid::new_v4(), "tint"),
        vec![
            Keyframe::new(0, vec![1.0, 0.0, 0.0, 1.0]),
            Keyframe::new(2_000_000, vec![0.0, 0.0, 1.0, 1.0]),
        ],
    );
    match sample_track_value(&t, 500_000).unwrap() {
        KeyValue::Vector(v) => {
            assert_eq!(v.len(), 4);
            for (got, want) in v.iter().zip([0.75, 0.0, 0.25, 1.0]) {
                assert!((got - want).abs() < 1e-9);
            }
        }
        other => panic!("expected a vector, got {other:?}"),
    }
}

// ---- keyframes on effect parameters ------------------------------------------------------------

fn video_project() -> (Project, Uuid, Uuid) {
    let mut p = Project::new("v8 effects", Settings::default());
    let media_id = Uuid::new_v4();
    p.media.push(MediaItem {
        id: media_id,
        path: "clip.mp4".into(),
        kind: MediaKind::Video,
        duration_us: Some(10_000_000),
        width: Some(640),
        height: Some(360),
        fps: Some(30.0),
        sequence_id: None,
    });
    let mut t = Track::new(TrackKind::Video, "V1");
    let clip = MediaClip {
        media_id,
        start_us: 1_000_000,
        source_in_us: 0,
        source_out_us: 4_000_000,
        ..MediaClip::default()
    };
    let clip_id = clip.id;
    t.clips.push(Clip::Video(clip));
    p.tracks.push(t);
    (p, clip_id, media_id)
}

#[test]
fn effect_parameter_keyframes_are_evaluated_per_frame_by_the_timeline_evaluation() {
    let (mut p, clip_id, _) = video_project();
    let blur = EffectInstance::new("builtin:blur@1").with_param("radius", 1.0);
    let tint = EffectInstance::new("builtin:chroma_key@1");
    let (blur_id, tint_id) = (blur.id, tint.id);
    if let Clip::Video(m) = &mut p.tracks[0].clips[0] {
        m.effects = vec![blur];
        m.key = Some(tint);
        m.keyframes = vec![
            KeyframeTrack::new(
                TrackProperty::effect_param(blur_id, "radius"),
                vec![Keyframe::new(0, 0.0), Keyframe::new(2_000_000, 20.0)],
            ),
            KeyframeTrack::new(
                TrackProperty::effect_param(tint_id, "tolerance"),
                vec![
                    Keyframe::new(0, 0.2).with_easing(Easing::Hold),
                    Keyframe::new(1_000_000, 0.9),
                ],
            ),
        ];
    }
    let params_at = |t_us: i64| -> (Vec<String>, f64, f64) {
        let ComposeStep::Layer(l) = &eval::active_steps(&p, t_us).unwrap()[0] else {
            panic!("a layer")
        };
        let refs = l.effects.iter().map(|e| e.item_ref.clone()).collect();
        let radius = l
            .effects
            .iter()
            .find(|e| e.id == blur_id)
            .unwrap()
            .number("radius")
            .unwrap();
        let tol = l
            .effects
            .iter()
            .find(|e| e.id == tint_id)
            .unwrap()
            .number("tolerance")
            .unwrap();
        (refs, radius, tol)
    };
    // Clip starts at 1 s: local time 0, 1 s, 2 s.
    let (refs, r0, t0) = params_at(1_000_000);
    assert_eq!(
        refs,
        ["builtin:chroma_key@1", "builtin:blur@1"],
        "render order: key slot first"
    );
    assert_eq!((r0, t0), (0.0, 0.2));
    let (_, r1, t1) = params_at(2_000_000);
    assert!((r1 - 10.0).abs() < 1e-9);
    assert_eq!(t1, 0.9, "the held key moved at its next key");
    let (_, r2, _) = params_at(3_000_000);
    assert!((r2 - 20.0).abs() < 1e-9);
    assert!(clip_id != Uuid::nil());
}

#[test]
fn an_animated_param_renders_like_the_same_static_value() {
    let Some(mut c) = compositor(32, 32) else {
        return;
    };
    let frame = {
        let mut f = solid(32, 32, [20, 20, 20]);
        for y in 0..32usize {
            for x in 16..32usize {
                let o = (y * 32 + x) * 4;
                f.pixels[o..o + 3].copy_from_slice(&[240, 240, 240]);
            }
        }
        f
    };
    let blur = EffectInstance::new("builtin:blur@1");
    let tracks = vec![KeyframeTrack::new(
        TrackProperty::effect_param(blur.id, "radius"),
        vec![Keyframe::new(0, 0.0), Keyframe::new(2_000_000, 8.0)],
    )];
    let mut render = |effects: Vec<EffectInstance>| {
        let mut l = layer(frame.clone());
        l.effects = effects;
        c.composite(&[], &[l]).unwrap()
    };
    let animated = render(evaluate_effect_stack(&[&blur], &tracks, 1_000_000));
    let fixed = render(vec![blur.clone().with_param("radius", 4.0)]);
    assert_eq!(animated, fixed);
    let sharp = render(evaluate_effect_stack(&[&blur], &tracks, 0));
    assert_ne!(animated, sharp, "radius 4 differs from radius 0");
}

// ---- pack items from a temp library ------------------------------------------------------------

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn identity_cube(scale: f32) -> String {
    let mut t = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                t.push_str(&format!(
                    "{} {} {}\n",
                    r as f32 * scale,
                    g as f32 * scale,
                    b as f32 * scale
                ));
            }
        }
    }
    t
}

/// A format-2 pack `test.fx`: a colour-mixing effect (`tintc`: a colour and an amount), a LUT
/// (`dim`, halves the picture) and a transition (`flip`, shows `to` once past the middle).
fn write_pack(dir: &Path) {
    write(
        &dir.join("pack.json"),
        r#"{ "format": 2, "id": "test.fx", "name": "Test FX", "release": "1.0.0", "license": "CC0-1.0",
            "items": [
              { "kind": "effect", "path": "effects/tintc/1" },
              { "kind": "transition", "path": "transitions/flip/1" },
              { "kind": "lut", "id": "dim", "label": "Dim", "path": "luts/dim.cube" } ] }"#,
    );
    write(
        &dir.join("effects/tintc/1/effect.json"),
        r#"{ "format": 1, "id": "tintc", "version": 1, "kind": "effect", "label": "Tint colour",
            "params": [
              { "key": "color", "type": "color", "default": [1, 1, 1, 1] },
              { "key": "amount", "type": "float", "min": 0, "max": 1, "default": 0, "initial": 1 } ],
            "bypass": "amount <= 0.0",
            "passes": [ { "type": "fragment", "shader": "tintc.wgsl", "inputs": ["src"], "output": "out" } ] }"#,
    );
    write(
        &dir.join("effects/tintc/1/tintc.wgsl"),
        "@fragment\nfn fs_main(in: VsOut) -> @location(0) vec4f {\n    let c = textureSample(src, samp_linear, in.uv);\n    return vec4f(mix(c.rgb, params.color.rgb, params.amount), c.a);\n}\n",
    );
    write(
        &dir.join("transitions/flip/1/effect.json"),
        r#"{ "format": 1, "id": "flip", "version": 1, "kind": "transition", "label": "Flip",
            "default_duration_ms": 400, "easing": "linear",
            "passes": [ { "type": "fragment", "shader": "flip.wgsl", "inputs": ["from", "to"], "output": "out" } ] }"#,
    );
    write(
        &dir.join("transitions/flip/1/flip.wgsl"),
        "@fragment\nfn fs_main(in: VsOut) -> @location(0) vec4f {\n    if (host.progress < 0.5) { return textureSample(in_from, samp_linear, in.uv); }\n    return textureSample(to, samp_linear, in.uv);\n}\n",
    );
    write(&dir.join("luts/dim.cube"), &identity_cube(0.5));
}

struct Env {
    root: PathBuf,
    packs: Vec<LoadedPack>,
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A throw-away library with `test.fx` installed (through the real install path), and the
/// packs loaded from it.
fn installed() -> Env {
    let root = std::env::temp_dir().join(format!("renderly-fx-v8-{}", Uuid::new_v4()));
    let source = root.join("source");
    write_pack(&source);
    let mut library = Library::open(&root.join("library")).unwrap();
    library.install(&source).unwrap();
    let packs = library.load_all();
    Env { root, packs }
}

fn instance(item_ref: &str) -> EffectInstance {
    EffectInstance::new(item_ref)
}

fn color(rgba: [f64; 4]) -> EffectParam {
    EffectParam::Vector(rgba.to_vec())
}

#[test]
fn a_pack_effect_runs_through_the_same_runtime_with_typed_params() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    let env = installed();
    let src = [40u8, 80, 120];
    let mut tint = instance("pack:test.fx/tintc@1");
    tint.params
        .insert("color".into(), color([1.0, 0.0, 0.0, 1.0]));
    tint.params
        .insert("amount".into(), EffectParam::Number(1.0));
    let mut render = |packs: &[LoadedPack], effects: Vec<EffectInstance>| {
        let mut l = layer(solid(16, 16, src));
        l.effects = effects;
        c.composite(packs, &[l]).unwrap()
    };
    near(
        px(&render(&env.packs, vec![tint.clone()]), 8, 16),
        [255, 0, 0],
        1,
        "amount 1 paints the colour",
    );
    let mut half = tint.clone();
    half.params
        .insert("amount".into(), EffectParam::Number(0.5));
    near(
        px(&render(&env.packs, vec![half]), 8, 16),
        [148, 40, 60],
        2,
        "amount 0.5 mixes",
    );
    // bypass: amount 0 runs no passes.
    let mut zero = tint.clone();
    zero.params
        .insert("amount".into(), EffectParam::Number(0.0));
    near(
        px(&render(&env.packs, vec![zero]), 8, 16),
        src,
        0,
        "bypassed",
    );
    // Not installed: pass-through, and the instance keeps its params.
    near(
        px(&render(&[], vec![tint.clone()]), 8, 16),
        src,
        0,
        "missing = pass-through",
    );
    assert_eq!(tint.params.len(), 2);
}

#[test]
fn a_colour_param_can_be_keyframed() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    let env = installed();
    let mut tint = instance("pack:test.fx/tintc@1");
    tint.params
        .insert("amount".into(), EffectParam::Number(1.0));
    let tracks = vec![KeyframeTrack::new(
        TrackProperty::effect_param(tint.id, "color"),
        vec![
            Keyframe::new(0, vec![1.0, 0.0, 0.0, 1.0]),
            Keyframe::new(2_000_000, vec![0.0, 0.0, 1.0, 1.0]),
        ],
    )];
    let mut l = layer(solid(16, 16, [10, 10, 10]));
    l.effects = evaluate_effect_stack(&[&tint], &tracks, 1_000_000);
    let out = c.composite(&env.packs, &[l]).unwrap();
    near(
        px(&out, 8, 16),
        [128, 0, 128],
        2,
        "halfway between red and blue",
    );
}

#[test]
fn a_pack_lut_is_a_filter_and_a_ref_to_it_resolves_to_the_lut_filter() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    let env = installed();
    let mut lut = instance("pack:test.fx/dim@1");
    lut.params
        .insert("intensity".into(), EffectParam::Number(1.0));
    let mut l = layer(solid(16, 16, [200, 100, 50]));
    l.effects = vec![lut.clone()];
    let out = c.composite(&env.packs, &[l]).unwrap();
    near(
        px(&out, 8, 16),
        [100, 50, 25],
        2,
        "the dim LUT halves the picture",
    );
    // The LUT is addressed by the same ref in the status report.
    assert_eq!(
        effect_status(&env.packs, "pack:test.fx/dim@1", None),
        EffectStatus::Ok
    );
}

#[test]
fn a_pack_transition_blends_two_layers() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    let env = installed();
    let t = |is_incoming, progress| LayerTransition {
        item_ref: "pack:test.fx/flip@1".into(),
        hash: None,
        params: Default::default(),
        progress,
        is_incoming,
        default_easing: true,
    };
    let mut run = |packs: &[LoadedPack], progress: f32| {
        let mut a = layer(solid(16, 16, [255, 0, 0]));
        a.transition = Some(t(false, progress));
        let mut b = layer(solid(16, 16, [0, 0, 255]));
        b.transition = Some(t(true, progress));
        c.composite(packs, &[a, b]).unwrap()
    };
    near(
        px(&run(&env.packs, 0.25), 8, 16),
        [255, 0, 0],
        1,
        "before the flip",
    );
    near(
        px(&run(&env.packs, 0.75), 8, 16),
        [0, 0, 255],
        1,
        "after the flip",
    );
    // Not installed: the outgoing clip stays until the cut.
    near(
        px(&run(&[], 0.75), 8, 16),
        [255, 0, 0],
        1,
        "missing transition passes through",
    );
}

#[test]
fn status_is_computed_on_load_from_the_installed_items() {
    let env = installed();
    let item = env.packs[0].items.iter().find(|i| i.id == "tintc").unwrap();
    let installed_hash = item.hash.clone().unwrap();
    assert_eq!(
        effect_status(&env.packs, "pack:test.fx/tintc@1", Some(&installed_hash)),
        EffectStatus::Ok
    );
    assert_eq!(
        effect_status(&env.packs, "pack:test.fx/tintc@1", Some("sha256:other")),
        EffectStatus::Changed
    );
    assert_eq!(
        effect_status(&env.packs, "pack:test.fx/tintc@2", None),
        EffectStatus::Missing
    );
    assert_eq!(
        effect_status(&env.packs, "pack:somebody.else/x@1", None),
        EffectStatus::Missing
    );

    // problem_effects lists only what is not ok, with clip and instance ids.
    let (mut p, clip_id, _) = video_project();
    let mut changed = instance("pack:test.fx/tintc@1");
    changed.hash = Some("sha256:stale".into());
    let missing = instance("pack:gone.pack/x@1");
    let fine = instance("builtin:blur@1");
    let (changed_id, missing_id) = (changed.id, missing.id);
    if let Clip::Video(m) = &mut p.tracks[0].clips[0] {
        m.effects = vec![fine, changed, missing];
        m.outgoing_transition = Some(renderly_core::project::ClipTransition::new(
            "pack:test.fx/nope@1",
            500_000,
        ));
    }
    let problems = problem_effects(&p, &env.packs);
    assert_eq!(problems.len(), 3);
    assert_eq!(problems[0].instance_id, Some(changed_id));
    assert_eq!(problems[0].status, EffectStatus::Changed);
    assert_eq!(problems[1].instance_id, Some(missing_id));
    assert_eq!(problems[1].status, EffectStatus::Missing);
    assert_eq!(problems[2].instance_id, None, "the transition");
    assert!(problems.iter().all(|e| e.clip_id == clip_id));
}

#[test]
fn the_preview_path_compiles_the_same_items_from_host_supplied_sources() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    let env = installed();
    // What the webview gets from the backend and hands to the wasm crate: the manifest and WGSL
    // as text, the LUT as .cube text.
    let source = env.root.join("source");
    let read = |rel: &str| std::fs::read_to_string(source.join(rel)).unwrap();
    let mut shaders = BTreeMap::new();
    shaders.insert("tintc.wgsl".to_string(), read("effects/tintc/1/tintc.wgsl"));
    let item = LoadedItem::from_sources(
        &read("effects/tintc/1/effect.json"),
        &shaders,
        &BTreeMap::new(),
        None,
    );
    let mut luts = BTreeMap::new();
    luts.insert(
        "dim".to_string(),
        renderly_core::packs::parse_cube_text(&read("luts/dim.cube")).unwrap(),
    );
    let host_packs = vec![LoadedPack::from_parts("test.fx", luts, vec![item])];

    let mut tint = instance("pack:test.fx/tintc@1");
    tint.params
        .insert("color".into(), color([0.0, 1.0, 0.0, 1.0]));
    tint.params
        .insert("amount".into(), EffectParam::Number(0.75));
    let lut = instance("pack:test.fx/dim@1");
    let mut render = |packs: &[LoadedPack]| {
        let mut l = layer(solid(16, 16, [90, 60, 30]));
        l.effects = vec![lut.clone(), tint.clone()];
        c.composite(packs, &[l]).unwrap()
    };
    assert_eq!(
        render(&env.packs),
        render(&host_packs),
        "preview and export resolve and run the same items"
    );
    let _ = LayerSource::Frame(solid(1, 1, [0, 0, 0]));
}

#[test]
fn effects_see_the_clips_time_progress_and_a_stable_per_instance_seed() {
    let Some(mut c) = compositor(16, 16) else {
        return;
    };
    // An effect driven only by host values: red grows with `progress` (time / duration); the
    // seed picks the green level, so two instances differ and one instance never changes.
    let dir = std::env::temp_dir().join(format!("renderly-host-{}", Uuid::new_v4()));
    write(
        &dir.join("pack.json"),
        r#"{ "format": 2, "id": "test.host", "name": "Host", "release": "1.0.0", "license": "CC0-1.0",
            "items": [ { "kind": "effect", "path": "effects/clock/1" } ] }"#,
    );
    write(
        &dir.join("effects/clock/1/effect.json"),
        r#"{ "format": 1, "id": "clock", "version": 1, "kind": "effect", "label": "Clock",
            "params": [],
            "passes": [ { "type": "fragment", "shader": "clock.wgsl", "inputs": ["src"], "output": "out" } ] }"#,
    );
    write(
        &dir.join("effects/clock/1/clock.wgsl"),
        "@fragment
fn fs_main(in: VsOut) -> @location(0) vec4f {
    let g = f32(host.seed % 256u) / 255.0;
    return vec4f(host.progress, g, host.time / 10.0 + f32(host.frame) / 255.0, 1.0);
}
",
    );
    let mut library = Library::open(&dir.join("library")).unwrap();
    library.install(&dir).unwrap();
    let packs = library.load_all();

    let fx = instance("pack:test.host/clock@1");
    let mut run = |instance: &EffectInstance, timing: FxTiming| {
        let mut l = layer(solid(16, 16, [0, 0, 0]));
        l.effects = vec![instance.clone()];
        l.timing = timing;
        let out = c.composite(&packs, &[l]).unwrap();
        px(&out, 8, 16)
    };
    let half = run(
        &fx,
        FxTiming {
            time: 1.0,
            duration: 2.0,
            frame: 5,
            speed: 1.0,
        },
    );
    let close = |got: u8, want: f64, what: &str| {
        assert!(
            (got as f64 - want).abs() <= 1.0,
            "{what}: got {got}, want {want}"
        );
    };
    close(half[0], 127.5, "progress = time / duration");
    close(
        half[2],
        255.0 * (1.0 / 10.0 + 5.0 / 255.0),
        "time and frame",
    );
    let later = run(
        &fx,
        FxTiming {
            time: 2.0,
            duration: 2.0,
            frame: 5,
            speed: 1.0,
        },
    );
    close(later[0], 255.0, "progress reaches 1");
    // The same instance has the same seed on every run; another instance has another one.
    assert_eq!(
        run(&fx, FxTiming::default())[1],
        run(&fx, FxTiming::default())[1]
    );
    let other = instance("pack:test.host/clock@1");
    assert_ne!(
        run(&fx, FxTiming::default())[1],
        run(&other, FxTiming::default())[1],
        "seeds differ between instances"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
