//! Shared fixtures for the OTIO tests.
#![allow(dead_code)]

use renderly_core::project::*;
use std::collections::BTreeMap;

pub fn media(path: &str, kind: MediaKind, dur: Option<f64>) -> MediaItem {
    MediaItem {
        id: Id::new_v4(),
        path: path.into(),
        kind,
        duration_secs: dur,
        width: (kind != MediaKind::Audio).then_some(1280),
        height: (kind != MediaKind::Audio).then_some(720),
        fps: (kind == MediaKind::Video).then_some(30.0),
        sequence_id: None,
    }
}

fn clip(media_id: Id, pos: f64, src_in: f64, src_out: f64) -> MediaClip {
    MediaClip {
        id: Id::new_v4(),
        media_id,
        position_secs: pos,
        source_in_secs: src_in,
        source_out_secs: src_out,
        ..MediaClip::default()
    }
}

fn key(t: f64, v: f64, easing: Easing) -> Keyframe {
    Keyframe {
        time_secs: t,
        value: v,
        easing,
        handle_left: None,
        handle_right: None,
    }
}

fn effect(id: &str, params: &[(&str, f64)]) -> EffectInstance {
    EffectInstance {
        id: Id::new_v4(),
        effect_id: id.into(),
        enabled: true,
        params: params
            .iter()
            .map(|(k, v)| (k.to_string(), *v))
            .collect::<BTreeMap<_, _>>(),
    }
}

/// A project that uses every feature OTIO has to carry: gaps, trims, speed (constant and
/// keyframed), transitions, effects, keyframes, masks, captions, audio roles / fades / denoise,
/// nested sequences (placed twice, plus one unplaced), an adjustment layer, multicam, an
/// off-frame-grid position, relative and absolute media paths.
pub fn rich_project() -> Project {
    let mut p = Project::new(
        "Rich project",
        Settings {
            fps: 30.0,
            width: 1280,
            height: 720,
            sample_rate: 48_000,
            duck_db: -9.0,
        },
    );
    let a = media("D:/media/a clip.mp4", MediaKind::Video, Some(10.0));
    let b = media("media/b.mov", MediaKind::Video, Some(8.0));
    let img = media("D:/media/logo.png", MediaKind::Image, None);
    let music = media("D:/media/music.wav", MediaKind::Audio, Some(30.0));
    let voice = media("/home/u/voice.wav", MediaKind::Audio, Some(12.0));
    let (a_id, b_id, img_id, music_id, voice_id) = (a.id, b.id, img.id, music.id, voice.id);
    p.media.extend([a, b, img, music, voice]);

    // Sequences: "Intro" is placed (twice), "Spare" is not placed anywhere.
    let mut intro_v = Track::new(TrackKind::Video, "Intro V");
    intro_v.clips.push(Clip::Video(clip(a_id, 0.0, 0.0, 2.0)));
    let mut intro_a = Track::new(TrackKind::Audio, "Intro A");
    intro_a
        .clips
        .push(Clip::Audio(clip(music_id, 0.0, 0.0, 2.0)));
    let intro = Sequence {
        id: Id::new_v4(),
        name: "Intro".into(),
        width: Some(640),
        height: Some(360),
        tracks: vec![intro_v, intro_a],
    };
    let mut spare_v = Track::new(TrackKind::Video, "Spare V");
    spare_v.clips.push(Clip::Video(clip(b_id, 1.0, 0.0, 1.0)));
    let spare = Sequence {
        id: Id::new_v4(),
        name: "Spare".into(),
        width: None,
        height: None,
        tracks: vec![spare_v],
    };
    let seq_media = |s: &Sequence| MediaItem {
        id: Id::new_v4(),
        path: "".into(),
        kind: MediaKind::Sequence,
        duration_secs: None,
        width: None,
        height: None,
        fps: None,
        sequence_id: Some(s.id),
    };
    let (intro_media, spare_media) = (seq_media(&intro), seq_media(&spare));
    let intro_media_id = intro_media.id;
    p.media.extend([intro_media, spare_media]);
    p.sequences.extend([intro, spare]);

    // V1: trim, gap, sped-up clip with a crossfade into a clip with everything on it, a wipe,
    // a still and a disabled clip.
    let mut v1 = Track::new(TrackKind::Video, "V1");
    v1.clips.push(Clip::Video(clip(a_id, 0.0, 1.0, 4.0)));
    let mut sped = clip(b_id, 4.5, 1.0, 5.0);
    sped.speed = 2.0;
    sped.outgoing_transition = Some(ClipTransition {
        kind: TransitionKind::Crossfade,
        duration_secs: 0.5,
    });
    v1.clips.push(Clip::Video(sped));
    let mut loaded = clip(a_id, 6.5, 3.0, 6.0);
    loaded.transform = ClipTransform {
        x: 0.1,
        y: -0.2,
        scale_x: 1.2,
        scale_y: 1.2,
        rotation_deg: 5.0,
        opacity: 0.9,
        fit_x: 0.3,
        fit_y: 0.0,
    };
    loaded.keyframes = vec![
        KeyframeTrack {
            property: AnimProperty::Opacity,
            keys: vec![key(0.0, 0.0, Easing::EaseIn), key(1.0, 1.0, Easing::Linear)],
        },
        KeyframeTrack {
            property: AnimProperty::PosX,
            keys: vec![
                Keyframe {
                    handle_left: Some((-0.1, 0.0)),
                    handle_right: Some((0.1, 0.2)),
                    ..key(0.0, -0.5, Easing::Bezier)
                },
                key(2.0, 0.5, Easing::Linear),
            ],
        },
    ];
    loaded.effects = vec![
        effect("builtin:blur", &[("radius", 4.0)]),
        effect(
            "builtin:color_adjust",
            &[("saturation", 1.3), ("contrast", 0.9)],
        ),
    ];
    loaded.mask = Some(ClipMask {
        enabled: true,
        invert: true,
        feather: 0.1,
        kind: ClipMaskKind::Rect {
            x: 0.1,
            y: 0.1,
            width: 0.5,
            height: 0.5,
        },
    });
    loaded.background_removal = Some(BackgroundRemoval {
        enabled: true,
        model_id: "heuristic".into(),
        threshold: 0.2,
        feather: 0.05,
        temporal: true,
        matte_cache_dir: None,
        matte_frames: None,
    });
    loaded.outgoing_transition = Some(ClipTransition {
        kind: TransitionKind::WipeLeft,
        duration_secs: 0.4,
    });
    let loaded_id = loaded.id;
    v1.clips.push(Clip::Video(loaded));
    v1.clips.push(Clip::Video(clip(img_id, 9.5, 0.0, 2.0)));
    let mut off = clip(a_id, 11.5, 0.0, 1.0);
    off.enabled = false;
    v1.clips.push(Clip::Video(off));

    // V2: adjustment layer, then Intro placed as a nested sequence.
    let mut v2 = Track::new(TrackKind::Video, "V2");
    let mut adj = AdjustmentClip::new(2.0, 3.0);
    adj.opacity = 0.6;
    adj.effects = vec![effect("builtin:color_adjust", &[("saturation", 0.2)])];
    adj.keyframes = vec![KeyframeTrack {
        property: AnimProperty::Opacity,
        keys: vec![key(0.0, 0.0, Easing::Linear), key(1.0, 0.6, Easing::Linear)],
    }];
    adj.mask = Some(ClipMask {
        enabled: true,
        invert: false,
        feather: 0.0,
        kind: ClipMaskKind::Ellipse {
            cx: 0.5,
            cy: 0.5,
            rx: 0.3,
            ry: 0.3,
        },
    });
    v2.clips.push(Clip::Adjustment(adj));
    let mut nested = clip(intro_media_id, 7.0, 0.0, 2.0);
    nested.effects = vec![effect("builtin:blur", &[("radius", 1.0)])];
    v2.clips.push(Clip::Video(nested));

    // V3: speed-keyframed clip, and a clip at an off-grid position.
    let mut v3 = Track::new(TrackKind::Video, "V3");
    let mut keyed = clip(a_id, 1.0, 2.0, 8.0);
    keyed.keyframes = vec![KeyframeTrack {
        property: AnimProperty::Speed,
        keys: vec![key(0.0, 1.0, Easing::Linear), key(2.0, 2.0, Easing::Linear)],
    }];
    let keyed_id = keyed.id;
    // Keep source_out consistent with the integrated speed so the clip is well-formed.
    let tl = keyed.timeline_duration_secs();
    assert!(tl > 0.0 && tl < 6.0);
    v3.clips.push(Clip::Video(keyed));
    v3.clips
        .push(Clip::Video(clip(a_id, 12.3456, 0.0123, 1.2345)));
    v3.locked = true;
    v3.hidden = true;

    // Captions.
    let mut caps = Track::new(TrackKind::Caption, "Captions");
    caps.clips.push(Clip::Caption(CaptionClip {
        id: Id::new_v4(),
        text: "Hello, \"world\" \u{2014} caf\u{e9}".into(),
        position_secs: 0.5,
        duration_secs: 1.5,
        style_id: "tiktok-bold-yellow".into(),
    }));
    caps.clips.push(Clip::Caption(CaptionClip {
        id: Id::new_v4(),
        text: "Second".into(),
        position_secs: 3.0,
        duration_secs: 1.0,
        style_id: "tiktok-minimal".into(),
    }));

    // Audio.
    let mut a1 = Track::new(TrackKind::Audio, "Music");
    a1.audio_role = Some(TrackAudioRole::Music);
    let mut m = clip(music_id, 0.0, 0.0, 9.0);
    m.gain_db = -6.0;
    m.fade_in_secs = 1.0;
    m.fade_out_secs = 2.0;
    m.audio_denoise = Some(AudioDenoise {
        enabled: true,
        backend: "rnnoise".into(),
        strength: 0.7,
    });
    m.keyframes = vec![KeyframeTrack {
        property: AnimProperty::Volume,
        keys: vec![
            key(0.0, 0.0, Easing::Linear),
            key(1.0, -3.0, Easing::Linear),
        ],
    }];
    let music_clip_id = m.id;
    a1.clips.push(Clip::Audio(m));
    a1.clips
        .push(Clip::Audio(clip(intro_media_id, 12.0, 0.0, 2.0)));
    let mut a2 = Track::new(TrackKind::Audio, "Voice");
    a2.audio_role = Some(TrackAudioRole::Voiceover);
    a2.muted = true;
    a2.clips.push(Clip::Audio(clip(voice_id, 1.0, 0.0, 6.0)));

    p.tracks = vec![v1, v2, v3, caps, a1, a2];
    p.multicam_groups.push(MulticamGroup {
        id: Id::new_v4(),
        name: "Angles".into(),
        angle_clip_ids: vec![loaded_id, keyed_id, music_clip_id],
        active_angle: 1,
        sync_offsets_secs: vec![0.0, 0.25, -0.1],
    });
    p.asset_pack_paths = vec!["packs/fx".into()];
    p.wasm_plugin_paths = vec!["plugins/gain".into()];
    p.segmentation_model_path = Some("models/seg".into());
    p.validate().expect("fixture is valid");
    p
}
