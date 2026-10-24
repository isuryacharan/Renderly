//! Nested-sequence commands (schema v7): `CreateSequence`, `RenameSequence`,
//! `SetSequenceSettings`, `DeleteSequence`, `NestClips`, `UnnestClip`.
//!
//! A sequence is placed on a timeline through its [`MediaKind::Sequence`] media item, so
//! creating/deleting one always creates/removes both together.

use super::{check_no_overlap, CommandError, CommandOutcome};
use crate::project::{
    AnimProperty, Clip, Id, MediaClip, MediaItem, MediaKind, Project, Sequence, Track, TrackKind,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;

/// Names one clip: its track and its id (the pair every clip command takes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ClipRef {
    pub track_id: Id,
    pub clip_id: Id,
}

fn validate_dimension(value: Option<u32>) -> Result<(), CommandError> {
    match value {
        Some(0) => Err(CommandError::InvalidDimensions),
        _ => Ok(()),
    }
}

pub(super) fn create_sequence(
    project: &mut Project,
    name: String,
    width: Option<u32>,
    height: Option<u32>,
    id: Option<Id>,
) -> Result<CommandOutcome, CommandError> {
    validate_dimension(width)?;
    validate_dimension(height)?;
    if let Some(wanted) = id {
        if project.find_sequence(wanted).is_some() {
            return Err(CommandError::DuplicateSequenceId(wanted));
        }
    }
    let sequence_id = id.unwrap_or_else(Id::new_v4);
    let media_id = Id::new_v4();
    project.sequences.push(Sequence {
        id: sequence_id,
        name,
        width,
        height,
        tracks: Vec::new(),
    });
    project
        .media
        .push(sequence_media_item(media_id, sequence_id));
    Ok(CommandOutcome::SequenceCreated {
        sequence_id,
        media_id,
    })
}

fn sequence_media_item(media_id: Id, sequence_id: Id) -> MediaItem {
    MediaItem {
        id: media_id,
        path: PathBuf::new(),
        kind: MediaKind::Sequence,
        duration_us: None,
        width: None,
        height: None,
        fps: None,
        sequence_id: Some(sequence_id),
    }
}

pub(super) fn rename_sequence(
    project: &mut Project,
    sequence_id: Id,
    name: String,
) -> Result<CommandOutcome, CommandError> {
    project
        .find_sequence_mut(sequence_id)
        .ok_or(CommandError::SequenceNotFound(sequence_id))?
        .name = name;
    Ok(CommandOutcome::Applied)
}

pub(super) fn set_sequence_settings(
    project: &mut Project,
    sequence_id: Id,
    width: Option<u32>,
    height: Option<u32>,
) -> Result<CommandOutcome, CommandError> {
    validate_dimension(width)?;
    validate_dimension(height)?;
    let seq = project
        .find_sequence_mut(sequence_id)
        .ok_or(CommandError::SequenceNotFound(sequence_id))?;
    // Both fields are set exactly as given: `None` resets that axis to the project size.
    seq.width = width;
    seq.height = height;
    Ok(CommandOutcome::Applied)
}

pub(super) fn delete_sequence(
    project: &mut Project,
    sequence_id: Id,
) -> Result<CommandOutcome, CommandError> {
    if project.find_sequence(sequence_id).is_none() {
        return Err(CommandError::SequenceNotFound(sequence_id));
    }
    let media_id = project.sequence_media(sequence_id).map(|m| m.id);
    let in_use = project.all_tracks().any(|t| {
        t.clips
            .iter()
            .filter_map(Clip::as_media)
            .any(|m| Some(m.media_id) == media_id)
    });
    if in_use {
        return Err(CommandError::SequenceInUse(sequence_id));
    }
    project.sequences.retain(|s| s.id != sequence_id);
    project.media.retain(|m| Some(m.id) != media_id);
    Ok(CommandOutcome::Applied)
}

fn same_flags(from: &Track) -> Track {
    let mut t = Track::new(from.kind, from.name.clone());
    t.audio_role = from.audio_role;
    t.muted = from.muted;
    t.locked = from.locked;
    t.hidden = from.hidden;
    t
}

pub(super) fn nest_clips(
    project: &mut Project,
    clips: Vec<ClipRef>,
    name: String,
) -> Result<CommandOutcome, CommandError> {
    // The move touches several tracks and the sequence list; restore everything on failure so
    // a rejected command never leaves a half-nested project.
    let backup = project.clone();
    nest_clips_inner(project, clips, name).inspect_err(|_| *project = backup)
}

fn nest_clips_inner(
    project: &mut Project,
    clips: Vec<ClipRef>,
    name: String,
) -> Result<CommandOutcome, CommandError> {
    let mut seen = HashSet::new();
    let refs: Vec<ClipRef> = clips.into_iter().filter(|r| seen.insert(*r)).collect();
    let Some(first) = refs.first() else {
        return Err(CommandError::InvalidNest(
            "clips must contain at least one clip".into(),
        ));
    };
    let container = project
        .container_of_track(first.track_id)
        .ok_or(CommandError::TrackNotFound(first.track_id))?;

    let (mut start, mut end) = (i64::MAX, i64::MIN);
    let (mut has_video, mut has_audio) = (false, false);
    for r in &refs {
        if project.container_of_track(r.track_id) != Some(container) {
            return Err(CommandError::InvalidNest(
                "all clips must be on the same timeline (the root or one sequence)".into(),
            ));
        }
        let clip = project
            .find_track(r.track_id)
            .and_then(|t| t.find_clip(r.clip_id))
            .ok_or(CommandError::ClipNotFound(r.clip_id, r.track_id))?;
        match clip {
            Clip::Video(_) | Clip::Adjustment(_) | Clip::Text(_) => has_video = true,
            Clip::Audio(_) => has_audio = true,
            Clip::Caption(_) => {
                return Err(CommandError::UnsupportedForSequence("caption clips"));
            }
        }
        start = start.min(clip.start_us());
        end = end.max(clip.end_us());
    }
    let duration = end - start;

    // Source tracks, bottom to top, mirrored as the sequence's own tracks so relative layering
    // is preserved.
    let source_track_ids: Vec<Id> = project
        .container_tracks(container)
        .ok_or(CommandError::TrackNotFound(first.track_id))?
        .iter()
        .filter(|t| refs.iter().any(|r| r.track_id == t.id))
        .map(|t| t.id)
        .collect();
    let mut seq_tracks: Vec<(Id, Track)> = Vec::new();
    for tid in &source_track_ids {
        let src = project
            .find_track(*tid)
            .ok_or(CommandError::TrackNotFound(*tid))?;
        seq_tracks.push((*tid, same_flags(src)));
    }

    for r in &refs {
        let track = project
            .find_track_mut(r.track_id)
            .ok_or(CommandError::TrackNotFound(r.track_id))?;
        let idx = track
            .clips
            .iter()
            .position(|c| c.id() == r.clip_id)
            .ok_or(CommandError::ClipNotFound(r.clip_id, r.track_id))?;
        let mut clip = track.clips.remove(idx);
        clip.set_start_us(clip.start_us() - start);
        if let Some((_, dest)) = seq_tracks.iter_mut().find(|(tid, _)| *tid == r.track_id) {
            dest.clips.push(clip);
        }
    }

    let sequence_id = Id::new_v4();
    let media_id = Id::new_v4();
    project.sequences.push(Sequence {
        id: sequence_id,
        name,
        width: None,
        height: None,
        tracks: seq_tracks.into_iter().map(|(_, t)| t).collect(),
    });
    project
        .media
        .push(sequence_media_item(media_id, sequence_id));

    let kind_candidates = |kind: TrackKind| -> Vec<Id> {
        // Top-most first: prefer replacing on the highest source track.
        source_track_ids
            .iter()
            .rev()
            .copied()
            .filter(|tid| project.find_track(*tid).is_some_and(|t| t.kind == kind))
            .collect()
    };
    let video_candidates = kind_candidates(TrackKind::Video);
    let audio_candidates = kind_candidates(TrackKind::Audio);

    let video_clip_id = if has_video {
        Some(place_outer_clip(
            project,
            container,
            TrackKind::Video,
            &video_candidates,
            media_id,
            start,
            duration,
        )?)
    } else {
        None
    };
    let audio_clip_id = if has_audio {
        Some(place_outer_clip(
            project,
            container,
            TrackKind::Audio,
            &audio_candidates,
            media_id,
            start,
            duration,
        )?)
    } else {
        None
    };

    Ok(CommandOutcome::ClipsNested {
        sequence_id,
        media_id,
        video_clip_id,
        audio_clip_id,
    })
}

/// Put the clip that stands in for the nested content on the first candidate track with room;
/// if none has, on a new track at the top of the container.
fn place_outer_clip(
    project: &mut Project,
    container: crate::project::ContainerId,
    kind: TrackKind,
    candidates: &[Id],
    media_id: Id,
    start: i64,
    duration: i64,
) -> Result<Id, CommandError> {
    let media_clip = MediaClip {
        id: Id::new_v4(),
        media_id,
        start_us: start,
        source_in_us: 0,
        source_out_us: duration,
        ..Default::default()
    };
    let clip_id = media_clip.id;
    let make = |m: MediaClip| match kind {
        TrackKind::Audio => Clip::Audio(m),
        _ => Clip::Video(m),
    };
    for tid in candidates {
        let Some(track) = project.find_track_mut(*tid) else {
            continue;
        };
        if check_no_overlap(track, start, duration, None).is_ok() {
            track.clips.push(make(media_clip));
            return Ok(clip_id);
        }
    }
    let mut track = Track::new(
        kind,
        match kind {
            TrackKind::Audio => "Nested audio",
            _ => "Nested",
        },
    );
    track.clips.push(make(media_clip));
    project
        .container_tracks_mut(container)
        .ok_or(CommandError::InvalidNest("timeline vanished".into()))?
        .push(track);
    Ok(clip_id)
}

pub(super) fn unnest_clip(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
) -> Result<CommandOutcome, CommandError> {
    let backup = project.clone();
    unnest_clip_inner(project, track_id, clip_id).inspect_err(|_| *project = backup)
}

fn unnest_clip_inner(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
) -> Result<CommandOutcome, CommandError> {
    let container = project
        .container_of_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let clip = track
        .find_clip(clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;
    let (outer, is_audio) = match clip {
        Clip::Video(m) => (m.clone(), false),
        Clip::Audio(m) => (m.clone(), true),
        Clip::Caption(_) | Clip::Text(_) | Clip::Adjustment(_) => {
            return Err(CommandError::CannotUnnest(
                "the clip does not place a sequence".into(),
            ));
        }
    };
    let sequence_id = project
        .find_media(outer.media_id)
        .filter(|m| m.kind == MediaKind::Sequence)
        .and_then(|m| m.sequence_id)
        .ok_or_else(|| CommandError::CannotUnnest("the clip does not place a sequence".into()))?;

    let has_speed_keys = outer
        .keyframes
        .iter()
        .any(|k| k.property == AnimProperty::Speed && !k.keys.is_empty());
    let reason = if outer.source_in_us != 0
        || outer.source_out_us != project.sequence_duration(sequence_id)
    {
        Some("the clip is trimmed")
    } else if (outer.speed - 1.0).abs() > 1e-9 || has_speed_keys {
        Some("the clip has a speed change")
    } else if !outer.effects.is_empty() {
        Some("the clip has effects")
    } else if outer.mask.is_some() {
        Some("the clip has a mask")
    } else if !outer.transform.is_identity() {
        Some("the clip has a transform")
    } else if !outer.keyframes.is_empty() {
        Some("the clip has keyframes")
    } else if outer.outgoing_transition.is_some() {
        Some("the clip has a transition")
    } else if outer.background_removal.is_some() {
        Some("the clip has background removal")
    } else if is_audio
        && (outer.gain_db != 0.0
            || outer.fade_in_us > 0
            || outer.fade_out_us > 0
            || outer.audio_denoise.is_some())
    {
        Some("the clip has gain, fades or denoise")
    } else {
        None
    };
    if let Some(reason) = reason {
        return Err(CommandError::CannotUnnest(reason.into()));
    }

    let wanted_kind = if is_audio {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    let inner_tracks = project
        .find_sequence(sequence_id)
        .ok_or(CommandError::SequenceNotFound(sequence_id))?
        .tracks
        .clone();
    let mut new_tracks: Vec<Track> = Vec::new();
    for inner in inner_tracks.iter().filter(|t| t.kind == wanted_kind) {
        let mut copy = same_flags(inner);
        for clip in &inner.clips {
            let mut c = clip.clone();
            match &mut c {
                Clip::Video(m) | Clip::Audio(m) => {
                    m.id = Id::new_v4();
                    let remap = m.reid_effects();
                    crate::project::remap_effect_tracks(&mut m.keyframes, &remap);
                    m.multicam_group_id = None;
                }
                Clip::Adjustment(a) => {
                    a.id = Id::new_v4();
                    let remap = a.reid_effects();
                    crate::project::remap_effect_tracks(&mut a.keyframes, &remap);
                }
                Clip::Text(t) => {
                    t.id = Id::new_v4();
                    let remap = t.reid_effects();
                    crate::project::remap_effect_tracks(&mut t.keyframes, &remap);
                }
                Clip::Caption(_) => continue,
            }
            c.set_start_us(c.start_us() + outer.start_us);
            copy.clips.push(c);
        }
        if !copy.clips.is_empty() {
            new_tracks.push(copy);
        }
    }

    let tracks = project
        .container_tracks_mut(container)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let track_idx = tracks
        .iter()
        .position(|t| t.id == track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    tracks[track_idx].clips.retain(|c| c.id() != clip_id);
    for (i, t) in new_tracks.into_iter().enumerate() {
        tracks.insert(track_idx + 1 + i, t);
    }
    Ok(CommandOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::super::{apply_command, Command, CommandError, CommandOutcome};
    use super::*;
    use crate::project::Settings;
    use crate::time::Time;

    fn project() -> Project {
        Project::new("seq-tests", Settings::default())
    }

    fn video_media(project: &mut Project, duration: f64) -> Id {
        let id = Id::new_v4();
        project.media.push(MediaItem {
            id,
            path: "a.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some((duration * 1e6).round() as i64),
            width: Some(1920),
            height: Some(1080),
            fps: Some(30.0),
            sequence_id: None,
        });
        id
    }

    fn audio_media(project: &mut Project, duration: f64) -> Id {
        let id = Id::new_v4();
        project.media.push(MediaItem {
            id,
            path: "a.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some((duration * 1e6).round() as i64),
            width: None,
            height: None,
            fps: None,
            sequence_id: None,
        });
        id
    }

    fn track(project: &mut Project, kind: TrackKind, sequence_id: Option<Id>) -> Id {
        let out = apply_command(
            project,
            Command::AddTrack {
                kind,
                name: "T".into(),
                id: None,
                sequence_id,
                magnetic: Some(false),
            },
        )
        .unwrap();
        let CommandOutcome::TrackAdded { track_id } = out else {
            panic!("expected TrackAdded")
        };
        track_id
    }

    fn clip(project: &mut Project, track_id: Id, media_id: Id, pos: f64, dur: f64) -> Id {
        let out = apply_command(
            project,
            Command::AddClip {
                track_id,
                media_id,
                start: Time::Us((pos * 1e6).round() as i64),
                source_in: Time::Us(0),
                source_out: Time::Us((dur * 1e6).round() as i64),
            },
        )
        .unwrap();
        let CommandOutcome::ClipAdded { clip_id } = out else {
            panic!("expected ClipAdded")
        };
        clip_id
    }

    fn create_sequence(project: &mut Project, name: &str) -> (Id, Id) {
        let out = apply_command(
            project,
            Command::CreateSequence {
                name: name.into(),
                width: None,
                height: None,
                id: None,
            },
        )
        .unwrap();
        let CommandOutcome::SequenceCreated {
            sequence_id,
            media_id,
        } = out
        else {
            panic!("expected SequenceCreated")
        };
        (sequence_id, media_id)
    }

    fn cref(track_id: Id, clip_id: Id) -> ClipRef {
        ClipRef { track_id, clip_id }
    }

    #[test]
    fn create_sequence_adds_sequence_and_media_item() {
        let mut p = project();
        let (seq, media) = create_sequence(&mut p, "Intro");
        assert_eq!(p.sequences.len(), 1);
        assert_eq!(p.sequences[0].name, "Intro");
        let item = p.find_media(media).unwrap();
        assert_eq!(item.kind, MediaKind::Sequence);
        assert_eq!(item.sequence_id, Some(seq));
        assert!(item.path.as_os_str().is_empty());
        assert!(p.validate().is_ok());
    }

    #[test]
    fn create_sequence_honors_id_and_rejects_duplicates_and_zero_size() {
        let mut p = project();
        let wanted = Id::new_v4();
        let out = apply_command(
            &mut p,
            Command::CreateSequence {
                name: "A".into(),
                width: Some(640),
                height: Some(360),
                id: Some(wanted),
            },
        )
        .unwrap();
        assert!(matches!(
            out,
            CommandOutcome::SequenceCreated { sequence_id, .. } if sequence_id == wanted
        ));
        assert_eq!(
            p.sequence_size(p.find_sequence(wanted).unwrap()),
            (640, 360)
        );

        let dup = apply_command(
            &mut p,
            Command::CreateSequence {
                name: "B".into(),
                width: None,
                height: None,
                id: Some(wanted),
            },
        );
        assert!(matches!(dup, Err(CommandError::DuplicateSequenceId(id)) if id == wanted));
        let zero = apply_command(
            &mut p,
            Command::CreateSequence {
                name: "C".into(),
                width: Some(0),
                height: None,
                id: None,
            },
        );
        assert!(matches!(zero, Err(CommandError::InvalidDimensions)));
        assert_eq!(
            p.sequences.len(),
            1,
            "failed commands must not leave a sequence behind"
        );
    }

    #[test]
    fn rename_and_set_settings() {
        let mut p = project();
        let (seq, _) = create_sequence(&mut p, "Old");
        apply_command(
            &mut p,
            Command::RenameSequence {
                sequence_id: seq,
                name: "New".into(),
            },
        )
        .unwrap();
        assert_eq!(p.find_sequence(seq).unwrap().name, "New");

        apply_command(
            &mut p,
            Command::SetSequenceSettings {
                sequence_id: seq,
                width: Some(100),
                height: Some(200),
            },
        )
        .unwrap();
        assert_eq!(p.sequence_size(p.find_sequence(seq).unwrap()), (100, 200));
        // null resets to inherit.
        apply_command(
            &mut p,
            Command::SetSequenceSettings {
                sequence_id: seq,
                width: None,
                height: Some(50),
            },
        )
        .unwrap();
        assert_eq!(
            p.sequence_size(p.find_sequence(seq).unwrap()),
            (p.settings.width, 50)
        );

        let missing = Id::new_v4();
        assert!(matches!(
            apply_command(
                &mut p,
                Command::RenameSequence {
                    sequence_id: missing,
                    name: "x".into()
                }
            ),
            Err(CommandError::SequenceNotFound(id)) if id == missing
        ));
        assert!(matches!(
            apply_command(
                &mut p,
                Command::SetSequenceSettings {
                    sequence_id: seq,
                    width: Some(0),
                    height: None
                }
            ),
            Err(CommandError::InvalidDimensions)
        ));
    }

    #[test]
    fn delete_sequence_removes_both_and_refuses_while_in_use() {
        let mut p = project();
        let v = video_media(&mut p, 10.0);
        let (seq, media) = create_sequence(&mut p, "S");
        let inner = track(&mut p, TrackKind::Video, Some(seq));
        clip(&mut p, inner, v, 0.0, 2.0);
        let root = track(&mut p, TrackKind::Video, None);
        let placed = clip(&mut p, root, media, 0.0, 2.0);

        assert!(matches!(
            apply_command(&mut p, Command::DeleteSequence { sequence_id: seq }),
            Err(CommandError::SequenceInUse(id)) if id == seq
        ));
        assert_eq!(p.sequences.len(), 1);

        apply_command(
            &mut p,
            Command::DeleteClip {
                track_id: root,
                clip_id: placed,
                ripple: false,
            },
        )
        .unwrap();
        apply_command(&mut p, Command::DeleteSequence { sequence_id: seq }).unwrap();
        assert!(p.sequences.is_empty());
        assert!(p.find_media(media).is_none());
        assert!(matches!(
            apply_command(&mut p, Command::DeleteSequence { sequence_id: seq }),
            Err(CommandError::SequenceNotFound(_))
        ));
    }

    #[test]
    fn add_track_in_sequence_and_unique_ids_across_containers() {
        let mut p = project();
        let (seq, _) = create_sequence(&mut p, "S");
        let root = track(&mut p, TrackKind::Video, None);
        let inner = track(&mut p, TrackKind::Video, Some(seq));
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.find_sequence(seq).unwrap().tracks.len(), 1);
        assert_eq!(p.container_of_track(root), Some(None));
        assert_eq!(p.container_of_track(inner), Some(Some(seq)));
        // find_track searches every container.
        assert!(p.find_track(inner).is_some());

        // Duplicate ids are rejected even across containers.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddTrack {
                    kind: TrackKind::Audio,
                    name: "dup".into(),
                    id: Some(inner),
                    sequence_id: None,
                    magnetic: Some(false),
                }
            ),
            Err(CommandError::DuplicateTrackId(id)) if id == inner
        ));
        // Unknown sequence, and caption tracks are root-only.
        let missing = Id::new_v4();
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddTrack {
                    kind: TrackKind::Video,
                    name: "x".into(),
                    id: None,
                    sequence_id: Some(missing),
                    magnetic: Some(false),
                }
            ),
            Err(CommandError::SequenceNotFound(id)) if id == missing
        ));
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddTrack {
                    kind: TrackKind::Caption,
                    name: "c".into(),
                    id: None,
                    sequence_id: Some(seq),
                    magnetic: Some(false),
                }
            ),
            Err(CommandError::UnsupportedForSequence(_))
        ));
    }

    #[test]
    fn move_and_delete_track_are_container_scoped() {
        let mut p = project();
        let (seq, _) = create_sequence(&mut p, "S");
        let r1 = track(&mut p, TrackKind::Video, None);
        let r2 = track(&mut p, TrackKind::Video, None);
        let s1 = track(&mut p, TrackKind::Video, Some(seq));
        let s2 = track(&mut p, TrackKind::Video, Some(seq));
        let s3 = track(&mut p, TrackKind::Video, Some(seq));

        // new_index is relative to the track's own container and clamped to it.
        apply_command(
            &mut p,
            Command::MoveTrack {
                track_id: s1,
                new_index: 99,
            },
        )
        .unwrap();
        let order: Vec<Id> = p
            .find_sequence(seq)
            .unwrap()
            .tracks
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(order, vec![s2, s3, s1]);
        assert_eq!(
            p.tracks.iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![r1, r2],
            "root order untouched"
        );

        apply_command(&mut p, Command::DeleteTrack { track_id: s2 }).unwrap();
        assert_eq!(p.find_sequence(seq).unwrap().tracks.len(), 2);
        assert_eq!(p.tracks.len(), 2);
    }

    #[test]
    fn add_clip_places_sequence_media_on_video_and_audio_tracks() {
        let mut p = project();
        let v = video_media(&mut p, 10.0);
        let (seq, media) = create_sequence(&mut p, "S");
        let inner = track(&mut p, TrackKind::Video, Some(seq));
        clip(&mut p, inner, v, 0.0, 4.0);
        assert!((p.media_duration(media).unwrap() - 4000000).abs() < 1);

        let vt = track(&mut p, TrackKind::Video, None);
        let at = track(&mut p, TrackKind::Audio, None);
        let vc = clip(&mut p, vt, media, 1.0, 4.0);
        let ac = clip(&mut p, at, media, 1.0, 4.0);
        assert!(matches!(
            p.find_track(vt).unwrap().find_clip(vc),
            Some(Clip::Video(_))
        ));
        assert!(matches!(
            p.find_track(at).unwrap().find_clip(ac),
            Some(Clip::Audio(_))
        ));

        // Source range is checked against the derived duration.
        let t2 = track(&mut p, TrackKind::Video, None);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddClip {
                    track_id: t2,
                    media_id: media,
                    start: Time::Us(0),
                    source_in: Time::Us(0),
                    source_out: Time::Us(5000000),
                }
            ),
            Err(CommandError::ExceedsMediaDuration(d)) if (d - 4000000).abs() < 1
        ));
        // Sequences cannot go on caption tracks.
        let cap = track(&mut p, TrackKind::Caption, None);
        assert!(matches!(
            apply_command(
                &mut p,
                Command::AddClip {
                    track_id: cap,
                    media_id: media,
                    start: Time::Us(0),
                    source_in: Time::Us(0),
                    source_out: Time::Us(1000000),
                }
            ),
            Err(CommandError::TrackKindMismatch(..))
        ));
        assert!(p.validate().is_ok());
    }

    #[test]
    fn add_clip_rejects_cycles_direct_and_transitive() {
        let mut p = project();
        let v = video_media(&mut p, 10.0);
        let (a, a_media) = create_sequence(&mut p, "A");
        let (b, b_media) = create_sequence(&mut p, "B");
        let ta = track(&mut p, TrackKind::Video, Some(a));
        let tb = track(&mut p, TrackKind::Video, Some(b));
        clip(&mut p, ta, v, 0.0, 2.0);
        clip(&mut p, tb, v, 0.0, 2.0);

        // A inside itself.
        let direct = apply_command(
            &mut p,
            Command::AddClip {
                track_id: ta,
                media_id: a_media,
                start: Time::Us(5000000),
                source_in: Time::Us(0),
                source_out: Time::Us(1000000),
            },
        );
        assert!(matches!(direct, Err(CommandError::SequenceCycle(id)) if id == a));

        // B inside A is fine; then A inside B would be a cycle.
        clip(&mut p, ta, b_media, 5.0, 1.0);
        let transitive = apply_command(
            &mut p,
            Command::AddClip {
                track_id: tb,
                media_id: a_media,
                start: Time::Us(5000000),
                source_in: Time::Us(0),
                source_out: Time::Us(1000000),
            },
        );
        assert!(matches!(transitive, Err(CommandError::SequenceCycle(id)) if id == a));
        assert!(p.validate().is_ok());
    }

    #[test]
    fn move_clip_across_containers_checks_cycles() {
        let mut p = project();
        let v = video_media(&mut p, 10.0);
        let (a, a_media) = create_sequence(&mut p, "A");
        let ta = track(&mut p, TrackKind::Video, Some(a));
        clip(&mut p, ta, v, 0.0, 2.0);
        let root = track(&mut p, TrackKind::Video, None);
        let placed = clip(&mut p, root, a_media, 0.0, 2.0);
        // Moving the clip that places A into A itself would make A contain A.
        let res = apply_command(
            &mut p,
            Command::MoveClip {
                track_id: root,
                clip_id: placed,
                start: Time::Us(5000000),
                new_track_id: Some(ta),
            },
        );
        assert!(matches!(res, Err(CommandError::SequenceCycle(id)) if id == a));
        assert!(p.find_track(root).unwrap().find_clip(placed).is_some());
    }

    #[test]
    fn nest_clips_moves_clips_and_replaces_them_with_one_video_clip() {
        let mut p = project();
        let v = video_media(&mut p, 20.0);
        let a = audio_media(&mut p, 20.0);
        let v1 = track(&mut p, TrackKind::Video, None);
        let v2 = track(&mut p, TrackKind::Video, None);
        let a1 = track(&mut p, TrackKind::Audio, None);
        let c1 = clip(&mut p, v1, v, 2.0, 3.0);
        let c2 = clip(&mut p, v2, v, 3.0, 4.0);
        let c3 = clip(&mut p, a1, a, 2.0, 5.0);
        // A clip that stays behind.
        let keep = clip(&mut p, v1, v, 10.0, 1.0);

        let out = apply_command(
            &mut p,
            Command::NestClips {
                clips: vec![cref(v1, c1), cref(v2, c2), cref(a1, c3)],
                name: "Nested".into(),
            },
        )
        .unwrap();
        let CommandOutcome::ClipsNested {
            sequence_id,
            media_id,
            video_clip_id,
            audio_clip_id,
        } = out
        else {
            panic!("expected ClipsNested")
        };
        let video_clip_id = video_clip_id.expect("a video clip replaces the video clips");
        let audio_clip_id = audio_clip_id.expect("an audio clip replaces the audio clips");

        let seq = p.find_sequence(sequence_id).unwrap();
        assert_eq!(seq.name, "Nested");
        // Layering preserved: one inner track per source track, bottom to top.
        assert_eq!(seq.tracks.len(), 3);
        assert_eq!(
            seq.tracks.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![TrackKind::Video, TrackKind::Video, TrackKind::Audio]
        );
        // Relative timing: earliest clip starts at 0, the other keeps its 1 s offset.
        assert_eq!(seq.tracks[0].clips[0].start_us(), 0);
        assert!((seq.tracks[1].clips[0].start_us() - 1000000).abs() < 1);
        assert_eq!(seq.tracks[2].clips[0].start_us(), 0);
        // Track ids stay globally unique; the moved clips keep their ids.
        assert_eq!(seq.tracks[0].clips[0].id(), c1);
        assert!(p.validate().is_ok());

        // Replaced by single clips spanning [2, 7) placing the new sequence media.
        let placed = p
            .all_tracks()
            .find_map(|t| t.find_clip(video_clip_id))
            .unwrap();
        let Clip::Video(m) = placed else { panic!() };
        assert_eq!(m.media_id, media_id);
        assert!((m.start_us - 2000000).abs() < 1);
        assert!((m.timeline_duration_us() - 5000000).abs() < 1);
        let placed_audio = p
            .all_tracks()
            .find_map(|t| t.find_clip(audio_clip_id))
            .unwrap();
        assert!(matches!(placed_audio, Clip::Audio(m) if m.media_id == media_id));
        // The unrelated clip is untouched.
        assert!(p.find_track(v1).unwrap().find_clip(keep).is_some());
        assert!(p.find_track(v1).unwrap().find_clip(c1).is_none());
    }

    #[test]
    fn nest_clips_errors_leave_the_project_untouched() {
        let mut p = project();
        let v = video_media(&mut p, 20.0);
        let (seq, _) = create_sequence(&mut p, "S");
        let inner = track(&mut p, TrackKind::Video, Some(seq));
        let ic = clip(&mut p, inner, v, 0.0, 2.0);
        let root = track(&mut p, TrackKind::Video, None);
        let rc = clip(&mut p, root, v, 0.0, 2.0);
        let cap = track(&mut p, TrackKind::Caption, None);
        let cc = match apply_command(
            &mut p,
            Command::AddCaption {
                track_id: cap,
                body: crate::commands::BodyInput::text("hi"),
                start: Time::Us(0),
                duration: Time::Us(1000000),
                style: Some(crate::project::TextStyleRef::builtin("default")),
                animations: None,
            },
        )
        .unwrap()
        {
            CommandOutcome::ClipAdded { clip_id } => clip_id,
            _ => panic!(),
        };
        let sequences_before = p.sequences.len();
        let media_before = p.media.len();

        // Empty.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::NestClips {
                    clips: vec![],
                    name: "n".into()
                }
            ),
            Err(CommandError::InvalidNest(_))
        ));
        // Clips from two timelines.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::NestClips {
                    clips: vec![cref(root, rc), cref(inner, ic)],
                    name: "n".into()
                }
            ),
            Err(CommandError::InvalidNest(_))
        ));
        // Caption clips are not supported inside sequences.
        assert!(matches!(
            apply_command(
                &mut p,
                Command::NestClips {
                    clips: vec![cref(root, rc), cref(cap, cc)],
                    name: "n".into()
                }
            ),
            Err(CommandError::UnsupportedForSequence(_))
        ));
        // Unknown clip.
        let missing = Id::new_v4();
        assert!(matches!(
            apply_command(
                &mut p,
                Command::NestClips {
                    clips: vec![cref(root, missing)],
                    name: "n".into()
                }
            ),
            Err(CommandError::ClipNotFound(c, t)) if c == missing && t == root
        ));
        assert_eq!(p.sequences.len(), sequences_before);
        assert_eq!(p.media.len(), media_before);
        assert!(p.find_track(root).unwrap().find_clip(rc).is_some());
    }

    #[test]
    fn nest_clips_places_the_replacement_on_a_new_track_when_the_source_is_blocked() {
        let mut p = project();
        let v = video_media(&mut p, 20.0);
        let t = track(&mut p, TrackKind::Video, None);
        let first = clip(&mut p, t, v, 0.0, 1.0);
        let between = clip(&mut p, t, v, 2.0, 1.0);
        let last = clip(&mut p, t, v, 4.0, 1.0);
        // Nest the outer two; the clip between them blocks the replacement's span.
        let out = apply_command(
            &mut p,
            Command::NestClips {
                clips: vec![cref(t, first), cref(t, last)],
                name: "n".into(),
            },
        )
        .unwrap();
        let CommandOutcome::ClipsNested { video_clip_id, .. } = out else {
            panic!()
        };
        assert_eq!(p.tracks.len(), 2, "a new track holds the replacement");
        assert!(p.tracks[0].find_clip(between).is_some());
        assert!(p.tracks[1].find_clip(video_clip_id.unwrap()).is_some());
    }

    #[test]
    fn unnest_clip_is_the_inverse_of_nest_for_untouched_clips() {
        let mut p = project();
        let v = video_media(&mut p, 20.0);
        let t = track(&mut p, TrackKind::Video, None);
        let c = clip(&mut p, t, v, 3.0, 2.0);
        let CommandOutcome::ClipsNested {
            sequence_id,
            video_clip_id,
            ..
        } = apply_command(
            &mut p,
            Command::NestClips {
                clips: vec![cref(t, c)],
                name: "n".into(),
            },
        )
        .unwrap()
        else {
            panic!()
        };
        let outer = video_clip_id.unwrap();

        apply_command(
            &mut p,
            Command::UnnestClip {
                track_id: t,
                clip_id: outer,
            },
        )
        .unwrap();
        // The sequence is kept; its clips were copied back at their original time on a new
        // track, and the placing clip is gone.
        assert!(p.find_sequence(sequence_id).is_some());
        assert!(p.find_track(t).unwrap().find_clip(outer).is_none());
        assert_eq!(p.tracks.len(), 2);
        let restored = &p.tracks[1].clips;
        assert_eq!(restored.len(), 1);
        assert!((restored[0].start_us() - 3000000).abs() < 1);
        assert!((restored[0].duration_us() - 2000000).abs() < 1);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn unnest_clip_names_the_reason_when_it_cannot() {
        let mut p = project();
        let v = video_media(&mut p, 20.0);
        let t = track(&mut p, TrackKind::Video, None);
        let c = clip(&mut p, t, v, 0.0, 4.0);
        let CommandOutcome::ClipsNested { video_clip_id, .. } = apply_command(
            &mut p,
            Command::NestClips {
                clips: vec![cref(t, c)],
                name: "n".into(),
            },
        )
        .unwrap() else {
            panic!()
        };
        let outer = video_clip_id.unwrap();
        let before = p.tracks.len();

        // A plain clip is not a nested clip.
        let plain = clip(&mut p, t, v, 10.0, 1.0);
        let err = apply_command(
            &mut p,
            Command::UnnestClip {
                track_id: t,
                clip_id: plain,
            },
        );
        assert!(matches!(&err, Err(CommandError::CannotUnnest(r)) if r.contains("sequence")));

        apply_command(
            &mut p,
            Command::SetClipSpeed {
                track_id: t,
                clip_id: outer,
                speed: 2.0,
            },
        )
        .unwrap();
        let err = apply_command(
            &mut p,
            Command::UnnestClip {
                track_id: t,
                clip_id: outer,
            },
        );
        assert!(matches!(&err, Err(CommandError::CannotUnnest(r)) if r.contains("speed")));
        assert_eq!(p.tracks.len(), before, "a rejected unnest changes nothing");
        assert!(p.find_track(t).unwrap().find_clip(outer).is_some());
    }

    #[test]
    fn sequence_commands_round_trip_as_json() {
        let cmd: Command = serde_json::from_str(
            r#"{"command":"NestClips","clips":[{"track_id":"00000000-0000-4000-8000-000000000001","clip_id":"00000000-0000-4000-8000-000000000002"}],"name":"x"}"#,
        )
        .unwrap();
        assert!(matches!(cmd, Command::NestClips { ref clips, .. } if clips.len() == 1));
        let cmd: Command =
            serde_json::from_str(r#"{"command":"CreateSequence","name":"s"}"#).unwrap();
        assert!(matches!(
            cmd,
            Command::CreateSequence {
                width: None,
                height: None,
                id: None,
                ..
            }
        ));
        // AddTrack's new field defaults, so every existing payload keeps working.
        let cmd: Command =
            serde_json::from_str(r#"{"command":"AddTrack","kind":"video","name":"V1"}"#).unwrap();
        assert!(matches!(
            cmd,
            Command::AddTrack {
                sequence_id: None,
                ..
            }
        ));
    }
}
