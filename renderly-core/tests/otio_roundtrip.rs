//! Renderly -> OTIO -> Renderly, and structural validity of what we write (docs/otio.md).

use renderly_core::otio::{
    export_otio, export_otio_with_options, import_otio, ExportOptions, ImportOptions, WarningKind,
};
use renderly_core::project::*;
use serde_json::Value;
use std::collections::BTreeMap;

mod common;
use common::rich_project;

fn as_json(p: &Project) -> Value {
    serde_json::to_value(p).unwrap()
}

fn roundtrip(p: &Project) -> (Project, Vec<renderly_core::otio::OtioWarning>, String) {
    let otio = export_otio(p).unwrap();
    let (back, warnings) = import_otio(&otio, &ImportOptions::default()).unwrap();
    (back, warnings, otio)
}

/// Compare as normalised JSON: `serde_json` values (key order irrelevant), floats compared
/// exactly — the metadata carries the exact times, so nothing is snapped away.
fn assert_same(a: &Project, b: &Project) {
    let (ja, jb) = (as_json(a), as_json(b));
    if ja != jb {
        // Find the first differing path for a readable failure.
        fn diff(path: &str, a: &Value, b: &Value) -> Option<String> {
            match (a, b) {
                (Value::Object(x), Value::Object(y)) => {
                    for k in x.keys().chain(y.keys()) {
                        let (l, r) = (
                            x.get(k).unwrap_or(&Value::Null),
                            y.get(k).unwrap_or(&Value::Null),
                        );
                        if let Some(d) = diff(&format!("{path}.{k}"), l, r) {
                            return Some(d);
                        }
                    }
                    None
                }
                (Value::Array(x), Value::Array(y)) if x.len() == y.len() => x
                    .iter()
                    .zip(y)
                    .enumerate()
                    .find_map(|(i, (l, r))| diff(&format!("{path}[{i}]"), l, r)),
                _ if a == b => None,
                _ => Some(format!("{path}: {a} != {b}")),
            }
        }
        panic!(
            "projects differ at {}",
            diff("$", &ja, &jb).unwrap_or_default()
        );
    }
}

#[test]
fn rich_project_roundtrips_losslessly() {
    let p = rich_project();
    let (back, warnings, _) = roundtrip(&p);
    assert_same(&p, &back);
    assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
}

#[test]
fn roundtrip_is_a_fixed_point_and_export_is_deterministic() {
    let p = rich_project();
    let first = export_otio(&p).unwrap();
    let (back, _) = import_otio(&first, &ImportOptions::default()).unwrap();
    assert_eq!(first, export_otio(&back).unwrap());
}

#[test]
fn project_without_any_clips_roundtrips() {
    let mut p = Project::new("empty", Settings::default());
    p.tracks.push(Track::new(TrackKind::Video, "V1"));
    p.tracks.push(Track::new(TrackKind::Audio, "A1"));
    let (back, w, _) = roundtrip(&p);
    assert_same(&p, &back);
    assert!(w.is_empty());
    let none = Project::new("nothing", Settings::default());
    let (back, _, _) = roundtrip(&none);
    assert_same(&none, &back);
}

#[test]
fn fractional_rates_roundtrip() {
    let mut p = rich_project();
    p.settings.fps = renderly_core::time::Fps::new(30000, 1001);
    let (back, _, otio) = roundtrip(&p);
    assert_same(&p, &back);
    let v: Value = serde_json::from_str(&otio).unwrap();
    assert!((v["global_start_time"]["rate"].as_f64().unwrap() - 29.97002997).abs() < 1e-6);
}

// ---- what is written ---------------------------------------------------------------------

fn walk(v: &Value, f: &mut impl FnMut(&Value)) {
    match v {
        Value::Object(m) => {
            if m.contains_key("OTIO_SCHEMA") {
                f(v);
            }
            // Do not descend into metadata: it is opaque to OTIO.
            for (k, c) in m {
                if k != "metadata" {
                    walk(c, f);
                }
            }
        }
        Value::Array(a) => a.iter().for_each(|c| walk(c, f)),
        _ => {}
    }
}

const KNOWN: &[&str] = &[
    "Timeline.1",
    "Stack.1",
    "Track.1",
    "Clip.2",
    "Gap.1",
    "Transition.1",
    "Effect.1",
    "LinearTimeWarp.1",
    "ExternalReference.1",
    "GeneratorReference.1",
    "Marker.2",
    "RationalTime.1",
    "TimeRange.1",
];

fn rt(v: &Value) -> (f64, f64) {
    (v["value"].as_f64().unwrap(), v["rate"].as_f64().unwrap())
}

#[test]
fn exported_json_satisfies_otio_structural_invariants() {
    let p = rich_project();
    let otio = export_otio(&p).unwrap();
    let root: Value = serde_json::from_str(&otio).unwrap();
    assert_eq!(root["OTIO_SCHEMA"], "Timeline.1");
    assert_eq!(root["tracks"]["OTIO_SCHEMA"], "Stack.1");
    assert_eq!(rt(&root["global_start_time"]), (0.0, 30.0));

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut ok = true;
    walk(&root, &mut |o| {
        let s = o["OTIO_SCHEMA"].as_str().unwrap().to_string();
        if !KNOWN.contains(&s.as_str()) {
            ok = false;
        }
        *counts.entry(s.clone()).or_default() += 1;
        match s.as_str() {
            "RationalTime.1" => {
                let (v, r) = rt(o);
                assert!(v.is_finite() && r == 30.0, "rate of {o}");
                assert_eq!(v.fract(), 0.0, "frame values are whole: {o}");
            }
            "TimeRange.1" => {
                let (start, _) = rt(&o["start_time"]);
                let (dur, _) = rt(&o["duration"]);
                assert!(start >= 0.0 && dur >= 0.0, "range is non-negative: {o}");
            }
            "Transition.1" => {
                assert!(rt(&o["in_offset"]).0 >= 0.0 && rt(&o["out_offset"]).0 >= 0.0);
            }
            _ => {}
        }
    });
    assert!(ok, "unknown schema in {counts:?}");
    for s in [
        "Clip.2",
        "Gap.1",
        "Transition.1",
        "Effect.1",
        "LinearTimeWarp.1",
        "Stack.1",
    ] {
        assert!(counts.contains_key(s), "expected a {s}; got {counts:?}");
    }
    // 2 timeline-level Stacks (root + one per placed Intro: video and audio copies) = 3.
    assert_eq!(counts["Stack.1"], 3);
    // Two media transitions plus the one between the two titles.
    assert_eq!(counts["Transition.1"], 3);

    // Each track's children tile the timeline: clip ranges sit inside the media they use,
    // and gaps are never zero-length.
    for track in root["tracks"]["children"].as_array().unwrap() {
        assert_eq!(track["OTIO_SCHEMA"], "Track.1");
        assert!(["Video", "Audio"].contains(&track["kind"].as_str().unwrap()));
        let mut prev_transition = false;
        for child in track["children"].as_array().unwrap() {
            let schema = child["OTIO_SCHEMA"].as_str().unwrap();
            let is_transition = schema == "Transition.1";
            assert!(
                !(is_transition && prev_transition),
                "no adjacent transitions"
            );
            prev_transition = is_transition;
            if schema == "Gap.1" {
                assert!(rt(&child["source_range"]["duration"]).0 >= 1.0);
            }
            if schema != "Clip.2" {
                continue;
            }
            let reference =
                &child["media_references"][child["active_media_reference_key"].as_str().unwrap()];
            if reference["OTIO_SCHEMA"] != "ExternalReference.1"
                || reference["available_range"].is_null()
            {
                continue;
            }
            let avail = rt(&reference["available_range"]["duration"]).0;
            let (start, _) = rt(&child["source_range"]["start_time"]);
            let (dur, _) = rt(&child["source_range"]["duration"]);
            let scalar = child["effects"]
                .as_array()
                .unwrap()
                .iter()
                .find(|e| e["OTIO_SCHEMA"] == "LinearTimeWarp.1")
                .map_or(1.0, |e| e["time_scalar"].as_f64().unwrap());
            // Source extent = start + duration * time_scalar, within one frame of the media.
            assert!(
                start + dur * scalar <= avail + 1.0,
                "clip {} reads past its media: {start}+{dur}*{scalar} > {avail}",
                child["name"]
            );
        }
    }
}

#[test]
fn clips_gaps_transitions_and_warps_map_to_the_documented_otio() {
    let p = rich_project();
    let root: Value = serde_json::from_str(&export_otio(&p).unwrap()).unwrap();
    let v1 = &root["tracks"]["children"][0];
    assert_eq!(v1["kind"], "Video");
    let kids = v1["children"].as_array().unwrap();
    let schemas: Vec<&str> = kids
        .iter()
        .map(|k| k["OTIO_SCHEMA"].as_str().unwrap())
        .collect();
    assert_eq!(
        schemas,
        [
            "Clip.2",
            "Gap.1",
            "Clip.2",
            "Transition.1",
            "Clip.2",
            "Transition.1",
            "Clip.2",
            "Clip.2"
        ]
    );
    // Clip 0: source 1s..4s -> frames 30..120, 3 s long.
    assert_eq!(rt(&kids[0]["source_range"]["start_time"]).0, 30.0);
    assert_eq!(rt(&kids[0]["source_range"]["duration"]).0, 90.0);
    // Gap 3.0..4.5 s = 45 frames.
    assert_eq!(rt(&kids[1]["source_range"]["duration"]).0, 45.0);
    // Sped-up clip: 2x, occupies 2 s = 60 frames of timeline, warp on the clip.
    assert_eq!(rt(&kids[2]["source_range"]["duration"]).0, 60.0);
    let warp = &kids[2]["effects"][0];
    assert_eq!(warp["OTIO_SCHEMA"], "LinearTimeWarp.1");
    assert_eq!(warp["time_scalar"].as_f64().unwrap(), 2.0);
    // Crossfade: SMPTE_Dissolve, whole 0.5 s (15 frames) before the cut.
    assert_eq!(kids[3]["transition_type"], "SMPTE_Dissolve");
    assert_eq!(rt(&kids[3]["in_offset"]).0, 15.0);
    assert_eq!(rt(&kids[3]["out_offset"]).0, 0.0);
    // Wipe: Custom_Transition named by, and carrying, the Renderly ref.
    assert_eq!(kids[5]["transition_type"], "Custom_Transition");
    assert_eq!(kids[5]["name"], "builtin:wipe_left@1");
    assert_eq!(
        kids[5]["metadata"]["renderly"]["transition"]["ref"],
        "builtin:wipe_left@1"
    );
    assert_eq!(rt(&kids[5]["in_offset"]).0, 12.0);
    // Effects are visible natively as Effect.1.
    let names: Vec<&str> = kids[4]["effects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["effect_name"].as_str().unwrap())
        .collect();
    // In render order: the key slot first, then the effects list.
    assert_eq!(
        names,
        [
            "builtin:chroma_key@1",
            "builtin:blur@1",
            "builtin:color_adjust@1"
        ]
    );
    assert_eq!(kids[4]["effects"][0]["metadata"]["renderly"]["slot"], "key");
    // External reference: absolute URL, percent-encoded, available range of 10 s.
    let r = &kids[0]["media_references"]["DEFAULT_MEDIA"];
    assert_eq!(r["target_url"], "file:///D:/media/a%20clip.mp4");
    assert_eq!(rt(&r["available_range"]["duration"]).0, 300.0);
    // The disabled clip stays in place but is disabled.
    assert_eq!(kids[7]["enabled"], false);
    // The still has no available range.
    assert!(kids[6]["media_references"]["DEFAULT_MEDIA"]["available_range"].is_null());
    // Relative media stays a relative reference without a base dir ...
    let b = &kids[2]["media_references"]["DEFAULT_MEDIA"];
    assert_eq!(b["target_url"], "media/b.mov");
    // ... and becomes an absolute file URL with one.
    let abs: Value = serde_json::from_str(
        &export_otio_with_options(
            &p,
            &ExportOptions {
                base_dir: Some("D:\\projects\\edit".into()),
            },
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        abs["tracks"]["children"][0]["children"][2]["media_references"]["DEFAULT_MEDIA"]
            ["target_url"],
        "file:///D:/projects/edit/media/b.mov"
    );
}

#[test]
fn captions_adjustments_sequences_and_tracks_map_as_documented() {
    let p = rich_project();
    let root: Value = serde_json::from_str(&export_otio(&p).unwrap()).unwrap();
    let tracks = root["tracks"]["children"].as_array().unwrap();
    // Captions: a Video track of generator clips (justified in docs/otio.md).
    let caps = &tracks[3];
    assert_eq!(caps["kind"], "Video");
    assert_eq!(caps["metadata"]["renderly"]["track"]["kind"], "caption");
    let c0 = &caps["children"][1];
    assert_eq!(
        c0["media_references"]["DEFAULT_MEDIA"]["OTIO_SCHEMA"],
        "GeneratorReference.1"
    );
    assert_eq!(
        c0["media_references"]["DEFAULT_MEDIA"]["generator_kind"],
        "renderly.caption"
    );
    assert_eq!(
        c0["media_references"]["DEFAULT_MEDIA"]["parameters"]["style_ref"],
        "builtin:tiktok-bold-yellow@1"
    );
    // 0.5 s lead-in gap.
    assert_eq!(caps["children"][0]["OTIO_SCHEMA"], "Gap.1");
    assert_eq!(rt(&caps["children"][0]["source_range"]["duration"]).0, 15.0);
    // Adjustment layer: a Gap with the effect list natively and the whole layer in metadata.
    let v2 = tracks[1]["children"].as_array().unwrap();
    assert_eq!(v2[1]["OTIO_SCHEMA"], "Gap.1");
    assert_eq!(v2[1]["effects"][0]["effect_name"], "builtin:color_adjust@1");
    assert_eq!(
        v2[1]["effects"][0]["metadata"]["renderly"]["slot"],
        "effects"
    );
    assert_eq!(v2[1]["metadata"]["renderly"]["adjustment"]["opacity"], 0.6);
    // Nested sequence: a Stack in place, with its tracks inside.
    let nested = &v2[3];
    assert_eq!(nested["OTIO_SCHEMA"], "Stack.1");
    assert_eq!(nested["name"], "Intro");
    assert_eq!(nested["children"].as_array().unwrap().len(), 2);
    assert_eq!(nested["children"][0]["kind"], "Video");
    assert_eq!(nested["children"][1]["kind"], "Audio");
    assert_eq!(nested["metadata"]["renderly"]["sequence"]["width"], 640);
    // Track flags: hidden video track is disabled, muted audio track is disabled.
    assert_eq!(tracks[2]["enabled"], false);
    assert_eq!(tracks[5]["enabled"], false);
    assert_eq!(tracks[4]["enabled"], true);
    assert_eq!(
        tracks[4]["metadata"]["renderly"]["track"]["audio_role"],
        "music"
    );
    // The unplaced sequence travels in the timeline metadata.
    assert_eq!(
        root["metadata"]["renderly"]["unplaced_sequences"][0]["name"],
        "Spare"
    );
}

// ---- reconciling edits made elsewhere ----------------------------------------------------

fn edit(otio: &str, f: impl FnOnce(&mut Value)) -> String {
    let mut v: Value = serde_json::from_str(otio).unwrap();
    f(&mut v);
    serde_json::to_string(&v).unwrap()
}

#[test]
fn an_edit_made_in_another_tool_wins_over_stored_times() {
    let p = rich_project();
    let otio = export_otio(&p).unwrap();
    // Trim the first clip of V1 by one second in another tool, drop the first transition and
    // relink one file.
    let edited = edit(&otio, |v| {
        let kids = v["tracks"]["children"][0]["children"]
            .as_array_mut()
            .unwrap();
        kids[0]["source_range"]["duration"]["value"] = 60.into();
        kids[3] = serde_json::json!({"OTIO_SCHEMA": "Gap.1", "source_range": {"OTIO_SCHEMA":"TimeRange.1",
            "start_time":{"OTIO_SCHEMA":"RationalTime.1","rate":30,"value":0},
            "duration":{"OTIO_SCHEMA":"RationalTime.1","rate":30,"value":0}}});
        kids[2]["media_references"]["DEFAULT_MEDIA"]["target_url"] =
            "file:///elsewhere/b2.mov".into();
    });
    let (back, w) = import_otio(&edited, &ImportOptions::default()).unwrap();
    let orig = &p.tracks[0].clips;
    let now = &back.tracks[0].clips;
    // Shorter first clip: 1s..3s now.
    let c0 = now[0].as_media().unwrap();
    assert_eq!((c0.source_in_us, c0.source_out_us), (1000000, 3000000));
    // The transition was removed natively, so it is gone although the stored clip had it.
    let c1 = now[1].as_media().unwrap();
    assert!(c1.outgoing_transition.is_none());
    assert!(orig[1].as_media().unwrap().outgoing_transition.is_some());
    // Relinked: a new media item at the new path; the clip keeps the rest of its data.
    let m = back.find_media(c1.media_id).unwrap();
    assert_eq!(m.path.to_string_lossy(), "/elsewhere/b2.mov");
    assert_ne!(c1.media_id, orig[1].as_media().unwrap().media_id);
    assert_eq!(c1.speed, 2.0);
    // Untouched clips still equal the originals exactly, ids included.
    assert_eq!(now[2].id(), orig[2].id());
    assert!(w.iter().all(|x| x.kind != WarningKind::UnknownSchema));
    back.validate().unwrap();
}

#[test]
fn stripping_renderly_metadata_still_gives_a_sane_project() {
    let p = rich_project();
    let otio = export_otio(&p).unwrap();
    let stripped = edit(&otio, |v| {
        fn strip(v: &mut Value) {
            match v {
                Value::Object(m) => {
                    if m.contains_key("metadata") {
                        m.insert("metadata".into(), serde_json::json!({}));
                    }
                    m.iter_mut()
                        .filter(|(k, _)| *k != "metadata")
                        .for_each(|(_, c)| strip(c));
                }
                Value::Array(a) => a.iter_mut().for_each(strip),
                _ => {}
            }
        }
        strip(v);
    });
    let (back, w) = import_otio(&stripped, &ImportOptions::default()).unwrap();
    back.validate().unwrap();
    assert_eq!(back.settings.fps.as_f64(), 30.0);
    assert_eq!(back.tracks.len(), p.tracks.len());
    // Native structure survives: same clip counts per track, speed from the time warp,
    // captions recognised from their generator, the crossfade from SMPTE_Dissolve.
    for (a, b) in p.tracks.iter().zip(&back.tracks) {
        assert_eq!(a.kind, b.kind, "{}", a.name);
        let na = a
            .clips
            .iter()
            .filter(|c| !matches!(c, Clip::Adjustment(_)))
            .count();
        let nb = b
            .clips
            .iter()
            .filter(|c| !matches!(c, Clip::Adjustment(_)))
            .count();
        assert_eq!(na, nb, "{}", a.name);
    }
    let sped = back.tracks[0].clips[1].as_media().unwrap();
    assert_eq!(sped.speed, 2.0);
    assert_eq!((sped.start_us, sped.source_in_us), (4500000, 1000000));
    assert!((sped.source_out_us - 5000000).abs() < 1);
    assert_eq!(
        sped.outgoing_transition
            .as_ref()
            .map(|t| (t.item_ref.as_str(), t.duration_us)),
        Some(("builtin:crossfade@1", 500000))
    );
    // The wipe is a Custom_Transition without Renderly metadata, but its name is the ref, so it
    // is recovered without approximation.
    assert!(!w.iter().any(|x| x.kind == WarningKind::Transition));
    let loaded = back.tracks[0].clips[2].as_media().unwrap();
    assert_eq!(
        loaded
            .outgoing_transition
            .as_ref()
            .map(|t| t.item_ref.as_str()),
        Some("builtin:wipe_left@1")
    );
    match &back.tracks[3].clips[0] {
        Clip::Caption(c) => {
            assert_eq!(c.body.text, "Hello, \"world\" \u{2014} caf\u{e9}");
            assert_eq!((c.start_us, c.duration_us), (500000, 1500000));
        }
        other => panic!("expected a caption, got {other:?}"),
    }
    // Sequences come back as sequences (the same one twice becomes two without metadata).
    assert_eq!(back.sequences.len(), 2);
}

#[test]
fn export_errors_are_reported_not_panicked() {
    let mut p = rich_project();
    p.settings.fps = renderly_core::time::Fps::new(0, 1);
    assert!(export_otio(&p).is_err());
    let mut p = rich_project();
    p.media.retain(|m| m.kind != MediaKind::Audio);
    assert!(export_otio(&p).is_err(), "dangling media reference");
}

#[test]
fn text_clips_markers_and_transcripts_map_as_documented() {
    let p = rich_project();
    let (back, warnings, otio) = roundtrip(&p);
    assert!(
        warnings
            .iter()
            .all(|w| w.kind != WarningKind::MarkersIgnored),
        "{warnings:?}"
    );
    let v: Value = serde_json::from_str(&otio).unwrap();

    // Markers: Marker.2 on the root stack, zero-length at the marker's frame.
    let markers = v["tracks"]["markers"].as_array().unwrap();
    assert_eq!(markers.len(), 2);
    assert_eq!(markers[0]["OTIO_SCHEMA"], "Marker.2");
    assert_eq!(markers[0]["name"], "intro");
    assert_eq!(markers[0]["color"], "RED");
    assert_eq!(markers[0]["marked_range"]["duration"]["value"], 0);
    assert_eq!(markers[0]["marked_range"]["start_time"]["value"], 30);
    // The beat marker has no colour of its own: the kind's default.
    assert_eq!(markers[1]["color"], "CYAN");
    // ...and its time off the frame grid comes back exactly.
    assert_eq!(back.markers, p.markers);
    assert_eq!(back.transcripts, p.transcripts);

    // Text clips: a generator clip with the slot effects, a native transition, the exact clip
    // in metadata; the track keeps its magnetic flag.
    let titles = v["tracks"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Titles")
        .unwrap();
    assert_eq!(titles["metadata"]["renderly"]["track"]["magnetic"], true);
    let kids = titles["children"].as_array().unwrap();
    let first = &kids[0];
    assert_eq!(first["name"], "EPISODE 12");
    let generator = &first["media_references"]["DEFAULT_MEDIA"];
    assert_eq!(generator["generator_kind"], "renderly.text");
    assert_eq!(generator["parameters"]["style_ref"], "builtin:tiktok-box@1");
    assert_eq!(first["effects"][0]["effect_name"], "builtin:blur@1");
    assert_eq!(kids[1]["OTIO_SCHEMA"], "Transition.1");
    assert_eq!(kids[2]["enabled"], false);
    assert!(first["metadata"]["renderly"]["text"].is_object());
    // The caption's words and style are in its snapshot and its track carries the default style.
    let caps = v["tracks"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "Captions")
        .unwrap();
    assert_eq!(
        caps["metadata"]["renderly"]["track"]["caption_style"]["ref"],
        "builtin:tiktok-box@1"
    );
    let caption = &caps["children"][1]["metadata"]["renderly"]["caption"];
    assert_eq!(caption["body"]["words"][0]["text"], "Hello,");
    assert_eq!(caption["animations"]["in"]["ref"], "builtin:fade-up@1");
}

#[test]
fn foreign_markers_and_bare_text_generators_import() {
    let otio = r#"{
        "OTIO_SCHEMA": "Timeline.1", "name": "t",
        "global_start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24, "value": 0 },
        "tracks": { "OTIO_SCHEMA": "Stack.1", "name": "tracks",
          "markers": [
            { "OTIO_SCHEMA": "Marker.2", "name": "cut here", "color": "GREEN",
              "marked_range": { "OTIO_SCHEMA": "TimeRange.1",
                "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24, "value": 48 },
                "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24, "value": 0 } } }
          ],
          "children": [
            { "OTIO_SCHEMA": "Track.1", "kind": "Video", "name": "T", "children": [
              { "OTIO_SCHEMA": "Clip.2", "name": "Hi",
                "source_range": { "OTIO_SCHEMA": "TimeRange.1",
                  "start_time": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24, "value": 0 },
                  "duration": { "OTIO_SCHEMA": "RationalTime.1", "rate": 24, "value": 48 } },
                "media_references": { "DEFAULT_MEDIA": {
                  "OTIO_SCHEMA": "GeneratorReference.1", "name": "text",
                  "generator_kind": "renderly.text",
                  "parameters": { "text": "Hi", "style_ref": "builtin:tiktok-box@1" } } },
                "active_media_reference_key": "DEFAULT_MEDIA" }
            ] }
          ] } }"#;
    let (p, warnings) = import_otio(otio, &ImportOptions::default()).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(p.markers.len(), 1);
    assert_eq!(p.markers[0].time_us, 2_000_000);
    assert_eq!(p.markers[0].label, "cut here");
    assert_eq!(p.markers[0].color, "#43A047");
    assert_eq!(p.markers[0].kind, MarkerKind::User);
    let Clip::Text(t) = &p.tracks[0].clips[0] else {
        panic!("{:?}", p.tracks[0].clips)
    };
    assert_eq!(t.body.text, "Hi");
    assert_eq!(t.style.item_ref, "builtin:tiktok-box@1");
    assert_eq!(t.duration_us, 2_000_000);
}
