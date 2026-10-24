//! End-to-end tests of in-process Robust Video Matting background removal.
//!
//! These need the RVM weights, which are never committed or bundled: point `RENDERLY_MODEL_DIR`
//! at a directory containing `rvm_mobilenetv3_fp32.onnx` (or install the model through
//! `renderly models download rvm-mobilenetv3-fp32`). Without them every test prints a note and
//! returns, so CI stays green. They also need `ffmpeg` on PATH.
//!
//! The clip is synthesised from the committed public-domain NASA portrait
//! (`assets/test-images/nasa_astronaut_portrait_400x500.jpg`) sliding across a flat lavfi
//! background, encoded losslessly (FFV1).

use renderly_core::commands::{apply_command, Command, CommandOutcome};
use renderly_core::ml::{RvmSession, RVM_MODEL_ID};
use renderly_core::project::{BackgroundRemoval, Id, Project, Settings, TrackKind};
use renderly_core::time::Time;
use renderly_core::{DecodeOptions, ExportSettings, FrameRenderer};
use std::path::{Path, PathBuf};

const W: u32 = 640;
const H: u32 = 360;
const FPS: f64 = 15.0;
const SECS: f64 = 2.0;
/// The portrait is scaled to this height on the canvas (400x500 -> 240x300).
const PORTRAIT_H: f64 = 300.0;
const SCALE: f64 = PORTRAIT_H / 500.0;

static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn skip_reason() -> Option<&'static str> {
    if !renderly_core::media::ffmpeg_available() {
        return Some("ffmpeg not available");
    }
    if !renderly_core::ml::models::is_available(RVM_MODEL_ID) {
        return Some("RVM weights not installed (set RENDERLY_MODEL_DIR)");
    }
    None
}

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("renderly-rvm-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).expect("tmp dir");
    d
}

fn ffmpeg(args: &[&str]) {
    let out = std::process::Command::new(renderly_core::media::ffmpeg_path().expect("ffmpeg"))
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .args(args)
        .output()
        .expect("spawn ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Portrait sliding right over a flat blue-grey background; FFV1 (lossless) in MKV.
fn synth_clip(dir: &Path) -> PathBuf {
    let portrait = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/test-images/nasa_astronaut_portrait_400x500.jpg");
    let out = dir.join("clip.mkv");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("color=c=0x4a6a8a:s={W}x{H}:r={FPS}:d={SECS}"),
        "-loop",
        "1",
        "-i",
        portrait.to_str().expect("utf8 path"),
        "-filter_complex",
        &format!("[1:v]scale=-2:{PORTRAIT_H}[p];[0:v][p]overlay=x='40+t*100':y=30:shortest=1"),
        "-c:v",
        "ffv1",
        "-pix_fmt",
        "yuv444p",
        "-t",
        &SECS.to_string(),
        out.to_str().expect("utf8 path"),
    ]);
    out
}

fn solid_png(dir: &Path, hex: &str) -> PathBuf {
    let out = dir.join("solid.png");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("color=c={hex}:s={W}x{H}"),
        "-frames:v",
        "1",
        out.to_str().expect("utf8 path"),
    ]);
    out
}

/// Canvas-space rectangle `(x0, y0, x1, y1)` of the part of the portrait that is the person's
/// torso/arms/prop (image-space 120..300 x 170..400) at source time `t`.
fn person_core(t: f64) -> (u32, u32, u32, u32) {
    let ox = 40.0 + 100.0 * t;
    let oy = 30.0;
    (
        (ox + 120.0 * SCALE) as u32,
        (oy + 170.0 * SCALE) as u32,
        (ox + 300.0 * SCALE) as u32,
        (oy + 400.0 * SCALE) as u32,
    )
}

/// Pure background to the right of the portrait.
fn background_strip(t: f64) -> (u32, u32, u32, u32) {
    let right = 40.0 + 100.0 * t + 400.0 * SCALE;
    ((right + 24.0) as u32, 20, W - 8, H - 20)
}

fn mean_alpha(matte: &[u8], width: u32, r: (u32, u32, u32, u32)) -> f64 {
    let (x0, y0, x1, y1) = r;
    let mut sum = 0u64;
    let mut n = 0u64;
    for y in y0..y1 {
        for x in x0..x1 {
            sum += u64::from(matte[(y * width + x) as usize]);
            n += 1;
        }
    }
    sum as f64 / n as f64 / 255.0
}

fn load_matte(dir: &Path, index: u32) -> (u32, u32, Vec<u8>) {
    let img = image::open(dir.join(renderly_core::project::MatteFrames::file_name(index)))
        .expect("matte frame")
        .to_luma8();
    (img.width(), img.height(), img.into_raw())
}

struct Scene {
    project: Project,
    track: Id,
    clip: Id,
}

fn matted_project(video: &Path) -> Scene {
    let mut project = Project::new(
        "rvm",
        Settings {
            fps: renderly_core::time::Fps::new(15, 1),
            width: W,
            height: H,
            ..Settings::default()
        },
    );
    let CommandOutcome::MediaImported { media_id } = apply_command(
        &mut project,
        Command::ImportMedia {
            path: video.to_string_lossy().into_owned(),
        },
    )
    .expect("import") else {
        panic!("expected MediaImported");
    };
    let CommandOutcome::TrackAdded { track_id } = apply_command(
        &mut project,
        Command::AddTrack {
            kind: TrackKind::Video,
            name: "V".into(),
            id: None,
            sequence_id: None,
            magnetic: Some(false),
        },
    )
    .expect("track") else {
        panic!("expected TrackAdded");
    };
    let CommandOutcome::ClipAdded { clip_id } = apply_command(
        &mut project,
        Command::AddClip {
            track_id,
            media_id,
            start: Time::Us(0),
            source_in: Time::Us(0),
            source_out: Time::Us((SECS * 1e6).round() as i64),
        },
    )
    .expect("clip") else {
        panic!("expected ClipAdded");
    };
    Scene {
        project,
        track: track_id,
        clip: clip_id,
    }
}

fn enable_rvm(scene: &mut Scene) {
    apply_command(
        &mut scene.project,
        Command::SetClipBackgroundRemoval {
            track_id: scene.track,
            clip_id: scene.clip,
            config: Some(BackgroundRemoval {
                enabled: true,
                model_id: "rvm".into(),
                threshold: 0.15,
                feather: 0.0,
                temporal: false,
                matte_cache_dir: None,
                matte_frames: None,
            }),
        },
    )
    .expect("set bg");
}

fn cfg(scene: &Scene) -> BackgroundRemoval {
    scene
        .project
        .find_track(scene.track)
        .and_then(|t| t.clips.iter().find(|c| c.id() == scene.clip))
        .and_then(|c| c.as_media())
        .and_then(|m| m.background_removal.clone())
        .expect("background removal config")
}

#[test]
fn rvm_matte_sequence_covers_person_and_clears_background() {
    if let Some(why) = skip_reason() {
        eprintln!("skipping rvm_matte_sequence_*: {why}");
        return;
    }
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmp("seq");
    let video = synth_clip(&dir);
    let mut scene = matted_project(&video);
    enable_rvm(&mut scene);

    let started = std::time::Instant::now();
    apply_command(
        &mut scene.project,
        Command::GenerateBackgroundMatte {
            track_id: scene.track,
            clip_id: scene.clip,
        },
    )
    .expect("generate matte");
    let elapsed = started.elapsed();

    let bg = cfg(&scene);
    let frames = bg.matte_frames.clone().expect("per-frame matte index");
    let cache = bg.matte_cache_dir.clone().expect("cache dir");
    assert_eq!((frames.width, frames.height), (W, H));
    assert_eq!(frames.fps.as_f64(), FPS);
    // The clip really has SECS*FPS frames; the plan allows one more but stops at end of stream.
    assert_eq!(frames.count, (SECS * FPS) as u32);
    assert!(cache.join("matte.png").is_file(), "representative matte");
    let sidecar = renderly_core::segmentation::read_sidecar(&cache).expect("sidecar");
    assert_eq!(sidecar.model_id, RVM_MODEL_ID);
    assert_eq!(sidecar.frame_count, frames.count);

    let bytes: u64 = (0..frames.count)
        .map(|i| {
            std::fs::metadata(cache.join(renderly_core::project::MatteFrames::file_name(i)))
                .map(|m| m.len())
                .unwrap_or(0)
        })
        .sum();
    eprintln!(
        "RVM bake: {} frames {W}x{H} in {:.2}s ({:.0} ms/frame), {} KiB total ({:.1} KiB/frame)",
        frames.count,
        elapsed.as_secs_f64(),
        elapsed.as_secs_f64() * 1000.0 / f64::from(frames.count),
        bytes / 1024,
        bytes as f64 / 1024.0 / f64::from(frames.count)
    );

    for t in [0.0_f64, 0.8, 1.6] {
        let idx = frames.index_at((t * 1e6).round() as i64);
        let (mw, _mh, matte) = load_matte(&cache, idx);
        let person = mean_alpha(&matte, mw, person_core(t));
        let backdrop = mean_alpha(&matte, mw, background_strip(t));
        eprintln!("t={t:.1}s frame {idx}: person coverage {person:.3}, background {backdrop:.3}");
        assert!(person > 0.85, "t={t}: person alpha {person}");
        assert!(backdrop < 0.05, "t={t}: background alpha {backdrop}");
    }
    // The subject moves: the matte frames at t=0 and t=1.6 are different images.
    let (_, _, first) = load_matte(&cache, 0);
    let (_, _, last) = load_matte(&cache, frames.index_at(1600000));
    assert_ne!(first, last);
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&cache).ok();
}

#[test]
fn rvm_state_carries_over_between_frames_and_cold_start_differs() {
    if let Some(why) = skip_reason() {
        eprintln!("skipping rvm_state_carries_over_*: {why}");
        return;
    }
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmp("state");
    let video = synth_clip(&dir);
    let mut reader = renderly_core::media::VideoReader::open(&video, 0.0).expect("open");
    let mut frames = Vec::new();
    for _ in 0..8 {
        frames.push(reader.read_frame().expect("read").expect("frame"));
    }

    // Warm run: frames 0..=7 through one session.
    let mut warm = RvmSession::new(W, H).expect("session");
    let mut warm_out = Vec::new();
    for f in &frames {
        warm_out.push(warm.process(&f.pixels).expect("process"));
    }
    // Cold start on frame 7 only.
    let mut cold = RvmSession::new(W, H).expect("session");
    let cold7 = cold.process(&frames[7].pixels).expect("process");
    let differing = warm_out[7]
        .iter()
        .zip(&cold7)
        .filter(|(a, b)| a != b)
        .count();
    eprintln!(
        "frame 7: warm vs cold differ on {differing} of {} pixels",
        cold7.len()
    );
    assert!(differing > 0, "recurrent state had no effect");

    // Determinism / reference: replaying 0..=7 in a fresh session reproduces frame 7 exactly,
    // and reset() really is a cold start.
    let mut replay = RvmSession::new(W, H).expect("session");
    let mut last = Vec::new();
    for f in &frames {
        last = replay.process(&f.pixels).expect("process");
    }
    assert_eq!(last, warm_out[7], "inference is deterministic");
    replay.reset();
    assert_eq!(
        replay.process(&frames[7].pixels).expect("process"),
        cold7,
        "reset() is a cold start"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn export_replaces_background_with_the_underlying_colour() {
    if let Some(why) = skip_reason() {
        eprintln!("skipping export_replaces_background_*: {why}");
        return;
    }
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmp("export");
    let video = synth_clip(&dir);
    let solid = solid_png(&dir, "0xff0000");

    // Bottom track: solid red still. Top track: the matted clip.
    let mut project = Project::new(
        "rvm-export",
        Settings {
            fps: renderly_core::time::Fps::new(15, 1),
            width: W,
            height: H,
            ..Settings::default()
        },
    );
    let import = |p: &mut Project, path: &Path| match apply_command(
        p,
        Command::ImportMedia {
            path: path.to_string_lossy().into_owned(),
        },
    )
    .expect("import")
    {
        CommandOutcome::MediaImported { media_id } => media_id,
        other => panic!("{other:?}"),
    };
    let red_media = import(&mut project, &solid);
    let clip_media = import(&mut project, &video);
    let add_track = |p: &mut Project| match apply_command(
        p,
        Command::AddTrack {
            kind: TrackKind::Video,
            name: "V".into(),
            id: None,
            sequence_id: None,
            magnetic: Some(false),
        },
    )
    .expect("track")
    {
        CommandOutcome::TrackAdded { track_id } => track_id,
        other => panic!("{other:?}"),
    };
    let bottom = add_track(&mut project);
    let top = add_track(&mut project);
    let add_clip = |p: &mut Project, track_id, media_id| match apply_command(
        p,
        Command::AddClip {
            track_id,
            media_id,
            start: Time::Us(0),
            source_in: Time::Us(0),
            source_out: Time::Us((SECS * 1e6).round() as i64),
        },
    )
    .expect("clip")
    {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        other => panic!("{other:?}"),
    };
    add_clip(&mut project, bottom, red_media);
    let clip = add_clip(&mut project, top, clip_media);
    let mut scene = Scene {
        project,
        track: top,
        clip,
    };
    enable_rvm(&mut scene);
    let settings = ExportSettings {
        width: W,
        height: H,
        fps: FPS,
        ..ExportSettings::default()
    };

    // Before baking: the live heuristic path is not RVM - skip; bake, then render.
    apply_command(
        &mut scene.project,
        Command::GenerateBackgroundMatte {
            track_id: top,
            clip_id: clip,
        },
    )
    .expect("generate matte");
    let cache = cfg(&scene).matte_cache_dir.expect("cache");

    let mut renderer = FrameRenderer::new(settings, DecodeOptions::default()).expect("renderer");
    let px = |rgba: &[u8], x: u32, y: u32| -> [u8; 4] {
        let i = ((y * W + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    };
    // Source frames for comparison (decoded straight from the lossless clip).
    for t in [0.0_f64, 0.8, 1.6] {
        let rgba = renderer
            .render(&scene.project, (t * 1e6).round() as i64)
            .expect("render");
        let (bx0, by0, bx1, by1) = background_strip(t);
        let bg = px(&rgba, (bx0 + bx1) / 2, (by0 + by1) / 2);
        let (cx0, cy0, cx1, cy1) = person_core(t);
        let body = px(&rgba, (cx0 + cx1) / 2, (cy0 + cy1) / 2);
        let mut reader = renderly_core::media::VideoReader::open(&video, t).expect("open");
        let src = reader.read_frame().expect("read").expect("frame");
        let si = (((cy0 + cy1) / 2 * W + (cx0 + cx1) / 2) * 4) as usize;
        let src_px = [src.pixels[si], src.pixels[si + 1], src.pixels[si + 2]];
        eprintln!("t={t:.1}s export: background px {bg:?} (want red), body px {body:?} vs source {src_px:?}");
        assert!(
            bg[0] > 240 && bg[1] < 12 && bg[2] < 12,
            "t={t}: background should show the red layer underneath, got {bg:?}"
        );
        for c in 0..3 {
            assert!(
                (i32::from(body[c]) - i32::from(src_px[c])).abs() <= 24,
                "t={t}: body should keep the source colour, got {body:?} vs {src_px:?}"
            );
        }
    }

    // A real export of the same project runs end to end and the result shows the same.
    let out = dir.join("out.mp4");
    renderly_core::export_project_with_settings(&scene.project, &out, settings, &mut |_| true)
        .expect("export");
    let mut reader = renderly_core::media::VideoReader::open(&out, 0.8).expect("open export");
    let f = reader.read_frame().expect("read").expect("frame");
    let (bx0, by0, bx1, by1) = background_strip(0.8);
    let i = ((((by0 + by1) / 2) * W + (bx0 + bx1) / 2) * 4) as usize;
    eprintln!("exported mp4 background px {:?}", &f.pixels[i..i + 3]);
    assert!(
        f.pixels[i] > 220 && f.pixels[i + 1] < 40 && f.pixels[i + 2] < 40,
        "exported background should be red"
    );
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&cache).ok();
}

/// Timing + size benchmark on a 10 s 1080p30 clip (not run by default; use
/// `cargo test --release -p renderly-core --test rvm_matting -- --ignored --nocapture`).
#[test]
#[ignore = "benchmark"]
fn rvm_bench_10s_1080p() {
    if let Some(why) = skip_reason() {
        eprintln!("skipping rvm_bench_10s_1080p: {why}");
        return;
    }
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tmp("bench");
    let portrait = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("assets/test-images/nasa_astronaut_portrait_400x500.jpg");
    let video = dir.join("bench.mp4");
    ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=0x4a6a8a:s=1920x1080:r=30:d=10",
        "-loop",
        "1",
        "-i",
        portrait.to_str().expect("utf8 path"),
        "-filter_complex",
        "[1:v]scale=-2:900[p];[0:v][p]overlay=x='100+t*100':y=90:shortest=1",
        "-c:v",
        "libx264",
        "-crf",
        "14",
        "-pix_fmt",
        "yuv420p",
        "-t",
        "10",
        video.to_str().expect("utf8 path"),
    ]);
    let mut scene = matted_project(&video);
    // `matted_project` uses the 2 s test range; widen the clip to the whole 10 s.
    for track in scene.project.tracks.iter_mut() {
        for clip in track.clips.iter_mut() {
            if let Some(m) = clip.as_media_mut() {
                m.source_out_us = 10_000_000;
            }
        }
    }
    enable_rvm(&mut scene);
    let started = std::time::Instant::now();
    apply_command(
        &mut scene.project,
        Command::GenerateBackgroundMatte {
            track_id: scene.track,
            clip_id: scene.clip,
        },
    )
    .expect("generate matte");
    let secs = started.elapsed().as_secs_f64();
    let bg = cfg(&scene);
    let frames = bg.matte_frames.clone().expect("frames");
    let cache = bg.matte_cache_dir.clone().expect("cache");
    let bytes: u64 = std::fs::read_dir(&cache)
        .expect("dir")
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum();
    eprintln!(
        "BENCH 10 s 1080p30: {} frames {}x{} in {secs:.1}s = {:.0} ms/frame, {:.2}x slower than realtime; matte dir {:.1} MiB ({:.1} MiB per minute)",
        frames.count,
        frames.width,
        frames.height,
        secs * 1000.0 / f64::from(frames.count),
        secs / 10.0,
        bytes as f64 / 1048576.0,
        bytes as f64 / 1048576.0 * 6.0
    );
    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&cache).ok();
}
