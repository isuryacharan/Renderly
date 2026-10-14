//! End-to-end tests that execute the built `renderly` binary.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_renderly");

/// Unique temp dir, removed on drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("renderly-cli-it-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn run<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Output {
    Command::new(BIN).args(args).output().expect("run renderly")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn new_project(dir: &TempDir, extra: &[&str]) -> PathBuf {
    let p = dir.path("p.renderly.json");
    let mut args: Vec<&std::ffi::OsStr> = vec!["new-project".as_ref(), p.as_os_str()];
    args.extend(extra.iter().map(|s| std::ffi::OsStr::new(*s)));
    let out = run(&args);
    assert!(out.status.success(), "new-project failed: {}", stderr(&out));
    p
}

fn read_json(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn apply(p: &Path, cmd: &str) -> Output {
    run(&["apply".as_ref(), p.as_os_str(), cmd.as_ref()])
}

fn assert_fails_unchanged(p: &Path, out: &Output, needle: &str) {
    let before = std::fs::read(p).unwrap();
    assert!(!out.status.success(), "expected failure");
    let err = stderr(out);
    assert!(err.contains(needle), "stderr missing {needle:?}: {err}");
    assert_eq!(before, std::fs::read(p).unwrap(), "file changed");
}

#[test]
fn new_project_defaults() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let v = read_json(&p);
    assert_eq!(v["name"], "untitled");
    assert_eq!(v["settings"]["width"], 1080);
    assert_eq!(v["settings"]["height"], 1920);
    assert_eq!(v["settings"]["fps"], 60.0);
    assert_eq!(v["settings"]["sample_rate"], 48000);
    assert_eq!(
        v["schema_version"],
        renderly_core::project::SCHEMA_VERSION as u64
    );
    // Must load as a real Project.
    let project: renderly_core::project::Project = serde_json::from_value(v).unwrap();
    assert!(project.tracks.is_empty());
}

#[test]
fn new_project_explicit_options() {
    let dir = TempDir::new();
    let p = new_project(
        &dir,
        &[
            "--name",
            "my edit",
            "--width",
            "640",
            "--height",
            "360",
            "--fps",
            "24",
            "--sample-rate",
            "44100",
        ],
    );
    let v = read_json(&p);
    assert_eq!(v["name"], "my edit");
    assert_eq!(v["settings"]["width"], 640);
    assert_eq!(v["settings"]["height"], 360);
    assert_eq!(v["settings"]["fps"], 24.0);
    assert_eq!(v["settings"]["sample_rate"], 44100);
}

#[test]
fn new_project_rejects_bad_args() {
    let dir = TempDir::new();
    let p = dir.path("x.json");
    let out = run(&[
        "new-project".as_ref(),
        p.as_os_str(),
        "--width".as_ref(),
        "abc".as_ref(),
    ]);
    assert!(!out.status.success());
    assert!(!p.exists());
}

#[test]
fn apply_add_track_mutates_file_and_prints_outcome() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let out = apply(&p, r#"{"command":"AddTrack","kind":"video","name":"V1"}"#);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("TrackAdded"), "{}", stdout(&out));
    let v = read_json(&p);
    assert_eq!(v["tracks"].as_array().unwrap().len(), 1);
    assert_eq!(v["tracks"][0]["name"], "V1");
    assert!(stdout(&out).contains(v["tracks"][0]["id"].as_str().unwrap()));
}

#[test]
fn apply_invalid_json_fails_unchanged() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let out = apply(&p, "{not json");
    assert_fails_unchanged(&p, &out, "parsing command JSON");
}

#[test]
fn apply_unknown_command_fails_unchanged() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let out = apply(&p, r#"{"command":"Explode"}"#);
    assert_fails_unchanged(&p, &out, "parsing command JSON");
}

#[test]
fn apply_core_failure_fails_unchanged() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let missing = uuid::Uuid::new_v4().to_string();
    let cmd = json!({
        "command": "AddClip",
        "track_id": missing,
        "media_id": uuid::Uuid::new_v4().to_string(),
        "position_secs": 0.0,
        "source_in_secs": 0.0,
        "source_out_secs": 1.0,
    })
    .to_string();
    let out = apply(&p, &cmd);
    assert_fails_unchanged(&p, &out, "command failed");
}

#[test]
fn apply_missing_project_fails() {
    let dir = TempDir::new();
    let p = dir.path("nope.json");
    let out = apply(&p, r#"{"command":"AddTrack","kind":"video","name":"V1"}"#);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("reading"), "{}", stderr(&out));
    assert!(!p.exists());
}

#[test]
fn apply_script_applies_in_order() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let script = dir.path("s.json");
    std::fs::write(
        &script,
        r#"[{"command":"AddTrack","kind":"video","name":"first"},
            {"command":"AddTrack","kind":"audio","name":"second"}]"#,
    )
    .unwrap();
    let out = run(&["apply-script".as_ref(), p.as_os_str(), script.as_os_str()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).matches("TrackAdded").count(), 2);
    let v = read_json(&p);
    let tracks = v["tracks"].as_array().unwrap();
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0]["name"], "first");
    assert_eq!(tracks[1]["name"], "second");
}

#[test]
fn apply_script_failure_midway_leaves_file_unchanged() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let script = dir.path("s.json");
    let bad = json!({
        "command": "AddClip",
        "track_id": uuid::Uuid::new_v4().to_string(),
        "media_id": uuid::Uuid::new_v4().to_string(),
        "position_secs": 0.0,
        "source_in_secs": 0.0,
        "source_out_secs": 1.0,
    });
    let cmds = json!([
        {"command":"AddTrack","kind":"video","name":"V1"},
        bad,
        {"command":"AddTrack","kind":"video","name":"V2"},
    ]);
    std::fs::write(&script, cmds.to_string()).unwrap();
    let out = run(&["apply-script".as_ref(), p.as_os_str(), script.as_os_str()]);
    assert_fails_unchanged(&p, &out, "command failed");
    // The first command did run (reported) but was not persisted.
    assert!(stdout(&out).contains("TrackAdded"));
    assert!(read_json(&p)["tracks"].as_array().unwrap().is_empty());
}

#[test]
fn apply_script_malformed_fails_unchanged() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let script = dir.path("s.json");

    std::fs::write(&script, "not json").unwrap();
    let out = run(&["apply-script".as_ref(), p.as_os_str(), script.as_os_str()]);
    assert_fails_unchanged(&p, &out, "parsing script JSON");

    // An object instead of an array is also malformed.
    std::fs::write(
        &script,
        r#"{"command":"AddTrack","kind":"video","name":"V1"}"#,
    )
    .unwrap();
    let out = run(&["apply-script".as_ref(), p.as_os_str(), script.as_os_str()]);
    assert_fails_unchanged(&p, &out, "parsing script JSON");

    // Missing script file.
    let out = run(&[
        "apply-script".as_ref(),
        p.as_os_str(),
        dir.path("missing.json").as_os_str(),
    ]);
    assert_fails_unchanged(&p, &out, "reading");
}

#[test]
fn show_round_trips_to_file() {
    let dir = TempDir::new();
    let p = new_project(&dir, &["--name", "shown"]);
    let out = apply(&p, r#"{"command":"AddTrack","kind":"audio","name":"A1"}"#);
    assert!(out.status.success());
    let out = run(&["show".as_ref(), p.as_os_str()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("\n  "), "expected pretty-printed JSON");
    let shown: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(shown, read_json(&p));
    assert_eq!(shown["name"], "shown");
}

#[test]
fn show_missing_or_corrupt_file_fails() {
    let dir = TempDir::new();
    let out = run(&["show".as_ref(), dir.path("nope.json").as_os_str()]);
    assert!(!out.status.success());
    let bad = dir.path("bad.json");
    std::fs::write(&bad, "garbage").unwrap();
    let out = run(&["show".as_ref(), bad.as_os_str()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("parsing project JSON"));
}

#[test]
fn export_unknown_preset_fails() {
    let dir = TempDir::new();
    let p = new_project(&dir, &[]);
    let o = dir.path("out.mp4");
    let out = run(&[
        "export".as_ref(),
        p.as_os_str(),
        o.as_os_str(),
        "--preset".as_ref(),
        "vimeo".as_ref(),
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown preset"), "{}", stderr(&out));
    assert!(!o.exists());
}

#[test]
fn export_empty_timeline_fails() {
    let dir = TempDir::new();
    let p = new_project(&dir, &["--fps", "10"]);
    let o = dir.path("out.mp4");
    let out = run(&["export".as_ref(), p.as_os_str(), o.as_os_str()]);
    assert!(!out.status.success());
    // Without ffmpeg the failure is "not found" instead; still non-zero.
    if renderly_core::media::ffmpeg_available() {
        assert!(
            stderr(&out).contains("no enabled video clips"),
            "{}",
            stderr(&out)
        );
    }
    assert!(!o.exists());
}

#[test]
fn export_real_video() {
    if !renderly_core::media::ffmpeg_available() {
        eprintln!("skipping export test: ffmpeg not on PATH");
        return;
    }
    let dir = TempDir::new();
    let src = dir.path("src.mp4");
    let status = Command::new(renderly_core::media::ffmpeg_path().unwrap())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=30",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&src)
        .status()
        .expect("ffmpeg");
    assert!(status.success());

    // Presets fix the output size, so keep it cheap with a low project fps.
    let p = new_project(&dir, &["--fps", "10"]);
    let out = apply(
        &p,
        &json!({"command":"ImportMedia","path":src.to_string_lossy()}).to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let out = apply(&p, r#"{"command":"AddTrack","kind":"video","name":"V1"}"#);
    assert!(out.status.success(), "{}", stderr(&out));

    let v = read_json(&p);
    let media_id = v["media"][0]["id"].as_str().unwrap().to_string();
    let track_id = v["tracks"][0]["id"].as_str().unwrap().to_string();
    let out = apply(
        &p,
        &json!({
            "command": "AddClip",
            "track_id": track_id,
            "media_id": media_id,
            "position_secs": 0.0,
            "source_in_secs": 0.0,
            "source_out_secs": 0.5,
        })
        .to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let o = dir.path("out.mp4");
    let out = run(&["export".as_ref(), p.as_os_str(), o.as_os_str()]);
    if !out.status.success() {
        if stderr(&out).contains("no suitable GPU adapter") {
            eprintln!("skipping export test: no GPU adapter");
            return;
        }
        panic!("export failed: {}", stderr(&out));
    }
    assert!(stdout(&out).contains("exported"), "{}", stdout(&out));
    assert!(std::fs::metadata(&o).unwrap().len() > 0);
}

/// Schema v7 end to end: nest clips into a sequence, add an adjustment layer, export.
#[test]
fn nest_clips_adjustment_layer_and_export() {
    if !renderly_core::media::ffmpeg_available() {
        eprintln!("skipping v7 export test: ffmpeg not on PATH");
        return;
    }
    let dir = TempDir::new();
    let src = dir.path("src.mp4");
    let status = Command::new(renderly_core::media::ffmpeg_path().unwrap())
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc=duration=1:size=320x240:rate=30",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&src)
        .status()
        .expect("ffmpeg");
    assert!(status.success());

    let p = new_project(&dir, &["--fps", "10"]);
    let (t1, t2, t3) = (
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    // Tracks with caller-supplied ids so a script can reference them.
    let script = dir.path("setup.json");
    std::fs::write(
        &script,
        json!([
            {"command":"ImportMedia","path":src.to_string_lossy()},
            {"command":"AddTrack","kind":"video","name":"V1","id":t1},
            {"command":"AddTrack","kind":"video","name":"V2","id":t2},
            {"command":"AddTrack","kind":"video","name":"FX","id":t3},
        ])
        .to_string(),
    )
    .unwrap();
    let out = run(&["apply-script".as_ref(), p.as_os_str(), script.as_os_str()]);
    assert!(out.status.success(), "{}", stderr(&out));
    let media_id = read_json(&p)["media"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();

    let mut clip_ids = Vec::new();
    for (track, pos) in [(&t1, 0.0), (&t2, 0.2)] {
        let out = apply(
            &p,
            &json!({
                "command": "AddClip", "track_id": track, "media_id": media_id,
                "position_secs": pos, "source_in_secs": 0.0, "source_out_secs": 0.5,
            })
            .to_string(),
        );
        assert!(out.status.success(), "{}", stderr(&out));
        let v = read_json(&p);
        let track_json = v["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == *track)
            .unwrap();
        clip_ids.push(track_json["clips"][0]["id"].as_str().unwrap().to_string());
    }

    // Nest both clips: they move into a new sequence and one clip replaces them.
    let out = apply(
        &p,
        &json!({
            "command": "NestClips",
            "name": "Combo",
            "clips": [
                {"track_id": t1, "clip_id": clip_ids[0]},
                {"track_id": t2, "clip_id": clip_ids[1]},
            ],
        })
        .to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("ClipsNested"), "{}", stdout(&out));
    let v = read_json(&p);
    assert_eq!(
        v["schema_version"],
        renderly_core::project::SCHEMA_VERSION as u64
    );
    assert_eq!(v["sequences"].as_array().unwrap().len(), 1);
    assert_eq!(v["sequences"][0]["name"], "Combo");
    assert_eq!(v["sequences"][0]["tracks"].as_array().unwrap().len(), 2);
    let seq_media = v["media"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["kind"] == "sequence")
        .expect("a sequence media item");
    assert_eq!(seq_media["sequence_id"], v["sequences"][0]["id"]);

    // An adjustment layer over the whole thing, desaturating it.
    let out = apply(
        &p,
        &json!({"command":"AddAdjustmentLayer","track_id":t3,"position_secs":0.0,"duration_secs":0.7})
            .to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));
    let v = read_json(&p);
    let fx_track = v["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == t3)
        .unwrap();
    assert_eq!(fx_track["clips"][0]["type"], "adjustment");
    let adj_id = fx_track["clips"][0]["id"].as_str().unwrap().to_string();
    let out = apply(
        &p,
        &json!({
            "command": "SetClipEffects", "track_id": t3, "clip_id": adj_id,
            "effects": [{
                "id": uuid::Uuid::new_v4().to_string(),
                "effect_id": "builtin:color_adjust",
                "params": {"saturation": 0.0}
            }],
        })
        .to_string(),
    );
    assert!(out.status.success(), "{}", stderr(&out));

    let o = dir.path("out.mp4");
    let out = run(&["export".as_ref(), p.as_os_str(), o.as_os_str()]);
    if !out.status.success() {
        if stderr(&out).contains("no suitable GPU adapter") {
            eprintln!("skipping v7 export test: no GPU adapter");
            return;
        }
        panic!("export failed: {}", stderr(&out));
    }
    assert!(stdout(&out).contains("exported"), "{}", stdout(&out));
    assert!(std::fs::metadata(&o).unwrap().len() > 0);

    // A sequence still placed by a clip cannot be deleted; the failed command changes nothing.
    let seq_id = v["sequences"][0]["id"].as_str().unwrap().to_string();
    let out = apply(
        &p,
        &json!({"command":"DeleteSequence","sequence_id":seq_id}).to_string(),
    );
    assert_fails_unchanged(&p, &out, "still placed");
}

// ---- OpenTimelineIO interop (docs/otio.md) --------------------------------------------

fn project_with_clip(dir: &TempDir) -> PathBuf {
    let p = new_project(dir, &["--fps", "30", "--width", "1280", "--height", "720"]);
    let mut v = read_json(&p);
    let media_id = uuid::Uuid::new_v4().to_string();
    v["media"] = json!([{
        "id": media_id, "path": "clips/a clip.mp4", "kind": "video",
        "duration_secs": 10.0, "width": 1280, "height": 720, "fps": 30.0
    }]);
    std::fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let out = apply(&p, r#"{"command":"AddTrack","kind":"video","name":"V1"}"#);
    assert!(out.status.success(), "{}", stderr(&out));
    let track = read_json(&p)["tracks"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (pos, a, b) in [(0.0, 1.0, 3.0), (4.0, 0.0, 2.5)] {
        let cmd = json!({"command": "AddClip", "track_id": track, "media_id": media_id,
            "position_secs": pos, "source_in_secs": a, "source_out_secs": b});
        let out = apply(&p, &cmd.to_string());
        assert!(out.status.success(), "{}", stderr(&out));
    }
    p
}

#[test]
fn export_otio_then_import_otio_roundtrips_a_project() {
    let dir = TempDir::new();
    let p = project_with_clip(&dir);
    let otio = dir.path("edit.otio");
    let out = run(&["export-otio".as_ref(), p.as_os_str(), otio.as_os_str()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("exported"));
    let v = read_json(&otio);
    assert_eq!(v["OTIO_SCHEMA"], "Timeline.1");
    let kids = v["tracks"]["children"][0]["children"].as_array().unwrap();
    assert_eq!(kids.len(), 3, "clip, gap, clip");
    // The relative media path was resolved against the project's directory.
    let url = kids[0]["media_references"]["DEFAULT_MEDIA"]["target_url"]
        .as_str()
        .unwrap();
    assert!(
        url.starts_with("file://") && url.ends_with("/clips/a%20clip.mp4"),
        "{url}"
    );

    let back = dir.path("back.renderly.json");
    let out = run(&["import-otio".as_ref(), otio.as_os_str(), back.as_os_str()]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("imported"), "{}", stdout(&out));
    let a = read_json(&p);
    let b = read_json(&back);
    assert_eq!(a["tracks"], b["tracks"], "tracks and clips are identical");
    assert_eq!(a["settings"], b["settings"]);
    // Media: same id; the path is now the absolute one written into the file.
    assert_eq!(a["media"][0]["id"], b["media"][0]["id"]);
    assert!(b["media"][0]["path"]
        .as_str()
        .unwrap()
        .ends_with("clips/a clip.mp4"));
    renderly_core::project::Project::from_json(&std::fs::read_to_string(&back).unwrap()).unwrap();
}

#[test]
fn import_otio_reads_a_foreign_sample_and_reports_warnings() {
    let dir = TempDir::new();
    let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../renderly-core/tests/fixtures/otio/nested_example.otio");
    let out_path = dir.path("nested.renderly.json");
    let out = run(&[
        "import-otio".as_ref(),
        sample.as_os_str(),
        out_path.as_os_str(),
        "--width".as_ref(),
        "1080".as_ref(),
        "--height".as_ref(),
        "1920".as_ref(),
    ]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stderr(&out).contains("warning:"),
        "warnings go to stderr: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("3 sequences"), "{}", stdout(&out));
    let v = read_json(&out_path);
    assert_eq!(v["settings"]["width"], 1080);
    assert_eq!(v["settings"]["height"], 1920);
    assert_eq!(v["settings"]["fps"], 24.0);
    assert_eq!(v["sequences"].as_array().unwrap().len(), 3);
    assert_eq!(
        v["schema_version"],
        renderly_core::project::SCHEMA_VERSION as u64
    );
}

#[test]
fn import_otio_rejects_non_otio_and_writes_nothing() {
    let dir = TempDir::new();
    let bad = dir.path("bad.otio");
    std::fs::write(&bad, r#"{"hello": "world"}"#).unwrap();
    let out_path = dir.path("never.renderly.json");
    let out = run(&[
        "import-otio".as_ref(),
        bad.as_os_str(),
        out_path.as_os_str(),
    ]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("OpenTimelineIO"), "{}", stderr(&out));
    assert!(!out_path.exists());
    let out = run(&[
        "import-otio".as_ref(),
        dir.path("missing.otio").as_os_str(),
        out_path.as_os_str(),
    ]);
    assert!(!out.status.success());
}
