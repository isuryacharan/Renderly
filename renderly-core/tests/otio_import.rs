//! Importing OTIO files written by other tools. The fixtures are real sample files from the
//! OpenTimelineIO repository (see tests/fixtures/otio/README.md for source and licence).

use renderly_core::otio::{import_otio, ImportOptions, OtioError, OtioWarning, WarningKind};
use renderly_core::project::*;
use serde_json::{json, Value};

fn us(v: i64) -> f64 {
    v as f64 / 1e6
}

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/otio")
        .join(format!("{name}.otio"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn import(name: &str) -> (Project, Vec<OtioWarning>) {
    let (p, w) = import_otio(&fixture(name), &ImportOptions::default()).unwrap();
    p.validate().unwrap();
    (p, w)
}

fn media_clips(t: &Track) -> Vec<&MediaClip> {
    t.clips.iter().filter_map(Clip::as_media).collect()
}

fn near(a: f64, b: f64) {
    assert!((a - b).abs() < 2e-6, "{a} != {b}");
}

fn count(w: &[OtioWarning], kind: WarningKind) -> usize {
    w.iter().filter(|x| x.kind == kind).count()
}

#[test]
fn simple_cut_clip_order_positions_and_source_ranges() {
    let (p, w) = import("simple_cut");
    assert!(w.is_empty(), "{w:?}");
    assert_eq!(p.name, "Figure 1 - Simple Cut List");
    assert_eq!(p.settings.fps.as_f64(), 24.0);
    assert_eq!((p.settings.width, p.settings.height), (1920, 1080));
    assert_eq!(p.tracks.len(), 1);
    let t = &p.tracks[0];
    assert_eq!((t.kind, t.name.as_str()), (TrackKind::Video, "Track-001"));
    let clips = media_clips(t);
    assert_eq!(clips.len(), 4);
    // (position, source_in, source_out) in frames @24: clip lengths 3, 6, 4, 6.
    let expect = [(0, 3, 6), (3, 2, 8), (9, 0, 4), (13, 0, 6)];
    for (c, (pos, a, b)) in clips.iter().zip(expect) {
        near(us(c.start_us), pos as f64 / 24.0);
        near(us(c.source_in_us), a as f64 / 24.0);
        near(us(c.source_out_us), b as f64 / 24.0);
        assert_eq!(c.speed, 1.0);
    }
    // Four distinct files; paths come from the file URLs; durations from available ranges.
    assert_eq!(p.media.len(), 4);
    let paths: Vec<String> = clips
        .iter()
        .map(|c| {
            p.find_media(c.media_id)
                .unwrap()
                .path
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    assert_eq!(
        paths,
        [
            "/folder/titles.mov",
            "/folder/wind-up.mov",
            "/folder/punchline.mov",
            "/folder/credits.mov"
        ]
    );
    near(
        us(p.find_media(clips[0].media_id)
            .unwrap()
            .duration_us
            .unwrap()),
        8.0 / 24.0,
    );
    // The last clip has no source_range: it exposes the whole available range, whose start
    // (frame 100) is a timecode offset, so the file-relative source starts at 0.
    near(
        us(p.find_media(clips[3].media_id)
            .unwrap()
            .duration_us
            .unwrap()),
        6.0 / 24.0,
    );
}

#[test]
fn transition_sample_becomes_a_crossfade_between_adjacent_clips() {
    let (p, w) = import("transition");
    let clips = media_clips(&p.tracks[0]);
    assert_eq!(clips.len(), 4);
    assert!(clips[0].outgoing_transition.is_none());
    let tr = clips[1]
        .outgoing_transition
        .as_ref()
        .expect("transition after clip 2");
    assert_eq!(tr.item_ref, "builtin:crossfade@1");
    // in_offset 2 + out_offset 3 frames = 5 frames, shortened to half of the 4-frame clip
    // that follows (Renderly's rule), with a warning saying so.
    near(us(tr.duration_us), 2.0 / 24.0);
    assert_eq!(count(&w, WarningKind::Transition), 1);
    assert!(clips[2].outgoing_transition.is_none() && clips[3].outgoing_transition.is_none());
}

#[test]
fn legacy_sequence_schema_and_dangling_transitions_import_without_error() {
    let (p, w) = import("transition_test");
    // `Sequence.1` is the old name of `Track.1`.
    assert_eq!(p.tracks.len(), 1);
    assert_eq!(p.tracks[0].name, "Sequence1");
    // Every clip has `media_reference: null`: gaps, with a warning each; the first and last
    // transitions have no clip on one side.
    assert!(p.tracks[0].clips.is_empty());
    assert_eq!(count(&w, WarningKind::MissingMedia), 3);
    assert_eq!(count(&w, WarningKind::Transition), 3);
}

#[test]
fn nested_stacks_and_tracks_become_sequences_with_time_warps() {
    let (p, w) = import("nested_example");
    assert_eq!(p.tracks.len(), 1);
    assert_eq!(p.sequences.len(), 3);
    let t = &p.tracks[0];
    let clips = media_clips(t);
    assert_eq!(clips.len(), 3, "the four MissingReference clips are gaps");
    // Positions: 238, +31+33 = 302, +29+63 = 394 frames @24 (gaps keep later items in place).
    near(us(clips[0].start_us), 238.0 / 24.0);
    near(us(clips[1].start_us), 302.0 / 24.0);
    near(us(clips[2].start_us), 394.0 / 24.0);
    // Each clip places a sequence through its media item.
    let names: Vec<&str> = clips
        .iter()
        .map(|c| {
            let seq = p.find_media(c.media_id).unwrap().sequence_id.unwrap();
            p.find_sequence(seq).unwrap().name.as_str()
        })
        .collect();
    assert_eq!(
        names,
        [
            "Nested Stack 1",
            "Nested Stack 2 (with effects)",
            "Nested Track"
        ]
    );
    near(us(clips[0].timeline_duration_us()), 31.0 / 24.0);
    // LinearTimeWarp 0.8545...: speed on the nested clip; the stack's source range is in
    // timeline time, so the inner span read is duration * speed.
    near(clips[1].speed, 0.8545454551724138);
    near(us(clips[1].timeline_duration_us()), 29.0 / 24.0);
    // FreezeFrame is not representable.
    assert_eq!(count(&w, WarningKind::TimeWarp), 1);
    assert_eq!(count(&w, WarningKind::MissingMedia), 7);
    for s in &p.sequences {
        assert_eq!(s.tracks.len(), 1);
    }
}

#[test]
fn premiere_export_mixed_rates_nested_stacks_audio_and_urls() {
    let (p, w) = import("premiere_example");
    // Mostly 30 fps (56 of 58 ranges); the two 15 fps ranges are converted with their own rate.
    assert_eq!(p.settings.fps.as_f64(), 30.0);
    assert_eq!(count(&w, WarningKind::Rate), 1);
    assert_eq!(p.tracks.len(), 8);
    let kinds: Vec<_> = p.tracks.iter().map(|t| t.kind).collect();
    assert_eq!(
        kinds,
        [TrackKind::Video; 4]
            .into_iter()
            .chain([TrackKind::Audio; 4])
            .collect::<Vec<_>>()
    );

    // V1: a 536-frame gap, then a 50-frame clip authored at 15 fps = 100 frames @30 = 3.333 s.
    let v1 = media_clips(&p.tracks[0]);
    assert_eq!(v1.len(), 1);
    near(us(v1[0].start_us), 536.0 / 30.0);
    near(us(v1[0].timeline_duration_us()), 50.0 / 15.0);
    near(us(v1[0].source_in_us), 0.0);
    // `file://localhost/D%3a/media/...` is a Windows path.
    let m = p.find_media(v1[0].media_id).unwrap();
    assert_eq!(m.path.to_string_lossy(), "D:/media/sc01_sh010_anim.mov");
    assert_eq!(m.kind, MediaKind::Video);
    near(us(m.duration_us.unwrap()), 100.0 / 30.0);

    // V2: gap 13, clip 100, gap 52, clip 157, clip 235, [transition] gap 79, nested stack 320.
    let v2 = media_clips(&p.tracks[1]);
    assert_eq!(v2.len(), 4);
    near(us(v2[0].start_us), 13.0 / 30.0);
    near(us(v2[1].start_us), (13.0 + 100.0 + 52.0) / 30.0);
    near(us(v2[2].start_us), (13.0 + 100.0 + 52.0 + 157.0) / 30.0);
    near(
        us(v2[3].start_us),
        (13.0 + 100.0 + 52.0 + 157.0 + 235.0 + 79.0) / 30.0,
    );
    near(us(v2[3].timeline_duration_us()), 320.0 / 30.0);
    // The dissolve after clip 235 is followed by a gap, so it cannot be kept.
    assert!(v2.iter().all(|c| c.outgoing_transition.is_none()));
    assert!(w
        .iter()
        .any(|x| x.kind == WarningKind::Transition && x.message.contains("adjacent")));
    // The nested stack is a sequence holding real tracks and clips.
    let seq = p
        .find_sequence(p.find_media(v2[3].media_id).unwrap().sequence_id.unwrap())
        .unwrap();
    // (This sample's nested stack has no children: an empty sequence, kept in place.)
    assert_eq!(seq.name, "sc01_sh010_anim");
    assert!(seq.tracks.is_empty());

    // V3 only holds a generator (a title) Renderly cannot render: a gap, not a clip.
    assert!(p.tracks[2].clips.is_empty());
    // V4: an asymmetric dissolve (12 + 13 frames) between two touching clips becomes a
    // crossfade of the total length.
    let v4 = media_clips(&p.tracks[3]);
    assert_eq!(v4.len(), 2);
    let tr = v4[0].outgoing_transition.as_ref().unwrap();
    assert_eq!(tr.item_ref, "builtin:crossfade@1");
    near(us(tr.duration_us), 25.0 / 30.0);
    // The clip after the transition starts at its source_range start (18 frames @30).
    near(us(v4[1].source_in_us), 18.0 / 30.0);

    // Audio tracks: clips reference audio media even when the file is a movie.
    let a1 = media_clips(&p.tracks[4]);
    assert_eq!(a1.len(), 2);
    assert_eq!(p.find_media(a1[0].media_id).unwrap().kind, MediaKind::Audio);
    assert_eq!(
        p.find_media(a1[0].media_id).unwrap().path,
        p.find_media(v1[0].media_id).unwrap().path,
        "same file, separate (audio) media item"
    );
    assert!(p.media.len() > 8);
    assert!(count(&w, WarningKind::MarkersIgnored) >= 1);
    assert_eq!(p.sequences.len(), 2);
}

#[test]
fn screening_example_missing_media_becomes_gaps_with_warnings() {
    let (p, w) = import("screening_example");
    assert_eq!(p.tracks.len(), 1);
    assert!(p.tracks[0].clips.is_empty());
    assert_eq!(count(&w, WarningKind::MissingMedia), 9);
    assert_eq!(count(&w, WarningKind::MarkersIgnored), 2);
    assert_eq!(count(&w, WarningKind::TrackTrimmed), 1);
    assert!(w.iter().any(|x| x.message.contains("ignored")));
}

// ---- variants derived from the real files -------------------------------------------------

fn mutate(name: &str, f: impl FnOnce(&mut Value)) -> String {
    let mut v: Value = serde_json::from_str(&fixture(name)).unwrap();
    f(&mut v);
    v.to_string()
}

fn link_all(v: &mut Value) {
    match v {
        Value::Object(m) => {
            if m.get("OTIO_SCHEMA").and_then(Value::as_str) == Some("Clip.2") {
                m.insert(
                    "media_references".into(),
                    json!({"DEFAULT_MEDIA": {"OTIO_SCHEMA": "ExternalReference.1",
                        "target_url": "file:///m/x%20y.mov"}}),
                );
            }
            m.values_mut().for_each(link_all);
        }
        Value::Array(a) => a.iter_mut().for_each(link_all),
        _ => {}
    }
}

#[test]
fn a_tracks_own_source_range_trims_its_content() {
    // The real file's track source_range (-86243 frames) does not overlap its content: it is
    // ignored with a warning rather than emptying the track.
    let json = mutate("screening_example", link_all);
    let (p, w) = import_otio(&json, &ImportOptions::default()).unwrap();
    let clips = media_clips(&p.tracks[0]);
    assert_eq!(clips.len(), 9);
    near(us(clips[0].start_us), 0.0);
    near(us(clips[0].timeline_duration_us()), 31.0 / 24.0);
    near(us(clips[1].start_us), 31.0 / 24.0);
    // No available_range: source times are used as written.
    near(us(clips[0].source_in_us), 86501.0 / 24.0);
    // All nine clips share one file.
    assert_eq!(p.media.len(), 1);
    assert_eq!(p.media[0].path.to_string_lossy(), "/m/x y.mov");
    assert!(w
        .iter()
        .any(|x| x.kind == WarningKind::TrackTrimmed && x.message.contains("does not overlap")));

    // Trimming from the head instead: start 10 frames in, 100 frames long.
    let json = mutate("screening_example", |v| {
        link_all(v);
        let sr = &mut v["tracks"]["children"][0]["source_range"];
        sr["start_time"]["value"] = 40.into();
        sr["duration"]["value"] = 100.into();
    });
    let (p, _) = import_otio(&json, &ImportOptions::default()).unwrap();
    let clips = media_clips(&p.tracks[0]);
    // Clip 0 (0..31) and clip 1 (31..81) are cut: clip 1 keeps frames 40..81 of the track.
    near(us(clips[0].start_us), 0.0);
    near(us(clips[0].timeline_duration_us()), 41.0 / 24.0);
    near(us(clips[0].source_in_us), (86557.0 + 9.0) / 24.0);
    let end: i64 = clips
        .iter()
        .map(|c| c.start_us + c.timeline_duration_us())
        .fold(0_i64, i64::max);
    near(us(end), 100.0 / 24.0);
}

#[test]
fn clip_1_and_clip_2_media_reference_forms_both_work() {
    let (p1, _) = import("simple_cut");
    // Rewrite simple_cut's Clip.1 objects as Clip.2 with media_references.
    let json = mutate("simple_cut", |v| {
        for c in v["tracks"]["children"][0]["children"]
            .as_array_mut()
            .unwrap()
        {
            let r = c["media_reference"].take();
            c["OTIO_SCHEMA"] = "Clip.2".into();
            c["media_references"] = json!({"alt": r.clone(), "main": r});
            c["active_media_reference_key"] = "main".into();
            c.as_object_mut().unwrap().remove("media_reference");
        }
    });
    let (p2, w) = import_otio(&json, &ImportOptions::default()).unwrap();
    assert!(w.is_empty());
    assert_eq!(
        media_clips(&p1.tracks[0]).len(),
        media_clips(&p2.tracks[0]).len()
    );
    for (a, b) in media_clips(&p1.tracks[0])
        .iter()
        .zip(media_clips(&p2.tracks[0]))
    {
        near(us(a.start_us), us(b.start_us));
        near(us(a.source_in_us), us(b.source_in_us));
        near(us(a.source_out_us), us(b.source_out_us));
    }
}

#[test]
fn unknown_schemas_and_fields_are_tolerated() {
    let json = mutate("simple_cut", |v| {
        v["some_future_field"] = json!({"x": [1, 2, 3]});
        v["metadata"]["vendor"] = json!({"a": 1});
        let kids = v["tracks"]["children"][0]["children"]
            .as_array_mut()
            .unwrap();
        kids[1]["OTIO_SCHEMA"] = "Clip.9".into(); // a future version of a known schema
        kids[1]["brand_new"] = true.into();
        kids.insert(
            2,
            json!({"OTIO_SCHEMA": "Hologram.3", "source_range": {"OTIO_SCHEMA":"TimeRange.1",
              "start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":24,"value":0},
              "duration":{"OTIO_SCHEMA":"RationalTime.1","rate":24,"value":24}}}),
        );
        let last = kids.len() - 1;
        kids[last]["effects"] = json!([{"OTIO_SCHEMA":"Effect.1","effect_name":"Blur","name":"b"}]);
        kids[0]["media_reference"]["OTIO_SCHEMA"] = "FancyReference.1".into();
    });
    let (p, w) = import_otio(&json, &ImportOptions::default()).unwrap();
    // The unknown reference and the unknown item become gaps (positions are kept), the rest
    // imports; every oddity is a warning, none an error.
    let clips = media_clips(&p.tracks[0]);
    assert_eq!(clips.len(), 3);
    near(us(clips[0].start_us), 3.0 / 24.0);
    assert!(count(&w, WarningKind::UnknownSchema) >= 2);
    assert!(count(&w, WarningKind::UnsupportedEffect) >= 1);
}

#[test]
fn collections_other_roots_nonfinite_numbers_and_non_file_urls() {
    let inner = fixture("simple_cut");
    let wrapped = format!(
        r#"{{"OTIO_SCHEMA":"SerializableCollection.1","name":"bin","children":[
            {{"OTIO_SCHEMA":"Clip.1","name":"x"}}, {inner}]}}"#
    );
    let (p, _) = import_otio(&wrapped, &ImportOptions::default()).unwrap();
    assert_eq!(p.tracks[0].clips.len(), 4);

    // A bare Track as the document root.
    let track = r#"{"OTIO_SCHEMA":"Track.1","kind":"Audio","name":"only","children":[
        {"OTIO_SCHEMA":"Clip.1","name":"c","source_range":{"OTIO_SCHEMA":"TimeRange.1",
          "start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":25,"value":0},
          "duration":{"OTIO_SCHEMA":"RationalTime.1","rate":25,"value":50}},
         "media_reference":{"OTIO_SCHEMA":"ExternalReference.1","target_url":"relative/a.wav",
           "available_range":{"OTIO_SCHEMA":"TimeRange.1",
           "start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":25,"value":NaN},
           "duration":{"OTIO_SCHEMA":"RationalTime.1","rate":25,"value":Infinity}}}},
        {"OTIO_SCHEMA":"Clip.1","name":"web","source_range":{"OTIO_SCHEMA":"TimeRange.1",
          "start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":25,"value":0},
          "duration":{"OTIO_SCHEMA":"RationalTime.1","rate":25,"value":25}},
         "media_reference":{"OTIO_SCHEMA":"ExternalReference.1","target_url":"https://example.com/a.wav"}}
    ]}"#;
    let options = ImportOptions {
        base_dir: Some("D:\\proj".into()),
        ..Default::default()
    };
    let (p, w) = import_otio(track, &options).unwrap();
    assert_eq!(p.settings.fps.as_f64(), 25.0);
    assert_eq!(p.tracks[0].kind, TrackKind::Audio);
    let clips = media_clips(&p.tracks[0]);
    assert_eq!(clips.len(), 1);
    // Relative URLs resolve against the .otio file's directory.
    assert_eq!(p.media[0].path.to_string_lossy(), "D:/proj/relative/a.wav");
    assert_eq!(p.media[0].kind, MediaKind::Audio);
    assert_eq!(
        count(&w, WarningKind::MissingMedia),
        1,
        "https URL is a gap"
    );

    assert!(matches!(
        import_otio(
            r#"{"OTIO_SCHEMA":"Clip.1","name":"x"}"#,
            &ImportOptions::default()
        )
        .map(|_| ()),
        Ok(())
    ));
    assert!(matches!(
        import_otio("not json", &ImportOptions::default()),
        Err(OtioError::Parse(_))
    ));
}

#[test]
fn options_override_dimensions_rate_and_name() {
    let options = ImportOptions {
        width: Some(1080),
        height: Some(1920),
        fps: Some(25.0),
        sample_rate: Some(44_100),
        name: Some("Mine".into()),
        ..Default::default()
    };
    let (p, _) = import_otio(&fixture("simple_cut"), &options).unwrap();
    assert_eq!(p.name, "Mine");
    assert_eq!(p.settings.fps.as_f64(), 25.0);
    assert_eq!(
        (p.settings.width, p.settings.height, p.settings.sample_rate),
        (1080, 1920, 44_100)
    );
    // Positions still come from each value's own rate, not the project rate.
    near(us(media_clips(&p.tracks[0])[1].start_us), 3.0 / 24.0);
}

#[test]
fn imported_projects_survive_a_save_and_reload() {
    for name in [
        "simple_cut",
        "transition",
        "nested_example",
        "premiere_example",
    ] {
        let (p, _) = import(name);
        let json = serde_json::to_string(&p).unwrap();
        let back = Project::from_json(&json).unwrap();
        assert_eq!(back.tracks.len(), p.tracks.len(), "{name}");
        assert_eq!(back.sequences.len(), p.sequences.len(), "{name}");
    }
}
