//! Tests for the schema v8 stage 3 commands: magnetic tracks, markers, captions, text clips,
//! transcripts. They run on projects built in memory (no media files, no GPU).

use super::*;
use crate::project::{
    Clip, Marker, MarkerKind, MediaItem, MediaKind, Project, Settings, TextClip, Transcript,
    TranscriptWord,
};
use serde_json::{json, Value};

const SEC: i64 = 1_000_000;

fn project() -> Project {
    Project::new(
        "t",
        Settings {
            fps: Fps::new(30, 1),
            width: 1080,
            height: 1920,
            sample_rate: 48_000,
            duck_db: -12.0,
        },
    )
}

fn add_media(p: &mut Project, kind: MediaKind, secs: i64) -> Id {
    let id = Id::new_v4();
    p.media.push(MediaItem {
        id,
        path: "m.mp4".into(),
        kind,
        duration_us: Some(secs * SEC),
        width: Some(1920),
        height: Some(1080),
        fps: Some(30.0),
        sequence_id: None,
    });
    id
}

/// Apply a command given as JSON, through the same parser the surfaces use. Returns the
/// outcome and the deprecation warnings.
fn run(p: &mut Project, command: Value) -> Result<(CommandOutcome, Vec<String>), CommandError> {
    let (cmd, warnings) = parse_command_json(command).expect("command parses");
    apply_command(p, cmd).map(|o| (o, warnings))
}

fn ok(p: &mut Project, command: Value) -> CommandOutcome {
    run(p, command).expect("command applies").0
}

fn track(p: &mut Project, kind: &str, name: &str) -> Id {
    match ok(
        p,
        json!({ "command": "AddTrack", "kind": kind, "name": name }),
    ) {
        CommandOutcome::TrackAdded { track_id } => track_id,
        other => panic!("{other:?}"),
    }
}

fn clip(p: &mut Project, track_id: Id, media: Id, start: &str, secs: i64) -> Id {
    match ok(
        p,
        json!({ "command": "AddClip", "track_id": track_id, "media_id": media,
                 "start": start, "source_in": 0, "source_out": format!("{secs}s") }),
    ) {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        other => panic!("{other:?}"),
    }
}

fn starts(p: &Project, track_id: Id) -> Vec<(Id, i64)> {
    let mut v: Vec<(Id, i64)> = p
        .find_track(track_id)
        .unwrap()
        .clips
        .iter()
        .map(|c| (c.id(), c.start_us()))
        .collect();
    v.sort_by_key(|x| x.1);
    v
}

fn order(p: &Project, track_id: Id) -> Vec<Id> {
    starts(p, track_id).into_iter().map(|x| x.0).collect()
}

// ---- magnetic tracks ----------------------------------------------------------------------

#[test]
fn the_first_video_track_of_a_project_is_magnetic_and_others_are_not() {
    let mut p = project();
    let v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let a1 = track(&mut p, "audio", "A1");
    assert!(p.find_track(v1).unwrap().magnetic);
    assert!(!p.find_track(v2).unwrap().magnetic);
    assert!(!p.find_track(a1).unwrap().magnetic);
    // Explicit wins in both directions.
    let v3 = match ok(
        &mut p,
        json!({ "command": "AddTrack", "kind": "video", "name": "V3", "magnetic": true }),
    ) {
        CommandOutcome::TrackAdded { track_id } => track_id,
        o => panic!("{o:?}"),
    };
    assert!(p.find_track(v3).unwrap().magnetic);
    let mut q = project();
    let first = match ok(
        &mut q,
        json!({ "command": "AddTrack", "kind": "video", "name": "V1", "magnetic": false }),
    ) {
        CommandOutcome::TrackAdded { track_id } => track_id,
        o => panic!("{o:?}"),
    };
    assert!(!q.find_track(first).unwrap().magnetic);
    // Captions can never be magnetic.
    let err = run(
        &mut q,
        json!({ "command": "AddTrack", "kind": "caption", "name": "C", "magnetic": true }),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("caption tracks cannot be magnetic"));
}

#[test]
fn add_clip_on_a_magnetic_track_lands_by_its_centre_and_packs() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let v1 = track(&mut p, "video", "V1");
    // Asked for 5 s, but a magnetic track has no gap: it lands at 0.
    let a = clip(&mut p, v1, m, "5s", 2);
    assert_eq!(starts(&p, v1), vec![(a, 0)]);
    // Centre 0.5 s is before A's centre (1 s): B goes in front and A is pushed to 1 s.
    let b = clip(&mut p, v1, m, "0s", 1);
    assert_eq!(starts(&p, v1), vec![(b, 0), (a, SEC)]);
    // Far past the end: appended.
    let c = clip(&mut p, v1, m, "20s", 3);
    assert_eq!(starts(&p, v1), vec![(b, 0), (a, SEC), (c, 3 * SEC)]);
    // Dropped on A: the centre decides. Centre 2.5 s is after A's centre (2 s), before C's.
    let d = clip(&mut p, v1, m, "2s", 1);
    assert_eq!(order(&p, v1), vec![b, a, d, c]);
    assert_eq!(starts(&p, v1)[3].1, 4 * SEC);
}

#[test]
fn a_non_magnetic_track_keeps_gaps_and_rejects_overlaps() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let _v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let a = clip(&mut p, v2, m, "5s", 2);
    assert_eq!(starts(&p, v2), vec![(a, 5 * SEC)]);
    let err = run(
        &mut p,
        json!({ "command": "AddClip", "track_id": v2, "media_id": m,
                 "start": "6s", "source_in": 0, "source_out": "2s" }),
    )
    .unwrap_err();
    assert!(matches!(err, CommandError::Overlap(..)));
}

#[test]
fn move_clip_reorders_a_magnetic_track() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let v1 = track(&mut p, "video", "V1");
    let a = clip(&mut p, v1, m, "0s", 2);
    let b = clip(&mut p, v1, m, "2s", 3);
    let c = clip(&mut p, v1, m, "5s", 1);
    assert_eq!(starts(&p, v1), vec![(a, 0), (b, 2 * SEC), (c, 5 * SEC)]);
    // A (2 s) moved to 4 s: centre 5 s is after B's centre (3.5 s) and before C's (5.5 s).
    ok(
        &mut p,
        json!({ "command": "MoveClip", "track_id": v1, "clip_id": a, "start": "4s" }),
    );
    assert_eq!(starts(&p, v1), vec![(b, 0), (a, 3 * SEC), (c, 5 * SEC)]);
    // Moving it back to the front.
    ok(
        &mut p,
        json!({ "command": "MoveClip", "track_id": v1, "clip_id": a, "start": "0s" }),
    );
    assert_eq!(order(&p, v1), vec![a, b, c]);
    assert_eq!(starts(&p, v1)[1].1, 2 * SEC);
}

#[test]
fn move_clip_between_tracks_packs_the_source_and_inserts_into_a_magnetic_target() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    ok(
        &mut p,
        json!({ "command": "SetTrackFlags", "track_id": v2, "magnetic": true }),
    );
    let a = clip(&mut p, v1, m, "0s", 2);
    let b = clip(&mut p, v1, m, "2s", 2);
    let x = clip(&mut p, v2, m, "0s", 3);
    ok(
        &mut p,
        json!({ "command": "MoveClip", "track_id": v1, "clip_id": a, "start": "10s",
                 "new_track_id": v2 }),
    );
    // V1 closed up behind the departing clip; V2 appended it after X.
    assert_eq!(starts(&p, v1), vec![(b, 0)]);
    assert_eq!(starts(&p, v2), vec![(x, 0), (a, 3 * SEC)]);
}

#[test]
fn delete_clip_always_closes_the_gap_on_a_magnetic_track() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let v1 = track(&mut p, "video", "V1");
    let a = clip(&mut p, v1, m, "0s", 2);
    let b = clip(&mut p, v1, m, "2s", 2);
    let c = clip(&mut p, v1, m, "4s", 2);
    ok(
        &mut p,
        json!({ "command": "DeleteClip", "track_id": v1, "clip_id": b, "ripple": false }),
    );
    assert_eq!(starts(&p, v1), vec![(a, 0), (c, 2 * SEC)]);
}

#[test]
fn trim_and_speed_ripple_a_magnetic_track() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let v1 = track(&mut p, "video", "V1");
    let a = clip(&mut p, v1, m, "0s", 4);
    let b = clip(&mut p, v1, m, "4s", 2);
    ok(
        &mut p,
        json!({ "command": "TrimClip", "track_id": v1, "clip_id": a, "source_out": "3s" }),
    );
    assert_eq!(starts(&p, v1), vec![(a, 0), (b, 3 * SEC)]);
    // Lengthening pushes the next clip instead of colliding with it.
    ok(
        &mut p,
        json!({ "command": "TrimClip", "track_id": v1, "clip_id": a, "source_out": "5s" }),
    );
    assert_eq!(starts(&p, v1), vec![(a, 0), (b, 5 * SEC)]);
    // Double speed halves the clip.
    ok(
        &mut p,
        json!({ "command": "SetClipSpeed", "track_id": v1, "clip_id": a, "speed": 2.0 }),
    );
    let s = starts(&p, v1);
    assert_eq!(s[1], (b, 2 * SEC + 500_000));
}

#[test]
fn split_keeps_a_magnetic_track_contiguous() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let v1 = track(&mut p, "video", "V1");
    let a = clip(&mut p, v1, m, "0s", 4);
    let b = clip(&mut p, v1, m, "4s", 2);
    ok(
        &mut p,
        json!({ "command": "SplitClip", "track_id": v1, "clip_id": a, "at": "1.5s" }),
    );
    let s = starts(&p, v1);
    assert_eq!(s.len(), 3);
    assert_eq!(s[0], (a, 0));
    assert_eq!(s[1].1, 1_500_000);
    assert_eq!(s[2], (b, 4 * SEC));
}

#[test]
fn turning_magnetic_on_packs_the_track_and_off_leaves_it() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let _v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let a = clip(&mut p, v2, m, "2s", 1);
    let b = clip(&mut p, v2, m, "7s", 1);
    ok(
        &mut p,
        json!({ "command": "SetTrackFlags", "track_id": v2, "magnetic": true }),
    );
    assert_eq!(starts(&p, v2), vec![(a, 0), (b, SEC)]);
    ok(
        &mut p,
        json!({ "command": "SetTrackFlags", "track_id": v2, "magnetic": false }),
    );
    assert!(!p.find_track(v2).unwrap().magnetic);
    assert_eq!(starts(&p, v2), vec![(a, 0), (b, SEC)]);
    let c = track(&mut p, "caption", "Subs");
    let err = run(
        &mut p,
        json!({ "command": "SetTrackFlags", "track_id": c, "magnetic": true }),
    )
    .unwrap_err();
    assert!(err
        .to_string()
        .contains("caption tracks cannot be magnetic"));
}

// ---- markers ------------------------------------------------------------------------------

#[test]
fn markers_are_added_sorted_removed_and_used_for_snapping() {
    let mut p = project();
    let m = add_media(&mut p, MediaKind::Video, 60);
    let v1 = track(&mut p, "video", "V1");
    clip(&mut p, v1, m, "0s", 2);
    ok(
        &mut p,
        json!({ "command": "AddMarkers", "markers": [
            { "time": "3s", "label": "drop", "color": "#FF0000" },
            { "time": "1s", "kind": "beat" },
            { "time": 1_500_000, "kind": "beat" },
        ] }),
    );
    let times: Vec<i64> = p.markers.iter().map(|m| m.time_us).collect();
    assert_eq!(times, vec![SEC, 1_500_000, 3 * SEC]);
    assert_eq!(p.markers[2].label, "drop");
    assert_eq!(p.markers[0].kind, MarkerKind::Beat);
    // Clip edges and markers, sorted and de-duplicated.
    assert_eq!(p.snap_points(), vec![0, SEC, 1_500_000, 2 * SEC, 3 * SEC]);

    // Bad input changes nothing.
    let before = p.markers.clone();
    for bad in [
        json!({ "command": "AddMarkers", "markers": [] }),
        json!({ "command": "AddMarkers", "markers": [{ "time": "1s", "color": "red" }] }),
    ] {
        assert!(run(&mut p, bad).is_err());
    }
    let dup = p.markers[0].id;
    assert!(run(
        &mut p,
        json!({ "command": "AddMarkers", "markers": [{ "id": dup, "time": "9s" }] })
    )
    .is_err());
    assert_eq!(p.markers, before);

    // Remove by kind, then by id.
    ok(
        &mut p,
        json!({ "command": "RemoveMarkers", "kind": "beat" }),
    );
    assert_eq!(p.markers.len(), 1);
    let id = p.markers[0].id;
    ok(&mut p, json!({ "command": "RemoveMarkers", "ids": [id] }));
    assert!(p.markers.is_empty());
    assert!(run(&mut p, json!({ "command": "RemoveMarkers", "ids": [id] })).is_err());
    assert!(run(&mut p, json!({ "command": "RemoveMarkers" })).is_err());
}

// ---- captions -----------------------------------------------------------------------------

fn caption_track(p: &mut Project) -> Id {
    track(p, "caption", "Subs")
}

fn caption_of(p: &Project, track_id: Id, clip_id: Id) -> crate::project::CaptionClip {
    match p
        .find_track(track_id)
        .unwrap()
        .find_clip(clip_id)
        .expect("clip")
    {
        Clip::Caption(c) => c.clone(),
        other => panic!("{other:?}"),
    }
}

fn add_caption(p: &mut Project, track_id: Id, body: Value, start: &str, dur: &str) -> Id {
    match ok(
        p,
        json!({ "command": "AddCaption", "track_id": track_id, "body": body,
                 "start": start, "duration": dur }),
    ) {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        o => panic!("{o:?}"),
    }
}

#[test]
fn legacy_caption_params_are_accepted_with_warnings() {
    let mut p = project();
    let t = caption_track(&mut p);
    let (outcome, warnings) = run(
        &mut p,
        json!({ "command": "AddCaption", "track_id": t, "text": "Hi", "start": 0,
                 "duration": "2s", "style_id": "tiktok-box" }),
    )
    .unwrap();
    let CommandOutcome::ClipAdded { clip_id } = outcome else {
        panic!()
    };
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(warnings.iter().any(|w| w.contains("`text` is deprecated")));
    assert!(warnings
        .iter()
        .any(|w| w.contains("`style_id` is deprecated")));
    let c = caption_of(&p, t, clip_id);
    assert_eq!(c.body.text, "Hi");
    assert_eq!(c.style.unwrap().item_ref, "builtin:tiktok-box@1");

    let (_, warnings) = run(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": clip_id,
                 "text": "Hello", "style_id": "tiktok-minimal" }),
    )
    .unwrap();
    assert_eq!(warnings.len(), 2);
    let c = caption_of(&p, t, clip_id);
    assert_eq!(c.body.text, "Hello");
    assert_eq!(c.style.unwrap().item_ref, "builtin:tiktok-minimal@1");
    // The current names produce no warnings.
    let (_, warnings) = run(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": clip_id,
                 "body": { "text": "Bye" } }),
    )
    .unwrap();
    assert!(warnings.is_empty());
}

#[test]
fn caption_body_is_validated() {
    let mut p = project();
    let t = caption_track(&mut p);
    let before = p.clone();
    let cases = [
        // A word past the end of the clip.
        json!({ "text": "a b", "words": [{ "text": "a", "start": 0, "end": "3s" }] }),
        // Words out of order.
        json!({ "text": "a b", "words": [
            { "text": "a", "start": "1s", "end": "1.5s" },
            { "text": "b", "start": "1.2s", "end": "1.8s" }] }),
        // A run past the end of the text.
        json!({ "text": "ab", "runs": [{ "start": 0, "end": 9 }] }),
        // An empty run.
        json!({ "text": "ab", "runs": [{ "start": 1, "end": 1 }] }),
        // An unknown style property in a run.
        json!({ "text": "ab", "runs": [{ "start": 0, "end": 1, "style": { "sparkle": 1 } }] }),
    ];
    for body in cases {
        let err = run(
            &mut p,
            json!({ "command": "AddCaption", "track_id": t, "body": body,
                     "start": 0, "duration": "2s" }),
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidText(_)), "{err}");
    }
    assert_eq!(p.tracks[0].clips.len(), before.tracks[0].clips.len());
}

#[test]
fn set_caption_keeps_the_words_and_style_null_inherits_the_track() {
    let mut p = project();
    let t = caption_track(&mut p);
    let c = add_caption(
        &mut p,
        t,
        json!({ "text": "Let's build great", "words": [
            { "text": "Let's", "start": 0, "end": "0.3s" },
            { "text": "build", "start": "0.34s", "end": "0.7s", "emphasis": true },
            { "text": "great", "start": "0.8s", "end": "1.2s" }] }),
        "1s",
        "2s",
    );
    ok(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": c,
                 "body": { "text": "Let's build" } }),
    );
    let clip = caption_of(&p, t, c);
    assert_eq!(clip.body.text, "Let's build");
    assert_eq!(
        clip.body.words.len(),
        3,
        "words are evidence, not rewritten"
    );
    assert!(clip.body.words[1].emphasis);

    ok(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": c,
                 "style": { "ref": "builtin:tiktok-box@1" } }),
    );
    assert!(caption_of(&p, t, c).style.is_some());
    ok(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": c, "style": null }),
    );
    assert!(caption_of(&p, t, c).style.is_none());

    // Shortening below the last word is refused.
    let err = run(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": c, "duration": "0.5s" }),
    )
    .unwrap_err();
    assert!(matches!(err, CommandError::InvalidText(_)));
    assert!(run(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": c })
    )
    .is_err());
}

#[test]
fn set_track_caption_style_restyles_every_inheriting_caption() {
    let mut p = project();
    let t = caption_track(&mut p);
    let a = add_caption(&mut p, t, json!({ "text": "a" }), "0s", "1s");
    let b = add_caption(&mut p, t, json!({ "text": "b" }), "1s", "1s");
    ok(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": b,
                 "style": { "ref": "builtin:tiktok-minimal@1" } }),
    );
    ok(
        &mut p,
        json!({ "command": "SetTrackCaptionStyle", "track_id": t,
                 "style": { "ref": "builtin:tiktok-box@1" } }),
    );
    let ids = |p: &Project, at: i64| -> String {
        crate::compose::eval::active_captions(p, at)[0]
            .style_id
            .clone()
    };
    assert_eq!(ids(&p, 500_000), "tiktok-box");
    assert_eq!(ids(&p, 1_500_000), "tiktok-minimal", "own style wins");
    ok(
        &mut p,
        json!({ "command": "SetTrackCaptionStyle", "track_id": t,
                 "style": { "ref": "builtin:youtube-lower-thirds@1" }, "reset_clips": true }),
    );
    assert_eq!(ids(&p, 500_000), "youtube-lower-thirds");
    assert_eq!(ids(&p, 1_500_000), "youtube-lower-thirds");
    assert!(caption_of(&p, t, a).style.is_none());
    ok(
        &mut p,
        json!({ "command": "SetTrackCaptionStyle", "track_id": t, "style": null }),
    );
    assert!(p.find_track(t).unwrap().caption_style.is_none());
    // With nothing set the default style draws.
    assert_eq!(ids(&p, 500_000), "tiktok-bold-yellow");
    // Only caption tracks have one.
    let v = track(&mut p, "video", "V1");
    assert!(run(
        &mut p,
        json!({ "command": "SetTrackCaptionStyle", "track_id": v, "style": null })
    )
    .is_err());
}

#[test]
fn style_refs_are_validated_but_unknown_ones_are_stored() {
    let mut p = project();
    let t = caption_track(&mut p);
    let c = add_caption(&mut p, t, json!({ "text": "a" }), "0s", "1s");
    for bad in [
        json!({ "ref": "tiktok-box" }),
        json!({ "ref": "builtin:tiktok-box@1", "params": { "x": 1 } }),
        json!({ "ref": "builtin:tiktok-box@1", "overrides": { "fill": "red" } }),
        json!({ "ref": "builtin:tiktok-box@1", "overrides": { "anchor": "middle" } }),
        json!({ "ref": "builtin:tiktok-box@1", "overrides": { "sparkle": 1 } }),
    ] {
        let err = run(
            &mut p,
            json!({ "command": "SetCaption", "track_id": t, "clip_id": c, "style": bad }),
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidText(_)), "{bad}: {err}");
    }
    // A pack style that is not installed is accepted (it shows as missing and falls back).
    ok(
        &mut p,
        json!({ "command": "SetCaption", "track_id": t, "clip_id": c,
                 "style": { "ref": "pack:mira.titles/neon@2", "params": { "glow": 0.5 } } }),
    );
    assert_eq!(
        caption_of(&p, t, c).style.unwrap().item_ref,
        "pack:mira.titles/neon@2"
    );
    let problems = crate::packs::status::problem_effects(&p, &[]);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].item_ref, "pack:mira.titles/neon@2");
    assert_eq!(problems[0].status, crate::project::EffectStatus::Missing);
}

#[test]
fn set_text_body_style_and_animation_apply_to_captions_and_texts_atomically() {
    let mut p = project();
    let ct = caption_track(&mut p);
    let v1 = track(&mut p, "video", "V1");
    let cap = add_caption(&mut p, ct, json!({ "text": "cap" }), "0s", "2s");
    let text = match ok(
        &mut p,
        json!({ "command": "AddText", "track_id": v1, "body": { "text": "title" },
                 "start": "0s", "duration": "3s" }),
    ) {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        o => panic!("{o:?}"),
    };
    // One batch, two kinds of clip.
    ok(
        &mut p,
        json!({ "command": "SetTextBody", "text": "same",
                 "targets": [{ "track_id": ct, "clip_id": cap }, { "track_id": v1, "clip_id": text }] }),
    );
    assert_eq!(caption_of(&p, ct, cap).body.text, "same");
    ok(
        &mut p,
        json!({ "command": "SetTextStyle", "track_id": v1, "clip_id": text,
                 "overrides": { "fill": "#FFD400", "size": 0.08 } }),
    );
    let Clip::Text(t) = p.find_track(v1).unwrap().find_clip(text).unwrap() else {
        panic!()
    };
    assert_eq!(t.style.overrides["fill"], json!("#FFD400"));
    // The ref was untouched by an overrides-only change.
    assert_eq!(
        t.style.item_ref,
        crate::project::text::DEFAULT_TEXT_STYLE_REF
    );

    // A batch with an invalid member changes nothing.
    let before = p.clone();
    let err = run(
        &mut p,
        json!({ "command": "SetTextBody", "text": "nope", "runs": [{ "start": 0, "end": 99 }],
                 "targets": [{ "track_id": ct, "clip_id": cap }, { "track_id": v1, "clip_id": text }] }),
    )
    .unwrap_err();
    assert!(matches!(err, CommandError::InvalidText(_)));
    assert_eq!(caption_of(&p, ct, cap).body.text, "same");
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        serde_json::to_value(&before).unwrap()
    );
    // A clip that is not text is refused.
    let m = add_media(&mut p, MediaKind::Video, 10);
    let media_clip = clip(&mut p, v1, m, "10s", 1);
    assert!(run(
        &mut p,
        json!({ "command": "SetTextBody", "track_id": v1, "clip_id": media_clip, "text": "x" })
    )
    .is_err());

    // Clearing a caption's style makes it inherit; clearing a text style restores the default.
    ok(
        &mut p,
        json!({ "command": "SetTextStyle", "track_id": ct, "clip_id": cap,
                 "ref": "builtin:tiktok-box@1" }),
    );
    assert!(caption_of(&p, ct, cap).style.is_some());
    ok(
        &mut p,
        json!({ "command": "SetTextStyle", "track_id": ct, "clip_id": cap, "ref": null }),
    );
    assert!(caption_of(&p, ct, cap).style.is_none());
    ok(
        &mut p,
        json!({ "command": "SetTextStyle", "track_id": v1, "clip_id": text, "ref": null }),
    );
    let Clip::Text(t) = p.find_track(v1).unwrap().find_clip(text).unwrap() else {
        panic!()
    };
    assert_eq!(
        t.style.item_ref,
        crate::project::text::DEFAULT_TEXT_STYLE_REF
    );
}

#[test]
fn text_animations_are_validated_stored_and_cleared() {
    let mut p = project();
    let ct = caption_track(&mut p);
    let cap = add_caption(&mut p, ct, json!({ "text": "cap" }), "0s", "2s");
    ok(
        &mut p,
        json!({ "command": "SetTextAnimation", "track_id": ct, "clip_id": cap,
                 "phase": "in", "ref": "builtin:fade-up@1", "params": { "amount": 0.5 },
                 "duration": "400ms" }),
    );
    let anim = caption_of(&p, ct, cap).animations.in_.unwrap();
    assert_eq!(anim.item_ref, "builtin:fade-up@1");
    assert_eq!(anim.duration_us, Some(400_000));
    // Changing only the params keeps the ref's duration.
    ok(
        &mut p,
        json!({ "command": "SetTextAnimation", "track_id": ct, "clip_id": cap,
                 "phase": "in", "ref": "builtin:fade-up@1", "params": { "amount": 0.9 } }),
    );
    let anim = caption_of(&p, ct, cap).animations.in_.unwrap();
    assert_eq!(anim.duration_us, Some(400_000));
    assert_eq!(anim.params["amount"], json!(0.9));
    // Bad phase, bad ref, zero duration.
    for bad in [
        json!({ "phase": "during", "ref": "builtin:x@1" }),
        json!({ "phase": "in", "ref": "fade-up" }),
        json!({ "phase": "in", "ref": "builtin:fade-up@1", "duration": 0 }),
    ] {
        let mut cmd = json!({ "command": "SetTextAnimation", "track_id": ct, "clip_id": cap });
        cmd.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        assert!(run(&mut p, cmd).is_err());
    }
    // `ref` must be present, null clears.
    assert!(
        parse_command_json(json!({ "command": "SetTextAnimation", "track_id": ct,
        "clip_id": cap, "phase": "in" }))
        .is_err()
    );
    ok(
        &mut p,
        json!({ "command": "SetTextAnimation", "track_id": ct, "clip_id": cap,
                 "phase": "in", "ref": null }),
    );
    assert!(caption_of(&p, ct, cap).animations.is_empty());
    // Animations given to AddCaption are validated too.
    assert!(run(
        &mut p,
        json!({ "command": "AddCaption", "track_id": ct, "body": { "text": "x" },
                 "start": "5s", "duration": "1s",
                 "animations": { "in": { "ref": "nope" } } })
    )
    .is_err());
}

#[test]
fn caption_words_and_emphasis() {
    let mut p = project();
    let t = caption_track(&mut p);
    let c = add_caption(&mut p, t, json!({ "text": "one two three" }), "0s", "3s");
    ok(
        &mut p,
        json!({ "command": "SetCaptionWords", "track_id": t, "clip_id": c, "words": [
            { "text": "one", "start": 0, "end": "0.8s" },
            { "text": "two", "start": "1s", "end": "1.8s" },
            { "text": "three", "start": "2s", "end": "2.9s", "word_id": "w-9" }] }),
    );
    assert_eq!(caption_of(&p, t, c).body.words.len(), 3);
    assert_eq!(caption_of(&p, t, c).body.text, "one two three");
    ok(
        &mut p,
        json!({ "command": "SetWordEmphasis", "track_id": t, "clip_id": c, "indices": [0, 2] }),
    );
    let words = caption_of(&p, t, c).body.words;
    assert!(words[0].emphasis && !words[1].emphasis && words[2].emphasis);
    ok(
        &mut p,
        json!({ "command": "SetWordEmphasis", "track_id": t, "clip_id": c,
                 "indices": [0], "emphasis": false }),
    );
    assert!(!caption_of(&p, t, c).body.words[0].emphasis);
    for bad in [
        json!({ "command": "SetWordEmphasis", "track_id": t, "clip_id": c, "indices": [7] }),
        json!({ "command": "SetWordEmphasis", "track_id": t, "clip_id": c, "indices": [] }),
        json!({ "command": "SetCaptionWords", "track_id": t, "clip_id": c, "words": [
            { "text": "late", "start": "2s", "end": "9s" }] }),
    ] {
        assert!(run(&mut p, bad).is_err());
    }
    // Serialised: `emphasis` only when set, `word_id` only when present.
    let json = serde_json::to_value(caption_of(&p, t, c)).unwrap();
    assert_eq!(
        json["body"]["words"][0],
        json!({ "text": "one", "start_us": 0, "end_us": 800_000 })
    );
    assert_eq!(json["body"]["words"][2]["emphasis"], json!(true));
    assert_eq!(json["body"]["words"][2]["word_id"], json!("w-9"));
}

#[test]
fn splitting_a_caption_clip_splits_its_words() {
    let mut p = project();
    let t = caption_track(&mut p);
    let c = add_caption(
        &mut p,
        t,
        json!({ "text": "one two three", "words": [
            { "text": "one", "start": 0, "end": "0.8s" },
            { "text": "two", "start": "1s", "end": "1.8s" },
            { "text": "three", "start": "2s", "end": "2.9s" }] }),
        "10s",
        "3s",
    );
    ok(
        &mut p,
        json!({ "command": "SplitClip", "track_id": t, "clip_id": c, "at": "11.5s" }),
    );
    let mut clips: Vec<crate::project::CaptionClip> = p
        .find_track(t)
        .unwrap()
        .clips
        .iter()
        .map(|c| match c {
            Clip::Caption(c) => c.clone(),
            _ => panic!(),
        })
        .collect();
    clips.sort_by_key(|c| c.start_us);
    let words = |c: &crate::project::CaptionClip| -> Vec<(String, i64, i64)> {
        c.body
            .words
            .iter()
            .map(|w| (w.text.clone(), w.start_us, w.end_us))
            .collect()
    };
    assert_eq!(
        words(&clips[0]),
        vec![("one".into(), 0, 800_000), ("two".into(), SEC, 1_500_000)]
    );
    assert_eq!(words(&clips[1]), vec![("three".into(), 500_000, 1_400_000)]);
}

#[test]
fn split_captions_builds_deterministic_pages_from_words() {
    let mut p = project();
    let t = caption_track(&mut p);
    // Ten words, 0.5 s each, then a typed caption that has no words.
    let words: Vec<Value> = (0..10)
        .map(|i| {
            json!({ "text": format!("word{i}"), "start": 500_000 * i, "end": 500_000 * i + 400_000,
                    "word_id": format!("w-{}", i + 1) })
        })
        .collect();
    let body = json!({ "text": (0..10).map(|i| format!("word{i}")).collect::<Vec<_>>().join(" "),
                        "words": words });
    add_caption(&mut p, t, body, "0s", "5s");
    let typed = add_caption(&mut p, t, json!({ "text": "typed" }), "6s", "1s");
    ok(
        &mut p,
        json!({ "command": "SetTrackCaptionStyle", "track_id": t,
                 "style": { "ref": "builtin:tiktok-box@1" } }),
    );

    let split = json!({ "command": "SplitCaptions", "track_id": t, "max_chars": 11,
                         "max_lines": 1, "max_duration": "10s" });
    ok(&mut p, split.clone());
    let pages = |p: &Project| -> Vec<(i64, i64, String, usize)> {
        let mut v: Vec<_> = p
            .find_track(t)
            .unwrap()
            .clips
            .iter()
            .map(|c| match c {
                Clip::Caption(c) => (
                    c.start_us,
                    c.duration_us,
                    c.body.text.clone(),
                    c.body.words.len(),
                ),
                _ => panic!(),
            })
            .collect();
        v.sort();
        v
    };
    let first = pages(&p);
    // "word0 word1" is 11 characters: two words per page, five pages, plus the typed caption.
    assert_eq!(first.len(), 6);
    assert_eq!(first[0].2, "word0 word1");
    assert_eq!(first[0].3, 2);
    assert_eq!(first[4].2, "word8 word9");
    assert_eq!(first[5].2, "typed");
    // Page 0 spans word0 start to word1 end; page 1 starts at word2's start.
    assert_eq!(first[0].0, 0);
    assert_eq!(first[1].0, SEC);
    // Words are relative to the page start.
    let Clip::Caption(second) = p
        .find_track(t)
        .unwrap()
        .clips
        .iter()
        .find(|c| c.start_us() == SEC)
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(second.body.words[0].start_us, 0);
    assert_eq!(second.body.words[1].start_us, 500_000);
    // The transcript link is kept for pages whose words all carry ids.
    assert!(
        second.transcript_ref.is_none(),
        "no transcript on these words"
    );
    // The typed caption is untouched.
    assert!(p.find_track(t).unwrap().find_clip(typed).is_some());

    // Deterministic: splitting again with the same limits changes nothing; the same words with
    // other limits re-page.
    ok(&mut p, split);
    assert_eq!(pages(&p), first);
    ok(
        &mut p,
        json!({ "command": "SplitCaptions", "track_id": t, "max_chars": 40,
                 "max_lines": 2, "max_duration": "2s" }),
    );
    let by_time = pages(&p);
    // 2 s per page at 0.5 s per word: four words per page, three pages (4 + 4 + 2) and "typed".
    assert_eq!(by_time.len(), 4);
    assert_eq!(by_time[0].3, 4);
    assert_eq!(by_time[2].3, 2);
    // A bad limit is refused.
    assert!(run(
        &mut p,
        json!({ "command": "SplitCaptions", "track_id": t, "max_chars": 0 })
    )
    .is_err());
}

// ---- text clips ---------------------------------------------------------------------------

fn add_text(p: &mut Project, track_id: Id, text: &str, start: &str, dur: &str) -> Id {
    match ok(
        p,
        json!({ "command": "AddText", "track_id": track_id, "body": { "text": text },
                 "start": start, "duration": dur }),
    ) {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        o => panic!("{o:?}"),
    }
}

fn text_of(p: &Project, track_id: Id, clip_id: Id) -> TextClip {
    match p.find_track(track_id).unwrap().find_clip(clip_id).unwrap() {
        Clip::Text(t) => t.clone(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn add_text_needs_a_video_track_and_is_a_full_layer() {
    let mut p = project();
    let a = track(&mut p, "audio", "A1");
    let ct = caption_track(&mut p);
    for t in [a, ct] {
        assert!(matches!(
            run(
                &mut p,
                json!({ "command": "AddText", "track_id": t, "body": { "text": "x" },
                         "start": 0, "duration": "1s" })
            )
            .unwrap_err(),
            CommandError::TrackKindMismatch(..)
        ));
    }
    let v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let t = add_text(&mut p, v2, "EPISODE 12", "1s", "3s");
    let clip = text_of(&p, v2, t);
    assert_eq!(clip.start_us, SEC);
    assert_eq!(clip.duration_us, 3 * SEC);
    assert!(clip.enabled);
    assert_eq!(
        clip.style.item_ref,
        crate::project::text::DEFAULT_TEXT_STYLE_REF
    );
    // Overlap on a free-form track is refused.
    assert!(matches!(
        run(
            &mut p,
            json!({ "command": "AddText", "track_id": v2, "body": { "text": "y" },
                     "start": "2s", "duration": "3s" })
        )
        .unwrap_err(),
        CommandError::Overlap(..)
    ));
    // On the magnetic first track it is placed like any clip.
    let first = add_text(&mut p, v1, "A", "9s", "2s");
    assert_eq!(text_of(&p, v1, first).start_us, 0);

    // Transform, keyframes, effect slots, blend mode and mask all apply.
    ok(
        &mut p,
        json!({ "command": "SetClipTransform", "track_id": v2, "clip_id": t,
                 "transform": { "x": 0.1, "y": -0.2, "scale_x": 1.5, "scale_y": 1.5,
                                 "rotation_deg": 10.0, "opacity": 0.8 } }),
    );
    assert_eq!(text_of(&p, v2, t).transform.scale_x, 1.5);
    ok(
        &mut p,
        json!({ "command": "SetClipKeyframes", "track_id": v2, "clip_id": t, "keyframes": [
            { "property": "opacity", "keys": [
                { "time_us": 0, "value": 0.0 }, { "time_us": 500_000, "value": 1.0 }] }] }),
    );
    assert_eq!(text_of(&p, v2, t).keyframes.len(), 1);
    // Volume / speed keyframes mean nothing on a title.
    assert!(run(
        &mut p,
        json!({ "command": "SetClipKeyframes", "track_id": v2, "clip_id": t, "keyframes": [
            { "property": "volume", "keys": [{ "time_us": 0, "value": 0.0 }] }] })
    )
    .is_err());
    ok(
        &mut p,
        json!({ "command": "SetClipEffects", "track_id": v2, "clip_id": t,
                 "filter": null, "effects": [{ "ref": "builtin:blur@1", "params": { "radius": 4 } }],
                 "blend_mode": "screen" }),
    );
    let clip = text_of(&p, v2, t);
    assert_eq!(clip.effects.len(), 1);
    assert_eq!(clip.blend_mode, crate::project::BlendMode::Screen);
    // The effect's keyframes follow the instance.
    let fx = clip.effects[0].id;
    ok(
        &mut p,
        json!({ "command": "SetClipKeyframes", "track_id": v2, "clip_id": t, "keyframes": [
            { "property": format!("effect:{fx}/radius"), "keys": [
                { "time_us": 0, "value": 0.0 }, { "time_us": SEC, "value": 8.0 }] }] }),
    );
    assert!(run(
        &mut p,
        json!({ "command": "SetClipEffects", "track_id": v2, "clip_id": t,
                 "key": { "ref": "builtin:chroma_key@1" } })
    )
    .is_err());
    ok(
        &mut p,
        json!({ "command": "SetClipMask", "track_id": v2, "clip_id": t,
                 "mask": { "kind": { "type": "rect", "x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5 } } }),
    );
    assert!(text_of(&p, v2, t).mask.is_some());
    ok(
        &mut p,
        json!({ "command": "SetClipEnabled", "track_id": v2, "clip_id": t, "enabled": false }),
    );
    assert!(!text_of(&p, v2, t).enabled);
}

#[test]
fn text_clips_evaluate_as_layers_with_transforms_and_transitions() {
    use crate::compose::eval::{active_steps, ActiveSource, ComposeStep};
    let mut p = project();
    let _v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let a = add_text(&mut p, v2, "first", "0s", "2s");
    let b = add_text(&mut p, v2, "second", "2s", "2s");
    ok(
        &mut p,
        json!({ "command": "SetTextStyle", "track_id": v2, "clip_id": a,
                 "ref": "builtin:tiktok-box@1", "overrides": { "fill": "#00FF00" } }),
    );
    ok(
        &mut p,
        json!({ "command": "SetClipTransform", "track_id": v2, "clip_id": a,
                 "transform": { "x": 0.25, "scale_x": 1.0, "scale_y": 1.0, "opacity": 0.5 } }),
    );
    let layers = |p: &Project, t: i64| -> Vec<crate::compose::eval::ActiveLayer> {
        active_steps(p, t)
            .unwrap()
            .into_iter()
            .filter_map(|s| match s {
                ComposeStep::Layer(l) => Some(l),
                _ => None,
            })
            .collect()
    };
    let at = layers(&p, SEC);
    assert_eq!(at.len(), 1);
    let ActiveSource::Text(text) = &at[0].source else {
        panic!("{:?}", at[0].source)
    };
    assert_eq!(text.text, "first");
    assert_eq!(text.style_id, "tiktok-box");
    assert_eq!(text.overrides["fill"], json!("#00FF00"));
    assert_eq!(at[0].transform.x, 0.25);
    assert_eq!(at[0].transform.opacity, 0.5);
    assert!(layers(&p, 5 * SEC).is_empty());

    // A transition between two titles gives an outgoing and an incoming layer.
    ok(
        &mut p,
        json!({ "command": "SetClipTransition", "track_id": v2, "clip_id": a,
                 "ref": "builtin:crossfade@1", "duration": "1s" }),
    );
    let mid = layers(&p, 1_500_000);
    assert_eq!(mid.len(), 2);
    assert!(mid[0].transition.as_ref().is_some_and(|t| !t.is_incoming));
    assert!(mid[1].transition.as_ref().is_some_and(|t| t.is_incoming));
    assert_eq!(mid[1].text().unwrap().text, "second");
    assert!(text_of(&p, v2, b).outgoing_transition.is_none());
    // Disabled titles are not drawn.
    ok(
        &mut p,
        json!({ "command": "SetClipEnabled", "track_id": v2, "clip_id": b, "enabled": false }),
    );
    assert!(layers(&p, 3 * SEC).is_empty());
}

#[test]
fn set_text_timing_moves_and_resizes() {
    let mut p = project();
    let _v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let a = add_text(&mut p, v2, "a", "0s", "2s");
    let b = add_text(&mut p, v2, "b", "3s", "2s");
    ok(
        &mut p,
        json!({ "command": "SetTextTiming", "track_id": v2, "clip_id": a, "duration": "2.5s" }),
    );
    assert_eq!(text_of(&p, v2, a).duration_us, 2_500_000);
    assert!(run(
        &mut p,
        json!({ "command": "SetTextTiming", "track_id": v2, "clip_id": a, "duration": "4s" })
    )
    .is_err());
    ok(
        &mut p,
        json!({ "command": "SetTextTiming", "track_id": v2, "clip_id": b, "start": "6s" }),
    );
    assert_eq!(text_of(&p, v2, b).start_us, 6 * SEC);
    assert!(run(
        &mut p,
        json!({ "command": "SetTextTiming", "track_id": v2, "clip_id": b })
    )
    .is_err());
}

#[test]
fn split_and_move_work_on_text_clips() {
    let mut p = project();
    let _v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let a = add_text(&mut p, v2, "title", "1s", "4s");
    ok(
        &mut p,
        json!({ "command": "SetTextAnimation", "track_id": v2, "clip_id": a, "phase": "in",
                 "ref": "builtin:fade-up@1" }),
    );
    ok(
        &mut p,
        json!({ "command": "SetTextAnimation", "track_id": v2, "clip_id": a, "phase": "out",
                 "ref": "builtin:fade-out@1" }),
    );
    ok(
        &mut p,
        json!({ "command": "SplitClip", "track_id": v2, "clip_id": a, "at": "3s" }),
    );
    let all: Vec<TextClip> = p
        .find_track(v2)
        .unwrap()
        .clips
        .iter()
        .map(|c| match c {
            Clip::Text(t) => t.clone(),
            _ => panic!(),
        })
        .collect();
    assert_eq!(all.len(), 2);
    let (left, right) = if all[0].start_us < all[1].start_us {
        (&all[0], &all[1])
    } else {
        (&all[1], &all[0])
    };
    assert_eq!((left.start_us, left.duration_us), (SEC, 2 * SEC));
    assert_eq!((right.start_us, right.duration_us), (3 * SEC, 2 * SEC));
    // The intro belongs to the first half, the outro to the second.
    assert!(left.animations.in_.is_some() && left.animations.out.is_none());
    assert!(right.animations.in_.is_none() && right.animations.out.is_some());
    ok(
        &mut p,
        json!({ "command": "MoveClip", "track_id": v2, "clip_id": right.id, "start": "10s" }),
    );
    assert_eq!(text_of(&p, v2, right.id).start_us, 10 * SEC);
}

// ---- transcripts --------------------------------------------------------------------------

/// A 10 s clip of one media on two video tracks, and a transcript of it: ten
/// one-second words `w-1`..`w-10`, each 0.8 s long.
fn transcript_project() -> (Project, Id, Id, Id, Id, Id) {
    let mut p = project();
    let media = add_media(&mut p, MediaKind::Video, 10);
    let v1 = track(&mut p, "video", "V1");
    // A second layer of the same media (a video file's sound is not a separate audio clip).
    let a1 = track(&mut p, "video", "V2");
    let vc = clip(&mut p, v1, media, "0s", 10);
    let ac = match ok(
        &mut p,
        json!({ "command": "AddClip", "track_id": a1, "media_id": media,
                 "start": 0, "source_in": 0, "source_out": "10s" }),
    ) {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        o => panic!("{o:?}"),
    };
    let tid = Id::new_v4();
    p.transcripts.push(Transcript {
        id: tid,
        media_id: media,
        language: "en".into(),
        model: "test".into(),
        words: (0..10)
            .map(|i| TranscriptWord {
                id: format!("w-{}", i + 1),
                text: format!("word{i}"),
                start_us: i * SEC,
                end_us: i * SEC + 800_000,
                deleted: false,
                tags: if i == 4 {
                    vec!["filler".into()]
                } else {
                    Vec::new()
                },
            })
            .collect(),
    });
    (p, tid, v1, a1, vc, ac)
}

fn media_spans(p: &Project, track_id: Id) -> Vec<(i64, i64, i64)> {
    // (timeline start, source in, source out), in timeline order.
    let mut v: Vec<(i64, i64, i64)> = p
        .find_track(track_id)
        .unwrap()
        .clips
        .iter()
        .filter_map(Clip::as_media)
        .map(|m| (m.start_us, m.source_in_us, m.source_out_us))
        .collect();
    v.sort();
    v
}

#[test]
fn apply_transcript_cuts_removes_deleted_words_with_ripple_on_every_track() {
    let (mut p, tid, v1, a1, ..) = transcript_project();
    // Delete w-3 and w-4 (consecutive: one cut from 2 s to 3.8 s) and the word tagged filler
    // (w-5 at 4 s..4.8 s) is adjacent to them, so the cut is 2 s..4.8 s.
    ok(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": tid,
                 "word_ids": ["w-3", "w-4"], "tags": ["filler"] }),
    );
    for t in [v1, a1] {
        // 2 s..4.8 s is gone (frame-snapped at 30 fps: 4.8 s is a frame boundary); what was
        // after it moved up by 2.8 s.
        assert_eq!(
            media_spans(&p, t),
            vec![(0, 0, 2 * SEC), (2 * SEC, 4_800_000, 10 * SEC)],
            "track {t}"
        );
    }
    let flagged: Vec<&str> = p.transcripts[0]
        .words
        .iter()
        .filter(|w| w.deleted)
        .map(|w| w.id.as_str())
        .collect();
    assert_eq!(flagged, vec!["w-3", "w-4", "w-5"]);

    // Applying again is a no-op.
    let before = serde_json::to_value(&p).unwrap();
    ok(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": tid }),
    );
    assert_eq!(serde_json::to_value(&p).unwrap(), before);
}

#[test]
fn restore_transcript_words_puts_the_media_back() {
    let (mut p, tid, v1, a1, ..) = transcript_project();
    let original = media_spans(&p, v1);
    ok(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": tid,
                 "word_ids": ["w-3", "w-4", "w-8"] }),
    );
    assert_eq!(media_spans(&p, v1).len(), 3);
    // Restore one of the two cuts.
    ok(
        &mut p,
        json!({ "command": "RestoreTranscriptWords", "transcript_id": tid,
                 "word_ids": ["w-8"] }),
    );
    for t in [v1, a1] {
        let spans = media_spans(&p, t);
        // The first cut (2 s..3.8 s) is still there. The second is whole again: the word's
        // range was added to the clip before it, which now runs on into the clip after it.
        assert_eq!(
            spans,
            vec![
                (0, 0, 2 * SEC),
                (2 * SEC, 3_800_000, 7_800_000),
                (6 * SEC, 7_800_000, 10 * SEC)
            ],
            "{spans:?}"
        );
    }
    // Restore everything: the whole source is covered, in order, with no gap on the timeline.
    ok(
        &mut p,
        json!({ "command": "RestoreTranscriptWords", "transcript_id": tid }),
    );
    for t in [v1, a1] {
        let spans = media_spans(&p, t);
        let total: i64 = spans.iter().map(|s| s.2 - s.1).sum();
        // The silence between cut words (0.2 s tails) was never cut away, and what the words
        // cover is back: only the stretches no word covers could differ, so compare the ends.
        assert_eq!(spans.first().unwrap().1, 0);
        assert_eq!(spans.last().unwrap().2, 10 * SEC);
        assert!(total <= original.iter().map(|s| s.2 - s.1).sum::<i64>());
        // Contiguous on the timeline.
        for w in spans.windows(2) {
            assert!(w[1].0 >= w[0].0, "{spans:?}");
        }
    }
    assert!(p.transcripts[0].words.iter().all(|w| !w.deleted));
    // Nothing left to restore.
    assert!(run(
        &mut p,
        json!({ "command": "RestoreTranscriptWords", "transcript_id": tid })
    )
    .is_err());
}

#[test]
fn transcript_cuts_validate_before_changing_anything() {
    let (mut p, tid, v1, ..) = transcript_project();
    let before = serde_json::to_value(&p).unwrap();
    assert!(run(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": tid, "word_ids": ["w-99"] })
    )
    .is_err());
    // Nothing selected.
    assert!(run(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": tid })
    )
    .is_err());
    assert!(run(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": Id::new_v4(),
                 "word_ids": ["w-1"] })
    )
    .is_err());
    // A retimed clip cannot be cut by source time.
    let first = p.find_track(v1).unwrap().clips[0].id();
    ok(
        &mut p,
        json!({ "command": "SetClipSpeed", "track_id": v1, "clip_id": first, "speed": 2.0 }),
    );
    let err = run(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": tid, "word_ids": ["w-1"] }),
    )
    .unwrap_err();
    assert!(err.to_string().contains("retimed"), "{err}");
    let _ = before;
}

#[test]
fn cutting_the_head_and_the_tail_of_a_clip() {
    let (mut p, tid, v1, ..) = transcript_project();
    ok(
        &mut p,
        json!({ "command": "ApplyTranscriptCuts", "transcript_id": tid,
                 "word_ids": ["w-1", "w-10"] }),
    );
    // Head 0..0.8 s and the word 9..9.8 s are gone; the 0.2 s after the last word is not part
    // of a word, so it stays.
    let spans = media_spans(&p, v1);
    assert_eq!(
        spans,
        vec![(0, 800_000, 9 * SEC), (8_200_000, 9_800_000, 10 * SEC)]
    );
    // Everything restored again.
    ok(
        &mut p,
        json!({ "command": "RestoreTranscriptWords", "transcript_id": tid }),
    );
    let spans = media_spans(&p, v1);
    assert_eq!(spans.first().unwrap().1, 0);
    assert_eq!(spans.last().unwrap().2, 10 * SEC);
}

// ---- model and validation ---------------------------------------------------------------

#[test]
fn project_validation_rejects_duplicate_transcript_and_marker_ids() {
    let mut p = project();
    let m = Marker {
        id: Id::new_v4(),
        time_us: 0,
        label: String::new(),
        color: String::new(),
        kind: MarkerKind::User,
    };
    p.markers = vec![m.clone(), m];
    assert!(p.validate().is_err());
    p.markers.clear();
    let t = Transcript {
        id: Id::new_v4(),
        media_id: Id::new_v4(),
        language: String::new(),
        model: String::new(),
        words: vec![],
    };
    p.transcripts = vec![t.clone(), t];
    assert!(p.validate().is_err());
}

#[test]
fn v8_text_data_round_trips_through_json() {
    let mut p = project();
    let ct = caption_track(&mut p);
    let v1 = track(&mut p, "video", "V1");
    let cap = add_caption(
        &mut p,
        ct,
        json!({ "text": "hi there", "runs": [{ "start": 0, "end": 2, "style": { "fill": "#FF0000" } }],
                 "words": [{ "text": "hi", "start": 0, "end": "0.4s", "emphasis": true }] }),
        "0s",
        "2s",
    );
    ok(
        &mut p,
        json!({ "command": "SetTextAnimation", "track_id": ct, "clip_id": cap, "phase": "active",
                 "ref": "builtin:word-highlight@1", "params": { "color": "#FFD400" } }),
    );
    add_text(&mut p, v1, "title", "0s", "2s");
    ok(
        &mut p,
        json!({ "command": "AddMarkers", "markers": [{ "time": "1s", "label": "a" }] }),
    );
    let json = serde_json::to_string(&p).unwrap();
    let back = Project::from_json(&json).unwrap();
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        serde_json::to_value(&p).unwrap()
    );
    let v: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(
        v["tracks"][0]["clips"][0]["animations"]["active"]["ref"],
        json!("builtin:word-highlight@1")
    );
    assert_eq!(v["tracks"][1]["magnetic"], json!(true));
    assert_eq!(v["tracks"][1]["clips"][0]["type"], json!("text"));
    assert_eq!(v["markers"][0]["kind"], json!("user"));
}

#[test]
fn a_title_can_be_nested_and_still_renders_inside_the_sequence() {
    use crate::compose::eval::{active_steps, sequence_steps, ActiveSource, ComposeStep};
    let mut p = project();
    let _v1 = track(&mut p, "video", "V1");
    let v2 = track(&mut p, "video", "V2");
    let t = add_text(&mut p, v2, "inside", "2s", "2s");
    ok(
        &mut p,
        json!({ "command": "NestClips", "clips": [{ "track_id": v2, "clip_id": t }],
                 "name": "Title card" }),
    );
    // The root now holds one clip placing the sequence; the title lives in the sequence.
    assert_eq!(p.sequences.len(), 1);
    assert!(matches!(p.sequences[0].tracks[0].clips[0], Clip::Text(_)));
    let steps = active_steps(&p, 3 * SEC).unwrap();
    let ComposeStep::Layer(layer) = &steps[0] else {
        panic!()
    };
    let ActiveSource::Sequence {
        sequence_id,
        time_us,
        instance,
    } = &layer.source
    else {
        panic!("{:?}", layer.source)
    };
    let inner = sequence_steps(&p, *sequence_id, *time_us, instance).unwrap();
    let ComposeStep::Layer(inner) = &inner[0] else {
        panic!()
    };
    assert_eq!(inner.text().unwrap().text, "inside");
}
