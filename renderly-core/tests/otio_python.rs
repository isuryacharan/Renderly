//! Validation against the real OpenTimelineIO library (Python bindings).
//!
//! Every test here is `#[ignore]`d: it needs a Python with `opentimelineio` installed
//! (`pip install opentimelineio`). Run them with
//!
//! ```text
//! cargo test -p renderly-core --test otio_python -- --ignored
//! ```
//!
//! Set `RENDERLY_OTIO_PYTHON` to the interpreter to use (default: `python`). Without the
//! library the tests print a notice and pass vacuously, so `--ignored` never fails just
//! because Python is missing.

use renderly_core::otio::{export_otio, import_otio, ImportOptions};
use renderly_core::project::*;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

mod common;

fn python() -> String {
    std::env::var("RENDERLY_OTIO_PYTHON").unwrap_or_else(|_| "python".to_string())
}

fn has_otio() -> bool {
    Command::new(python())
        .args(["-I", "-c", "import opentimelineio"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("renderly-otio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn run(script: &str, args: &[&str]) -> String {
    let out = Command::new(python())
        .args(["-I", "-c", script])
        .args(args)
        .output()
        .expect("python runs");
    assert!(
        out.status.success(),
        "python failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Load a file with the real library; print per-track child counts by type and durations.
const SUMMARISE: &str = r#"
import sys, json, opentimelineio as otio
tl = otio.adapters.read_from_file(sys.argv[1])
def kinds(t):
    out = {}
    for c in t:
        n = type(c).__name__
        out[n] = out.get(n, 0) + 1
    return out
print(json.dumps({
    "duration": tl.duration().to_seconds(),
    "tracks": [{"kind": t.kind, "name": t.name, "children": len(t), "kinds": kinds(t),
                "duration": t.duration().to_seconds()} for t in tl.tracks],
}))
"#;

/// Read with the library and write it back (metadata must survive).
const REWRITE: &str = r#"
import sys, opentimelineio as otio
tl = otio.adapters.read_from_file(sys.argv[1])
otio.adapters.write_to_file(tl, sys.argv[2])
"#;

/// Author a timeline with the library: two clips at 24 fps, a gap, a dissolve, a nested stack.
const AUTHOR: &str = r#"
import sys, opentimelineio as otio
R = otio.opentime.RationalTime
TR = otio.opentime.TimeRange
def ref(url, n):
    return otio.schema.ExternalReference(target_url=url,
        available_range=TR(R(0, 24), R(n, 24)))
tl = otio.schema.Timeline(name="authored")
v = otio.schema.Track(name="V1", kind=otio.schema.TrackKind.Video)
v.append(otio.schema.Clip(name="a", media_reference=ref("file:///media/a.mov", 240),
    source_range=TR(R(24, 24), R(48, 24))))
v.append(otio.schema.Gap(source_range=TR(R(0, 24), R(12, 24))))
v.append(otio.schema.Clip(name="b", media_reference=ref("file:///C:/media/b%20c.mov", 240),
    source_range=TR(R(0, 24), R(72, 24))))
v.append(otio.schema.Transition(transition_type=otio.schema.TransitionTypes.SMPTE_Dissolve,
    in_offset=R(6, 24), out_offset=R(6, 24)))
v.append(otio.schema.Clip(name="c", media_reference=ref("file:///media/c.mov", 240),
    source_range=TR(R(0, 24), R(48, 24))))
inner = otio.schema.Stack(name="inner")
it = otio.schema.Track(kind=otio.schema.TrackKind.Video)
it.append(otio.schema.Clip(name="d", media_reference=ref("file:///media/d.mov", 240),
    source_range=TR(R(0, 24), R(24, 24))))
inner.append(it)
inner.source_range = TR(R(0, 24), R(24, 24))
v.append(inner)
tl.tracks.append(v)
otio.adapters.write_to_file(tl, sys.argv[1])
"#;

#[test]
#[ignore = "needs python with opentimelineio installed"]
fn the_real_library_loads_our_export_and_agrees_on_structure() {
    if !has_otio() {
        eprintln!(
            "opentimelineio is not installed for `{}`: skipping",
            python()
        );
        return;
    }
    let p = common::rich_project();
    let path = scratch("rich.otio");
    std::fs::write(&path, export_otio(&p).unwrap()).unwrap();
    let summary: Value = serde_json::from_str(&run(SUMMARISE, &[path.to_str().unwrap()])).unwrap();

    let fps = p.settings.fps;
    let tracks = summary["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), p.tracks.len());
    for (t, got) in p.tracks.iter().zip(tracks) {
        assert_eq!(got["name"], t.name.as_str());
        assert_eq!(
            got["kind"],
            if t.kind == TrackKind::Audio {
                "Audio"
            } else {
                "Video"
            }
        );
        // The library's track duration is the end of the last clip, in whole frames.
        let end = t.clips.iter().map(Clip::end_secs).fold(0.0, f64::max);
        let want = (end * fps + 0.5).floor() / fps;
        assert!(
            (got["duration"].as_f64().unwrap() - want).abs() < 1.5 / fps,
            "track {}: {} vs {}",
            t.name,
            got["duration"],
            want
        );
        // Every clip is a Clip/Stack child; the rest are Gaps and Transitions.
        let kinds = got["kinds"].as_object().unwrap();
        let non_gap: u64 = kinds
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "Gap" | "Transition"))
            .map(|(_, v)| v.as_u64().unwrap())
            .sum();
        let want_non_gap = t
            .clips
            .iter()
            .filter(|c| !matches!(c, Clip::Adjustment(_)))
            .count() as u64;
        assert_eq!(non_gap, want_non_gap, "track {}", t.name);
    }
    let end = p
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .map(Clip::end_secs)
        .fold(0.0, f64::max);
    assert!((summary["duration"].as_f64().unwrap() - end).abs() < 2.0 / fps);
}

#[test]
#[ignore = "needs python with opentimelineio installed"]
fn the_real_library_preserves_renderly_metadata_through_a_rewrite() {
    if !has_otio() {
        eprintln!(
            "opentimelineio is not installed for `{}`: skipping",
            python()
        );
        return;
    }
    let p = common::rich_project();
    let a = scratch("rich-in.otio");
    let b = scratch("rich-out.otio");
    std::fs::write(&a, export_otio(&p).unwrap()).unwrap();
    run(REWRITE, &[a.to_str().unwrap(), b.to_str().unwrap()]);
    let (back, _) = import_otio(
        &std::fs::read_to_string(&b).unwrap(),
        &ImportOptions::default(),
    )
    .expect("our importer reads the library's output");
    assert_eq!(
        serde_json::to_value(&p).unwrap(),
        serde_json::to_value(&back).unwrap(),
        "Renderly -> OTIO -> (real library rewrite) -> Renderly is lossless"
    );
}

#[test]
#[ignore = "needs python with opentimelineio installed"]
fn files_authored_by_the_real_library_import() {
    if !has_otio() {
        eprintln!(
            "opentimelineio is not installed for `{}`: skipping",
            python()
        );
        return;
    }
    let path = scratch("authored.otio");
    run(AUTHOR, &[path.to_str().unwrap()]);
    let (p, w) = import_otio(
        &std::fs::read_to_string(&path).unwrap(),
        &ImportOptions::default(),
    )
    .unwrap();
    p.validate().unwrap();
    assert_eq!(p.settings.fps, 24.0);
    assert_eq!(p.tracks.len(), 1);
    let clips: Vec<&MediaClip> = p.tracks[0]
        .clips
        .iter()
        .filter_map(Clip::as_media)
        .collect();
    assert_eq!(
        clips.len(),
        4,
        "a, b, c and the nested stack; warnings: {w:?}"
    );
    let near = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} != {b}");
    near(clips[0].source_in_secs, 1.0);
    near(clips[0].timeline_duration_secs(), 2.0);
    near(clips[1].position_secs, 2.5);
    near(clips[1].timeline_duration_secs(), 3.0);
    assert_eq!(
        p.find_media(clips[1].media_id)
            .unwrap()
            .path
            .to_string_lossy(),
        "C:/media/b c.mov"
    );
    // Dissolve of 6 + 6 frames = 0.5 s between b and c (shortened if it exceeds half a clip).
    let tr = clips[1].outgoing_transition.as_ref().expect("dissolve");
    near(tr.duration_secs, 0.5);
    near(clips[2].position_secs, 5.5);
    near(clips[3].position_secs, 7.5);
    assert_eq!(p.sequences.len(), 1);
    assert_eq!(p.sequences[0].name, "inner");
}
