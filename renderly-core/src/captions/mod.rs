//! Built-in caption style presets and CPU text rasterization, shared by native export
//! (burn-in) and the wasm preview compositor (docs/preview-webview.md P3). `ab_glyph` is
//! pure Rust with no fs/GPU dependency in its rasterization path, so the same glyph-outline
//! code runs on both targets — only font *acquisition* differs (`load_font` below): native
//! reads `RENDERLY_FONT_PATH` / system font files, wasm32 has no filesystem and embeds a
//! bundled font instead (see `renderly-wasm`'s `assets/Roboto-Bold.ttf`, Apache-2.0).
//!
//! `render_caption_for_project` (pack-style lookup, needs `fs`-backed pack loading) is
//! native-only; the wasm compositor calls the pure [`render_caption`] with builtin styles
//! only — a pack-authored custom caption style previews as the nearest builtin approximation
//! until pack loading lands in wasm (tracked alongside the other pack-LUT preview gaps in
//! `renderly-wasm/src/compositor.rs`'s module doc).

use crate::frame::RgbaFrame;
use ab_glyph::{point, Font, FontArc, Glyph, PxScale, ScaleFont};
use serde_json::Value;
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use thiserror::Error;

/// A style's overrides, as stored on a clip (`style.overrides`). The renderer reads `fill`
/// (`#RRGGBB[AA]`), `size` (fraction of the output height) and `anchor` (`top`, `center`,
/// `bottom`); the other properties are stored until the text renderer lands (docs/text-format.md).
pub type StyleOverrides = BTreeMap<String, Value>;

#[derive(Debug, Error)]
pub enum CaptionError {
    #[error("no usable font found; set RENDERLY_FONT_PATH to a .ttf file")]
    NoFont,
    #[error("font load failed: {0}")]
    FontLoad(String),
}

/// Built-in TikTok-style caption presets (Phase 1).
pub const STYLE_TIKTOK_BOLD_YELLOW: &str = "tiktok-bold-yellow";
pub const STYLE_TIKTOK_MINIMAL: &str = "tiktok-minimal";
pub const STYLE_TIKTOK_BOX: &str = "tiktok-box";
pub const STYLE_YOUTUBE_LOWER: &str = "youtube-lower-thirds";

pub const BUILTIN_STYLES: &[&str] = &[
    STYLE_TIKTOK_BOLD_YELLOW,
    STYLE_TIKTOK_MINIMAL,
    STYLE_TIKTOK_BOX,
    STYLE_YOUTUBE_LOWER,
];

struct StyleSpec {
    font_scale: f32,
    text_rgba: [u8; 4],
    outline_rgba: Option<[u8; 4]>,
    shadow_offset: Option<(i32, i32)>,
    box_rgba: Option<[u8; 4]>,
    vertical_anchor: f32, // 0=top, 1=bottom
}

fn style_spec(style_id: &str) -> StyleSpec {
    match style_id {
        STYLE_TIKTOK_MINIMAL => StyleSpec {
            font_scale: 48.0,
            text_rgba: [255, 255, 255, 255],
            outline_rgba: None,
            shadow_offset: Some((2, 2)),
            box_rgba: None,
            vertical_anchor: 0.82,
        },
        STYLE_TIKTOK_BOX => StyleSpec {
            font_scale: 44.0,
            text_rgba: [255, 255, 255, 255],
            outline_rgba: None,
            shadow_offset: None,
            box_rgba: Some([0, 0, 0, 160]),
            vertical_anchor: 0.80,
        },
        STYLE_YOUTUBE_LOWER => StyleSpec {
            font_scale: 36.0,
            text_rgba: [255, 255, 255, 255],
            outline_rgba: None,
            shadow_offset: None,
            box_rgba: Some([20, 20, 20, 200]),
            vertical_anchor: 0.88,
        },
        _ => StyleSpec {
            // tiktok-bold-yellow (default)
            font_scale: 52.0,
            text_rgba: [255, 255, 255, 255],
            outline_rgba: Some([255, 220, 0, 255]),
            shadow_offset: None,
            box_rgba: None,
            vertical_anchor: 0.78,
        },
    }
}

/// wasm32 has no filesystem: embed a bundled font so the preview always has *something* to
/// rasterize with. `RENDERLY_FONT_PATH`/system-font resolution (native only) may pick a
/// different face for export than this preview font — documented in
/// docs/preview-webview.md P3 as a known, acceptable parity gap (glyph metrics differ
/// slightly from Arial/DejaVu, so caption line-wrap width can shift a few px between
/// preview and export).
#[cfg(target_arch = "wasm32")]
static BUNDLED_FONT: &[u8] = include_bytes!("../../../renderly-wasm/assets/Roboto-Bold.ttf");

#[cfg(target_arch = "wasm32")]
pub(crate) fn load_font() -> Result<FontArc, CaptionError> {
    FontArc::try_from_slice(BUNDLED_FONT).map_err(|e| CaptionError::FontLoad(e.to_string()))
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn load_font() -> Result<FontArc, CaptionError> {
    if let Ok(path) = std::env::var("RENDERLY_FONT_PATH") {
        let data = std::fs::read(&path).map_err(|e| CaptionError::FontLoad(e.to_string()))?;
        return FontArc::try_from_vec(data).map_err(|e| CaptionError::FontLoad(e.to_string()));
    }

    for candidate in font_candidates() {
        if candidate.is_file() {
            if let Ok(data) = std::fs::read(&candidate) {
                if let Ok(font) = FontArc::try_from_vec(data) {
                    return Ok(font);
                }
            }
        }
    }

    Err(CaptionError::NoFont)
}

#[cfg(not(target_arch = "wasm32"))]
fn font_candidates() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    #[cfg(windows)]
    {
        paths.push(Path::new(r"C:\Windows\Fonts\arialbd.ttf").to_path_buf());
        paths.push(Path::new(r"C:\Windows\Fonts\arial.ttf").to_path_buf());
    }
    #[cfg(target_os = "macos")]
    {
        paths.push(Path::new("/System/Library/Fonts/Supplemental/Arial Bold.ttf").to_path_buf());
    }
    #[cfg(target_os = "linux")]
    {
        paths.push(Path::new("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf").to_path_buf());
    }
    paths
}

#[cfg(not(target_arch = "wasm32"))]
fn style_from_pack(style: &crate::packs::PackCaptionStyle) -> StyleSpec {
    let anchor = match style.anchor.as_str() {
        "top" => 0.18,
        "center" | "middle" => 0.5,
        _ => 0.82,
    };
    StyleSpec {
        font_scale: (style.font_scale * 900.0).clamp(24.0, 96.0),
        text_rgba: style.fill_rgba,
        outline_rgba: style.stroke_rgba,
        shadow_offset: style
            .shadow_rgba
            .map(|_| (style.shadow_offset[0] as i32, style.shadow_offset[1] as i32)),
        box_rgba: style.box_rgba,
        vertical_anchor: anchor,
    }
}

/// Rasterize a caption, resolving `style_id` against builtins then loaded asset packs.
/// Native-only: pack loading needs filesystem access (see `packs::load_installed_packs`).
/// The wasm preview compositor calls [`render_caption`] directly (builtin styles only).
#[cfg(not(target_arch = "wasm32"))]
pub fn render_caption_for_project(
    _project: &crate::project::Project,
    text: &str,
    style_id: &str,
    overrides: &StyleOverrides,
    width: u32,
    height: u32,
) -> Result<RgbaFrame, CaptionError> {
    let packs = crate::packs::load_installed_packs();
    if let Some(pack_style) = crate::packs::find_caption_style(&packs, style_id) {
        let mut spec = style_from_pack(pack_style);
        apply_overrides(&mut spec, overrides, height);
        return render_caption_with_spec(text, &spec, width, height);
    }
    render_caption_styled(text, style_id, overrides, width, height)
}

/// Rasterize a single caption line into an RGBA frame sized to the output resolution.
pub fn render_caption(
    text: &str,
    style_id: &str,
    width: u32,
    height: u32,
) -> Result<RgbaFrame, CaptionError> {
    render_caption_styled(text, style_id, &StyleOverrides::new(), width, height)
}

/// [`render_caption`] with the style's overrides applied.
pub fn render_caption_styled(
    text: &str,
    style_id: &str,
    overrides: &StyleOverrides,
    width: u32,
    height: u32,
) -> Result<RgbaFrame, CaptionError> {
    let mut spec = style_spec(style_id);
    apply_overrides(&mut spec, overrides, height);
    render_caption_with_spec(text, &spec, width, height)
}

/// Map the overrides the renderer understands onto the style. Anything else is ignored here.
fn apply_overrides(spec: &mut StyleSpec, overrides: &StyleOverrides, height: u32) {
    if let Some(rgba) = overrides
        .get("fill")
        .and_then(Value::as_str)
        .and_then(crate::project::text::parse_hex_color)
    {
        spec.text_rgba = rgba;
    }
    if let Some(size) = overrides.get("size").and_then(Value::as_f64) {
        if size.is_finite() && size > 0.0 {
            spec.font_scale = (size as f32 * height as f32).clamp(8.0, 512.0);
        }
    }
    match overrides.get("anchor").and_then(Value::as_str) {
        Some("top") => spec.vertical_anchor = 0.18,
        // The anchor is the baseline: centre the text block, not its baseline.
        Some("center") => {
            spec.vertical_anchor = 0.5 + 0.35 * spec.font_scale / height.max(1) as f32;
        }
        _ => {}
    }
}

fn render_caption_with_spec(
    text: &str,
    spec: &StyleSpec,
    width: u32,
    height: u32,
) -> Result<RgbaFrame, CaptionError> {
    let font = load_font()?;
    let scale = PxScale::from(spec.font_scale);
    let font_scaled = font.as_scaled(scale);

    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let text = text.trim();
    if text.is_empty() {
        return Ok(RgbaFrame {
            width,
            height,
            pixels,
        });
    }

    let glyph_width: f32 = text
        .chars()
        .map(|c| font_scaled.h_advance(font.glyph_id(c)))
        .sum();
    let start_x = ((width as f32 - glyph_width) / 2.0).max(0.0) as i32;
    let baseline_y = (height as f32 * spec.vertical_anchor) as i32;

    if let Some(box_rgba) = spec.box_rgba {
        let pad_x = 24;
        let pad_y = 12;
        let bx0 = (start_x - pad_x).max(0) as u32;
        let by0 = (baseline_y - spec.font_scale as i32 - pad_y).max(0) as u32;
        let bx1 = (start_x as u32 + glyph_width as u32 + pad_x as u32).min(width);
        let by1 = (baseline_y as u32 + pad_y as u32).min(height);
        fill_rect(&mut pixels, width, height, bx0, by0, bx1, by1, box_rgba);
    }

    if let Some((dx, dy)) = spec.shadow_offset {
        draw_text(
            &font,
            scale,
            text,
            start_x + dx,
            baseline_y + dy,
            [0, 0, 0, 180],
            &mut pixels,
            width,
            height,
        );
    }

    if let Some(outline) = spec.outline_rgba {
        for ox in -2..=2 {
            for oy in -2..=2 {
                if ox == 0 && oy == 0 {
                    continue;
                }
                draw_text(
                    &font,
                    scale,
                    text,
                    start_x + ox,
                    baseline_y + oy,
                    outline,
                    &mut pixels,
                    width,
                    height,
                );
            }
        }
    }

    draw_text(
        &font,
        scale,
        text,
        start_x,
        baseline_y,
        spec.text_rgba,
        &mut pixels,
        width,
        height,
    );

    Ok(RgbaFrame {
        width,
        height,
        pixels,
    })
}

#[allow(clippy::too_many_arguments)]
fn fill_rect(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    rgba: [u8; 4],
) {
    for y in y0..y1 {
        for x in x0..x1 {
            blend_pixel(pixels, width, height, x as i32, y as i32, rgba);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    font: &FontArc,
    scale: PxScale,
    text: &str,
    origin_x: i32,
    origin_y: i32,
    rgba: [u8; 4],
    pixels: &mut [u8],
    width: u32,
    height: u32,
) {
    let font_scaled = font.as_scaled(scale);
    let mut cursor_x = origin_x as f32;
    let baseline_y = origin_y as f32;

    for ch in text.chars() {
        let glyph_id = font.glyph_id(ch);
        let glyph: Glyph = glyph_id.with_scale_and_position(scale, point(cursor_x, baseline_y));
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|x, y, coverage| {
                let px = bounds.min.x as i32 + x as i32;
                let py = bounds.min.y as i32 + y as i32;
                let mut c = rgba;
                c[3] = (coverage * rgba[3] as f32).round().clamp(0.0, 255.0) as u8;
                blend_pixel(pixels, width, height, px, py, c);
            });
        }
        cursor_x += font_scaled.h_advance(glyph_id);
    }
}

/// Source-over compositing of one straight-alpha RGBA sample onto the caption canvas, which is
/// also straight alpha (the layer convention the compositor's `ALPHA_BLENDING` expects).
/// Keeping the canvas alpha honest is what lets semi-transparent boxes and shadows show the
/// video through them, and keeps glyph edges at their coverage alpha instead of a dark fringe.
fn blend_pixel(pixels: &mut [u8], width: u32, height: u32, x: i32, y: i32, rgba: [u8; 4]) {
    if x < 0 || y < 0 || x >= width as i32 || y >= height as i32 || rgba[3] == 0 {
        return;
    }
    let idx = ((y as u32 * width + x as u32) * 4) as usize;
    let src_a = rgba[3] as f32 / 255.0;
    let dst_a = pixels[idx + 3] as f32 / 255.0;
    let out_a = src_a + dst_a * (1.0 - src_a);
    if out_a <= 0.0 {
        return;
    }
    for i in 0..3 {
        let src = rgba[i] as f32;
        let dst = pixels[idx + i] as f32;
        let out = (src * src_a + dst * dst_a * (1.0 - src_a)) / out_a;
        pixels[idx + i] = out.round().clamp(0.0, 255.0) as u8;
    }
    pixels[idx + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_styles_list_is_non_empty() {
        assert!(BUILTIN_STYLES.contains(&STYLE_TIKTOK_BOLD_YELLOW));
    }

    #[test]
    fn render_caption_produces_pixels_when_font_available() {
        if load_font().is_err() {
            eprintln!("skipping caption render test: no font");
            return;
        }
        let frame = render_caption("test caption", STYLE_TIKTOK_MINIMAL, 640, 360).unwrap();
        assert_eq!(frame.pixels.len(), 640 * 360 * 4);
        assert!(frame.pixels.iter().any(|&b| b > 0));
    }

    /// Blend `src` over a 1x1 canvas that starts as `dst` and return the resulting pixel.
    fn blend_once(dst: [u8; 4], src: [u8; 4]) -> [u8; 4] {
        let mut pixels = dst.to_vec();
        blend_pixel(&mut pixels, 1, 1, 0, 0, src);
        [pixels[0], pixels[1], pixels[2], pixels[3]]
    }

    #[test]
    fn blend_opaque_over_transparent_is_exact_colour() {
        assert_eq!(
            blend_once([0, 0, 0, 0], [200, 100, 50, 255]),
            [200, 100, 50, 255]
        );
    }

    #[test]
    fn blend_semi_over_transparent_keeps_alpha_and_colour() {
        assert_eq!(blend_once([0, 0, 0, 0], [255, 0, 0, 128]), [255, 0, 0, 128]);
    }

    #[test]
    fn blend_semi_over_semi_is_source_over() {
        // Blue at alpha 128 under red at alpha 128: out_a = 0.502 + 0.502 * 0.498 = 0.752.
        let [r, g, b, a] = blend_once([0, 0, 255, 128], [255, 0, 0, 128]);
        assert!((190..=194).contains(&a), "alpha {a}");
        // Premultiplied sums divided by out_a: r = 255 * 0.502 / 0.752 ~= 170, b ~= 85.
        assert!((r as i32 - 170).abs() <= 2, "red {r}");
        assert_eq!(g, 0);
        assert!((b as i32 - 85).abs() <= 2, "blue {b}");
    }

    #[test]
    fn blend_opaque_over_semi_is_source_colour() {
        assert_eq!(
            blend_once([0, 0, 255, 128], [255, 0, 0, 255]),
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn blend_zero_coverage_is_noop() {
        assert_eq!(
            blend_once([10, 20, 30, 40], [255, 255, 255, 0]),
            [10, 20, 30, 40]
        );
    }

    /// The tiktok-box preset must stay translucent: inside the box padding (clear of glyphs)
    /// the canvas carries the box alpha and colour, and far outside the box it is empty.
    #[test]
    fn caption_box_pixels_carry_box_alpha() {
        if load_font().is_err() {
            eprintln!("skipping caption box alpha test: no font");
            return;
        }
        let (w, h) = (640u32, 360u32);
        let text = "Hello";
        let frame = render_caption(text, STYLE_TIKTOK_BOX, w, h).unwrap();
        // Same geometry as `render_caption_with_spec`: text centred, box padded 24px left.
        let font = load_font().unwrap();
        let scaled = font.as_scaled(PxScale::from(44.0));
        let glyph_width: f32 = text
            .chars()
            .map(|c| scaled.h_advance(font.glyph_id(c)))
            .sum();
        let start_x = ((w as f32 - glyph_width) / 2.0) as u32;
        let at = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [
                frame.pixels[i],
                frame.pixels[i + 1],
                frame.pixels[i + 2],
                frame.pixels[i + 3],
            ]
        };
        // Box left padding: 8px inside the box edge (24px pad), clear of the first glyph and
        // above the cap height (box top = baseline - 44 - 12 = 232).
        let inside = at(start_x - 24 + 8, 240);
        assert!(
            (inside[3] as i32 - 160).abs() <= 1,
            "box alpha {} should be ~160",
            inside[3]
        );
        for (c, v) in inside[..3].iter().enumerate() {
            assert!(*v <= 1, "box colour channel {c} = {v} should be ~0");
        }
        assert_eq!(
            at(5, 5)[3],
            0,
            "pixel far outside the box must be transparent"
        );
    }

    /// "F" is left-heavy; mirrored rendering puts more ink on the right half of its bbox.
    #[test]
    fn caption_text_is_not_mirrored() {
        if load_font().is_err() {
            eprintln!("skipping caption mirror test: no font");
            return;
        }
        let frame = render_caption("F", STYLE_TIKTOK_MINIMAL, 400, 200).unwrap();
        let (mut min_x, mut max_x, mut sum_x, mut count) = (u32::MAX, 0u32, 0f64, 0u64);
        for y in 0..frame.height {
            for x in 0..frame.width {
                let idx = ((y * frame.width + x) * 4) as usize;
                let alpha = frame.pixels[idx + 3];
                if alpha > 32 {
                    min_x = min_x.min(x);
                    max_x = max_x.max(x);
                    sum_x += x as f64;
                    count += 1;
                }
            }
        }
        assert!(count > 50, "expected visible glyph pixels");
        let centroid = sum_x / count as f64;
        let mid = (min_x + max_x) as f64 / 2.0;
        assert!(
            centroid < mid,
            "caption glyph appears mirrored (centroid {centroid} >= mid {mid})"
        );
    }
}
