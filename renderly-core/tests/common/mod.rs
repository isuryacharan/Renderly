//! Shared fixtures for the OTIO tests.
#![allow(dead_code)]

use renderly_core::project::*;

/// Seconds to µs, for fixtures that are written in seconds.
pub fn secs(s: f64) -> i64 {
    (s * 1_000_000.0).round() as i64
}

pub fn media(path: &str, kind: MediaKind, dur: Option<f64>) -> MediaItem {
    MediaItem {
        id: Id::new_v4(),
        path: path.into(),
        kind,
        duration_us: dur.map(secs),
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
        start_us: secs(pos),
        source_in_us: secs(src_in),
        source_out_us: secs(src_out),
        ..MediaClip::default()
    }
}

fn key(t: f64, v: f64, easing: Easing) -> Keyframe {
    Keyframe::new(secs(t), v).with_easing(easing)
}

fn effect(item_ref: &str, params: &[(&str, f64)]) -> EffectInstance {
    EffectInstance::new(item_ref).with_params(params)
}

/// A project that uses every feature OTIO has to carry: gaps, trims, speed (constant and
/// keyframed), transitions, effects, keyframes, masks, captions, audio roles / fades / denoise,
/// nested sequences (placed twice, plus one unplaced), an adjustment layer, multicam, an
/// off-frame-grid position, relative and absolute media paths.
pub fn rich_project() -> Project {
    let transcript_id = Id::new_v4();
    let mut p = Project::new(
        "Rich project",
        Settings {
            fps: renderly_core::time::Fps::new(30, 1),
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
        duration_us: None,
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
    sped.outgoing_transition = Some(ClipTransition::new("builtin:crossfade@1", 500_000));
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
            property: AnimProperty::Opacity.into(),
            keys: vec![key(0.0, 0.0, Easing::Auto), key(1.0, 1.0, Easing::Linear)],
        },
        KeyframeTrack {
            property: AnimProperty::PosX.into(),
            keys: vec![
                Keyframe {
                    handle_left: Some((-100000, 0.0)),
                    handle_right: Some((100000, 0.2)),
                    ..key(0.0, -0.5, Easing::Bezier)
                },
                key(2.0, 0.5, Easing::Linear),
            ],
        },
    ];
    loaded.key = Some(effect(
        "builtin:chroma_key@1",
        &[("tolerance", 0.4), ("softness", 0.2)],
    ));
    loaded.blend_mode = BlendMode::Screen;
    let blur = effect("builtin:blur@1", &[("radius", 4.0)]);
    let blur_id = blur.id;
    loaded.effects = vec![
        blur,
        effect(
            "builtin:color_adjust@1",
            &[("saturation", 1.3), ("contrast", 0.9)],
        ),
    ];
    loaded.keyframes.push(KeyframeTrack::new(
        TrackProperty::effect_param(blur_id, "radius"),
        vec![key(0.0, 0.0, Easing::Hold), key(1.5, 8.0, Easing::Linear)],
    ));
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
    loaded.outgoing_transition = Some(ClipTransition::new("builtin:wipe_left@1", 400_000));
    let loaded_id = loaded.id;
    v1.clips.push(Clip::Video(loaded));
    v1.clips.push(Clip::Video(clip(img_id, 9.5, 0.0, 2.0)));
    let mut off = clip(a_id, 11.5, 0.0, 1.0);
    off.enabled = false;
    v1.clips.push(Clip::Video(off));

    // V2: adjustment layer, then Intro placed as a nested sequence.
    let mut v2 = Track::new(TrackKind::Video, "V2");
    let mut adj = AdjustmentClip::new(secs(2.0), secs(3.0));
    adj.opacity = 0.6;
    adj.effects = vec![effect("builtin:color_adjust@1", &[("saturation", 0.2)])];
    adj.keyframes = vec![KeyframeTrack {
        property: AnimProperty::Opacity.into(),
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
    nested.effects = vec![effect("builtin:blur@1", &[("radius", 1.0)])];
    v2.clips.push(Clip::Video(nested));

    // V3: speed-keyframed clip, and a clip at an off-grid position.
    let mut v3 = Track::new(TrackKind::Video, "V3");
    let mut keyed = clip(a_id, 1.0, 2.0, 8.0);
    keyed.keyframes = vec![KeyframeTrack {
        property: AnimProperty::Speed.into(),
        keys: vec![key(0.0, 1.0, Easing::Linear), key(2.0, 2.0, Easing::Linear)],
    }];
    let keyed_id = keyed.id;
    // Keep source_out consistent with the integrated speed so the clip is well-formed.
    let tl = keyed.timeline_duration_us();
    assert!(tl > 0 && tl < 6000000);
    v3.clips.push(Clip::Video(keyed));
    v3.clips
        .push(Clip::Video(clip(a_id, 12.3456, 0.0123, 1.2345)));
    v3.locked = true;
    v3.hidden = true;

    // Captions.
    let mut caps = Track::new(TrackKind::Caption, "Captions");
    caps.clips.push(Clip::Caption(CaptionClip {
        id: Id::new_v4(),
        ..CaptionClip::new(500_000, 1500000, "Hello, \"world\" \u{2014} caf\u{e9}")
            .with_style("tiktok-bold-yellow")
    }));
    caps.clips.push(Clip::Caption(CaptionClip {
        id: Id::new_v4(),
        ..CaptionClip::new(3000000, 1000000, "Second").with_style("tiktok-minimal")
    }));
    // Word timings, runs, an animation and a transcript link survive the round trip.
    if let Some(Clip::Caption(first)) = caps.clips.first_mut() {
        first.body.runs = vec![TextRun {
            start: 0,
            end: 5,
            style: [("fill".to_string(), serde_json::json!("#FF0000"))].into(),
        }];
        first.body.words = vec![
            CaptionWord {
                text: "Hello,".into(),
                start_us: 0,
                end_us: 400_000,
                emphasis: true,
                word_id: Some("w-1".into()),
            },
            CaptionWord {
                text: "world".into(),
                start_us: 500_000,
                end_us: 900_000,
                emphasis: false,
                word_id: Some("w-2".into()),
            },
        ];
        first.animations.in_ = Some(TextAnimationRef {
            item_ref: "builtin:fade-up@1".into(),
            hash: None,
            params: [("amount".to_string(), serde_json::json!(0.5))].into(),
            duration_us: Some(300_000),
        });
        first.transcript_ref = Some(TranscriptRef {
            transcript_id,
            first_word: "w-1".into(),
            last_word: "w-2".into(),
        });
    }
    caps.caption_style = Some(TextStyleRef::builtin("tiktok-box"));

    // Audio.
    let mut a1 = Track::new(TrackKind::Audio, "Music");
    a1.audio_role = Some(TrackAudioRole::Music);
    let mut m = clip(music_id, 0.0, 0.0, 9.0);
    m.gain_db = -6.0;
    m.fade_in_us = 1_000_000;
    m.fade_out_us = 2_000_000;
    m.audio_denoise = Some(AudioDenoise {
        enabled: true,
        backend: "rnnoise".into(),
        strength: 0.7,
    });
    m.keyframes = vec![KeyframeTrack {
        property: AnimProperty::Volume.into(),
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

    // Titles: two touching text clips with a transition, on a magnetic track.
    let mut titles = Track::new(TrackKind::Video, "Titles");
    titles.magnetic = true;
    let mut t1 = TextClip::new(0, secs(2.0), TextBody::plain("EPISODE 12"));
    t1.style = TextStyleRef::builtin("tiktok-box");
    t1.style
        .overrides
        .insert("fill".into(), serde_json::json!("#FFD400"));
    t1.transform.y = -0.25;
    t1.effects = vec![effect("builtin:blur@1", &[("radius", 2.0)])];
    t1.blend_mode = BlendMode::Screen;
    t1.outgoing_transition = Some(ClipTransition::new("builtin:crossfade@1", secs(0.5)));
    let mut t2 = TextClip::new(secs(2.0), secs(2.0), TextBody::plain("Part two"));
    t2.enabled = false;
    titles.clips.push(Clip::Text(t1));
    titles.clips.push(Clip::Text(t2));

    p.tracks = vec![v1, v2, v3, caps, a1, a2, titles];
    p.markers = vec![
        Marker {
            id: Id::new_v4(),
            time_us: secs(1.0),
            label: "intro".into(),
            color: "#E53935".into(),
            kind: MarkerKind::User,
        },
        Marker {
            id: Id::new_v4(),
            time_us: 2_345_678,
            label: String::new(),
            color: String::new(),
            kind: MarkerKind::Beat,
        },
    ];
    p.transcripts = vec![Transcript {
        id: transcript_id,
        media_id: a_id,
        language: "en".into(),
        model: "ggml-base.en".into(),
        words: vec![
            TranscriptWord {
                id: "w-1".into(),
                text: "Hello,".into(),
                start_us: 100_000,
                end_us: 500_000,
                deleted: false,
                tags: vec![],
            },
            TranscriptWord {
                id: "w-2".into(),
                text: "um".into(),
                start_us: 600_000,
                end_us: 800_000,
                deleted: true,
                tags: vec!["filler".into()],
            },
        ],
    }];
    p.multicam_groups.push(MulticamGroup {
        id: Id::new_v4(),
        name: "Angles".into(),
        angle_clip_ids: vec![loaded_id, keyed_id, music_clip_id],
        active_angle: 1,
        sync_offsets_us: vec![0, 250000, -100000],
    });
    p.legacy_pack_paths = vec!["packs/fx".into(), "plugins/gain".into()];
    p.segmentation_model_path = Some("models/seg".into());
    p.validate().expect("fixture is valid");
    p
}
