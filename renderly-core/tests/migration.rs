//! Schema migrations: every committed fixture (v1..v7) loads at the current schema, with the
//! expected integer-µs values and rational frame rate (docs/schema-v8-design.md section 6).

use renderly_core::project::{Clip, Project, SCHEMA_VERSION};
use renderly_core::time::Fps;
use std::path::PathBuf;

fn load(version: u32) -> (Project, renderly_core::project::migrate::MigrationReport) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/projects")
        .join(format!("v{version}.renderly.json"));
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    Project::from_json_with_report(&data).unwrap_or_else(|e| panic!("v{version}: {e}"))
}

fn video_clips(p: &Project, track: usize) -> Vec<&renderly_core::project::MediaClip> {
    p.tracks[track]
        .clips
        .iter()
        .filter_map(Clip::as_media)
        .collect()
}

#[test]
fn current_schema_is_eight() {
    assert_eq!(SCHEMA_VERSION, 8);
}

#[test]
fn v1_migrates_seconds_to_micros() {
    let (p, report) = load(1);
    assert_eq!(report.from_version, 1);
    assert!(report.migrated());
    assert_eq!(p.schema_version, 8);
    assert_eq!(p.settings.fps, Fps::new(30, 1));
    assert_eq!(p.media[0].duration_us, Some(10_000_000));
    let clip = video_clips(&p, 0)[0];
    assert_eq!(clip.start_us, 1_500_000);
    assert_eq!(clip.source_in_us, 500_000);
    assert_eq!(clip.source_out_us, 2_500_000);
    let caption = match &p.tracks[2].clips[0] {
        Clip::Caption(c) => c,
        other => panic!("expected a caption, got {other:?}"),
    };
    assert_eq!(caption.start_us, 250_000);
    assert_eq!(caption.duration_us, 1_750_000);
}

#[test]
fn v2_migrates_keyframes_and_handles() {
    let (p, report) = load(2);
    assert_eq!(report.from_version, 2);
    let clip = video_clips(&p, 0)[0];
    let keys = &clip.keyframes[0].keys;
    assert_eq!(keys[0].time_us, 0);
    assert_eq!(keys[1].time_us, 1_000_000);
    assert_eq!(keys[1].handle_left, Some((-330_000, 0.0)));
    assert_eq!(keys[1].handle_right, Some((330_000, 0.0)));
    // Non-time values are untouched.
    assert_eq!(clip.transform.x, 0.1);
    assert_eq!(clip.effects[0].item_ref, "builtin:blur@1");
}

#[test]
fn v3_migrates_transitions() {
    let (p, report) = load(3);
    assert_eq!(report.from_version, 3);
    let clips = video_clips(&p, 0);
    let tr = clips[0].outgoing_transition.as_ref().expect("transition");
    assert_eq!(tr.duration_us, 500_000);
    assert_eq!(clips[1].start_us, 1_500_000);
    assert_eq!(clips[1].source_in_us, 2_000_000);
}

#[test]
fn v4_migrates_speed_clips() {
    let (p, report) = load(4);
    assert_eq!(report.from_version, 4);
    let clip = video_clips(&p, 0)[0];
    assert_eq!(clip.start_us, 500_000);
    assert_eq!(clip.source_in_us, 1_000_000);
    assert_eq!(clip.source_out_us, 5_000_000);
    assert_eq!(clip.speed, 2.0);
    // 4 s of source at 2x is 2 s on the timeline.
    assert_eq!(clip.timeline_duration_us(), 2_000_000);
}

#[test]
fn v5_migrates_speed_keyframes() {
    let (p, report) = load(5);
    assert_eq!(report.from_version, 5);
    let clip = video_clips(&p, 0)[0];
    let keys = &clip.keyframes[0].keys;
    assert_eq!(keys[0].time_us, 0);
    assert_eq!(keys[1].time_us, 2_000_000);
    assert_eq!(keys[1].value, 2.0);
}

#[test]
fn v6_migrates_matte_frames_and_multicam_offsets() {
    let (p, report) = load(6);
    assert_eq!(report.from_version, 6);
    let clip = video_clips(&p, 0)[0];
    let frames = clip
        .background_removal
        .as_ref()
        .and_then(|b| b.matte_frames.as_ref())
        .expect("matte frames");
    assert_eq!(frames.start_us, 500_000);
    assert_eq!(frames.fps, Fps::new(24, 1));
    assert_eq!(frames.count, 4);
    assert_eq!(p.multicam_groups[0].sync_offsets_us, vec![250_000]);
}

#[test]
fn v7_migrates_every_time_field_and_snaps_frame_positions() {
    let (p, report) = load(7);
    assert_eq!(report.from_version, 7);
    assert_eq!(
        report.steps,
        vec![
            "v7->v8:time",
            "v7->v8:effects",
            "v7->v8:transitions",
            "v7->v8:easing",
            "v7->v8:packs",
            "v7->v8:captions"
        ]
    );
    // 25 fps: 1.0004 s is 0.4 ms from frame 25 (1 s), so it snaps; 2.9996 s snaps to 3 s.
    assert_eq!(p.settings.fps, Fps::new(25, 1));
    let clips = video_clips(&p, 0);
    assert_eq!(clips[0].start_us, 1_000_000);
    assert_eq!(clips[0].source_out_us, 2_000_000);
    assert_eq!(clips[1].start_us, 3_000_000);
    // The transition duration is a frame-positioned time too.
    let tr = clips[0].outgoing_transition.as_ref().expect("transition");
    assert_eq!(tr.duration_us, 500_000);
    // Keyframe times are rounded, not snapped: 1.0015 s stays 1_001_500 µs.
    let keys = &clips[0].keyframes[0].keys;
    assert_eq!(keys[1].time_us, 1_001_500);
    assert_eq!(keys[0].handle_left, Some((-100_000, 0.0)));
    assert_eq!(keys[0].handle_right, Some((200_000, 0.1)));

    // Adjustment layer, caption, fades: all in µs.
    let adj = match &p.tracks[1].clips[0] {
        Clip::Adjustment(a) => a,
        other => panic!("expected an adjustment, got {other:?}"),
    };
    assert_eq!(adj.start_us, 500_000);
    assert_eq!(adj.duration_us, 2_000_000);
    let caption = match &p.tracks[3].clips[0] {
        Clip::Caption(c) => c,
        other => panic!("expected a caption, got {other:?}"),
    };
    assert_eq!(caption.start_us, 250_000);
    let audio = p.tracks[2].clips[0].as_media().expect("audio");
    assert_eq!(audio.fade_in_us, 250_000);
    assert_eq!(audio.fade_out_us, 500_000);

    // Nested sequence content.
    let inner = &p.sequences[0].tracks[0].clips[0];
    let inner = inner.as_media().expect("nested clip");
    assert_eq!(inner.source_in_us, 3_000_000);
    assert_eq!(inner.source_out_us, 4_500_000);

    // Pack paths became the transitional list.
    assert_eq!(p.legacy_pack_paths.len(), 1);
}

#[test]
fn migrated_projects_save_as_current_and_load_without_migrating() {
    for version in 1..=7 {
        let (p, _) = load(version);
        let json = serde_json::to_string(&p).expect("serialise");
        let (again, report) = Project::from_json_with_report(&json).expect("reload");
        assert!(!report.migrated(), "v{version} re-migrated after save");
        assert_eq!(again.schema_version, 8);
        assert_eq!(again.settings.fps, p.settings.fps);
    }
}

#[test]
fn newer_files_are_refused() {
    let json = r#"{"schema_version": 9, "id": "00000000-0000-0000-0000-000000000000", "name": "x", "settings": {"fps": {"num": 30, "den": 1}, "width": 1, "height": 1, "sample_rate": 48000}, "media": [], "tracks": []}"#;
    assert!(Project::from_json(json).is_err());
}

// ---- v7 -> v8: effects, transitions, easing, packs (docs/schema-v8-design.md section 6) ------

use renderly_core::project::migrate::{legacy_effect_ref, migrate_to_current};
use serde_json::{json, Value};

fn effects_fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/projects/v7_effects.renderly.json");
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&data).expect("fixture parses")
}

/// The v7 fixture before and after the migration, as JSON.
fn migrated_effects_fixture() -> (Value, Value) {
    let before = effects_fixture();
    let (after, report) = migrate_to_current(before.clone()).expect("migrates");
    assert_eq!(report.from_version, 7);
    (before, after)
}

fn track<'a>(project: &'a Value, name: &str) -> &'a Value {
    project["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == name)
        .unwrap_or_else(|| panic!("no track {name}"))
}

fn first_clip<'a>(project: &'a Value, track_name: &str) -> &'a Value {
    &track(project, track_name)["clips"][0]
}

/// The refs a v8 clip runs, in the order it runs them: filter, key, adjust, effects[].
fn run_order(clip: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for slot in ["filter", "key", "adjust"] {
        if let Some(e) = clip.get(slot).filter(|e| !e.is_null()) {
            out.push(e["ref"].as_str().unwrap().to_string());
        }
    }
    for e in clip["effects"].as_array().into_iter().flatten() {
        out.push(e["ref"].as_str().unwrap().to_string());
    }
    out
}

fn v7_order(clip: &Value) -> Vec<String> {
    clip["effects"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|e| legacy_effect_ref(e["effect_id"].as_str().unwrap()))
        .collect()
}

#[test]
fn effect_ids_become_versioned_refs_and_the_run_order_never_changes() {
    let (before, after) = migrated_effects_fixture();
    assert!(
        !after.to_string().contains("effect_id"),
        "no v7 effect_id is left"
    );
    let mut checked = 0;
    for (b, a) in before["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .zip(after["tracks"].as_array().unwrap())
    {
        for (bc, ac) in b["clips"]
            .as_array()
            .unwrap()
            .iter()
            .zip(a["clips"].as_array().unwrap())
        {
            if bc["effects"].is_null() {
                continue;
            }
            assert_eq!(
                run_order(ac),
                v7_order(bc),
                "track {}: v7 ran the list in order; v8 must run the same refs in the same order",
                b["name"]
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 11,
        "fixture covers the reorder cases ({checked})"
    );
}

#[test]
fn a_leading_lut_becomes_the_filter_and_a_leading_chroma_key_becomes_the_key() {
    let (_, after) = migrated_effects_fixture();

    // [LUT, chroma_key, blur] -> filter, key, effects[blur].
    let c = first_clip(&after, "lut_key_blur");
    assert_eq!(c["filter"]["ref"], "pack:local.fx/warm@1");
    assert_eq!(c["filter"]["params"], json!({ "intensity": 0.8 }));
    assert_eq!(c["filter"]["enabled"], true);
    assert_eq!(c["key"]["ref"], "builtin:chroma_key@1");
    assert_eq!(c["key"]["params"], json!({ "tolerance": 0.4 }));
    assert_eq!(c["effects"].as_array().unwrap().len(), 1);
    assert_eq!(c["effects"][0]["ref"], "builtin:blur@1");
    assert_eq!(c["effects"][0]["params"], json!({ "radius": 6.0 }));

    // Instance ids survive (keyframes and hover preview address them).
    assert_eq!(c["filter"]["id"], "e7000000-0000-4000-8000-0000000003e9");

    // [chroma_key, LUT]: the key moves; the LUT is not first, so it stays in effects[].
    let c = first_clip(&after, "key_then_lut");
    assert!(c.get("filter").is_none());
    assert_eq!(c["key"]["ref"], "builtin:chroma_key@1");
    assert_eq!(c["effects"][0]["ref"], "pack:local.fx/warm@1");

    // [blur, LUT, chroma_key]: nothing is first, everything stays in effects[] in order.
    let c = first_clip(&after, "blur_lut_key");
    assert!(c.get("filter").is_none() && c.get("key").is_none());
    let refs: Vec<&str> = c["effects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["ref"].as_str().unwrap())
        .collect();
    assert_eq!(
        refs,
        [
            "builtin:blur@1",
            "pack:local.fx/warm@1",
            "builtin:chroma_key@1"
        ]
    );

    // [LUT, blur, LUT]: only the first LUT is the filter.
    let c = first_clip(&after, "lut_blur_lut");
    assert_eq!(c["filter"]["ref"], "pack:local.fx/warm@1");
    assert_eq!(c["effects"][0]["ref"], "builtin:blur@1");
    assert_eq!(c["effects"][1]["ref"], "pack:local.fx/cool@1");
    assert_eq!(c["effects"][1]["params"], json!({ "intensity": 0.5 }));

    // A disabled leading LUT is still the filter, disabled.
    let c = first_clip(&after, "disabled_lut_first");
    assert_eq!(c["filter"]["enabled"], false);
    assert_eq!(c["effects"][0]["ref"], "builtin:glitch@1");

    // A pack id that already has a publisher keeps it; builtin LUT effects are plain effects.
    let c = first_clip(&after, "dotted_pack_lut");
    assert_eq!(c["filter"]["ref"], "pack:mira.looks/print@1");
    let c = first_clip(&after, "lut_contrast");
    assert!(c.get("filter").is_none());
    assert_eq!(c["effects"][0]["ref"], "builtin:lut_contrast@1");
}

#[test]
fn wasm_effects_stay_in_the_effects_list_and_adjustment_layers_have_no_key() {
    let (_, after) = migrated_effects_fixture();
    let c = first_clip(&after, "wasm_adjust");
    assert!(c.get("filter").is_none() && c.get("key").is_none());
    assert_eq!(c["effects"][0]["ref"], "wasm:gain");
    assert_eq!(c["effects"][1]["ref"], "builtin:color_adjust@1");
    let audio = first_clip(&after, "A1");
    assert_eq!(audio["effects"][0]["ref"], "wasm:gain");

    // Adjustment layer: a chroma key first stays in effects[] (no key slot there) ...
    let adjust = track(&after, "adjust")["clips"].as_array().unwrap().clone();
    assert!(adjust[0].get("key").is_none());
    assert_eq!(adjust[0]["effects"][0]["ref"], "builtin:chroma_key@1");
    // ... and a LUT first is the filter.
    assert_eq!(adjust[1]["filter"]["ref"], "pack:local.fx/warm@1");
    assert_eq!(adjust[1]["effects"][0]["ref"], "builtin:blur@1");
}

#[test]
fn every_transition_kind_becomes_a_builtin_ref() {
    let (before, after) = migrated_effects_fixture();
    let kinds: Vec<&str> = track(&before, "transitions")["clips"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["outgoing_transition"]["kind"].as_str())
        .collect();
    assert_eq!(kinds.len(), 10);
    let clips = track(&after, "transitions")["clips"].as_array().unwrap();
    for (i, kind) in kinds.iter().enumerate() {
        assert_eq!(
            clips[i]["outgoing_transition"],
            json!({
                "ref": format!("builtin:{kind}@1"),
                "duration_us": 500000,
                "params": {},
                "easing": null,
            }),
            "transition {kind}"
        );
    }
    assert!(clips[10].get("outgoing_transition").is_none());
    // The migrated refs are the builtin transition items.
    let known = renderly_core::compose::builtins::transition_ids();
    for kind in kinds {
        assert!(known.contains(&kind), "{kind} is a builtin transition");
    }
}

fn approx(a: &Value, b: f64) {
    let a = a.as_f64().unwrap();
    assert!((a - b).abs() < 1e-9, "{a} != {b}");
}

#[test]
fn ease_in_and_ease_out_become_exact_bezier_handles() {
    let (_, after) = migrated_effects_fixture();
    let tracks = first_clip(&after, "eased")["keyframes"].as_array().unwrap();
    let by_prop = |p: &str| tracks.iter().find(|t| t["property"] == p).unwrap();

    // opacity: ease_in from 0 -> 1 over 1 s: control points (1/3, 0) and (2/3, 1/3).
    let keys = by_prop("opacity")["keys"].as_array().unwrap();
    assert_eq!(keys[0]["easing"], "bezier");
    assert_eq!(keys[0]["time_us"], 0);
    assert_eq!(keys[0]["handle_right"][0], 333_333);
    approx(&keys[0]["handle_right"][1], 0.0);
    assert_eq!(keys[1]["handle_left"][0], -333_333);
    approx(&keys[1]["handle_left"][1], -2.0 / 3.0);
    assert_eq!(keys[1]["easing"], "linear");

    // pos_x: ease_out over 1.5 s then ease_in over 0.5 s, scaled to each span.
    let keys = by_prop("pos_x")["keys"].as_array().unwrap();
    assert_eq!(keys[0]["easing"], "bezier");
    assert_eq!(keys[0]["handle_right"][0], 500_000);
    approx(&keys[0]["handle_right"][1], 2.0 / 3.0); // dv = 1.0
    assert_eq!(keys[1]["handle_left"][0], -500_000);
    approx(&keys[1]["handle_left"][1], 0.0);
    assert_eq!(keys[1]["easing"], "bezier");
    assert_eq!(keys[1]["handle_right"][0], 166_667);
    approx(&keys[1]["handle_right"][1], 0.0);
    assert_eq!(keys[2]["handle_left"][0], -166_667);
    approx(&keys[2]["handle_left"][1], 1.0 / 3.0); // -2/3 * dv, dv = -0.5

    // ease_in_out is not one cubic: it keeps the legacy variant and gains no handles.
    let keys = by_prop("scale_x")["keys"].as_array().unwrap();
    assert_eq!(keys[0]["easing"], "ease_in_out");
    assert!(keys[0].get("handle_right").is_none());

    // An explicit bezier keeps its handles (time offsets now in microseconds).
    let keys = by_prop("rotation")["keys"].as_array().unwrap();
    assert_eq!(keys[0]["easing"], "bezier");
    assert_eq!(keys[0]["handle_right"], json!([400_000, 5.0]));
    assert_eq!(keys[1]["handle_left"], json!([-400_000, 0.0]));

    // The result is a valid v8 project: the whole fixture deserialises.
    let data = serde_json::to_string(&after).unwrap();
    Project::from_json(&data).expect("migrated fixture loads");
}

#[test]
fn migrated_ease_in_and_out_evaluate_to_the_v7_formulas() {
    // The acceptance test for "exact": sample the migrated track against the v7 maths.
    use renderly_core::project::evaluate_transform;
    let (_, after) = migrated_effects_fixture();
    let data = serde_json::to_string(&after).unwrap();
    let project = Project::from_json(&data).unwrap();
    let clip = project
        .tracks
        .iter()
        .find(|t| t.name == "eased")
        .and_then(|t| t.clips.first())
        .and_then(Clip::as_media)
        .unwrap()
        .clone();
    for step in 0..=100 {
        let u = step as f64 / 100.0;
        // opacity: ease_in 0 -> 1 over 1 s.
        let t_us = (u * 1_000_000.0) as i64;
        let v = evaluate_transform(&clip, clip.start_us + t_us).opacity;
        assert!(
            (v - u * u).abs() < 2e-3,
            "ease_in at {u}: got {v}, v7 gave {}",
            u * u
        );
        // pos_x: ease_out -0.5 -> 0.5 over 1.5 s.
        let t_us = (u * 1_500_000.0) as i64;
        let v = evaluate_transform(&clip, clip.start_us + t_us).x;
        let expected = -0.5 + (1.0 - (1.0 - u) * (1.0 - u));
        assert!(
            (v - expected).abs() < 2e-3,
            "ease_out at {u}: got {v}, v7 gave {expected}"
        );
    }
}

#[test]
fn pack_and_plugin_paths_move_to_the_transitional_list() {
    let (_, after) = migrated_effects_fixture();
    assert!(after.get("asset_pack_paths").is_none());
    assert!(after.get("wasm_plugin_paths").is_none());
    // Order kept, duplicates (packs/fx is in both v7 lists) collapsed.
    assert_eq!(
        after["legacy_pack_paths"],
        json!(["packs/fx", "packs/mira.looks", "plugins/gain"])
    );
    // Core never touches the library: loading is pure.
    let (p, report) =
        Project::from_json_with_report(&serde_json::to_string(&effects_fixture()).unwrap())
            .expect("loads");
    assert!(report.steps.contains(&"v7->v8:packs"));
    assert_eq!(p.legacy_pack_paths.len(), 3);
}

#[test]
fn a_migrated_project_saves_as_v8_with_no_v7_fields() {
    let (p, _) =
        Project::from_json_with_report(&serde_json::to_string(&effects_fixture()).unwrap())
            .expect("loads");
    let saved = serde_json::to_string(&p).unwrap();
    for gone in [
        "effect_id",
        "\"kind\":\"crossfade\"",
        "asset_pack_paths",
        "wasm_plugin_paths",
        "ease_in\"",
        "ease_out\"",
    ] {
        assert!(!saved.contains(gone), "saved file still has {gone}");
    }
    let (again, report) = Project::from_json_with_report(&saved).unwrap();
    assert!(!report.migrated());
    assert_eq!(
        serde_json::to_value(&again).unwrap(),
        serde_json::to_value(&p).unwrap()
    );
}

#[test]
fn slot_split_preserves_order_for_every_short_effect_list() {
    // Exhaustive over lists of up to four effects drawn from the cases that matter to the
    // split: a pack LUT, the chroma key, a plain effect and a WASM plugin.
    use renderly_core::project::migrate::split_legacy_effect_slots;
    let kinds = [
        "pack:fx:lut:warm",
        "builtin:chroma_key",
        "builtin:blur",
        "wasm:gain",
    ];
    let mut lists: Vec<Vec<usize>> = vec![vec![]];
    for len in 1..=4 {
        let mut next = Vec::new();
        for prefix in lists.iter().filter(|l| l.len() == len - 1) {
            for k in 0..kinds.len() {
                let mut l = prefix.clone();
                l.push(k);
                next.push(l);
            }
        }
        lists.extend(next);
    }
    assert_eq!(lists.len(), 1 + 4 + 16 + 64 + 256);
    for list in lists {
        let v7: Vec<Value> = list
            .iter()
            .enumerate()
            .map(|(i, k)| {
                json!({ "id": format!("{i}"), "effect_id": kinds[*k], "enabled": true, "params": {} })
            })
            .collect();
        for allow_key in [true, false] {
            let (filter, key, rest) = split_legacy_effect_slots(&v7, allow_key);
            let mut run: Vec<String> = Vec::new();
            run.extend(
                filter
                    .iter()
                    .map(|e| e["ref"].as_str().unwrap().to_string()),
            );
            run.extend(key.iter().map(|e| e["ref"].as_str().unwrap().to_string()));
            run.extend(rest.iter().map(|e| e["ref"].as_str().unwrap().to_string()));
            let expected: Vec<String> = list.iter().map(|k| legacy_effect_ref(kinds[*k])).collect();
            assert_eq!(run, expected, "{list:?} (allow_key {allow_key})");
            if let Some(f) = &filter {
                assert!(f["ref"].as_str().unwrap().starts_with("pack:local.fx/"));
                assert_eq!(list[0], 0, "only a leading LUT is the filter");
            }
            if let Some(k) = &key {
                assert_eq!(k["ref"], "builtin:chroma_key@1");
                assert!(allow_key);
            }
        }
    }
}

// ---- captions sub-step ------------------------------------------------------------------

fn captions_fixture() -> Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/projects/v7_captions.renderly.json");
    let data = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
    serde_json::from_str(&data).expect("fixture parses")
}

#[test]
fn captions_become_bodies_and_a_shared_style_moves_to_the_track() {
    let (after, report) = migrate_to_current(captions_fixture()).expect("migrates");
    assert!(report.steps.contains(&"v7->v8:captions"));

    // Every track gains `magnetic: false` (no surprise ripples in old projects); the project
    // gains empty markers and transcripts.
    for t in after["tracks"].as_array().unwrap() {
        assert_eq!(t["magnetic"], json!(false), "track {}", t["name"]);
    }
    assert_eq!(after["markers"], json!([]));
    assert_eq!(after["transcripts"], json!([]));

    // All clips share `tiktok-box`: the style moves to the track, the clips inherit.
    let shared = track(&after, "Shared style");
    assert_eq!(
        shared["caption_style"],
        json!({ "ref": "builtin:tiktok-box@1", "params": {}, "overrides": {} })
    );
    assert_eq!(
        shared["clips"][0],
        json!({
            "type": "caption",
            "id": "c8000000-0000-4000-8000-000000000002",
            "body": { "text": "Hello there", "runs": [], "words": [] },
            "start_us": 500_000,
            "duration_us": 1_500_000,
            "style": null,
            "animations": {},
        })
    );
    assert_eq!(shared["clips"][1]["start_us"], json!(2_000_000));
    assert!(shared["clips"][1].get("text").is_none());
    assert!(shared["clips"][1].get("style_id").is_none());
}

#[test]
fn mixed_styles_stay_on_the_clips_and_odd_ids_become_valid_refs() {
    let (after, _) = migrate_to_current(captions_fixture()).expect("migrates");
    let mixed = track(&after, "Mixed styles");
    assert!(mixed.get("caption_style").is_none());
    assert_eq!(
        mixed["clips"][0]["style"],
        json!({ "ref": "builtin:tiktok-minimal@1", "params": {}, "overrides": {} })
    );
    // "My Pack Style" is not a valid item id: lower-cased, spaces become dashes.
    assert_eq!(
        mixed["clips"][1]["style"]["ref"],
        json!("builtin:my-pack-style@1")
    );
    // An empty caption track has nothing to share.
    assert!(track(&after, "Empty").get("caption_style").is_none());
}

#[test]
fn the_migrated_caption_project_loads_and_resolves_its_styles() {
    let data = serde_json::to_string(&captions_fixture()).unwrap();
    let (p, _) = Project::from_json_with_report(&data).expect("loads");
    assert!(p.tracks.iter().all(|t| !t.magnetic));
    let shared = &p.tracks[1];
    assert_eq!(
        shared.caption_style.as_ref().map(|s| s.item_ref.as_str()),
        Some("builtin:tiktok-box@1")
    );
    // A clip without a style draws with its track's style; the render id is the item id.
    let active = renderly_core::compose::eval::active_captions(&p, 1_000_000);
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].text, "Hello there");
    assert_eq!(active[0].style_id, "tiktok-box");
    // Mixed track: each clip carries its own.
    let active = renderly_core::compose::eval::active_captions(&p, 5_500_000);
    assert_eq!(active[0].style_id, "tiktok-minimal");
}

#[test]
fn a_schema_8_file_with_v7_captions_is_upgraded_and_a_current_one_is_left_alone() {
    // Written by a development build: version 8 time and effects, v7 caption fields.
    let (migrated, _) = migrate_to_current(captions_fixture()).unwrap();
    let mut dev = migrated.clone();
    for t in dev["tracks"].as_array_mut().unwrap() {
        let track_style = t.get("caption_style").cloned();
        for c in t["clips"].as_array_mut().unwrap() {
            if c["type"] == "caption" {
                let text = c["body"]["text"].clone();
                let style = track_style
                    .as_ref()
                    .map_or_else(|| c["style"]["ref"].clone(), |s| s["ref"].clone());
                let id = style
                    .as_str()
                    .unwrap()
                    .replace("builtin:", "")
                    .replace("@1", "");
                let obj = c.as_object_mut().unwrap();
                obj.remove("body");
                obj.remove("style");
                obj.remove("animations");
                obj.insert("text".into(), text);
                obj.insert("style_id".into(), json!(id));
            }
        }
        t.as_object_mut().unwrap().remove("caption_style");
    }
    let (upgraded, report) = migrate_to_current(dev).unwrap();
    assert_eq!(report.from_version, 8);
    assert_eq!(report.steps, vec!["v7->v8:captions"]);
    assert_eq!(upgraded["tracks"], migrated["tracks"]);
    // A current file has nothing to do.
    let (same, report) = migrate_to_current(migrated.clone()).unwrap();
    assert!(!report.migrated());
    assert_eq!(same, migrated);
}
