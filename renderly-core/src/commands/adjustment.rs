//! `AddAdjustmentLayer` / `SetAdjustmentLayer` (schema v7): adjustment layers are
//! [`Clip::Adjustment`] clips on video tracks that apply their effects to everything
//! composited beneath them. The other clip commands (`SetClipEffects`, `SetClipMask`,
//! `SetClipKeyframes`, `SetClipEnabled`, `MoveClip`, `SplitClip`, `DeleteClip`) learn about
//! the new clip kind in `mod.rs`.

use super::{check_no_overlap, CommandError, CommandOutcome};
use crate::project::{AdjustmentClip, Clip, Id, Project, TrackKind};

pub(super) fn add_adjustment_layer(
    project: &mut Project,
    track_id: Id,
    start_us: i64,
    duration_us: i64,
) -> Result<CommandOutcome, CommandError> {
    if start_us < 0 {
        return Err(CommandError::InvalidAdjustment(
            "start_us must be a finite number >= 0".into(),
        ));
    }
    if duration_us <= 0 {
        return Err(CommandError::InvalidDuration(duration_us));
    }
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Video {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Video,
        ));
    }
    check_no_overlap(track, start_us, duration_us, None)?;

    let clip = AdjustmentClip::new(start_us, duration_us);
    let clip_id = clip.id;
    track.clips.push(Clip::Adjustment(clip));
    Ok(CommandOutcome::ClipAdded { clip_id })
}

pub(super) fn set_adjustment_layer(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    start_us: Option<i64>,
    duration_us: Option<i64>,
    opacity: Option<f64>,
) -> Result<CommandOutcome, CommandError> {
    if start_us.is_none() && duration_us.is_none() && opacity.is_none() {
        return Err(CommandError::SetAdjustmentLayerRequiresChange);
    }
    if let Some(p) = start_us {
        if p < 0 {
            return Err(CommandError::InvalidAdjustment(
                "start_us must be a finite number >= 0".into(),
            ));
        }
    }
    if let Some(d) = duration_us {
        if d <= 0 {
            return Err(CommandError::InvalidDuration(d));
        }
    }
    if let Some(o) = opacity {
        if !o.is_finite() || !(0.0..=1.0).contains(&o) {
            return Err(CommandError::InvalidAdjustment(
                "opacity must be in 0..=1".into(),
            ));
        }
    }

    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let existing = track
        .find_clip(clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?
        .as_adjustment()
        .ok_or(CommandError::NotAdjustmentClip(clip_id))?;
    let new_position = start_us.unwrap_or(existing.start_us);
    let new_duration = duration_us.unwrap_or(existing.duration_us);
    check_no_overlap(track, new_position, new_duration, Some(clip_id))?;

    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let adj = track
        .clips
        .iter_mut()
        .find(|c| c.id() == clip_id)
        .and_then(Clip::as_adjustment_mut)
        .ok_or(CommandError::NotAdjustmentClip(clip_id))?;
    adj.start_us = new_position;
    adj.duration_us = new_duration;
    if let Some(o) = opacity {
        adj.opacity = o;
    }
    Ok(CommandOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::super::{apply_command, Command, CommandError, CommandOutcome, EffectSpec};
    use super::*;
    use crate::project::{
        AnimProperty, ClipMask, ClipMaskKind, Keyframe, KeyframeTrack, MediaItem, MediaKind,
        Settings, Track,
    };
    use crate::time::Time;

    fn project_with_video_track() -> (Project, Id) {
        let mut p = Project::new("adj-tests", Settings::default());
        let t = Track::new(TrackKind::Video, "V1");
        let id = t.id;
        p.tracks.push(t);
        (p, id)
    }

    fn add(p: &mut Project, track_id: Id, pos: f64, dur: f64) -> Id {
        match apply_command(
            p,
            Command::AddAdjustmentLayer {
                track_id,
                start: Time::Us((pos * 1e6).round() as i64),
                duration: Time::Us((dur * 1e6).round() as i64),
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn effect(item_ref: &str) -> EffectSpec {
        EffectSpec::new(item_ref)
    }

    fn adjustment(p: &Project, track: Id, clip: Id) -> &AdjustmentClip {
        p.find_track(track)
            .unwrap()
            .find_clip(clip)
            .unwrap()
            .as_adjustment()
            .unwrap()
    }

    #[test]
    fn add_adjustment_layer_adds_a_clip_with_defaults() {
        let (mut p, t) = project_with_video_track();
        let c = add(&mut p, t, 4.0, 6.0);
        let a = adjustment(&p, t, c);
        assert_eq!((a.start_us, a.duration_us), (4000000, 6000000));
        assert!(a.enabled);
        assert_eq!(a.opacity, 1.0);
        assert!(a.effects.is_empty() && a.mask.is_none() && a.keyframes.is_empty());
        assert_eq!(p.find_track(t).unwrap().clips[0].end_us(), 10000000);
    }

    #[test]
    fn add_adjustment_layer_errors() {
        let (mut p, t) = project_with_video_track();
        add(&mut p, t, 0.0, 5.0);
        // Overlap with the existing adjustment clip.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddAdjustmentLayer {
                    track_id: t,
                    start: Time::Us(4000000),
                    duration: Time::Us(2000000),
                }
            ),
            Err(CommandError::Overlap(..))
        ));
        // Non-video track.
        let a = Track::new(TrackKind::Audio, "A");
        let aid = a.id;
        p.tracks.push(a);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddAdjustmentLayer {
                    track_id: aid,
                    start: Time::Us(0),
                    duration: Time::Us(1000000),
                }
            ),
            Err(CommandError::TrackKindMismatch(..))
        ));
        // Unknown track, bad span.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddAdjustmentLayer {
                    track_id: Id::new_v4(),
                    start: Time::Us(0),
                    duration: Time::Us(1000000),
                }
            ),
            Err(CommandError::TrackNotFound(_))
        ));
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddAdjustmentLayer {
                    track_id: t,
                    start: Time::Us(10000000),
                    duration: Time::Us(0),
                }
            ),
            Err(CommandError::InvalidDuration(_))
        ));
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddAdjustmentLayer {
                    track_id: t,
                    start: Time::Us(-1_000_000),
                    duration: Time::Us(1000000),
                }
            ),
            Err(CommandError::InvalidAdjustment(_))
        ));
    }

    #[test]
    fn set_adjustment_layer_updates_fields_and_checks_overlap() {
        let (mut p, t) = project_with_video_track();
        let c = add(&mut p, t, 0.0, 2.0);
        add(&mut p, t, 5.0, 2.0);
        apply_command(
            &mut p,
            Command::SetAdjustmentLayer {
                track_id: t,
                clip_id: c,
                start: Some(Time::Us(1000000)),
                duration: Some(Time::Us(3000000)),
                opacity: Some(0.25),
            },
        )
        .unwrap();
        let a = adjustment(&p, t, c);
        assert_eq!(
            (a.start_us, a.duration_us, a.opacity),
            (1000000, 3000000, 0.25)
        );

        let set = |start_us: Option<i64>, duration_us: Option<i64>, opacity: Option<f64>| {
            Command::SetAdjustmentLayer {
                track_id: t,
                clip_id: c,
                start: start_us.map(Time::Us),
                duration: duration_us.map(Time::Us),
                opacity,
            }
        };
        // At least one field.
        assert!(matches!(
            apply_command(&mut p, set(None, None, None)),
            Err(CommandError::SetAdjustmentLayerRequiresChange)
        ));
        // Out-of-range opacity.
        assert!(matches!(
            apply_command(&mut p, set(None, None, Some(1.5))),
            Err(CommandError::InvalidAdjustment(_))
        ));
        // Growing into the neighbour.
        assert!(matches!(
            apply_command(&mut p, set(None, Some(10000000), None)),
            Err(CommandError::Overlap(..))
        ));
        // Unknown clip.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::SetAdjustmentLayer {
                    track_id: t,
                    clip_id: Id::new_v4(),
                    start: None,
                    duration: None,
                    opacity: Some(0.5),
                }
            ),
            Err(CommandError::ClipNotFound(..))
        ));
    }

    #[test]
    fn set_adjustment_layer_rejects_media_clips() {
        let (mut p, t) = project_with_video_track();
        let media = Id::new_v4();
        p.media.push(MediaItem {
            id: media,
            path: "a.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(10000000),
            width: Some(10),
            height: Some(10),
            fps: Some(30.0),
            sequence_id: None,
        });
        let CommandOutcome::ClipAdded { clip_id } = apply_command(
            &mut p,
            Command::AddClip {
                track_id: t,
                media_id: media,
                start: Time::Us(0),
                source_in: Time::Us(0),
                source_out: Time::Us(1000000),
            },
        )
        .unwrap() else {
            panic!()
        };
        assert!(matches!(
            apply_command(
                &mut p,
                Command::SetAdjustmentLayer {
                    track_id: t,
                    clip_id,
                    start: None,
                    duration: None,
                    opacity: Some(0.5),
                }
            ),
            Err(CommandError::NotAdjustmentClip(id)) if id == clip_id
        ));
    }

    #[test]
    fn existing_clip_commands_accept_adjustment_clips() {
        let (mut p, t) = project_with_video_track();
        let c = add(&mut p, t, 0.0, 4.0);

        // Effects: builtin ok, validated like media clips.
        apply_command(
            &mut p,
            Command::set_effects(t, c, vec![effect("builtin:blur@1")]),
        )
        .unwrap();
        assert_eq!(adjustment(&p, t, c).effects.len(), 1);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::set_effects(t, c, vec![effect("builtin:nope@1")])
            ),
            Err(CommandError::InvalidEffects(_))
        ));

        // Mask.
        let mask = ClipMask {
            enabled: true,
            invert: false,
            feather: 0.1,
            kind: ClipMaskKind::Rect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 0.5,
            },
        };
        apply_command(
            &mut p,
            Command::SetClipMask {
                track_id: t,
                clip_id: c,
                mask: Some(mask.clone()),
            },
        )
        .unwrap();
        assert_eq!(adjustment(&p, t, c).mask, Some(mask));

        // Keyframes: opacity only.
        let kf = |property: AnimProperty| KeyframeTrack {
            property: property.into(),
            keys: vec![Keyframe::new(0, 0.5).with_easing(Default::default())],
        };
        apply_command(
            &mut p,
            Command::SetClipKeyframes {
                track_id: t,
                clip_id: c,
                keyframes: vec![kf(AnimProperty::Opacity)],
            },
        )
        .unwrap();
        assert_eq!(adjustment(&p, t, c).keyframes.len(), 1);
        assert!((adjustment(&p, t, c).opacity_at(1000000) - 0.5).abs() < 1e-9);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::SetClipKeyframes {
                    track_id: t,
                    clip_id: c,
                    keyframes: vec![kf(AnimProperty::PosX)]
                }
            ),
            Err(CommandError::InvalidKeyframes(_))
        ));

        // Enabled.
        apply_command(
            &mut p,
            Command::SetClipEnabled {
                track_id: t,
                clip_id: c,
                enabled: false,
            },
        )
        .unwrap();
        assert!(!adjustment(&p, t, c).enabled);
    }

    #[test]
    fn wasm_plugin_effects_are_rejected_on_adjustment_layers() {
        let (mut p, t) = project_with_video_track();
        let c = add(&mut p, t, 0.0, 1.0);
        // A registered plugin: only its manifest matters for effect-id validation.
        let dir = std::env::temp_dir().join(format!("renderly-adj-wasm-{}", Id::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("invert.wasm"), b" asm").unwrap();
        std::fs::write(
            dir.join("plugin.json"),
            r#"{"id":"invert","name":"Invert","wasm":"invert.wasm"}"#,
        )
        .unwrap();
        p.legacy_pack_paths.push(dir.clone());

        let res = apply_command(
            &mut p,
            Command::set_effects(t, c, vec![effect("wasm:invert")]),
        );
        std::fs::remove_dir_all(&dir).ok();
        match res {
            Err(CommandError::InvalidEffects(msg)) => {
                assert!(msg.contains("adjustment"), "{msg}");
            }
            other => panic!("expected the adjustment-layer rejection, got {other:?}"),
        }
        assert!(adjustment(&p, t, c).effects.is_empty());
    }

    #[test]
    fn trim_is_rejected_for_adjustment_layers() {
        let (mut p, t) = project_with_video_track();
        let c = add(&mut p, t, 0.0, 4.0);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::TrimClip {
                    track_id: t,
                    clip_id: c,
                    source_in: Some(Time::Us(1000000)),
                    source_out: None,
                }
            ),
            Err(CommandError::TrimAdjustment(id)) if id == c
        ));
    }

    #[test]
    fn move_split_delete_work_on_adjustment_layers() {
        let (mut p, t) = project_with_video_track();
        let t2 = Track::new(TrackKind::Video, "V2");
        let t2_id = t2.id;
        p.tracks.push(t2);
        let c = add(&mut p, t, 0.0, 4.0);
        apply_command(
            &mut p,
            Command::set_effects(t, c, vec![effect("builtin:blur@1")]),
        )
        .unwrap();

        // Split: both halves are adjustment layers; effects are cloned with fresh ids.
        let CommandOutcome::ClipSplit { left_id, right_id } = apply_command(
            &mut p,
            Command::SplitClip {
                track_id: t,
                clip_id: c,
                at: Time::Us(1500000),
            },
        )
        .unwrap() else {
            panic!()
        };
        let (l, r) = (adjustment(&p, t, left_id), adjustment(&p, t, right_id));
        assert_eq!((l.start_us, l.duration_us), (0, 1500000));
        assert_eq!((r.start_us, r.duration_us), (1500000, 2500000));
        assert_ne!(l.effects[0].id, r.effects[0].id);
        assert_eq!(l.effects[0].item_ref, r.effects[0].item_ref);

        // Move to another video track at a new time.
        apply_command(
            &mut p,
            Command::MoveClip {
                track_id: t,
                clip_id: right_id,
                start: Time::Us(6000000),
                new_track_id: Some(t2_id),
            },
        )
        .unwrap();
        assert_eq!(adjustment(&p, t2_id, right_id).start_us, 6000000);
        // ...but not onto an audio track.
        let a = Track::new(TrackKind::Audio, "A");
        let aid = a.id;
        p.tracks.push(a);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::MoveClip {
                    track_id: t,
                    clip_id: left_id,
                    start: Time::Us(0),
                    new_track_id: Some(aid),
                }
            ),
            Err(CommandError::TrackKindMismatch(..))
        ));

        // Ripple delete shifts later adjustment layers left.
        let later = add(&mut p, t, 3.0, 1.0);
        apply_command(
            &mut p,
            Command::DeleteClip {
                track_id: t,
                clip_id: left_id,
                ripple: true,
            },
        )
        .unwrap();
        assert_eq!(adjustment(&p, t, later).start_us, 1500000);
    }

    #[test]
    fn adjustment_clip_serializes_as_documented() {
        let (mut p, t) = project_with_video_track();
        let c = add(&mut p, t, 1.0, 2.0);
        let json = serde_json::to_value(p.find_track(t).unwrap().find_clip(c).unwrap()).unwrap();
        assert_eq!(json["type"], "adjustment");
        assert_eq!(json["start_us"], 1_000_000);
        assert_eq!(json["opacity"], 1.0);
        assert!(json.get("mask").is_none(), "unset mask is omitted");
        let back: Clip = serde_json::from_value(json).unwrap();
        assert!(back.as_adjustment().is_some());
    }

    #[test]
    fn sequence_media_is_rejected_by_frame_analysis_commands() {
        let mut p = Project::new("x", Settings::default());
        let CommandOutcome::SequenceCreated {
            media_id: seq_media,
            sequence_id,
        } = apply_command(
            &mut p,
            Command::CreateSequence {
                name: "S".into(),
                width: None,
                height: None,
                id: None,
            },
        )
        .unwrap()
        else {
            panic!()
        };
        let inner = Track::new(TrackKind::Video, "I");
        let inner_id = inner.id;
        p.find_sequence_mut(sequence_id).unwrap().tracks.push(inner);
        let v = Id::new_v4();
        p.media.push(MediaItem {
            id: v,
            path: "a.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(5000000),
            width: Some(10),
            height: Some(10),
            fps: Some(30.0),
            sequence_id: None,
        });
        apply_command(
            &mut p,
            Command::AddClip {
                track_id: inner_id,
                media_id: v,
                start: Time::Us(0),
                source_in: Time::Us(0),
                source_out: Time::Us(2000000),
            },
        )
        .unwrap();
        let t = Track::new(TrackKind::Video, "V");
        let vtrack = t.id;
        p.tracks.push(t);
        let CommandOutcome::ClipAdded { clip_id } = apply_command(
            &mut p,
            Command::AddClip {
                track_id: vtrack,
                media_id: seq_media,
                start: Time::Us(0),
                source_in: Time::Us(0),
                source_out: Time::Us(2000000),
            },
        )
        .unwrap() else {
            panic!()
        };

        for cmd in [
            Command::TrackMotion {
                track_id: vtrack,
                clip_id,
                sample_count: 50,
                region: None,
                track_rotation_scale: false,
                target: None,
            },
            Command::StabilizeClip {
                track_id: vtrack,
                clip_id,
                strength: None,
                sample_count: None,
            },
            Command::AutoReframeClip {
                track_id: vtrack,
                clip_id,
                target_aspect: 1.0,
            },
            Command::GenerateBackgroundMatte {
                track_id: vtrack,
                clip_id,
            },
        ] {
            let name = format!("{cmd:?}");
            let res = apply_command(&mut p, cmd);
            assert!(
                matches!(res, Err(CommandError::UnsupportedForSequence(_))),
                "{name} -> {res:?}"
            );
        }
        let cap = Track::new(TrackKind::Caption, "C");
        let cap_id = cap.id;
        p.tracks.push(cap);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::GenerateCaptions {
                    media_id: seq_media,
                    track_id: cap_id,
                    style: Some(crate::project::TextStyleRef::builtin("default")),
                    timeline_offset: Time::Us(0),
                }
            ),
            Err(CommandError::UnsupportedForSequence(_))
        ));
    }
}
