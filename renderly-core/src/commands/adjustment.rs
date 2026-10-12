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
    position_secs: f64,
    duration_secs: f64,
) -> Result<CommandOutcome, CommandError> {
    if !position_secs.is_finite() || position_secs < 0.0 {
        return Err(CommandError::InvalidAdjustment(
            "position_secs must be a finite number >= 0".into(),
        ));
    }
    if !duration_secs.is_finite() || duration_secs <= 0.0 {
        return Err(CommandError::InvalidDuration(duration_secs));
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
    check_no_overlap(track, position_secs, duration_secs, None)?;

    let clip = AdjustmentClip::new(position_secs, duration_secs);
    let clip_id = clip.id;
    track.clips.push(Clip::Adjustment(clip));
    Ok(CommandOutcome::ClipAdded { clip_id })
}

pub(super) fn set_adjustment_layer(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    position_secs: Option<f64>,
    duration_secs: Option<f64>,
    opacity: Option<f64>,
) -> Result<CommandOutcome, CommandError> {
    if position_secs.is_none() && duration_secs.is_none() && opacity.is_none() {
        return Err(CommandError::SetAdjustmentLayerRequiresChange);
    }
    if let Some(p) = position_secs {
        if !p.is_finite() || p < 0.0 {
            return Err(CommandError::InvalidAdjustment(
                "position_secs must be a finite number >= 0".into(),
            ));
        }
    }
    if let Some(d) = duration_secs {
        if !d.is_finite() || d <= 0.0 {
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
    let new_position = position_secs.unwrap_or(existing.position_secs);
    let new_duration = duration_secs.unwrap_or(existing.duration_secs);
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
    adj.position_secs = new_position;
    adj.duration_secs = new_duration;
    if let Some(o) = opacity {
        adj.opacity = o;
    }
    Ok(CommandOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::super::{apply_command, Command, CommandError, CommandOutcome};
    use super::*;
    use crate::project::{
        AnimProperty, ClipMask, ClipMaskKind, EffectInstance, Keyframe, KeyframeTrack, MediaItem,
        MediaKind, Settings, Track,
    };

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
                position_secs: pos,
                duration_secs: dur,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn effect(effect_id: &str) -> EffectInstance {
        EffectInstance {
            id: Id::new_v4(),
            effect_id: effect_id.into(),
            enabled: true,
            params: Default::default(),
        }
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
        assert_eq!((a.position_secs, a.duration_secs), (4.0, 6.0));
        assert!(a.enabled);
        assert_eq!(a.opacity, 1.0);
        assert!(a.effects.is_empty() && a.mask.is_none() && a.keyframes.is_empty());
        assert_eq!(p.find_track(t).unwrap().clips[0].end_secs(), 10.0);
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
                    position_secs: 4.0,
                    duration_secs: 2.0
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
                    position_secs: 0.0,
                    duration_secs: 1.0
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
                    position_secs: 0.0,
                    duration_secs: 1.0
                }
            ),
            Err(CommandError::TrackNotFound(_))
        ));
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddAdjustmentLayer {
                    track_id: t,
                    position_secs: 10.0,
                    duration_secs: 0.0
                }
            ),
            Err(CommandError::InvalidDuration(_))
        ));
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddAdjustmentLayer {
                    track_id: t,
                    position_secs: -1.0,
                    duration_secs: 1.0
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
                position_secs: Some(1.0),
                duration_secs: Some(3.0),
                opacity: Some(0.25),
            },
        )
        .unwrap();
        let a = adjustment(&p, t, c);
        assert_eq!(
            (a.position_secs, a.duration_secs, a.opacity),
            (1.0, 3.0, 0.25)
        );

        let set = |position_secs, duration_secs, opacity| Command::SetAdjustmentLayer {
            track_id: t,
            clip_id: c,
            position_secs,
            duration_secs,
            opacity,
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
            apply_command(&mut p, set(None, Some(10.0), None)),
            Err(CommandError::Overlap(..))
        ));
        // Unknown clip.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::SetAdjustmentLayer {
                    track_id: t,
                    clip_id: Id::new_v4(),
                    position_secs: None,
                    duration_secs: None,
                    opacity: Some(0.5)
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
            duration_secs: Some(10.0),
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
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 1.0,
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
                    position_secs: None,
                    duration_secs: None,
                    opacity: Some(0.5)
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
            Command::SetClipEffects {
                track_id: t,
                clip_id: c,
                effects: vec![effect("builtin:blur")],
            },
        )
        .unwrap();
        assert_eq!(adjustment(&p, t, c).effects.len(), 1);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::SetClipEffects {
                    track_id: t,
                    clip_id: c,
                    effects: vec![effect("builtin:nope")]
                }
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
        let kf = |property| KeyframeTrack {
            property,
            keys: vec![Keyframe {
                time_secs: 0.0,
                value: 0.5,
                easing: Default::default(),
                handle_left: None,
                handle_right: None,
            }],
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
        assert!((adjustment(&p, t, c).opacity_at(1.0) - 0.5).abs() < 1e-9);
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
        p.wasm_plugin_paths.push(dir.clone());

        let res = apply_command(
            &mut p,
            Command::SetClipEffects {
                track_id: t,
                clip_id: c,
                effects: vec![effect("wasm:invert")],
            },
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
                    new_source_in_secs: Some(1.0),
                    new_source_out_secs: None
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
            Command::SetClipEffects {
                track_id: t,
                clip_id: c,
                effects: vec![effect("builtin:blur")],
            },
        )
        .unwrap();

        // Split: both halves are adjustment layers; effects are cloned with fresh ids.
        let CommandOutcome::ClipSplit { left_id, right_id } = apply_command(
            &mut p,
            Command::SplitClip {
                track_id: t,
                clip_id: c,
                at_secs: 1.5,
            },
        )
        .unwrap() else {
            panic!()
        };
        let (l, r) = (adjustment(&p, t, left_id), adjustment(&p, t, right_id));
        assert_eq!((l.position_secs, l.duration_secs), (0.0, 1.5));
        assert_eq!((r.position_secs, r.duration_secs), (1.5, 2.5));
        assert_ne!(l.effects[0].id, r.effects[0].id);
        assert_eq!(l.effects[0].effect_id, r.effects[0].effect_id);

        // Move to another video track at a new time.
        apply_command(
            &mut p,
            Command::MoveClip {
                track_id: t,
                clip_id: right_id,
                new_position_secs: 6.0,
                new_track_id: Some(t2_id),
            },
        )
        .unwrap();
        assert_eq!(adjustment(&p, t2_id, right_id).position_secs, 6.0);
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
                    new_position_secs: 0.0,
                    new_track_id: Some(aid)
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
        assert_eq!(adjustment(&p, t, later).position_secs, 1.5);
    }

    #[test]
    fn adjustment_clip_serializes_as_documented() {
        let (mut p, t) = project_with_video_track();
        let c = add(&mut p, t, 1.0, 2.0);
        let json = serde_json::to_value(p.find_track(t).unwrap().find_clip(c).unwrap()).unwrap();
        assert_eq!(json["type"], "adjustment");
        assert_eq!(json["position_secs"], 1.0);
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
            duration_secs: Some(5.0),
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
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
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
                position_secs: 0.0,
                source_in_secs: 0.0,
                source_out_secs: 2.0,
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
                    style_id: "default".into(),
                    timeline_offset_secs: 0.0
                }
            ),
            Err(CommandError::UnsupportedForSequence(_))
        ));
    }
}
