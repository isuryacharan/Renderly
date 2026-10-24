//! Tests for the schema v8 effect commands: slots, refs, batch targets, transitions with refs,
//! keyframes on effect parameters, easing and the deprecated v7 shapes.

use super::{apply_command, parse_command_json, ClipRef, Command, CommandError, EffectSpec};
use crate::library::test_support::isolated_library;
use crate::library::Library;
use crate::project::{
    AdjustmentClip, AnimProperty, BlendMode, Clip, ClipTransform, Easing, EffectParam, Id,
    KeyValue, Keyframe, KeyframeTrack, MediaClip, MediaItem, MediaKind, ProgressEasing, Project,
    Settings, Track, TrackKind, TrackProperty,
};
use crate::time::Time;
use serde_json::json;

struct Fixture {
    project: Project,
    track: Id,
    clips: Vec<Id>,
}

/// One video track holding `n` back-to-back 4 s clips.
fn video(n: usize) -> Fixture {
    let mut project = Project::new("effects", Settings::default());
    let media_id = Id::new_v4();
    project.media.push(MediaItem {
        id: media_id,
        path: "clip.mp4".into(),
        kind: MediaKind::Video,
        duration_us: Some(60_000_000),
        width: Some(1920),
        height: Some(1080),
        fps: Some(30.0),
        sequence_id: None,
    });
    let mut track = Track::new(TrackKind::Video, "V1");
    let mut clips = Vec::new();
    for i in 0..n {
        let clip = MediaClip {
            media_id,
            start_us: i as i64 * 4_000_000,
            source_in_us: 0,
            source_out_us: 4_000_000,
            ..MediaClip::default()
        };
        clips.push(clip.id);
        track.clips.push(Clip::Video(clip));
    }
    let track_id = track.id;
    project.tracks.push(track);
    Fixture {
        project,
        track: track_id,
        clips,
    }
}

fn clip(f: &Fixture, i: usize) -> &MediaClip {
    f.project
        .find_track(f.track)
        .unwrap()
        .find_clip(f.clips[i])
        .unwrap()
        .as_media()
        .unwrap()
}

fn set_effects(f: &Fixture, i: usize, effects: Vec<EffectSpec>) -> Command {
    Command::set_effects(f.track, f.clips[i], effects)
}

fn slots(f: &Fixture, i: usize, change: serde_json::Value) -> Command {
    let mut v = json!({ "command": "SetClipEffects", "track_id": f.track, "clip_id": f.clips[i] });
    v.as_object_mut()
        .unwrap()
        .extend(change.as_object().unwrap().clone());
    parse_command_json(v).unwrap().0
}

impl Fixture {
    fn apply(&mut self, cmd: Command) -> Result<crate::commands::CommandOutcome, CommandError> {
        apply_command(&mut self.project, cmd)
    }

    /// Apply `cmd`, which must fail; the message of the error.
    fn err(&mut self, cmd: Command) -> String {
        match apply_command(&mut self.project, cmd) {
            Err(e) => e.to_string(),
            Ok(o) => panic!("expected an error, got {o:?}"),
        }
    }
}

// ---- SetClipEffects: refs, slots, ids ----------------------------------------------------

#[test]
fn effects_are_applied_by_ref_with_typed_params_and_initial_values() {
    let mut f = video(1);
    f.apply(set_effects(
        &f,
        0,
        vec![
            EffectSpec::new("builtin:blur@1"),
            EffectSpec::new("builtin:color_adjust@1").with_params(&[("saturation", 9.0)]),
        ],
    ))
    .unwrap();
    let c = clip(&f, 0);
    assert_eq!(c.effects.len(), 2);
    assert_eq!(c.effects[0].item_ref, "builtin:blur@1");
    // No params given: the effect's `initial` values (a freshly added blur is visible).
    assert_eq!(c.effects[0].number("radius"), Some(8.0));
    // Out-of-range values are clamped to the declared range (saturation max 4).
    assert_eq!(c.effects[1].number("saturation"), Some(4.0));
    // Builtins carry no hash.
    assert!(c.effects[0].hash.is_none());
}

#[test]
fn slots_filter_key_adjust_and_effects_are_set_cleared_and_left_alone() {
    let mut f = video(1);
    f.apply(slots(
        &f,
        0,
        json!({
            "key": { "ref": "builtin:chroma_key@1" },
            "adjust": { "ref": "builtin:color_adjust@1", "params": { "exposure": 0.5 } },
            "effects": [{ "ref": "builtin:blur@1", "params": { "radius": 3 } }],
            "blend_mode": "multiply",
        }),
    ))
    .unwrap();
    let c = clip(&f, 0);
    assert_eq!(c.key.as_ref().unwrap().item_ref, "builtin:chroma_key@1");
    assert_eq!(c.adjust.as_ref().unwrap().number("exposure"), Some(0.5));
    assert_eq!(c.effects.len(), 1);
    assert_eq!(c.blend_mode, BlendMode::Multiply);
    let order: Vec<&str> = c
        .effect_stack()
        .iter()
        .map(|e| e.item_ref.as_str())
        .collect();
    assert_eq!(
        order,
        [
            "builtin:chroma_key@1",
            "builtin:color_adjust@1",
            "builtin:blur@1"
        ],
        "render order: filter, key, adjust, effects"
    );

    // Omitted = unchanged; null = cleared.
    f.apply(slots(&f, 0, json!({ "key": null }))).unwrap();
    let c = clip(&f, 0);
    assert!(c.key.is_none());
    assert!(c.adjust.is_some() && c.effects.len() == 1);
    assert_eq!(c.blend_mode, BlendMode::Multiply);
    f.apply(slots(&f, 0, json!({ "blend_mode": "normal" })))
        .unwrap();
    assert_eq!(clip(&f, 0).blend_mode, BlendMode::Normal);
}

#[test]
fn slot_rules_and_bad_input_are_rejected_with_a_message_that_helps() {
    let mut f = video(1);
    let c = slots(&f, 0, json!({ "filter": { "ref": "builtin:blur@1" } }));
    assert!(f.err(c).contains("filter slot"));
    let c = slots(&f, 0, json!({ "key": { "ref": "builtin:blur@1" } }));
    assert!(f.err(c).contains("key slot"));
    let c = slots(&f, 0, json!({ "effects": [{ "ref": "builtin:nope@1" }] }));
    assert!(f.err(c).contains("unknown builtin"));
    let c = slots(&f, 0, json!({ "effects": [{ "ref": "builtin:blur" }] }));
    assert!(
        f.err(c).contains("builtin:<id>@<version>"),
        "refs carry a version"
    );
    let c = slots(
        &f,
        0,
        json!({ "effects": [{ "ref": "pack:nobody.nothing/x@1" }] }),
    );
    let _lib = isolated_library();
    assert!(f.err(c).contains("not installed"));
    let c = slots(
        &f,
        0,
        json!({ "effects": [{ "ref": "builtin:blur@1", "params": { "nope": 1 } }] }),
    );
    assert!(f.err(c).contains("no param 'nope'"));
    let c = slots(
        &f,
        0,
        json!({ "effects": [{ "ref": "builtin:blur@1", "params": { "radius": [1, 2] } }] }),
    );
    assert!(f.err(c).contains("expects a number"));
    let c = slots(
        &f,
        0,
        json!({ "effects": [{ "ref": "builtin:crossfade@1" }] }),
    );
    assert!(f.err(c).contains("transition"));
    // Nothing to change.
    let c = slots(&f, 0, json!({}));
    assert!(f.err(c).contains("at least one"));
    assert!(
        clip(&f, 0).effects.is_empty(),
        "failed commands change nothing"
    );
}

#[test]
fn duplicate_instance_ids_are_rejected_and_ids_keep_an_instance_in_place() {
    let mut f = video(1);
    let id = Id::new_v4();
    let spec = |item: &str| EffectSpec {
        id: Some(id),
        ..EffectSpec::new(item)
    };
    let cmd = set_effects(
        &f,
        0,
        vec![spec("builtin:blur@1"), spec("builtin:glitch@1")],
    );
    assert!(f.err(cmd).contains("duplicate effect id"));

    f.apply(set_effects(&f, 0, vec![spec("builtin:blur@1")]))
        .unwrap();
    assert_eq!(clip(&f, 0).effects[0].id, id);
    // Editing in place (same id and ref, params omitted) keeps the instance and its params.
    f.apply(set_effects(
        &f,
        0,
        vec![EffectSpec {
            enabled: false,
            ..spec("builtin:blur@1")
        }],
    ))
    .unwrap();
    let e = &clip(&f, 0).effects[0];
    assert_eq!(
        (e.id, e.enabled, e.number("radius")),
        (id, false, Some(8.0))
    );
}

#[test]
fn audio_clips_take_the_effects_list_only() {
    let mut f = video(1);
    f.project.media.push(MediaItem {
        id: Id::new_v4(),
        path: "a.wav".into(),
        kind: MediaKind::Audio,
        duration_us: Some(1_000_000),
        width: None,
        height: None,
        fps: None,
        sequence_id: None,
    });
    let audio_media = f.project.media[1].id;
    let mut track = Track::new(TrackKind::Audio, "A1");
    let m = MediaClip {
        media_id: audio_media,
        source_out_us: 1_000_000,
        ..MediaClip::default()
    };
    let (tid, cid) = (track.id, m.id);
    track.clips.push(Clip::Audio(m));
    f.project.tracks.push(track);
    let cmd = parse_command_json(json!({
        "command": "SetClipEffects", "track_id": tid, "clip_id": cid, "blend_mode": "screen"
    }))
    .unwrap()
    .0;
    assert!(f.err(cmd).contains("audio clips"));
}

#[test]
fn adjustment_layers_have_slots_and_a_blend_mode_but_no_key() {
    let mut f = video(1);
    let adj = AdjustmentClip::new(0, 2_000_000);
    let adj_id = adj.id;
    f.project.tracks[0].clips.push(Clip::Adjustment(adj));
    let track = f.track;
    let cmd = move |change: serde_json::Value| {
        let mut v = json!({ "command": "SetClipEffects", "track_id": track, "clip_id": adj_id });
        v.as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        parse_command_json(v).unwrap().0
    };
    let c = cmd(json!({ "key": { "ref": "builtin:chroma_key@1" } }));
    assert!(f.err(c).contains("no key slot"));
    f.apply(cmd(json!({
        "adjust": { "ref": "builtin:color_adjust@1" },
        "effects": [{ "ref": "builtin:blur@1" }],
        "blend_mode": "soft_light",
    })))
    .unwrap();
    let a = f.project.tracks[0]
        .find_clip(adj_id)
        .unwrap()
        .as_adjustment()
        .unwrap();
    assert_eq!(a.blend_mode, BlendMode::SoftLight);
    assert!(a.adjust.is_some() && a.effects.len() == 1);
}

// ---- batch targets ------------------------------------------------------------------------------

#[test]
fn targets_apply_one_change_to_many_clips_with_fresh_ids_each() {
    let mut f = video(3);
    let targets: Vec<serde_json::Value> = f
        .clips
        .iter()
        .map(|c| json!({ "track_id": f.track, "clip_id": c }))
        .collect();
    let id = Id::new_v4();
    let cmd = parse_command_json(json!({
        "command": "SetClipEffects", "targets": targets,
        "effects": [{ "id": id, "ref": "builtin:blur@1" }],
        "blend_mode": "screen",
    }))
    .unwrap()
    .0;
    f.apply(cmd).unwrap();
    let ids: Vec<Id> = (0..3).map(|i| clip(&f, i).effects[0].id).collect();
    assert!(ids.iter().all(|i| *i != id), "batch ignores the given id");
    assert_eq!(
        ids.iter().collect::<std::collections::HashSet<_>>().len(),
        3
    );
    assert!((0..3).all(|i| clip(&f, i).blend_mode == BlendMode::Screen));
}

#[test]
fn a_batch_is_all_or_nothing() {
    let mut f = video(2);
    let targets = vec![
        ClipRef {
            track_id: f.track,
            clip_id: f.clips[0],
        },
        ClipRef {
            track_id: f.track,
            clip_id: Id::new_v4(),
        },
    ];
    let cmd = Command::SetClipEffects {
        track_id: None,
        clip_id: None,
        targets,
        filter: None,
        key: None,
        adjust: None,
        effects: Some(vec![EffectSpec::new("builtin:blur@1")]),
        blend_mode: None,
    };
    assert!(matches!(f.apply(cmd), Err(CommandError::ClipNotFound(..))));
    assert!(
        clip(&f, 0).effects.is_empty(),
        "the valid target was not touched either"
    );
    // Both selectors at once is ambiguous.
    let cmd = parse_command_json(json!({
        "command": "SetClipTransform", "track_id": f.track, "clip_id": f.clips[0],
        "targets": [{ "track_id": f.track, "clip_id": f.clips[1] }],
        "transform": ClipTransform::default(),
    }))
    .unwrap()
    .0;
    assert!(f.err(cmd).contains("not both"));
}

#[test]
fn transforms_apply_to_every_target_in_one_command() {
    let mut f = video(2);
    let cmd = parse_command_json(json!({
        "command": "SetClipTransform",
        "targets": [
            { "track_id": f.track, "clip_id": f.clips[0] },
            { "track_id": f.track, "clip_id": f.clips[1] },
        ],
        "transform": { "x": 0.25, "opacity": 0.5 },
    }))
    .unwrap()
    .0;
    f.apply(cmd).unwrap();
    for i in 0..2 {
        assert_eq!(
            (clip(&f, i).transform.x, clip(&f, i).transform.opacity),
            (0.25, 0.5)
        );
    }
}

// ---- keyframes on effect parameters -------------------------------------------------------------

fn effect_track(id: Id, param: &str, keys: Vec<Keyframe>) -> KeyframeTrack {
    KeyframeTrack::new(TrackProperty::effect_param(id, param), keys)
}

fn set_keyframes(f: &Fixture, tracks: Vec<KeyframeTrack>) -> Command {
    Command::SetClipKeyframes {
        track_id: f.track,
        clip_id: f.clips[0],
        keyframes: tracks,
    }
}

#[test]
fn effect_parameter_tracks_are_validated_against_the_effect() {
    let mut f = video(1);
    let id = Id::new_v4();
    f.apply(set_effects(
        &f,
        0,
        vec![
            EffectSpec {
                id: Some(id),
                ..EffectSpec::new("builtin:blur@1")
            },
            EffectSpec::new("builtin:glitch@1"),
        ],
    ))
    .unwrap();
    // A numeric param can be animated.
    let ok = effect_track(
        id,
        "radius",
        vec![Keyframe::new(0, 0.0), Keyframe::new(1_000_000, 20.0)],
    );
    f.apply(set_keyframes(&f, vec![ok.clone()])).unwrap();
    assert_eq!(clip(&f, 0).keyframes.len(), 1);
    // Unknown instance, unknown param, wrong shape.
    let bad = effect_track(Id::new_v4(), "radius", vec![Keyframe::new(0, 1.0)]);
    assert!(f
        .err(set_keyframes(&f, vec![bad]))
        .contains("no effect instance"));
    let bad = effect_track(id, "nope", vec![Keyframe::new(0, 1.0)]);
    let msg = f.err(set_keyframes(&f, vec![bad]));
    assert!(
        msg.contains("no param 'nope'") && msg.contains("radius"),
        "{msg}"
    );
    let bad = effect_track(id, "radius", vec![Keyframe::new(0, vec![1.0, 2.0])]);
    assert!(f.err(set_keyframes(&f, vec![bad])).contains("numbers"));
    // A param declared `animatable: false` (the glitch seed) is rejected.
    let glitch = clip(&f, 0).effects[1].id;
    let bad = effect_track(glitch, "seed", vec![Keyframe::new(0, 1.0)]);
    assert!(f
        .err(set_keyframes(&f, vec![bad]))
        .contains("not animatable"));
    // Property strings parse: a typo is a clear error.
    let err = parse_command_json(json!({
        "command": "SetClipKeyframes", "track_id": f.track, "clip_id": f.clips[0],
        "keyframes": [{ "property": "opacty", "keys": [] }],
    }))
    .unwrap_err()
    .to_string();
    assert!(err.contains("unknown keyframe property 'opacty'"), "{err}");
}

#[test]
fn bool_choice_and_lut_params_cannot_be_keyframed() {
    let _lib = isolated_library();
    let dir = std::env::temp_dir().join(format!("renderly-cmd-pack-{}", Id::new_v4()));
    write_pack_with_all_param_kinds(&dir);
    Library::open(&crate::library::default_root())
        .unwrap()
        .install(&dir)
        .unwrap();
    let mut f = video(1);
    let id = Id::new_v4();
    f.apply(set_effects(
        &f,
        0,
        vec![EffectSpec {
            id: Some(id),
            params: Some(
                [
                    ("flag".to_string(), EffectParam::Bool(true)),
                    ("mode".to_string(), EffectParam::Number(1.0)),
                    (
                        "tint".to_string(),
                        EffectParam::Vector(vec![0.0, 1.0, 0.0, 1.0]),
                    ),
                    ("pos".to_string(), EffectParam::Vector(vec![0.25, 0.75])),
                ]
                .into_iter()
                .collect(),
            ),
            ..EffectSpec::new("pack:test.kinds/all@1")
        }],
    ))
    .unwrap();
    let e = &clip(&f, 0).effects[0];
    assert!(
        e.hash.is_some(),
        "a pack effect records the hash of the item it was applied from"
    );
    assert_eq!(e.params["flag"], EffectParam::Bool(true));
    assert_eq!(e.params["mode"], EffectParam::Number(1.0));

    for (param, value) in [
        ("flag", KeyValue::Number(1.0)),
        ("mode", KeyValue::Number(1.0)),
        ("lut", KeyValue::Number(0.0)),
    ] {
        let t = effect_track(id, param, vec![Keyframe::new(0, value)]);
        let msg = f.err(set_keyframes(&f, vec![t]));
        assert!(
            msg.contains("not animatable") || msg.contains("no param"),
            "{param}: {msg}"
        );
    }
    // Colour and vec2 params take arrays of the right length.
    let ok = effect_track(id, "tint", vec![Keyframe::new(0, vec![1.0, 0.0, 0.0, 1.0])]);
    let ok2 = effect_track(id, "pos", vec![Keyframe::new(0, vec![0.0, 1.0])]);
    f.apply(set_keyframes(&f, vec![ok, ok2])).unwrap();
    let bad = effect_track(id, "tint", vec![Keyframe::new(0, vec![1.0, 0.0])]);
    assert!(f.err(set_keyframes(&f, vec![bad])).contains("[r, g, b, a]"));
    let _ = std::fs::remove_dir_all(&dir);
}

fn write_pack_with_all_param_kinds(dir: &std::path::Path) {
    let w = |rel: &str, text: &str| {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    w(
        "pack.json",
        r#"{ "format": 2, "id": "test.kinds", "name": "Kinds", "release": "1.0.0", "license": "CC0-1.0",
            "items": [ { "kind": "effect", "path": "effects/all/1" }, { "kind": "lut", "id": "id2", "label": "Id", "path": "luts/id.cube" } ] }"#,
    );
    w(
        "effects/all/1/effect.json",
        r#"{ "format": 1, "id": "all", "version": 1, "kind": "effect", "label": "All kinds",
            "params": [
              { "key": "flag", "type": "bool", "label": "Flag", "default": false },
              { "key": "mode", "type": "choice", "label": "Mode", "default": 0, "options": [ { "id": "a", "label": "A" }, { "id": "b", "label": "B" } ] },
              { "key": "level", "type": "float", "label": "Level", "min": 0, "max": 1, "default": 0.5 },
              { "key": "tint", "type": "color", "label": "Tint", "default": [1, 1, 1, 1] },
              { "key": "pos", "type": "vec2", "label": "Position", "default": [0.5, 0.5] },
              { "key": "lut", "type": "lut", "label": "LUT" } ],
            "passes": [ { "type": "fragment", "shader": "all.wgsl", "inputs": ["src", "lut"], "output": "out" } ] }"#,
    );
    w(
        "effects/all/1/all.wgsl",
        "@fragment\nfn fs_main(in: VsOut) -> @location(0) vec4f {\n    let c = textureSample(src, samp_linear, in.uv);\n    return vec4f(c.rgb * params.level + params.tint.rgb * 0.0 + vec3f(params.pos.x * 0.0 + params.flag * 0.0 + f32(params.mode) * 0.0), c.a);\n}\n",
    );
    let mut cube = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                cube.push_str(&format!("{r} {g} {b}\n"));
            }
        }
    }
    w("luts/id.cube", &cube);
}

#[test]
fn deleting_an_effect_deletes_its_keyframe_tracks() {
    let mut f = video(1);
    let (a, b) = (Id::new_v4(), Id::new_v4());
    f.apply(set_effects(
        &f,
        0,
        vec![
            EffectSpec {
                id: Some(a),
                ..EffectSpec::new("builtin:blur@1")
            },
            EffectSpec {
                id: Some(b),
                ..EffectSpec::new("builtin:glitch@1")
            },
        ],
    ))
    .unwrap();
    f.apply(set_keyframes(
        &f,
        vec![
            effect_track(
                a,
                "radius",
                vec![Keyframe::new(0, 1.0), Keyframe::new(1_000_000, 2.0)],
            ),
            effect_track(b, "intensity", vec![Keyframe::new(0, 0.1)]),
            KeyframeTrack::new(AnimProperty::Opacity, vec![Keyframe::new(0, 1.0)]),
        ],
    ))
    .unwrap();
    assert_eq!(clip(&f, 0).keyframes.len(), 3);
    // Replace the list keeping only `b`: a's tracks go, b's and the opacity track stay.
    f.apply(set_effects(
        &f,
        0,
        vec![EffectSpec {
            id: Some(b),
            ..EffectSpec::new("builtin:glitch@1")
        }],
    ))
    .unwrap();
    let props: Vec<String> = clip(&f, 0)
        .keyframes
        .iter()
        .map(|t| t.property.to_string())
        .collect();
    assert_eq!(
        props,
        [format!("effect:{b}/intensity"), "opacity".to_string()]
    );
    // Clearing every slot removes the rest.
    f.apply(set_effects(&f, 0, vec![])).unwrap();
    assert_eq!(clip(&f, 0).keyframes.len(), 1);
}

#[test]
fn splitting_a_clip_follows_effect_tracks_to_the_new_instances() {
    let mut f = video(1);
    let id = Id::new_v4();
    f.apply(set_effects(
        &f,
        0,
        vec![EffectSpec {
            id: Some(id),
            ..EffectSpec::new("builtin:blur@1")
        }],
    ))
    .unwrap();
    f.apply(set_keyframes(
        &f,
        vec![effect_track(
            id,
            "radius",
            vec![
                Keyframe::new(0, 0.0),
                Keyframe::new(1_000_000, 4.0),
                Keyframe::new(3_000_000, 8.0),
            ],
        )],
    ))
    .unwrap();
    let crate::commands::CommandOutcome::ClipSplit { left_id, right_id } = f
        .apply(Command::SplitClip {
            track_id: f.track,
            clip_id: f.clips[0],
            at: Time::Us(2_000_000),
        })
        .unwrap()
    else {
        panic!("split")
    };
    let track = f.project.find_track(f.track).unwrap();
    let left = track.find_clip(left_id).unwrap().as_media().unwrap();
    let right = track.find_clip(right_id).unwrap().as_media().unwrap();
    assert_eq!(left.effects[0].id, id);
    assert_ne!(right.effects[0].id, id);
    assert_eq!(
        left.keyframes[0].property,
        TrackProperty::effect_param(id, "radius")
    );
    assert_eq!(
        right.keyframes[0].property,
        TrackProperty::effect_param(right.effects[0].id, "radius"),
        "the right half's track addresses the right half's instance"
    );
    assert_eq!(right.keyframes[0].keys[0].time_us, 1_000_000);
}

// ---- easing ---------------------------------------------------------------------------------------

fn ease(f: &Fixture, property: &str, time: &str, easing: serde_json::Value) -> Command {
    parse_command_json(json!({
        "command": "SetKeyframeEasing", "track_id": f.track, "clip_id": f.clips[0],
        "property": property, "time": time, "easing": easing,
    }))
    .unwrap()
    .0
}

fn opacity_keys(f: &Fixture) -> Vec<Keyframe> {
    clip(f, 0)
        .keyframes
        .iter()
        .find(|t| t.property == AnimProperty::Opacity)
        .unwrap()
        .keys
        .clone()
}

fn with_opacity_keys(keys: Vec<Keyframe>) -> Fixture {
    let mut f = video(1);
    f.apply(set_keyframes(
        &f,
        vec![KeyframeTrack::new(AnimProperty::Opacity, keys)],
    ))
    .unwrap();
    f
}

#[test]
fn easing_presets_are_written_as_bezier_handles() {
    // 2 s span, opacity 0.2 -> 0.8 (dv 0.6).
    for (name, p) in [
        ("ease_in", [0.42, 0.0, 1.0, 1.0]),
        ("ease_out", [0.0, 0.0, 0.58, 1.0]),
        ("ease_in_out", [0.42, 0.0, 0.58, 1.0]),
    ] {
        let mut f = with_opacity_keys(vec![Keyframe::new(0, 0.2), Keyframe::new(2_000_000, 0.8)]);
        let cmd = ease(&f, "opacity", "0s", json!(name));
        f.apply(cmd).unwrap();
        let keys = opacity_keys(&f);
        assert_eq!(keys[0].easing, Easing::Bezier, "{name}");
        let hr = keys[0].handle_right.unwrap();
        let hl = keys[1].handle_left.unwrap();
        assert_eq!(
            hr.0,
            (p[0] * 2_000_000.0_f64).round() as i64,
            "{name} right x"
        );
        assert!((hr.1 - p[1] * 0.6).abs() < 1e-9, "{name} right y");
        assert_eq!(
            hl.0,
            ((p[2] - 1.0) * 2_000_000.0_f64).round() as i64,
            "{name} left x"
        );
        assert!((hl.1 - (p[3] - 1.0) * 0.6).abs() < 1e-9, "{name} left y");
    }
    // CSS control points directly.
    let mut f = with_opacity_keys(vec![Keyframe::new(0, 0.0), Keyframe::new(1_000_000, 1.0)]);
    let cmd = ease(
        &f,
        "opacity",
        "0s",
        json!({ "bezier": [0.1, 0.2, 0.3, 0.4] }),
    );
    f.apply(cmd).unwrap();
    let keys = opacity_keys(&f);
    assert_eq!(keys[0].handle_right, Some((100_000, 0.2)));
    assert_eq!(keys[1].handle_left.map(|h| h.0), Some(-700_000));
}

#[test]
fn linear_hold_and_auto_clear_the_spans_handles() {
    let mut f = with_opacity_keys(vec![Keyframe::new(0, 0.0), Keyframe::new(1_000_000, 1.0)]);
    let cmd = ease(&f, "opacity", "0s", json!("ease_in"));
    f.apply(cmd).unwrap();
    for (name, want) in [
        ("hold", Easing::Hold),
        ("auto", Easing::Auto),
        ("linear", Easing::Linear),
    ] {
        let cmd = ease(&f, "opacity", "0s", json!(name));
        f.apply(cmd).unwrap();
        let keys = opacity_keys(&f);
        assert_eq!(keys[0].easing, want);
        assert!(keys[0].handle_right.is_none() && keys[1].handle_left.is_none());
    }
}

#[test]
fn easing_errors_name_the_problem() {
    let mut f = with_opacity_keys(vec![Keyframe::new(0, 0.0), Keyframe::new(1_000_000, 1.0)]);
    let cmd = ease(&f, "opacity", "1s", json!("linear"));
    assert!(f.err(cmd).contains("last key"));
    let cmd = ease(&f, "opacity", "0.5s", json!("linear"));
    let msg = f.err(cmd);
    assert!(
        msg.contains("no key of 'opacity'") && msg.contains("0, 1000000"),
        "{msg}"
    );
    let cmd = ease(&f, "pos_x", "0s", json!("linear"));
    assert!(f.err(cmd).contains("no keyframe track 'pos_x'"));
    let err = parse_command_json(json!({
        "command": "SetKeyframeEasing", "track_id": f.track, "clip_id": f.clips[0],
        "property": "opacity", "time": "0", "easing": "wobbly",
    }))
    .unwrap_err()
    .to_string();
    assert!(err.contains("unknown easing 'wobbly'"), "{err}");
}

#[test]
fn a_key_inserted_inside_a_bezier_span_splits_the_curve_without_changing_the_motion() {
    use crate::project::sample_track_value;
    let mut f = with_opacity_keys(vec![Keyframe::new(0, 0.0), Keyframe::new(2_000_000, 1.0)]);
    let cmd = ease(&f, "opacity", "0s", json!("ease_in_out"));
    f.apply(cmd).unwrap();
    let before = clip(&f, 0).keyframes[0].clone();
    // Insert a key at 0.7 s on the curve, as "add keyframe here" does: the value is the current
    // value of the track, with no handles.
    let t = 700_000;
    let on_curve = sample_track_value(&before, t).unwrap().num();
    let mut keys = before.keys.clone();
    keys.push(Keyframe::new(t, on_curve));
    f.apply(set_keyframes(
        &f,
        vec![KeyframeTrack::new(AnimProperty::Opacity, keys)],
    ))
    .unwrap();
    let after = clip(&f, 0).keyframes[0].clone();
    assert_eq!(after.keys.len(), 3);
    assert_eq!(after.keys[1].easing, Easing::Bezier);
    assert!(after.keys[1].handle_left.is_some() && after.keys[1].handle_right.is_some());
    for step in 0..=200 {
        let u = step * 10_000;
        let (a, b) = (
            sample_track_value(&before, u).unwrap().num(),
            sample_track_value(&after, u).unwrap().num(),
        );
        assert!((a - b).abs() < 2e-3, "motion changed at {u} us: {a} vs {b}");
    }
    // A key that is *not* on the curve is taken as written (no handles invented).
    let mut keys = before.keys.clone();
    keys.push(Keyframe::new(t, 0.9));
    f.apply(set_keyframes(
        &f,
        vec![KeyframeTrack::new(AnimProperty::Opacity, keys)],
    ))
    .unwrap();
    let off = clip(&f, 0).keyframes[0].keys[1].clone();
    assert!(off.handle_left.is_none() && off.handle_right.is_none());
}

// ---- transitions -----------------------------------------------------------------------------------

fn set_transition(f: &Fixture, i: usize, body: serde_json::Value) -> Command {
    let mut v =
        json!({ "command": "SetClipTransition", "track_id": f.track, "clip_id": f.clips[i] });
    v.as_object_mut()
        .unwrap()
        .extend(body.as_object().unwrap().clone());
    parse_command_json(v).unwrap().0
}

#[test]
fn transitions_are_set_by_ref_with_duration_params_and_easing() {
    let mut f = video(3);
    let cmd = set_transition(
        &f,
        0,
        json!({ "ref": "builtin:crossfade@1", "duration": "0.5s" }),
    );
    f.apply(cmd).unwrap();
    let t = clip(&f, 0).outgoing_transition.clone().unwrap();
    assert_eq!(
        (t.item_ref.as_str(), t.duration_us, t.easing),
        ("builtin:crossfade@1", 500_000, None)
    );
    assert!(t.hash.is_none() && t.params.is_empty());

    // Replace the ref, keep the duration (omitted), set an easing preset.
    let cmd = set_transition(
        &f,
        0,
        json!({ "ref": "builtin:wipe_left@1", "easing": "ease_in_out" }),
    );
    f.apply(cmd).unwrap();
    let t = clip(&f, 0).outgoing_transition.clone().unwrap();
    assert_eq!(t.item_ref, "builtin:wipe_left@1");
    assert_eq!(
        t.duration_us, 500_000,
        "an existing transition keeps its duration"
    );
    assert_eq!(t.easing, Some(ProgressEasing::EASE_IN_OUT));
    // null easing = the transition's declared default.
    let cmd = set_transition(
        &f,
        0,
        json!({ "ref": "builtin:wipe_left@1", "easing": null }),
    );
    f.apply(cmd).unwrap();
    assert_eq!(
        clip(&f, 0).outgoing_transition.as_ref().unwrap().easing,
        None
    );

    // Clear with ref null.
    let cmd = set_transition(&f, 0, json!({ "ref": null }));
    f.apply(cmd).unwrap();
    assert!(clip(&f, 0).outgoing_transition.is_none());
}

#[test]
fn transition_errors() {
    let mut f = video(2);
    let c = set_transition(
        &f,
        0,
        json!({ "ref": "builtin:blur@1", "duration": "0.5s" }),
    );
    assert!(f.err(c).contains("not a transition"));
    let c = set_transition(
        &f,
        0,
        json!({ "ref": "builtin:wobble@1", "duration": "0.5s" }),
    );
    assert!(f.err(c).contains("unknown builtin"));
    let c = set_transition(&f, 0, json!({ "ref": "builtin:crossfade@1" }));
    assert!(f.err(c).contains("duration is required"));
    let c = set_transition(
        &f,
        0,
        json!({ "ref": "builtin:crossfade@1", "duration": "3s" }),
    );
    assert!(f.err(c).contains("half the clip"));
    let c = set_transition(
        &f,
        1,
        json!({ "ref": "builtin:crossfade@1", "duration": "0.5s" }),
    );
    assert!(f.err(c).contains("no following clip"));
    // `ref` is required (null clears); a call without it is an error, not a silent clear.
    let err = parse_command_json(json!({
        "command": "SetClipTransition", "track_id": f.track, "clip_id": f.clips[0]
    }))
    .unwrap_err()
    .to_string();
    assert!(err.contains("ref"), "{err}");
}

#[test]
fn apply_to_all_sets_every_target_and_a_format_1_alias_runs_its_builtin() {
    let _lib = isolated_library();
    let starter =
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../examples/packs/starter");
    let mut library = Library::open(&crate::library::default_root()).unwrap();
    library.install(&starter).unwrap();

    let mut f = video(4);
    let targets: Vec<serde_json::Value> = (0..3)
        .map(|i| json!({ "track_id": f.track, "clip_id": f.clips[i] }))
        .collect();
    let cmd = parse_command_json(json!({
        "command": "SetClipTransition", "targets": targets,
        "ref": "pack:local.starter/quick-wipe@1", "duration": "0.5s",
    }))
    .unwrap()
    .0;
    f.apply(cmd).unwrap();
    for i in 0..3 {
        let t = clip(&f, i).outgoing_transition.as_ref().unwrap();
        assert_eq!(t.item_ref, "pack:local.starter/quick-wipe@1");
    }
    assert!(clip(&f, 3).outgoing_transition.is_none());

    // The alias resolves to the builtin it names, so it renders.
    let packs = crate::packs::load_installed_packs();
    assert_eq!(
        crate::packs::status::effect_status(&packs, "pack:local.starter/quick-wipe@1", None),
        crate::project::EffectStatus::Ok
    );
}

// ---- deprecated v7 shapes -------------------------------------------------------------------------------

#[test]
fn the_v7_command_shapes_are_accepted_with_deprecation_warnings() {
    let mut f = video(2);
    // effects with effect_id replace the whole stack: a leading LUT would become the filter.
    let (cmd, warnings) = parse_command_json(json!({
        "command": "SetClipEffects", "track_id": f.track, "clip_id": f.clips[0],
        "effects": [
            { "id": Id::new_v4(), "effect_id": "builtin:chroma_key", "enabled": true, "params": {} },
            { "id": Id::new_v4(), "effect_id": "builtin:blur", "enabled": true, "params": { "radius": 4.0 } },
        ],
    }))
    .unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("effect_id") && warnings[0].contains("deprecated"));
    f.apply(cmd).unwrap();
    let refs: Vec<&str> = clip(&f, 0)
        .effect_stack()
        .iter()
        .map(|e| e.item_ref.as_str())
        .collect();
    assert_eq!(
        refs,
        ["builtin:chroma_key@1", "builtin:blur@1"],
        "order preserved"
    );
    assert_eq!(
        clip(&f, 0).effects[0].number("radius"),
        None,
        "chroma key stays in effects[]"
    );

    // transition: { kind, duration_us }
    let (cmd, warnings) = parse_command_json(json!({
        "command": "SetClipTransition", "track_id": f.track, "clip_id": f.clips[0],
        "transition": { "kind": "wipe_up", "duration_us": 400000 },
    }))
    .unwrap();
    assert!(warnings[0].contains("transition"));
    f.apply(cmd).unwrap();
    let t = clip(&f, 0).outgoing_transition.clone().unwrap();
    assert_eq!(
        (t.item_ref.as_str(), t.duration_us),
        ("builtin:wipe_up@1", 400_000)
    );
    let (cmd, _) = parse_command_json(json!({
        "command": "SetClipTransition", "track_id": f.track, "clip_id": f.clips[0], "transition": null,
    }))
    .unwrap();
    f.apply(cmd).unwrap();
    assert!(clip(&f, 0).outgoing_transition.is_none());

    // ease_in / ease_out keys
    let (cmd, warnings) = parse_command_json(json!({
        "command": "SetClipKeyframes", "track_id": f.track, "clip_id": f.clips[0],
        "keyframes": [{ "property": "opacity", "keys": [
            { "time_us": 0, "value": 0.0, "easing": "ease_in" },
            { "time_us": 1000000, "value": 1.0, "easing": "linear" } ] }],
    }))
    .unwrap();
    assert!(warnings[0].contains("ease_in"));
    f.apply(cmd).unwrap();
    let keys = opacity_keys(&f);
    assert_eq!(keys[0].easing, Easing::Bezier);
    assert_eq!(keys[0].handle_right, Some((333_333, 0.0)));
}
