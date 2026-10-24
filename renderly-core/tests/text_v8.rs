//! Text clips and caption style resolution at the pixel level (schema v8 stage 3). Titles and
//! captions are drawn by the existing caption renderer, so a title in a style is the same
//! bitmap as a caption in that style; the title is then a layer (transform, blend mode) like
//! any other.
//!
//! GPU tests skip (return) when no wgpu adapter is available, like the other suites.

use renderly_core::commands::{apply_command, parse_command_json, CommandOutcome};
use renderly_core::export::{render_frame_at, ExportError, ExportSettings};
use renderly_core::project::{Id, Project, Settings};
use renderly_core::time::Fps;
use serde_json::{json, Value};

const W: u32 = 320;
const H: u32 = 240;

fn project() -> Project {
    Project::new(
        "text",
        Settings {
            fps: Fps::new(30, 1),
            width: W,
            height: H,
            sample_rate: 48_000,
            duck_db: -12.0,
        },
    )
}

fn ok(p: &mut Project, command: Value) -> CommandOutcome {
    let (cmd, _) = parse_command_json(command).expect("parses");
    apply_command(p, cmd).expect("applies")
}

fn add_track(p: &mut Project, kind: &str) -> Id {
    match ok(
        p,
        json!({ "command": "AddTrack", "kind": kind, "name": kind, "magnetic": false }),
    ) {
        CommandOutcome::TrackAdded { track_id } => track_id,
        o => panic!("{o:?}"),
    }
}

fn add_clip(p: &mut Project, command: Value) -> Id {
    match ok(p, command) {
        CommandOutcome::ClipAdded { clip_id } => clip_id,
        o => panic!("{o:?}"),
    }
}

/// The frame at 1 s, or `None` when this machine has no GPU adapter.
fn frame(p: &Project) -> Option<Vec<u8>> {
    let settings = ExportSettings {
        width: W,
        height: H,
        fps: 30.0,
        ..ExportSettings::default()
    };
    match render_frame_at(p, 1_000_000, settings) {
        Ok(px) => Some(px),
        Err(ExportError::Compose(renderly_core::compose::ComposeError::NoAdapter)) => {
            eprintln!("skipping: no wgpu adapter");
            None
        }
        Err(e) => panic!("{e}"),
    }
}

/// Pixels that are not the black background.
fn lit(px: &[u8]) -> usize {
    px.chunks(4)
        .filter(|c| c[0] > 40 || c[1] > 40 || c[2] > 40)
        .count()
}

/// Centroid x of the lit pixels.
fn centroid_x(px: &[u8]) -> f64 {
    let (mut sum, mut n) = (0.0_f64, 0.0_f64);
    for (i, c) in px.chunks(4).enumerate() {
        if c[0] > 40 || c[1] > 40 || c[2] > 40 {
            sum += (i as u32 % W) as f64;
            n += 1.0;
        }
    }
    sum / n.max(1.0)
}

fn title(p: &mut Project, track: Id, extra: Value) -> Id {
    let mut cmd = json!({ "command": "AddText", "track_id": track, "body": { "text": "Title" },
                           "start": 0, "duration": "3s" });
    cmd.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    add_clip(p, cmd)
}

#[test]
fn a_title_is_drawn_and_a_disabled_one_is_not() {
    let mut p = project();
    let v = add_track(&mut p, "video");
    let t = title(&mut p, v, json!({}));
    let Some(with) = frame(&p) else { return };
    assert!(lit(&with) > 200, "the title has pixels: {}", lit(&with));
    ok(
        &mut p,
        json!({ "command": "SetClipEnabled", "track_id": v, "clip_id": t, "enabled": false }),
    );
    let without = frame(&p).unwrap();
    assert_eq!(lit(&without), 0);
}

#[test]
fn a_title_in_a_style_is_the_same_picture_as_a_caption_in_that_style() {
    let mut p = project();
    let v = add_track(&mut p, "video");
    // No anchor override: the style's own placement, as a caption has.
    title(
        &mut p,
        v,
        json!({ "style": { "ref": "builtin:tiktok-box@1" } }),
    );
    let Some(as_title) = frame(&p) else { return };

    let mut q = project();
    let c = add_track(&mut q, "caption");
    add_clip(
        &mut q,
        json!({ "command": "AddCaption", "track_id": c, "body": { "text": "Title" },
                 "start": 0, "duration": "3s", "style": { "ref": "builtin:tiktok-box@1" } }),
    );
    let as_caption = frame(&q).unwrap();
    assert!(lit(&as_title) > 200);
    assert_eq!(as_title, as_caption, "same renderer, same bitmap");

    // A caption that inherits the same style from its track draws identically.
    let mut r = project();
    let c = add_track(&mut r, "caption");
    ok(
        &mut r,
        json!({ "command": "SetTrackCaptionStyle", "track_id": c,
                 "style": { "ref": "builtin:tiktok-box@1" } }),
    );
    add_clip(
        &mut r,
        json!({ "command": "AddCaption", "track_id": c, "body": { "text": "Title" },
                 "start": 0, "duration": "3s" }),
    );
    assert_eq!(frame(&r).unwrap(), as_caption);
}

#[test]
fn style_overrides_change_the_colour_and_the_placement() {
    let mut p = project();
    let v = add_track(&mut p, "video");
    let t = title(
        &mut p,
        v,
        json!({ "style": { "ref": "builtin:tiktok-minimal@1",
                             "overrides": { "fill": "#00FF00", "anchor": "top" } } }),
    );
    let Some(top) = frame(&p) else { return };
    // Green text: some pixel is clearly green and none is clearly red.
    let green = top
        .chunks(4)
        .filter(|c| c[1] > 200 && c[0] < 60 && c[2] < 60)
        .count();
    assert!(green > 100, "green pixels: {green}");
    // `top` puts the text in the upper part of the frame.
    let first_lit_row = top
        .chunks((W * 4) as usize)
        .position(|row| row.chunks(4).any(|c| c[1] > 200))
        .unwrap();
    assert!(first_lit_row < (H / 3) as usize, "row {first_lit_row}");
    ok(
        &mut p,
        json!({ "command": "SetTextStyle", "track_id": v, "clip_id": t,
                 "overrides": { "fill": "#0000FF", "anchor": "bottom" } }),
    );
    let bottom = frame(&p).unwrap();
    let blue = bottom
        .chunks(4)
        .filter(|c| c[2] > 200 && c[0] < 60 && c[1] < 60)
        .count();
    assert!(blue > 100, "blue pixels: {blue}");
    assert_eq!(
        bottom
            .chunks(4)
            .filter(|c| c[1] > 200 && c[0] < 60 && c[2] < 60)
            .count(),
        0
    );
}

#[test]
fn a_title_is_a_layer_it_moves_fades_and_blends() {
    let mut p = project();
    let v = add_track(&mut p, "video");
    let t = title(&mut p, v, json!({}));
    let Some(centred) = frame(&p) else { return };
    ok(
        &mut p,
        json!({ "command": "SetClipTransform", "track_id": v, "clip_id": t,
                 "transform": { "x": 0.5, "scale_x": 1.0, "scale_y": 1.0, "opacity": 1.0 } }),
    );
    let moved = frame(&p).unwrap();
    assert!(
        centroid_x(&moved) > centroid_x(&centred) + 20.0,
        "{} vs {}",
        centroid_x(&moved),
        centroid_x(&centred)
    );
    ok(
        &mut p,
        json!({ "command": "SetClipTransform", "track_id": v, "clip_id": t,
                 "transform": { "x": 0.0, "scale_x": 1.0, "scale_y": 1.0, "opacity": 0.4 } }),
    );
    let faded = frame(&p).unwrap();
    let peak = |px: &[u8]| px.chunks(4).map(|c| c[0]).max().unwrap();
    assert!(peak(&faded) < peak(&centred), "opacity dims the title");
    assert!(peak(&faded) > 60, "but it is still there");
    // A blend mode applies like on any layer: multiply over a black frame leaves black.
    ok(
        &mut p,
        json!({ "command": "SetClipTransform", "track_id": v, "clip_id": t,
                 "transform": { "x": 0.0, "scale_x": 1.0, "scale_y": 1.0, "opacity": 1.0 } }),
    );
    ok(
        &mut p,
        json!({ "command": "SetClipEffects", "track_id": v, "clip_id": t,
                 "blend_mode": "multiply" }),
    );
    assert_eq!(
        lit(&frame(&p).unwrap()),
        0,
        "white multiplied onto black is black"
    );
}
