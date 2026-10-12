//! Golden-frame + preview/export parity suite (docs/parity-board.md "Export pixel-parity
//! suite").
//!
//! # What this guards
//!
//! 1. **Golden frames.** Every scene below is rendered through the real export path,
//!    [`renderly_core::export::FrameRenderer`] (ffmpeg decode -> compose layers -> wgpu
//!    `Compositor` -> RGBA readback), at 320x180, and compared with a committed PNG under
//!    `tests/golden/`. Tolerances are per-scene ([`Tol`]) and failures report max / mean abs
//!    diff and the percentage of pixels over tolerance, and write the actual frame plus a diff
//!    image under `target/tmp/golden-failures/`.
//! 2. **Preview vs export parity.** The preview compositor (`renderly-wasm`) only builds for
//!    wasm32, so it cannot run under `cargo test`. Instead the layer-wiring that used to be
//!    duplicated between the two paths now lives in ONE target-independent place in
//!    `renderly-core` and both paths call it:
//!    - `compose::eval::active_steps` / `active_captions` (which layers and adjustment
//!      layers, source times, keyframed transforms, transition progress, nested sequences),
//!    - `compose::build_scene` (adjustment steps; recursion into nested sequences at their
//!      own resolution - the host only supplies each media layer's pixels),
//!    - `ComposeLayer::from_active` / `ComposeLayer::caption` (how an evaluated layer becomes
//!      a compositor layer),
//!    - `Compositor::preview_layer_mask` (which mask the preview composites with),
//!    - `mask::project_matte_requests` / `Compositor::set_matte_rgba` and
//!      `packs::referenced_pack_luts` / `parse_cube_text` / `LoadedPack::from_luts` (how
//!      file-backed mattes and pack `.cube` LUTs reach the compositor without a filesystem),
//!    - the `Compositor` itself (all WGSL: effects, masks, transitions, composite).
//!
//!    [`PreviewPipeline`] below is a native stand-in for `WasmCompositor::render`: it feeds
//!    the same pre-decoded RGBA through exactly those shared functions in the same order,
//!    uploads each layer into a `LayerSource::Texture` (what the wasm path does via
//!    `copy_external_image_to_texture`) on a device the *test* owns, and `render_caption`
//!    (builtin styles) instead of `render_caption_for_project`. Host-supplied data follows
//!    the wasm flow: the stand-in "host" reads each matte / `.cube` file from disk as raw
//!    bytes (the Tauri command), decodes the PNG to RGBA (the browser's `createImageBitmap`)
//!    or takes the `.cube` text, and registers it through the SAME core entry points
//!    `WasmCompositor::set_matte` / `set_pack_lut` call - never through the compositor's
//!    native `image::open` fallback. It then asserts the result matches `FrameRenderer`.
//!
//!    **Covered**: layer selection, transform/effect/mask/transition wiring, texture-source
//!    vs frame-source compositing, captions via the builtin rasterizer, raster / generated
//!    mattes and pack `.cube` LUTs fed fs-free.
//!    **Not covered** here: background removal without a baked matte cache (export builds a
//!    live heuristic matte from CPU pixels; the preview has none - documented gap).
//!    **Not covered** (needs a browser): `copy_external_image_to_texture` colour handling,
//!    browser video decode, the surface blit (RT -> swapchain format, linear resample),
//!    `resolve_layer_source`'s `#incoming` element keys, and the texture/caption caches.
//!
//! # Determinism
//!
//! Sources are small images generated in-test from integer math. They go through ffmpeg
//! because `FrameRenderer` decodes everything via the ffmpeg CLI, but as **FFV1 (lossless)
//! Matroska** looped from the PNG - bit-exact by spec, so no lossy codec (x264) version can
//! move a pixel. A guard test asserts the decoded frame equals the source PNG exactly. Raw `.png`
//! stills are covered separately by `still_image_media_renders_for_its_whole_span`.
//!
//! Captions use the bundled Roboto Bold (`renderly-wasm/assets`) via `RENDERLY_FONT_PATH`, so
//! the glyphs are identical on every OS (and are the very font the preview embeds).
//!
//! # Environment
//!
//! - `RENDERLY_UPDATE_GOLDEN=1` rewrites `tests/golden/*.png` (only files whose pixels changed).
//! - `RENDERLY_GOLDEN_PREVIEW_BACKENDS=vulkan|dx12|gl` forces the preview stand-in's wgpu
//!   backend (default: all), handy for measuring cross-backend drift.
//! - Skips (eprintln + return, like the in-crate GPU tests) when ffmpeg or a wgpu adapter is
//!   unavailable.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};

use image::{GrayImage, Luma, Rgba, RgbaImage};
use renderly_core::captions::render_caption;
use renderly_core::commands::ExportPreset;
use renderly_core::compose::{
    build_scene, eval, upload_layer_pixels, ActiveLayer, ComposeError, ComposeLayer, Compositor,
    LayerSource, SceneError, SceneHost, SceneStep, BUILTIN_EFFECT_IDS,
};
use renderly_core::export::{DecodeOptions, ExportSettings, FrameRenderer};
use renderly_core::mask::project_matte_requests;
use renderly_core::media::{ffmpeg_available, ffmpeg_path};
use renderly_core::packs::{parse_cube_text, referenced_pack_luts, CubeLut, LoadedPack};
use renderly_core::project::{
    AdjustmentClip, AnimProperty, BackgroundRemoval, CaptionClip, Clip, ClipMask, ClipMaskKind,
    ClipTransform, ClipTransition, Easing, EffectInstance, Keyframe, KeyframeTrack, MediaClip,
    MediaItem, MediaKind, Project, Sequence, Settings, Track, TrackKind, TransitionKind,
};
use uuid::Uuid;

const W: u32 = 320;
const H: u32 = 180;

// ---------------------------------------------------------------------------------------
// Tolerances & metrics
// ---------------------------------------------------------------------------------------

/// A pixel "differs" when any channel's abs diff exceeds `channel`. The comparison fails when
/// more than `max_frac` of the pixels differ, or any single channel diff exceeds `hard_max`.
#[derive(Clone, Copy, Debug)]
struct Tol {
    channel: u8,
    max_frac: f64,
    hard_max: u8,
    /// Compare `block`x`block` block means instead of single pixels (1 = per pixel). Used only
    /// for shaders whose look is a `fract(sin(..)*43758)` hash (glitch, blur_dissolve): those
    /// flip individual pixels / slices differently on different GPUs, but the block average
    /// is stable. W and H must be divisible by `block`.
    block: u32,
}

/// Golden default. Same-backend runs are bit-exact (observed 0 diff); the slack is for other
/// GPU backends (DX12 here; Vulkan/lavapipe/Metal on CI) whose bilinear / blur / float
/// rounding differs by a few LSB. 3 LSB per channel is invisible to the eye but a real
/// regression (wrong transform, missing effect, shifted transition) moves thousands of
/// pixels by tens of LSB, which the 0.1% (57 px of 57,600) budget and the 48 hard cap catch.
const GOLDEN_TOL: Tol = Tol {
    channel: 3,
    max_frac: 0.001,
    hard_max: 48,
    block: 1,
};

/// Preview-style vs export: same shaders, same adapter, same inputs -> expected bit-exact.
/// 1 LSB / 0.05% only leaves room for driver nondeterminism, so wiring drift (a dropped
/// effect, wrong opacity, missing mask) cannot hide.
const PARITY_TOL: Tol = Tol {
    channel: 1,
    max_frac: 0.0005,
    hard_max: 8,
    block: 1,
};

/// For the sin-hash shaders (see [`Tol::block`]): 10x10 block means, +-24 per channel.
const HASH_NOISE_TOL: Tol = Tol {
    channel: 24,
    max_frac: 0.05,
    hard_max: 96,
    block: 10,
};

#[derive(Debug)]
struct Metrics {
    max_abs: u8,
    mean_abs: f64,
    frac_over: f64,
    over_px: usize,
}

fn block_means(px: &[u8], block: u32) -> Vec<u8> {
    assert!(
        W.is_multiple_of(block) && H.is_multiple_of(block),
        "block must divide frame size"
    );
    let (bw, bh) = (W / block, H / block);
    let mut out = Vec::with_capacity((bw * bh * 4) as usize);
    for by in 0..bh {
        for bx in 0..bw {
            let mut sum = [0u32; 4];
            for y in by * block..(by + 1) * block {
                for x in bx * block..(bx + 1) * block {
                    let i = ((y * W + x) * 4) as usize;
                    for (c, s) in sum.iter_mut().enumerate() {
                        *s += px[i + c] as u32;
                    }
                }
            }
            let n = block * block;
            out.extend(sum.iter().map(|s| ((s + n / 2) / n) as u8));
        }
    }
    out
}

fn compare(actual: &[u8], expected: &[u8], tol: &Tol) -> Metrics {
    assert_eq!(actual.len(), expected.len(), "frame size mismatch");
    if tol.block > 1 {
        let (a, e) = (
            block_means(actual, tol.block),
            block_means(expected, tol.block),
        );
        return compare(&a, &e, &Tol { block: 1, ..*tol });
    }
    let mut max_abs = 0u8;
    let mut sum = 0u64;
    let mut over = 0usize;
    for (a, e) in actual
        .as_chunks::<4>()
        .0
        .iter()
        .zip(expected.as_chunks::<4>().0)
    {
        let mut px_max = 0u8;
        for c in 0..4 {
            let d = a[c].abs_diff(e[c]);
            sum += d as u64;
            px_max = px_max.max(d);
        }
        max_abs = max_abs.max(px_max);
        if px_max > tol.channel {
            over += 1;
        }
    }
    let px = actual.len() / 4;
    Metrics {
        max_abs,
        mean_abs: sum as f64 / actual.len() as f64,
        frac_over: over as f64 / px as f64,
        over_px: over,
    }
}

fn within(m: &Metrics, tol: &Tol) -> bool {
    m.frac_over <= tol.max_frac && m.max_abs <= tol.hard_max
}

fn describe(m: &Metrics, tol: &Tol) -> String {
    let unit = if tol.block > 1 {
        format!("{0}x{0} blocks", tol.block)
    } else {
        "pixels".to_string()
    };
    format!(
        "max abs diff {} (hard cap {}), mean abs diff {:.4}, {:.3}% {unit} over +-{} ({} of them, budget {:.3}%)",
        m.max_abs,
        tol.hard_max,
        m.mean_abs,
        m.frac_over * 100.0,
        tol.channel,
        m.over_px,
        tol.max_frac * 100.0
    )
}

fn png_bytes(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let mut out = Vec::new();
    PngEncoder::new_with_quality(&mut out, CompressionType::Best, FilterType::Adaptive)
        .write_image(rgba, w, h, image::ExtendedColorType::Rgba8)
        .expect("png encode");
    out
}

fn tmp_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("golden-failures");
    std::fs::create_dir_all(&dir).expect("create failure dir");
    dir
}

/// Writes `<stem>.<a_label>.png`, `<stem>.<b_label>.png` and `<stem>.diff.png` and returns a
/// printable summary of the paths.
fn write_failure_images(stem: &str, a_label: &str, a: &[u8], b_label: &str, b: &[u8]) -> String {
    let dir = tmp_dir();
    let pa = dir.join(format!("{stem}.{a_label}.png"));
    let pb = dir.join(format!("{stem}.{b_label}.png"));
    let pd = dir.join(format!("{stem}.diff.png"));
    std::fs::write(&pa, png_bytes(W, H, a)).expect("write a");
    std::fs::write(&pb, png_bytes(W, H, b)).expect("write b");
    // Diff: dimmed copy of `a` where equal, bright red scaled by the diff where not.
    let mut diff = vec![255u8; a.len()];
    for ((o, pa_), pb_) in diff
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(a.as_chunks::<4>().0)
        .zip(b.as_chunks::<4>().0)
    {
        let d = (0..4).map(|c| pa_[c].abs_diff(pb_[c])).max().unwrap_or(0);
        if d == 0 {
            o[0] = pa_[0] / 4;
            o[1] = pa_[1] / 4;
            o[2] = pa_[2] / 4;
        } else {
            o[0] = 255;
            o[1] = 255u8.saturating_sub(d.saturating_mul(8));
            o[2] = 0;
        }
        o[3] = 255;
    }
    std::fs::write(&pd, png_bytes(W, H, &diff)).expect("write diff");
    format!(
        "  {a_label}: {}\n  {b_label}: {}\n  diff:   {}",
        pa.display(),
        pb.display(),
        pd.display()
    )
}

fn update_mode() -> bool {
    matches!(std::env::var("RENDERLY_UPDATE_GOLDEN"), Ok(v) if !v.is_empty() && v != "0")
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn time_tag(t: f64) -> String {
    format!("t{:04}ms", (t * 1000.0).round() as i64)
}

fn assert_matches_golden(scene: &str, t: f64, actual: &[u8], tol: &Tol) {
    let stem = format!("{scene}_{}", time_tag(t));
    let path = golden_dir().join(format!("{stem}.png"));

    if update_mode() {
        let unchanged = image::open(&path)
            .map(|i| i.to_rgba8().into_raw() == actual)
            .unwrap_or(false);
        if !unchanged {
            std::fs::create_dir_all(golden_dir()).expect("create golden dir");
            std::fs::write(&path, png_bytes(W, H, actual)).expect("write golden");
            eprintln!("golden updated: {}", path.display());
        }
        return;
    }

    let expected = image::open(&path)
        .unwrap_or_else(|e| {
            panic!(
                "golden {} missing/unreadable ({e}). Generate with RENDERLY_UPDATE_GOLDEN=1 \
                 cargo test -p renderly-core --test golden_frames",
                path.display()
            )
        })
        .to_rgba8();
    assert_eq!(
        (expected.width(), expected.height()),
        (W, H),
        "golden {} has wrong size",
        path.display()
    );
    let m = compare(actual, expected.as_raw(), tol);
    if !within(&m, tol) {
        let files = write_failure_images(&stem, "actual", actual, "golden", expected.as_raw());
        panic!(
            "golden mismatch for {stem}: {}\n{files}\n(if intentional: RENDERLY_UPDATE_GOLDEN=1 \
             cargo test -p renderly-core --test golden_frames)",
            describe(&m, tol)
        );
    }
}

// ---------------------------------------------------------------------------------------
// Source images (deterministic integer / simple float math)
// ---------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Src {
    /// 320x180 colour bars + grey ramp + RGB gradient; black triangle top-left and white
    /// square bottom-right as orientation cues. Opaque.
    Bars,
    /// 160x90 checker (cyan/orange) with a fully transparent magenta-RGB border and a
    /// half-transparent white band. Exercises straight alpha.
    Sprite,
    /// 320x180 green screen with a soft-edged gradient disc and a blue block. Opaque.
    Green,
    /// 320x180 diagonal orange/purple stripes with a white grid. Opaque.
    Stripes,
    /// 128x128 square with rings (aspect != output: cover-fit crops top/bottom).
    Square,
}

impl Src {
    const ALL: [Src; 5] = [
        Src::Bars,
        Src::Sprite,
        Src::Green,
        Src::Stripes,
        Src::Square,
    ];

    fn name(self) -> &'static str {
        match self {
            Src::Bars => "bars",
            Src::Sprite => "sprite",
            Src::Green => "green",
            Src::Stripes => "stripes",
            Src::Square => "square",
        }
    }

    fn id(self) -> Uuid {
        Uuid::from_u128(0x6015_0000 + Src::ALL.iter().position(|s| *s == self).unwrap() as u128)
    }

    fn size(self) -> (u32, u32) {
        match self {
            Src::Bars | Src::Green | Src::Stripes => (320, 180),
            Src::Sprite => (160, 90),
            Src::Square => (128, 128),
        }
    }

    fn image(self) -> RgbaImage {
        match self {
            Src::Bars => gen_bars(),
            Src::Sprite => gen_sprite(),
            Src::Green => gen_green(),
            Src::Stripes => gen_stripes(),
            Src::Square => gen_square(),
        }
    }

    fn png_path(self) -> PathBuf {
        fixture_dir().join(format!("{}.png", self.name()))
    }

    fn mkv_path(self) -> PathBuf {
        fixture_dir().join(format!("{}.mkv", self.name()))
    }
}

fn gen_bars() -> RgbaImage {
    const BARS: [[u8; 3]; 8] = [
        [235, 235, 235],
        [235, 235, 16],
        [16, 235, 235],
        [16, 235, 16],
        [235, 16, 235],
        [235, 16, 16],
        [16, 16, 235],
        [30, 30, 30],
    ];
    RgbaImage::from_fn(320, 180, |x, y| {
        if x + y < 28 {
            return Rgba([0, 0, 0, 255]);
        }
        if (296..312).contains(&x) && (150..166).contains(&y) {
            return Rgba([255, 255, 255, 255]);
        }
        if y < 120 {
            let [r, g, b] = BARS[(x / 40) as usize];
            Rgba([r, g, b, 255])
        } else if y < 150 {
            let v = (x * 255 / 319) as u8;
            Rgba([v, v, v, 255])
        } else {
            let r = (x * 255 / 319) as u8;
            let g = ((y - 150) * 255 / 29) as u8;
            Rgba([r, g, 255 - r, 255])
        }
    })
}

fn gen_sprite() -> RgbaImage {
    RgbaImage::from_fn(160, 90, |x, y| {
        if x < 12 || y < 12 || x >= 148 || y >= 78 {
            // Transparent, but with loud RGB: premultiplication / colour bleed would show.
            Rgba([255, 0, 255, 0])
        } else if (38..52).contains(&y) {
            Rgba([255, 255, 255, 128])
        } else if (x / 10 + y / 10).is_multiple_of(2) {
            Rgba([0, 200, 255, 255])
        } else {
            Rgba([255, 140, 0, 255])
        }
    })
}

fn gen_green() -> RgbaImage {
    RgbaImage::from_fn(320, 180, |x, y| {
        if (10..70).contains(&x) && (120..170).contains(&y) {
            return Rgba([20, 40, 230, 255]);
        }
        let subject = [
            (x * 255 / 319) as f32,
            60.0 + (y * 150 / 179) as f32,
            200.0 - (x * 150 / 319) as f32,
        ];
        let green = [0.0f32, 255.0, 0.0];
        let dx = x as f32 - 160.0;
        let dy = y as f32 - 90.0;
        let d = (dx * dx + dy * dy).sqrt();
        let t = if d <= 56.0 {
            0.0
        } else if d < 70.0 {
            (d - 56.0) / 14.0
        } else {
            1.0
        };
        let mix = |i: usize| (subject[i] + (green[i] - subject[i]) * t).round() as u8;
        Rgba([mix(0), mix(1), mix(2), 255])
    })
}

fn gen_stripes() -> RgbaImage {
    RgbaImage::from_fn(320, 180, |x, y| {
        if x % 80 < 2 || y % 60 < 2 {
            Rgba([255, 255, 255, 255])
        } else if ((x + y) / 16) % 2 == 0 {
            Rgba([250, 120, 20, 255])
        } else {
            Rgba([60, 20, 140, 255])
        }
    })
}

fn gen_square() -> RgbaImage {
    RgbaImage::from_fn(128, 128, |x, y| {
        let dx = x as f32 - 63.5;
        let dy = y as f32 - 63.5;
        let ring = ((dx * dx + dy * dy).sqrt() / 10.0) as u32;
        let b = if ring.is_multiple_of(2) { 220 } else { 40 };
        Rgba([(x * 2) as u8, (y * 2) as u8, b, 255])
    })
}

fn gen_matte() -> GrayImage {
    GrayImage::from_fn(160, 90, |x, y| {
        let v = if y < 10 {
            0
        } else if x < 80 {
            255
        } else {
            (255 - (x - 80) * 255 / 79) as u8
        };
        Luma([v])
    })
}

// ---------------------------------------------------------------------------------------
// Fixture preparation (once per process) and environment gating
// ---------------------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("golden-fixtures-{}", std::process::id()))
}

fn pack_dir() -> PathBuf {
    fixture_dir().join("pack")
}

fn matte_path() -> PathBuf {
    fixture_dir().join("matte.png")
}

fn generated_matte_cache_dir() -> PathBuf {
    fixture_dir().join("gen-cache")
}

fn roboto_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../renderly-wasm/assets/Roboto-Bold.ttf")
}

struct Prepared {
    /// media id -> source pixels as the "browser decoded" them (straight from the PNG).
    images: HashMap<Uuid, RgbaImage>,
}

fn prepare() -> Option<Prepared> {
    if !ffmpeg_available() {
        eprintln!("golden_frames: skipping - ffmpeg not available");
        return None;
    }
    match Compositor::new(8, 8) {
        Ok(_) => {}
        Err(ComposeError::NoAdapter) => {
            eprintln!("golden_frames: skipping - no wgpu adapter");
            return None;
        }
        Err(e) => panic!("compositor init failed: {e}"),
    }

    // Deterministic caption glyphs on every OS (and the exact font the preview embeds).
    // Edition 2021: set_var is safe; this runs once, before any rendering thread reads it.
    let roboto = roboto_path();
    assert!(roboto.is_file(), "missing {}", roboto.display());
    std::env::set_var("RENDERLY_FONT_PATH", &roboto);

    // Best-effort cleanup of fixture dirs left by earlier runs (they embed the pid).
    if let Ok(rd) = std::fs::read_dir(env!("CARGO_TARGET_TMPDIR")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("golden-fixtures-")
                && e.path() != fixture_dir()
                && e.metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| m.elapsed().ok())
                    .is_some_and(|age| age.as_secs() > 600)
            {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    let dir = fixture_dir();
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let ffmpeg = ffmpeg_path().expect("ffmpeg path");

    let mut images = HashMap::new();
    for src in Src::ALL {
        let img = src.image();
        img.save(src.png_path()).expect("write source png");
        // Loop the PNG into 4 s of lossless FFV1 (alpha preserved as bgra). 10 fps keeps the
        // fixtures a few KB; every frame is identical so seek position is irrelevant.
        let status = Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-y", "-loop", "1"])
            .args(["-framerate", "10", "-i"])
            .arg(src.png_path())
            .args(["-t", "4", "-c:v", "ffv1", "-pix_fmt", "bgra"])
            .arg(src.mkv_path())
            .status()
            .expect("spawn ffmpeg");
        assert!(
            status.success(),
            "ffmpeg failed to build {} fixture",
            src.name()
        );
        images.insert(src.id(), img);
    }

    gen_matte().save(matte_path()).expect("write matte");
    std::fs::create_dir_all(generated_matte_cache_dir()).expect("gen cache dir");
    gen_matte()
        .save(generated_matte_cache_dir().join("matte.png"))
        .expect("write generated matte");

    // A tiny asset pack with one .cube LUT that rotates channels (r,g,b) -> (g,b,r).
    let pack = pack_dir();
    std::fs::create_dir_all(&pack).expect("pack dir");
    std::fs::write(
        pack.join("pack.json"),
        r#"{"id":"goldenpack","name":"Golden","luts":[{"id":"rot","label":"Rot","path":"rot.cube"}]}"#,
    )
    .expect("pack.json");
    let mut cube = String::from("TITLE \"rot\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                cube.push_str(&format!("{g} {b} {r}\n"));
            }
        }
    }
    std::fs::write(pack.join("rot.cube"), cube).expect("cube");

    Some(Prepared { images })
}

fn prepared() -> Option<&'static Prepared> {
    static P: OnceLock<Option<Prepared>> = OnceLock::new();
    P.get_or_init(prepare).as_ref()
}

/// Everything a rendering test needs, plus a process-wide lock. Rendering tests are
/// serialised on purpose: `FrameRenderer`'s ffmpeg readers are long-lived child processes
/// blocked on full stdout pipes, and on Windows concurrently spawned children inherit each
/// other's pipe handles, so a parallel `ffprobe ... .output()` never sees EOF and the whole
/// run deadlocks (observed with the default test-thread count). One GPU device at a time also
/// keeps CI adapters (lavapipe) happy. The cost is ~40 s for the suite in a debug build.
struct Env {
    images: &'static HashMap<Uuid, RgbaImage>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn env() -> Option<Env> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let p = prepared()?;
    Some(Env {
        images: &p.images,
        _guard: guard,
    })
}

// ---------------------------------------------------------------------------------------
// Scene description
// ---------------------------------------------------------------------------------------

struct Scene {
    name: String,
    times: Vec<f64>,
    project: Project,
    tol: Tol,
}

impl Scene {
    fn tol(mut self, tol: Tol) -> Self {
        self.tol = tol;
        self
    }

    fn caption(mut self, text: &str, style: &str, position: f64, duration: f64) -> Self {
        let mut track = Track::new(TrackKind::Caption, "Captions");
        track.clips.push(Clip::Caption(CaptionClip {
            id: Uuid::new_v4(),
            text: text.into(),
            position_secs: position,
            duration_secs: duration,
            style_id: style.into(),
        }));
        self.project.tracks.push(track);
        self
    }

    fn pack(mut self) -> Self {
        self.project.asset_pack_paths.push(pack_dir());
        self
    }

    fn hide_track(mut self, index: usize) -> Self {
        self.project.tracks[index].hidden = true;
        self
    }
}

/// One media clip (or sequence clip, or adjustment layer) + the source it plays, with
/// builder-style modifiers.
struct Layer {
    /// `None` for sequence clips and adjustment layers (no source file).
    src: Option<Src>,
    clip: MediaClip,
    /// Set for adjustment layers; the `clip` is then unused.
    adjust: Option<AdjustmentClip>,
}

impl Layer {
    fn new(src: Src, position: f64, duration: f64) -> Self {
        Layer {
            src: Some(src),
            clip: MediaClip {
                media_id: src.id(),
                position_secs: position,
                source_in_secs: 0.0,
                source_out_secs: duration,
                ..Default::default()
            },
            adjust: None,
        }
    }

    /// A clip placing the sequence media `media_id`; plays `[source_in, source_in + duration)`
    /// of the sequence at `position`.
    fn sequence(media_id: Uuid, position: f64, source_in: f64, duration: f64) -> Self {
        Layer {
            src: None,
            clip: MediaClip {
                media_id,
                position_secs: position,
                source_in_secs: source_in,
                source_out_secs: source_in + duration,
                ..Default::default()
            },
            adjust: None,
        }
    }

    fn adjustment(position: f64, duration: f64) -> Self {
        Layer {
            src: None,
            clip: MediaClip::default(),
            adjust: Some(AdjustmentClip::new(position, duration)),
        }
    }

    fn adj_opacity(mut self, opacity: f64) -> Self {
        if let Some(a) = self.adjust.as_mut() {
            a.opacity = opacity;
        }
        self
    }

    fn transform(mut self, t: ClipTransform) -> Self {
        self.clip.transform = t;
        self
    }

    fn effect(mut self, effect_id: &str, params: &[(&str, f64)]) -> Self {
        let instance = EffectInstance {
            id: Uuid::new_v4(),
            effect_id: effect_id.into(),
            enabled: true,
            params: params.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        };
        match self.adjust.as_mut() {
            Some(a) => a.effects.push(instance),
            None => self.clip.effects.push(instance),
        }
        self
    }

    fn disabled_effect(mut self, effect_id: &str, params: &[(&str, f64)]) -> Self {
        self = self.effect(effect_id, params);
        self.clip.effects.last_mut().unwrap().enabled = false;
        self
    }

    fn mask(mut self, kind: ClipMaskKind, feather: f64, invert: bool) -> Self {
        let mask = Some(ClipMask {
            enabled: true,
            invert,
            feather,
            kind,
        });
        match self.adjust.as_mut() {
            Some(a) => a.mask = mask,
            None => self.clip.mask = mask,
        }
        self
    }

    fn keys(mut self, property: AnimProperty, keys: Vec<Keyframe>) -> Self {
        self.clip.keyframes.push(KeyframeTrack { property, keys });
        self
    }

    fn speed(mut self, speed: f64) -> Self {
        self.clip.speed = speed;
        self
    }

    fn transition(mut self, kind: TransitionKind, duration_secs: f64) -> Self {
        self.clip.outgoing_transition = Some(ClipTransition {
            kind,
            duration_secs,
        });
        self
    }

    fn disabled(mut self) -> Self {
        self.clip.enabled = false;
        self
    }
}

fn kf(time_secs: f64, value: f64, easing: Easing) -> Keyframe {
    Keyframe {
        time_secs,
        value,
        easing,
        handle_left: None,
        handle_right: None,
    }
}

fn xf(x: f64, y: f64, sx: f64, sy: f64, rot: f64, opacity: f64) -> ClipTransform {
    ClipTransform {
        x,
        y,
        scale_x: sx,
        scale_y: sy,
        rotation_deg: rot,
        opacity,
    }
}

fn new_project() -> Project {
    Project::new(
        "golden",
        Settings {
            fps: 30.0,
            width: W,
            height: H,
            sample_rate: 48_000,
            duck_db: -12.0,
        },
    )
}

fn ensure_media(project: &mut Project, src: Src, kind: MediaKind) {
    if project.media.iter().any(|m| m.id == src.id()) {
        return;
    }
    let (w, h) = src.size();
    project.media.push(MediaItem {
        id: src.id(),
        path: match kind {
            MediaKind::Image => src.png_path(),
            _ => src.mkv_path(),
        },
        kind,
        duration_secs: Some(4.0),
        width: Some(w),
        height: Some(h),
        fps: Some(10.0),
        sequence_id: None,
    });
}

/// Each inner Vec becomes one video track (outer order = bottom to top).
fn scene_tracks(name: &str, times: &[f64], tracks: Vec<Vec<Layer>>) -> Scene {
    let mut project = new_project();
    for layers in tracks {
        let track = video_track(&mut project, layers);
        project.tracks.push(track);
    }
    Scene {
        name: name.into(),
        times: times.to_vec(),
        project,
        tol: GOLDEN_TOL,
    }
}

/// One video track holding `layers` (registering their source media on `project`).
fn video_track(project: &mut Project, layers: Vec<Layer>) -> Track {
    let mut track = Track::new(TrackKind::Video, "V");
    for l in layers {
        if let Some(src) = l.src {
            ensure_media(project, src, MediaKind::Video);
        }
        match l.adjust {
            Some(a) => track.clips.push(Clip::Adjustment(a)),
            None => track.clips.push(Clip::Video(l.clip)),
        }
    }
    track
}

/// Add a nested sequence (one video track per inner Vec, bottom to top; `size` of `None`
/// inherits the project size) and its media item. Returns the media id to place with
/// [`Layer::sequence`].
fn add_sequence(
    project: &mut Project,
    name: &str,
    size: Option<(u32, u32)>,
    tracks: Vec<Vec<Layer>>,
) -> Uuid {
    let tracks: Vec<Track> = tracks
        .into_iter()
        .map(|layers| video_track(project, layers))
        .collect();
    let sequence_id = Uuid::new_v4();
    let media_id = Uuid::new_v4();
    project.sequences.push(Sequence {
        id: sequence_id,
        name: name.into(),
        width: size.map(|s| s.0),
        height: size.map(|s| s.1),
        tracks,
    });
    project.media.push(MediaItem {
        id: media_id,
        path: PathBuf::new(),
        kind: MediaKind::Sequence,
        duration_secs: None,
        width: None,
        height: None,
        fps: None,
        sequence_id: Some(sequence_id),
    });
    media_id
}

/// Like [`scene_tracks`], but `build` may first register sequences on the project and then
/// returns the root tracks (each inner Vec one video track, bottom to top).
fn scene_with_sequences(
    name: &str,
    times: &[f64],
    build: impl FnOnce(&mut Project) -> Vec<Vec<Layer>>,
) -> Scene {
    let mut project = new_project();
    let tracks = build(&mut project);
    for layers in tracks {
        let track = video_track(&mut project, layers);
        project.tracks.push(track);
    }
    Scene {
        name: name.into(),
        times: times.to_vec(),
        project,
        tol: GOLDEN_TOL,
    }
}

/// One layer per track, bottom to top.
fn scene(name: &str, times: &[f64], layers: Vec<Layer>) -> Scene {
    scene_tracks(name, times, layers.into_iter().map(|l| vec![l]).collect())
}

fn full(src: Src) -> Layer {
    Layer::new(src, 0.0, 3.0)
}

#[allow(clippy::vec_init_then_push)]
fn scenes() -> Vec<Scene> {
    let mut v = Vec::new();

    v.push(scene("baseline_bars", &[0.5], vec![full(Src::Bars)]));
    v.push(scene("cover_fit_square", &[0.5], vec![full(Src::Square)]));

    // --- transform / opacity / keyframes / z-order -------------------------------------
    v.push(scene(
        "transform_pos_scale_rot",
        &[1.0],
        vec![
            full(Src::Stripes),
            full(Src::Sprite).transform(xf(0.35, -0.25, 0.7, 0.7, 25.0, 1.0)),
        ],
    ));
    v.push(scene(
        "opacity",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Bars).transform(xf(0.0, 0.0, 1.0, 1.0, 0.0, 0.4)),
        ],
    ));
    v.push(scene(
        "keyframed_transform_ease",
        &[0.5, 1.5],
        vec![
            full(Src::Stripes),
            full(Src::Sprite)
                .keys(
                    AnimProperty::PosX,
                    vec![
                        kf(0.0, -0.6, Easing::EaseInOut),
                        kf(2.0, 0.6, Easing::Linear),
                    ],
                )
                .keys(
                    AnimProperty::PosY,
                    vec![
                        Keyframe {
                            time_secs: 0.0,
                            value: 0.5,
                            easing: Easing::Bezier,
                            handle_left: None,
                            handle_right: Some((0.6, -0.3)),
                        },
                        Keyframe {
                            time_secs: 2.0,
                            value: -0.5,
                            easing: Easing::Linear,
                            handle_left: Some((-0.6, 0.1)),
                            handle_right: None,
                        },
                    ],
                )
                .keys(
                    AnimProperty::ScaleX,
                    vec![kf(0.0, 0.35, Easing::EaseIn), kf(2.0, 0.8, Easing::Linear)],
                )
                .keys(
                    AnimProperty::ScaleY,
                    vec![kf(0.0, 0.35, Easing::EaseOut), kf(2.0, 0.8, Easing::Linear)],
                )
                .keys(
                    AnimProperty::Rotation,
                    vec![kf(0.0, 0.0, Easing::EaseOut), kf(2.0, 90.0, Easing::Linear)],
                )
                .keys(
                    AnimProperty::Opacity,
                    vec![kf(0.0, 1.0, Easing::Linear), kf(2.0, 0.4, Easing::Linear)],
                ),
        ],
    ));
    v.push(scene(
        "z_order_alpha",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Sprite).transform(xf(-0.25, 0.0, 0.8, 0.8, 0.0, 1.0)),
            full(Src::Bars).transform(xf(0.2, 0.1, 0.45, 0.45, -12.0, 1.0)),
            full(Src::Sprite).transform(xf(0.25, -0.15, 0.6, 0.6, 8.0, 0.85)),
        ],
    ));

    // --- every builtin effect, non-default params --------------------------------------
    v.push(scene(
        "fx_color_adjust",
        &[0.5],
        vec![full(Src::Bars).effect(
            "builtin:color_adjust",
            &[("exposure", 0.4), ("contrast", 1.35), ("saturation", 0.4)],
        )],
    ));
    v.push(scene(
        "fx_blur",
        &[0.5],
        vec![full(Src::Bars).effect("builtin:blur", &[("radius", 6.0)])],
    ));
    v.push(
        scene(
            "fx_glitch",
            &[0.5],
            vec![full(Src::Bars).effect(
                "builtin:glitch",
                &[("intensity", 0.8), ("slice", 0.6), ("seed", 3.0)],
            )],
        )
        .tol(HASH_NOISE_TOL),
    );
    v.push(scene(
        "fx_lut_contrast",
        &[0.5],
        vec![full(Src::Bars).effect("builtin:lut_contrast", &[("intensity", 1.0)])],
    ));
    v.push(scene(
        "fx_lut_warm",
        &[0.5],
        vec![full(Src::Bars).effect("builtin:lut_warm", &[("intensity", 0.8)])],
    ));
    v.push(scene(
        "fx_chroma_key",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Green).effect(
                "builtin:chroma_key",
                &[
                    ("key_r", 0.0),
                    ("key_g", 1.0),
                    ("key_b", 0.0),
                    ("tolerance", 0.35),
                    ("softness", 0.15),
                ],
            ),
        ],
    ));
    v.push(scene(
        "fx_chain_with_disabled",
        &[0.5],
        vec![full(Src::Bars)
            .effect("builtin:color_adjust", &[("saturation", 1.8)])
            .effect("builtin:blur", &[("radius", 3.0)])
            .effect("builtin:lut_warm", &[("intensity", 0.5)])
            .disabled_effect("builtin:glitch", &[("intensity", 1.0)])],
    ));
    v.push(
        scene(
            "fx_pack_cube_lut",
            &[0.5],
            vec![full(Src::Bars).effect("pack:goldenpack:lut:rot", &[("intensity", 1.0)])],
        )
        .pack(),
    );

    // --- masks (masked bars over stripes, so the cut-out shows the layer below) ---------
    let rect = || ClipMaskKind::Rect {
        x: 0.2,
        y: 0.15,
        width: 0.5,
        height: 0.6,
    };
    let ellipse = || ClipMaskKind::Ellipse {
        cx: 0.5,
        cy: 0.5,
        rx: 0.3,
        ry: 0.4,
    };
    v.push(scene(
        "mask_rect_hard",
        &[0.5],
        vec![full(Src::Stripes), full(Src::Bars).mask(rect(), 0.0, false)],
    ));
    v.push(scene(
        "mask_rect_feather",
        &[0.5],
        vec![full(Src::Stripes), full(Src::Bars).mask(rect(), 0.6, false)],
    ));
    v.push(scene(
        "mask_ellipse_feather",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Bars).mask(ellipse(), 0.4, false),
        ],
    ));
    v.push(scene(
        "mask_ellipse_inverted",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Bars).mask(ellipse(), 0.2, true),
        ],
    ));
    v.push(scene(
        "mask_raster_matte",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Bars).mask(ClipMaskKind::Raster { path: matte_path() }, 0.0, false),
        ],
    ));
    v.push(scene(
        "mask_generated_matte",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Bars).mask(
                ClipMaskKind::Generated {
                    cache_dir: generated_matte_cache_dir(),
                },
                0.3,
                true,
            ),
        ],
    ));

    // --- transitions: all ten kinds, mid-way; crossfade also at 25% / 75% ----------------
    for kind in TransitionKind::ALL {
        // fade_black is pure black at exactly 50%, so sample it at 35% instead.
        let times: &[f64] = match kind {
            TransitionKind::Crossfade => &[1.25, 1.5, 1.75],
            TransitionKind::FadeBlack => &[1.35],
            _ => &[1.5],
        };
        let mut sc = scene_tracks(
            &format!("transition_{}", kind.as_str()),
            times,
            vec![vec![
                Layer::new(Src::Bars, 0.0, 2.0).transition(*kind, 1.0),
                Layer::new(Src::Stripes, 2.0, 2.0),
            ]],
        );
        if *kind == TransitionKind::BlurDissolve {
            sc = sc.tol(HASH_NOISE_TOL);
        }
        v.push(sc);
    }

    // --- captions -------------------------------------------------------------------------
    v.push(
        scene("caption_bold_yellow", &[1.0], vec![full(Src::Bars)]).caption(
            "Golden",
            "tiktok-bold-yellow",
            0.0,
            3.0,
        ),
    );
    v.push(
        scene(
            "caption_box",
            &[1.0, 3.5],
            vec![Layer::new(Src::Stripes, 0.0, 4.0)],
        )
        .caption("Same pixels", "tiktok-box", 0.0, 3.0),
    );

    // --- timeline semantics observable in pixels ------------------------------------------
    // Speed does not change a still's pixels, but it does change the clip's timeline span:
    // at 2x the 4 s source ends at t=2, so bars show at 1.5 and stripes at 2.5.
    v.push(scene_tracks(
        "speed_2x_shortens_clip",
        &[1.5, 2.5],
        vec![vec![
            Layer::new(Src::Bars, 0.0, 4.0).speed(2.0),
            Layer::new(Src::Stripes, 2.0, 2.0),
        ]],
    ));
    v.push(
        scene_tracks(
            "hidden_and_disabled_skipped",
            &[0.5],
            vec![
                vec![full(Src::Bars)],
                vec![full(Src::Sprite)],
                vec![full(Src::Stripes).disabled()],
            ],
        )
        .hide_track(1),
    );
    v.push(scene(
        "gap_after_clip_black",
        &[3.0],
        vec![Layer::new(Src::Bars, 0.0, 2.0)],
    ));

    // --- schema v7: adjustment layers ---------------------------------------------------
    // Greyscale + extra contrast applied to everything beneath the adjustment layer.
    let grade = |l: Layer| {
        l.effect(
            "builtin:color_adjust",
            &[("saturation", 0.0), ("contrast", 1.4)],
        )
    };
    v.push(scene(
        "adjust_over_two_layers",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Bars).transform(xf(-0.3, 0.0, 0.6, 0.6, 0.0, 1.0)),
            grade(Layer::adjustment(0.0, 3.0)),
        ],
    ));
    v.push(scene(
        "adjust_opacity_and_mask",
        &[0.5],
        vec![
            full(Src::Stripes),
            full(Src::Bars).transform(xf(-0.3, 0.0, 0.6, 0.6, 0.0, 1.0)),
            Layer::adjustment(0.0, 3.0)
                .effect(
                    "builtin:color_adjust",
                    &[("saturation", 0.0), ("exposure", 0.8)],
                )
                .adj_opacity(0.6)
                .mask(
                    ClipMaskKind::Ellipse {
                        cx: 0.5,
                        cy: 0.5,
                        rx: 0.3,
                        ry: 0.4,
                    },
                    0.3,
                    false,
                ),
        ],
    ));
    v.push(scene(
        "adjust_layer_above_untouched",
        &[0.5],
        vec![
            full(Src::Stripes),
            grade(Layer::adjustment(0.0, 3.0)),
            full(Src::Sprite).transform(xf(0.25, -0.15, 0.6, 0.6, 8.0, 1.0)),
        ],
    ));

    // --- schema v7: nested sequences ----------------------------------------------------
    // A square 180x180 sequence (own resolution, cover-fit into the 16:9 frame) holding bars
    // plus a sprite, placed with a transform and an effect over stripes.
    v.push(scene_with_sequences(
        "nested_transform_effect",
        &[0.5],
        |p| {
            let seq = add_sequence(
                p,
                "Square",
                Some((180, 180)),
                vec![
                    vec![full(Src::Bars)],
                    vec![full(Src::Sprite).transform(xf(0.3, -0.3, 0.5, 0.5, 0.0, 1.0))],
                ],
            );
            vec![
                vec![full(Src::Stripes)],
                vec![Layer::sequence(seq, 0.0, 0.0, 3.0)
                    .transform(xf(0.2, 0.1, 0.7, 0.7, 10.0, 1.0))
                    .effect("builtin:color_adjust", &[("saturation", 0.35)])],
            ]
        },
    ));
    // One "two-shot" sequence (bars for 1 s, then stripes for 1 s) placed twice in the same
    // frame at different sequence times: shot 1 full-frame, shot 2 as a small overlay.
    v.push(scene_with_sequences(
        "nested_twice_different_times",
        &[0.5],
        |p| {
            let seq = add_sequence(
                p,
                "Two-shot",
                None,
                vec![vec![
                    Layer::new(Src::Bars, 0.0, 1.0),
                    Layer::new(Src::Stripes, 1.0, 1.0),
                ]],
            );
            vec![
                vec![Layer::sequence(seq, 0.0, 0.0, 1.0)],
                vec![Layer::sequence(seq, 0.0, 1.0, 1.0)
                    .transform(xf(0.45, -0.4, 0.4, 0.4, 0.0, 1.0))],
            ]
        },
    ));
    // Transparency survives nesting: a sprite (transparent border, half-transparent band)
    // inside a sequence still shows the stripes underneath through its transparent parts.
    v.push(scene_with_sequences("nested_transparency", &[0.5], |p| {
        let seq = add_sequence(
            p,
            "Sprite",
            None,
            vec![vec![
                full(Src::Sprite).transform(xf(-0.1, 0.05, 0.8, 0.8, 12.0, 1.0))
            ]],
        );
        vec![
            vec![full(Src::Stripes)],
            vec![Layer::sequence(seq, 0.0, 0.0, 3.0)],
        ]
    }));
    // An adjustment layer inside a sequence affects only that sequence's own content.
    v.push(scene_with_sequences("nested_adjust_inside", &[0.5], |p| {
        let seq = add_sequence(
            p,
            "Graded",
            None,
            vec![
                vec![full(Src::Bars)],
                vec![Layer::adjustment(0.0, 3.0).effect(
                    "builtin:color_adjust",
                    &[("saturation", 0.0), ("contrast", 1.3)],
                )],
            ],
        );
        vec![
            vec![full(Src::Stripes)],
            vec![Layer::sequence(seq, 0.0, 0.0, 3.0).transform(xf(0.0, 0.0, 0.6, 0.6, 0.0, 1.0))],
        ]
    }));
    // A crossfade from a sequence clip into a plain media clip.
    v.push(scene_with_sequences(
        "nested_transition_into_media",
        &[1.5],
        |p| {
            let seq = add_sequence(p, "Bars", None, vec![vec![full(Src::Bars)]]);
            vec![vec![
                Layer::sequence(seq, 0.0, 0.0, 2.0).transition(TransitionKind::Crossfade, 1.0),
                Layer::new(Src::Stripes, 2.0, 2.0),
            ]]
        },
    ));

    v
}

fn find_scene(name: &str) -> Scene {
    scenes()
        .into_iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no scene named {name}"))
}

// ---------------------------------------------------------------------------------------
// Export path
// ---------------------------------------------------------------------------------------

fn export_settings() -> ExportSettings {
    ExportSettings {
        width: W,
        height: H,
        fps: 30.0,
        ..ExportSettings::default()
    }
}

fn export_renderer() -> FrameRenderer {
    FrameRenderer::new(export_settings(), DecodeOptions::default()).expect("FrameRenderer::new")
}

// ---------------------------------------------------------------------------------------
// Preview-style path (native stand-in for renderly-wasm's WasmCompositor::render)
// ---------------------------------------------------------------------------------------

/// See the module docs for exactly what this does and does not cover. Mirrors
/// `renderly-wasm/src/compositor.rs::render` step for step: sync host-supplied assets
/// (`sync_assets`) -> `eval::active_layers` -> per-layer texture (stand-in for
/// `copy_external_image_to_texture`) -> `ComposeLayer::from_active(..,
/// Compositor::preview_layer_mask)` -> captions via builtin `render_caption` +
/// `ComposeLayer::caption` -> `Compositor::composite(&packs, ..)`.
struct PreviewPipeline {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    compositor: Compositor,
    /// matte keys already registered (the wasm side keeps its own `mattes` map).
    mattes: HashSet<String>,
    /// pack id -> lut id -> parsed cube, as `WasmCompositor::pack_luts`.
    pack_luts: BTreeMap<String, BTreeMap<String, CubeLut>>,
    /// `pack_luts` as `LoadedPack`s, as `WasmCompositor::packs`.
    packs: Vec<LoadedPack>,
}

impl PreviewPipeline {
    fn new() -> Option<Self> {
        let backends = match std::env::var("RENDERLY_GOLDEN_PREVIEW_BACKENDS").as_deref() {
            Ok("vulkan") => wgpu::Backends::VULKAN,
            Ok("dx12") => wgpu::Backends::DX12,
            Ok("gl") => wgpu::Backends::GL,
            _ => wgpu::Backends::all(),
        };
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("golden-preview-standin"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        let (device, queue) = (Arc::new(device), Arc::new(queue));
        let compositor =
            Compositor::with_device(Arc::clone(&device), Arc::clone(&queue), W, H).ok()?;
        Some(Self {
            device,
            queue,
            compositor,
            mattes: HashSet::new(),
            pack_luts: BTreeMap::new(),
            packs: Vec::new(),
        })
    }

    /// The webview-side half of the wasm flow (`previewAssets.ts` + the Tauri commands),
    /// played by the test: ask core what the project needs, fetch bytes "from the backend",
    /// decode like the browser would, and register through the wasm crate's entry points.
    /// The compositor's native `image::open` fallback is never reached because every
    /// referenced matte is registered here first (and `preview_layer_mask` drops any that
    /// are not).
    fn sync_assets(&mut self, project: &Project) {
        for req in project_matte_requests(project) {
            if self.mattes.contains(&req.key) {
                continue;
            }
            let bytes = std::fs::read(&req.path).expect("host reads matte bytes");
            let rgba = image::load_from_memory(&bytes)
                .expect("host decodes matte")
                .to_rgba8();
            self.compositor
                .set_matte_rgba(&req.key, rgba.width(), rgba.height(), rgba.as_raw())
                .expect("set_matte_rgba");
            self.mattes.insert(req.key);
        }
        let mut changed = false;
        for (pack_id, lut_id) in referenced_pack_luts(project) {
            if self
                .pack_luts
                .get(&pack_id)
                .is_some_and(|l| l.contains_key(&lut_id))
            {
                continue;
            }
            // Stand-in for the backend command: find the project pack with this id and read
            // the LUT's `.cube` text.
            let (root, rel) = project
                .asset_pack_paths
                .iter()
                .find_map(|root| {
                    let manifest: serde_json::Value = serde_json::from_str(
                        &std::fs::read_to_string(root.join("pack.json")).ok()?,
                    )
                    .ok()?;
                    if manifest["id"] != pack_id.as_str() {
                        return None;
                    }
                    let rel = manifest["luts"]
                        .as_array()?
                        .iter()
                        .find(|l| l["id"] == lut_id.as_str())?["path"]
                        .as_str()?
                        .to_string();
                    Some((root.clone(), rel))
                })
                .expect("pack lut exists");
            let text = std::fs::read_to_string(root.join(rel)).expect("read cube text");
            let cube = parse_cube_text(&text).expect("parse cube");
            self.pack_luts
                .entry(pack_id)
                .or_default()
                .insert(lut_id, cube);
            changed = true;
        }
        if changed {
            self.packs = self
                .pack_luts
                .iter()
                .map(|(id, luts)| LoadedPack::from_luts(id, luts.clone()))
                .collect();
            self.compositor.clear_pack_lut_cache();
        }
    }

    /// Same usages the wasm path gives its per-media textures.
    fn upload(&self, w: u32, h: u32, rgba: &[u8]) -> wgpu::Texture {
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("preview-standin-media"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        upload_layer_pixels(&self.queue, &texture, w, h, rgba);
        texture
    }

    fn render(&mut self, project: &Project, t: f64, images: &HashMap<Uuid, RgbaImage>) -> Vec<u8> {
        self.sync_assets(project);
        let steps = eval::active_steps(project, t).expect("active_steps");
        let captions = eval::active_captions(project, t);

        let mut host = PreviewHost {
            device: &self.device,
            queue: &self.queue,
            compositor: &self.compositor,
            images,
        };
        let mut scene = build_scene(project, steps, &mut host).unwrap_or_else(|e| match e {
            SceneError::Eval(e) => panic!("build_scene: {e}"),
            SceneError::Host(never) => match never {},
        });
        for cap in &captions {
            let frame = render_caption(&cap.text, &cap.style_id, W, H).expect("render_caption");
            let texture = self.upload(W, H, &frame.pixels);
            scene.push(SceneStep::Layer(ComposeLayer::caption(
                LayerSource::Texture {
                    texture,
                    width: W,
                    height: H,
                },
            )));
        }
        self.compositor
            .composite_steps(&self.packs, &scene)
            .expect("preview composite")
    }
}

/// Stand-in for the wasm `SceneHost`: media layers come from pre-decoded images uploaded as
/// textures (what `copy_external_image_to_texture` does for a `<video>`/`<img>`), masks
/// resolve through `preview_layer_mask`.
struct PreviewHost<'a> {
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    compositor: &'a Compositor,
    images: &'a HashMap<Uuid, RgbaImage>,
}

impl SceneHost for PreviewHost<'_> {
    type Error = std::convert::Infallible;

    fn media_layer(&mut self, layer: &ActiveLayer) -> Result<Option<ComposeLayer>, Self::Error> {
        let (media_id, _, _) = layer.media().expect("media layer");
        let img = self
            .images
            .get(&media_id)
            .expect("decoded source for media id");
        let (w, h) = (img.width(), img.height());
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("preview-standin-media"),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        upload_layer_pixels(self.queue, &texture, w, h, img.as_raw());
        Ok(Some(ComposeLayer::from_active(
            layer,
            LayerSource::Texture {
                texture,
                width: w,
                height: h,
            },
            self.compositor.preview_layer_mask(layer),
        )))
    }

    fn sequence_mask(&mut self, layer: &ActiveLayer) -> Option<ClipMask> {
        self.compositor.preview_layer_mask(layer)
    }
}

/// `RENDERLY_GOLDEN_PREVIEW_BACKENDS` set to a specific backend turns the parity test into a
/// cross-backend drift probe: it prints metrics against each scene's *golden* tolerance
/// instead of failing on the (deliberately exact) parity tolerance. Used to calibrate `Tol`s.
fn cross_backend_probe() -> bool {
    matches!(
        std::env::var("RENDERLY_GOLDEN_PREVIEW_BACKENDS").as_deref(),
        Ok("vulkan" | "dx12" | "gl")
    )
}

fn scene_tol_for(name: &str) -> Tol {
    scenes()
        .into_iter()
        .find(|s| s.name == name)
        .map_or(GOLDEN_TOL, |s| s.tol)
}

/// Returns `Err(report)` when preview-style and export disagree beyond `PARITY_TOL`.
fn parity_check(
    label: &str,
    export: &mut FrameRenderer,
    preview: &mut PreviewPipeline,
    project: &Project,
    t: f64,
    images: &HashMap<Uuid, RgbaImage>,
) -> Result<Metrics, String> {
    let exported = export.render(project, t).expect("export render");
    let previewed = preview.render(project, t, images);
    let m = compare(&previewed, &exported, &PARITY_TOL);
    if within(&m, &PARITY_TOL) || cross_backend_probe() {
        if cross_backend_probe() {
            let g = scene_tol_for(label);
            let gm = compare(&previewed, &exported, &g);
            eprintln!(
                "  cross-backend {label} @ {t}s: pixel metrics: {} | vs scene golden tol: {} -> {}",
                describe(&m, &PARITY_TOL),
                describe(&gm, &g),
                if within(&gm, &g) { "ok" } else { "EXCEEDS" }
            );
        }
        Ok(m)
    } else {
        let stem = format!("parity_{label}_{}", time_tag(t));
        let files = write_failure_images(&stem, "preview", &previewed, "export", &exported);
        Err(format!(
            "{label} @ {t}s: preview-style differs from export: {}\n{files}",
            describe(&m, &PARITY_TOL)
        ))
    }
}

// ---------------------------------------------------------------------------------------
// Golden tests (one #[test] per scene so failures are individually named)
// ---------------------------------------------------------------------------------------

fn run_golden(name: &str) {
    let Some(_env) = env() else { return };
    let scene = find_scene(name);
    let mut renderer = export_renderer();
    for &t in &scene.times {
        let pixels = renderer.render(&scene.project, t).expect("export render");
        // Guard against the classic golden failure mode: an all-black/empty frame that
        // happens to match a bad committed golden. Only the gap scene may be empty.
        if name != "gap_after_clip_black" {
            let lit = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|p| p[0] > 8 || p[1] > 8 || p[2] > 8)
                .count();
            assert!(
                lit > pixels.len() / 4 / 10,
                "{name} @ {t}s rendered a (nearly) black frame ({lit} lit px)"
            );
        }
        assert_matches_golden(name, t, &pixels, &scene.tol);
    }
}

macro_rules! golden_tests {
    ($($name:ident),+ $(,)?) => {
        $(
            #[test]
            fn $name() {
                run_golden(&stringify!($name).replacen("golden_", "", 1));
            }
        )+

        const GOLDEN_TEST_SCENES: &[&str] = &[$(
            // strip the `golden_` prefix at compile time via a const slice
            {
                const N: &str = stringify!($name);
                // `golden_` is 7 bytes
                N.split_at(7).1
            }
        ),+];
    };
}

golden_tests!(
    golden_baseline_bars,
    golden_cover_fit_square,
    golden_transform_pos_scale_rot,
    golden_opacity,
    golden_keyframed_transform_ease,
    golden_z_order_alpha,
    golden_fx_color_adjust,
    golden_fx_blur,
    golden_fx_glitch,
    golden_fx_lut_contrast,
    golden_fx_lut_warm,
    golden_fx_chroma_key,
    golden_fx_chain_with_disabled,
    golden_fx_pack_cube_lut,
    golden_mask_rect_hard,
    golden_mask_rect_feather,
    golden_mask_ellipse_feather,
    golden_mask_ellipse_inverted,
    golden_mask_raster_matte,
    golden_mask_generated_matte,
    golden_transition_crossfade,
    golden_transition_fade_black,
    golden_transition_wipe_left,
    golden_transition_wipe_right,
    golden_transition_wipe_up,
    golden_transition_wipe_down,
    golden_transition_slide_left,
    golden_transition_slide_right,
    golden_transition_iris,
    golden_transition_blur_dissolve,
    golden_caption_bold_yellow,
    golden_caption_box,
    golden_speed_2x_shortens_clip,
    golden_hidden_and_disabled_skipped,
    golden_gap_after_clip_black,
    golden_adjust_over_two_layers,
    golden_adjust_opacity_and_mask,
    golden_adjust_layer_above_untouched,
    golden_nested_transform_effect,
    golden_nested_twice_different_times,
    golden_nested_transparency,
    golden_nested_adjust_inside,
    golden_nested_transition_into_media,
);

// ---------------------------------------------------------------------------------------
// Suite-integrity tests (no GPU / ffmpeg needed)
// ---------------------------------------------------------------------------------------

#[test]
fn every_scene_has_a_golden_test_and_committed_png() {
    let defined: Vec<String> = scenes().into_iter().map(|s| s.name).collect();
    let tested: HashSet<&str> = GOLDEN_TEST_SCENES.iter().copied().collect();
    for name in &defined {
        assert!(
            tested.contains(name.as_str()),
            "scene {name} has no golden_ test in the golden_tests! list"
        );
    }
    assert_eq!(
        defined.len(),
        tested.len(),
        "golden_tests! lists a stale scene"
    );

    if update_mode() {
        return;
    }
    let mut expected_files = HashSet::new();
    for s in scenes() {
        for &t in &s.times {
            expected_files.insert(format!("{}_{}.png", s.name, time_tag(t)));
        }
    }
    let on_disk: HashSet<String> = std::fs::read_dir(golden_dir())
        .expect("tests/golden exists")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    let missing: Vec<_> = expected_files.difference(&on_disk).collect();
    let orphans: Vec<_> = on_disk.difference(&expected_files).collect();
    assert!(missing.is_empty(), "missing goldens: {missing:?}");
    assert!(
        orphans.is_empty(),
        "orphan goldens (delete them): {orphans:?}"
    );
}

#[test]
fn every_builtin_effect_is_covered_by_a_scene() {
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    for s in scenes() {
        for track in &s.project.tracks {
            for clip in &track.clips {
                if let Some(c) = clip.as_media() {
                    for e in c.effects.iter().filter(|e| e.enabled) {
                        *used.entry(e.effect_id.clone()).or_default() += 1;
                    }
                }
            }
        }
    }
    for id in BUILTIN_EFFECT_IDS {
        assert!(
            used.contains_key(*id),
            "builtin effect {id} has no golden scene - add one to scenes()"
        );
    }
}

#[test]
fn every_transition_kind_is_covered_by_a_scene() {
    let mut used = HashSet::new();
    for s in scenes() {
        for track in &s.project.tracks {
            for clip in &track.clips {
                if let Some(t) = clip.as_media().and_then(|c| c.outgoing_transition.as_ref()) {
                    used.insert(t.kind);
                }
            }
        }
    }
    for kind in TransitionKind::ALL {
        assert!(
            used.contains(kind),
            "transition {kind:?} has no golden scene"
        );
    }
}

/// `preview_layer_mask`: shape / luma masks always pass; file-backed mattes only once
/// registered (until then the layer renders unmasked rather than failing the frame); a baked
/// background-removal matte wins over the clip's own mask, exactly like export.
#[test]
fn preview_layer_mask_requires_registered_file_mattes() {
    let Some(_env) = env() else { return };
    let Some(mut preview) = PreviewPipeline::new() else {
        return;
    };
    let mk = |kind| ClipMask {
        enabled: true,
        invert: false,
        feather: 0.0,
        kind,
    };
    let layer_with = |mask: Option<ClipMask>, bg: Option<BackgroundRemoval>| {
        let mut project = new_project();
        ensure_media(&mut project, Src::Bars, MediaKind::Video);
        let mut clip = Layer::new(Src::Bars, 0.0, 3.0).clip;
        clip.mask = mask;
        clip.background_removal = bg;
        let mut track = Track::new(TrackKind::Video, "V");
        track.clips.push(Clip::Video(clip));
        project.tracks.push(track);
        let mut layers = eval::leaf_media_layers(&project, 0.5).expect("leaf_media_layers");
        layers.remove(0)
    };

    let rect = mk(ClipMaskKind::Rect {
        x: 0.0,
        y: 0.0,
        width: 1.0,
        height: 1.0,
    });
    assert_eq!(
        preview
            .compositor
            .preview_layer_mask(&layer_with(Some(rect.clone()), None)),
        Some(rect)
    );
    let raster = mk(ClipMaskKind::Raster { path: matte_path() });
    let raster_layer = layer_with(Some(raster.clone()), None);
    assert!(
        preview
            .compositor
            .preview_layer_mask(&raster_layer)
            .is_none(),
        "unregistered matte must drop, not error"
    );
    let key = renderly_core::mask::matte_cache_key(&raster.kind).expect("raster key");
    preview
        .compositor
        .set_matte(&key, 2, 2, &[255; 4])
        .expect("set_matte");
    assert_eq!(
        preview.compositor.preview_layer_mask(&raster_layer),
        Some(raster)
    );
    preview.compositor.remove_matte(&key);
    assert!(preview
        .compositor
        .preview_layer_mask(&raster_layer)
        .is_none());

    // Baked background removal -> Generated matte (feather from the config), replacing the
    // clip's own mask once registered.
    let bg = BackgroundRemoval {
        enabled: true,
        model_id: "heuristic".into(),
        threshold: 0.5,
        feather: 0.25,
        temporal: false,
        matte_cache_dir: Some(generated_matte_cache_dir()),
        matte_frames: None,
    };
    let bg_layer = layer_with(None, Some(bg.clone()));
    assert!(preview.compositor.preview_layer_mask(&bg_layer).is_none());
    let gen_mask = renderly_core::mask::cached_background_mask(&bg).expect("baked mask");
    let gen_key = renderly_core::mask::matte_cache_key(&gen_mask.kind).expect("gen key");
    preview
        .compositor
        .set_matte(&gen_key, 2, 2, &[255; 4])
        .expect("set_matte");
    assert_eq!(
        preview.compositor.preview_layer_mask(&bg_layer),
        Some(gen_mask)
    );
}

#[test]
fn compare_reports_metrics() {
    let a = vec![10u8; 16];
    let mut b = a.clone();
    b[0] = 30; // one pixel, one channel, diff 20
    let m = compare(&a, &b, &GOLDEN_TOL);
    assert_eq!(m.max_abs, 20);
    assert_eq!(m.over_px, 1);
    assert!((m.frac_over - 0.25).abs() < 1e-9);
    assert!(!within(&m, &GOLDEN_TOL));
    assert!(within(&compare(&a, &a, &GOLDEN_TOL), &GOLDEN_TOL));
}

// ---------------------------------------------------------------------------------------
// Decode fidelity, parity and perception tests
// ---------------------------------------------------------------------------------------

/// If the lossless-FFV1 fixture route ever stops being bit-exact, every golden would be
/// suspect - fail here first with a clear message.
#[test]
fn fixture_decode_is_bit_exact() {
    let Some(env) = env() else { return };
    let mut renderer = export_renderer();
    // Full-frame, identity transform, opaque 16:9 source -> output must equal the source.
    let scene = find_scene("baseline_bars");
    let out = renderer.render(&scene.project, 0.5).expect("render");
    let src = &env.images[&Src::Bars.id()];
    let m = compare(
        &out,
        src.as_raw(),
        &Tol {
            channel: 0,
            max_frac: 0.0,
            hard_max: 0,
            block: 1,
        },
    );
    assert!(
        m.max_abs <= 1,
        "FFV1 decode + identity composite must reproduce the PNG (+-1 LSB for the sampler): {m:?}"
    );
}

/// Preview-style pipeline == FrameRenderer for every scene.
#[test]
fn preview_style_pipeline_matches_export_for_all_scenes() {
    let Some(env) = env() else { return };
    let Some(mut preview) = PreviewPipeline::new() else {
        eprintln!("golden_frames: skipping parity - no adapter for preview stand-in");
        return;
    };
    let mut failures = Vec::new();
    let mut worst = 0u8;
    let mut checked = 0;
    for s in scenes() {
        let mut export = export_renderer();
        for &t in &s.times {
            checked += 1;
            match parity_check(
                &s.name,
                &mut export,
                &mut preview,
                &s.project,
                t,
                env.images,
            ) {
                Ok(m) => {
                    eprintln!(
                        "  parity {} @ {t}s: max {} mean {:.5}",
                        s.name, m.max_abs, m.mean_abs
                    );
                    worst = worst.max(m.max_abs)
                }
                Err(e) => failures.push(e),
            }
        }
    }
    eprintln!(
        "golden_frames: preview/export parity checked {checked} frames, worst abs diff {worst}"
    );
    assert!(
        failures.is_empty(),
        "{} preview/export parity failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `perceive::render_frame_png` (the MCP/CLI perception path) is the export render, PNG
/// encoded - byte-for-byte the same pixels.
#[test]
fn perceive_png_matches_frame_renderer() {
    let Some(_env) = env() else { return };
    let scene = find_scene("transform_pos_scale_rot");
    let png = renderly_core::perceive::render_frame_png(
        &scene.project,
        1.0,
        ExportPreset::Custom {
            width: W,
            height: H,
            fps: 30.0,
        },
    )
    .expect("render_frame_png");
    let decoded = image::load_from_memory(&png)
        .expect("decode png")
        .to_rgba8();
    let direct = export_renderer()
        .render(&scene.project, 1.0)
        .expect("render");
    assert_eq!(decoded.as_raw(), &direct);
}

/// Renders the same project twice with fresh renderers: guards the "stable across runs"
/// property the tolerances rely on.
#[test]
fn export_render_is_deterministic_across_renderers() {
    let Some(_env) = env() else { return };
    for name in ["keyframed_transform_ease", "fx_glitch", "transition_iris"] {
        let scene = find_scene(name);
        let t = scene.times[0];
        let a = export_renderer().render(&scene.project, t).unwrap();
        let b = export_renderer().render(&scene.project, t).unwrap();
        assert_eq!(a, b, "{name}: two fresh renderers disagree");
    }
}

// ---------------------------------------------------------------------------------------
// Dedicated preview/export parity tests for features that used to be preview gaps (they are
// also part of the all-scenes test above), plus known bugs, which stay #[ignore]d until fixed.
// Run ignored ones with: cargo test -p renderly-core --test golden_frames -- --ignored
// ---------------------------------------------------------------------------------------

fn assert_gap_scenes_match(names: &[&str]) {
    let Some(env) = env() else { return };
    let Some(mut preview) = PreviewPipeline::new() else {
        return;
    };
    let mut failures = Vec::new();
    for name in names {
        let s = find_scene(name);
        let mut export = export_renderer();
        for &t in &s.times {
            if let Err(e) = parity_check(name, &mut export, &mut preview, &s.project, t, env.images)
            {
                failures.push(e);
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Raster + generated (baked-cache) mattes: preview gets them fs-free via `set_matte_rgba`.
#[test]
fn parity_gap_raster_and_generated_mattes() {
    assert_gap_scenes_match(&["mask_raster_matte", "mask_generated_matte"]);
}

/// Pack `.cube` LUT: preview gets it fs-free via `parse_cube_text` + `LoadedPack::from_luts`.
#[test]
fn parity_gap_pack_cube_lut() {
    assert_gap_scenes_match(&["fx_pack_cube_lut"]);
}

/// Regression: `FrameRenderer` used to push `MediaKind::Image` through the ffmpeg
/// `VideoReader`, which failed on `probe_video` ("missing duration") so any export or
/// `render_frame` of a project with an image clip errored. Stills are now decoded once with
/// the `image` crate and held for the clip's whole span.
#[test]
fn still_image_media_renders_for_its_whole_span() {
    let Some(env) = env() else { return };
    let Some(mut preview) = PreviewPipeline::new() else {
        return;
    };
    let mut project = new_project();
    ensure_media(&mut project, Src::Bars, MediaKind::Image);
    let mut track = Track::new(TrackKind::Video, "V");
    track
        .clips
        .push(Clip::Video(Layer::new(Src::Bars, 0.0, 3.0).clip));
    project.tracks.push(track);
    let mut export = export_renderer();
    for t in [0.0, 1.0] {
        parity_check(
            "still_png",
            &mut export,
            &mut preview,
            &project,
            t,
            env.images,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }
}

/// REAL BUG (shared by export AND preview - the `ab_glyph` caption rasterizer in
/// `captions::blend_pixel`): it blends the style colour against the *transparent black* caption
/// canvas but then forces `alpha = 255`, so a semi-transparent `box_rgba` (tiktok-box is
/// `[0,0,0,160]`, youtube-lower-thirds `[20,20,20,200]`) and the `[0,0,0,180]` shadow come out
/// as OPAQUE dark pixels instead of letting the video show through (and anti-aliased glyph
/// edges get a dark fringe instead of coverage alpha). The committed `caption_box` /
/// `caption_bold_yellow` goldens pin today's (buggy) look - regenerate them when this is fixed.
#[test]
#[ignore = "BUG: caption box/shadow alpha is forced opaque in captions::blend_pixel (tiktok-box [0,0,0,160] renders solid black over video); see doc comment"]
fn bug_caption_box_is_opaque_instead_of_semi_transparent() {
    let Some(_env) = env() else { return };
    let scene = find_scene("caption_box");
    let out = export_renderer()
        .render(&scene.project, 1.0)
        .expect("render");
    // Left padding of the box (x 46..66, y 94..100) holds no glyph ink; the stripes below are
    // orange (R=250) / purple (R=60), so a 160/255 black box leaves mean R of roughly 57.
    let mut sum = 0u32;
    let mut n = 0u32;
    for y in 94..100u32 {
        for x in 46..66u32 {
            sum += out[((y * W + x) * 4) as usize] as u32;
            n += 1;
        }
    }
    let mean_r = sum as f64 / n as f64;
    assert!(
        mean_r > 20.0,
        "caption box over the stripes is opaque black (mean R {mean_r:.1}); video should show through"
    );
}
