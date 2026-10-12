//! Command-level tests for per-frame background-removal mattes (no model weights needed; the
//! tests that run the network live in `tests/rvm_matting.rs`).

use super::*;
use crate::project::{
    BackgroundRemoval, MatteFrames, MediaItem, MediaKind, Project, Settings, TrackKind,
};
use crate::segmentation::{MatteSidecar, MatteStatus, SIDECAR_FILE};
use std::path::{Path, PathBuf};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("renderly-matte-cmd-{tag}-{}", Id::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A project with one video clip (timeline 5..10 s showing source 2..7 s) of a fake source file.
fn project_with_clip(source: &Path) -> (Project, Id, Id) {
    let mut project = Project::new("t", Settings::default());
    let media_id = Id::new_v4();
    project.media.push(MediaItem {
        id: media_id,
        path: source.to_path_buf(),
        kind: MediaKind::Video,
        duration_secs: Some(20.0),
        width: Some(1280),
        height: Some(720),
        fps: Some(30.0),
        sequence_id: None,
    });
    let CommandOutcome::TrackAdded { track_id } = apply_command(
        &mut project,
        Command::AddTrack {
            kind: TrackKind::Video,
            name: "V".into(),
            id: None,
            sequence_id: None,
        },
    )
    .unwrap() else {
        panic!("expected TrackAdded");
    };
    let CommandOutcome::ClipAdded { clip_id } = apply_command(
        &mut project,
        Command::AddClip {
            track_id,
            media_id,
            position_secs: 5.0,
            source_in_secs: 2.0,
            source_out_secs: 7.0,
        },
    )
    .unwrap() else {
        panic!("expected ClipAdded");
    };
    (project, track_id, clip_id)
}

fn rvm_config() -> BackgroundRemoval {
    BackgroundRemoval {
        enabled: true,
        model_id: "rvm".into(),
        threshold: 0.15,
        feather: 0.0,
        temporal: false,
        matte_cache_dir: None,
        matte_frames: None,
    }
}

fn clip_bg(project: &Project, track: Id, clip: Id) -> Option<BackgroundRemoval> {
    project
        .find_track(track)?
        .clips
        .iter()
        .find(|c| c.id() == clip)?
        .as_media()?
        .background_removal
        .clone()
}

fn write_sidecar(dir: &Path, source: &Path, start: f64, end: f64) -> MatteSidecar {
    let meta = std::fs::metadata(source).unwrap();
    let sidecar = MatteSidecar {
        version: 1,
        model_id: crate::ml::RVM_MODEL_ID.into(),
        model_sha256: "test".into(),
        source_path: source.to_string_lossy().into_owned(),
        source_mtime_ms: meta
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
        source_size: meta.len(),
        range_start_secs: start,
        range_end_secs: end,
        fps: 30.0,
        frame_count: ((end - start) * 30.0).round() as u32 + 1,
        width: 64,
        height: 36,
        stamp: 42,
    };
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join(SIDECAR_FILE),
        serde_json::to_string(&sidecar).unwrap(),
    )
    .unwrap();
    std::fs::write(dir.join("matte.png"), b"png").unwrap();
    sidecar
}

#[test]
fn rvm_without_the_model_is_a_typed_error_and_changes_nothing() {
    // Point the model lookup at an empty directory. If this machine has the weights installed
    // in the real model cache the command would legitimately proceed, so skip then.
    let spec = crate::ml::models::spec(crate::ml::RVM_MODEL_ID).unwrap();
    if crate::ml::models::resolve_in(spec, None, &crate::ml::model_cache_dir()).is_ok() {
        eprintln!("skipping: RVM weights are installed in the model cache");
        return;
    }
    let dir = tmp("nomodel");
    let source = dir.join("src.mp4");
    std::fs::write(&source, b"not really a video").unwrap();
    let (mut project, track, clip) = project_with_clip(&source);
    apply_command(
        &mut project,
        Command::SetClipBackgroundRemoval {
            track_id: track,
            clip_id: clip,
            config: Some(rvm_config()),
        },
    )
    .unwrap();

    let empty = dir.join("empty-models");
    std::fs::create_dir_all(&empty).unwrap();
    let previous = std::env::var_os(crate::ml::DEFAULT_MODEL_DIR_ENV);
    std::env::set_var(crate::ml::DEFAULT_MODEL_DIR_ENV, &empty);
    let result = apply_command(
        &mut project,
        Command::GenerateBackgroundMatte {
            track_id: track,
            clip_id: clip,
        },
    );
    match previous {
        Some(v) => std::env::set_var(crate::ml::DEFAULT_MODEL_DIR_ENV, v),
        None => std::env::remove_var(crate::ml::DEFAULT_MODEL_DIR_ENV),
    }

    match result {
        Err(CommandError::ModelNotAvailable { model_id }) => {
            assert_eq!(model_id, crate::ml::RVM_MODEL_ID);
        }
        other => panic!("expected ModelNotAvailable, got {other:?}"),
    }
    let after = clip_bg(&project, track, clip).unwrap();
    assert_eq!(after.matte_cache_dir, None);
    assert_eq!(after.matte_frames, None);
    assert!(!crate::segmentation::rvm_cache_dir(
        project.tracks[0].clips[0].as_media().unwrap().media_id,
        clip
    )
    .exists());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn attach_requires_a_fresh_sidecar_and_wires_the_per_frame_index() {
    let dir = tmp("attach");
    let source = dir.join("src.mp4");
    std::fs::write(&source, b"source bytes").unwrap();
    let (mut project, track, clip) = project_with_clip(&source);
    apply_command(
        &mut project,
        Command::SetClipBackgroundRemoval {
            track_id: track,
            clip_id: clip,
            config: Some(rvm_config()),
        },
    )
    .unwrap();
    let cache = dir.join("matte-cache");

    // No sequence yet.
    let err = apply_command(
        &mut project,
        Command::AttachBackgroundMatte {
            track_id: track,
            clip_id: clip,
            cache_dir: cache.clone(),
        },
    )
    .unwrap_err();
    assert!(matches!(err, CommandError::Analysis(_)), "{err}");

    // A sequence that does not cover the clip's used source range (2..7 s) is refused.
    write_sidecar(&cache, &source, 2.0, 4.0);
    let err = apply_command(
        &mut project,
        Command::AttachBackgroundMatte {
            track_id: track,
            clip_id: clip,
            cache_dir: cache.clone(),
        },
    )
    .unwrap_err();
    assert!(err.to_string().contains("outside"), "{err}");

    // A covering one is accepted and wired: index + Generated mask.
    let sidecar = write_sidecar(&cache, &source, 2.0, 7.0);
    apply_command(
        &mut project,
        Command::AttachBackgroundMatte {
            track_id: track,
            clip_id: clip,
            cache_dir: cache.clone(),
        },
    )
    .unwrap();
    let bg = clip_bg(&project, track, clip).unwrap();
    assert_eq!(bg.matte_cache_dir.as_deref(), Some(cache.as_path()));
    assert_eq!(bg.matte_frames, Some(sidecar.frames()));
    let mc = project.tracks[0].clips[0].as_media().unwrap();
    assert!(matches!(
        mc.mask.as_ref().map(|m| &m.kind),
        Some(ClipMaskKind::Generated { cache_dir }) if *cache_dir == cache
    ));
    assert_eq!(
        crate::segmentation::matte_status(&project, mc),
        MatteStatus::Ready { frames: Some(151) }
    );

    // Timeline -> source -> matte frame: at t=6 s the clip shows source 3 s = matte frame 30.
    let reqs = crate::mask::frame_matte_requests_at(&project, 6.0);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].index, 30);
    assert_eq!(reqs[0].stamp, 42);
    assert_eq!(reqs[0].path, cache.join(MatteFrames::file_name(30)));
    assert_eq!(reqs[0].key, format!("gen:{}", cache.display()));
    // Host-side scoping: only frame files of a sequence this project uses may be served.
    assert!(crate::mask::project_has_frame_mattes(&project));
    assert!(crate::mask::is_frame_matte_path(
        &project,
        &cache.join("f000123.png")
    ));
    assert!(!crate::mask::is_frame_matte_path(
        &project,
        &cache.join("matte.json")
    ));
    assert!(!crate::mask::is_frame_matte_path(
        &project,
        &dir.join("other").join("f000123.png")
    ));
    assert!(!crate::mask::is_frame_matte_path(
        &project,
        &cache.join("f12.png")
    ));

    // Outside the clip nothing is requested; the end clamps to the last frame.
    assert!(crate::mask::frame_matte_requests_at(&project, 1.0).is_empty());
    assert_eq!(
        crate::mask::frame_matte_requests_at(&project, 9.999)[0].index,
        150
    );

    // Staleness: the source changing on disk, or the trim growing past the baked range.
    std::fs::write(&source, b"source bytes, edited").unwrap();
    let mc = project.tracks[0].clips[0].as_media().unwrap();
    assert!(matches!(
        crate::segmentation::matte_status(&project, mc),
        MatteStatus::Stale { ref reason } if reason.contains("changed")
    ));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn status_for_clips_without_a_matte_and_legacy_configs() {
    let dir = tmp("status");
    let source = dir.join("src.mp4");
    std::fs::write(&source, b"x").unwrap();
    let (mut project, track, clip) = project_with_clip(&source);
    let mc = project.tracks[0].clips[0].as_media().unwrap();
    assert_eq!(
        crate::segmentation::matte_status(&project, mc),
        MatteStatus::NotGenerated
    );
    apply_command(
        &mut project,
        Command::SetClipBackgroundRemoval {
            track_id: track,
            clip_id: clip,
            config: Some(rvm_config()),
        },
    )
    .unwrap();
    let mc = project.tracks[0].clips[0].as_media().unwrap();
    assert_eq!(
        crate::segmentation::matte_status(&project, mc),
        MatteStatus::NotGenerated
    );

    // A v7 project saved before `matte_frames` existed still loads (additive optional field).
    let legacy: BackgroundRemoval = serde_json::from_str(
        r#"{"enabled":true,"model_id":"heuristic","threshold":0.2,"feather":0.1,"temporal":false,"matte_cache_dir":"c"}"#,
    )
    .unwrap();
    assert_eq!(legacy.matte_frames, None);
    // ... and a config without a frame index does not serialise the field at all.
    assert!(!serde_json::to_string(&legacy)
        .unwrap()
        .contains("matte_frames"));
    std::fs::remove_dir_all(&dir).ok();
}
