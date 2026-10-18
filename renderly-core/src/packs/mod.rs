//! Declarative asset packs (`pack.json` + assets). See docs/asset-pack.md (format 1) and
//! docs/effect-pack-format.md (format 2: typed items, versions, hashes, refs).
//!
//! - [`load_pack`] reads either format; format 1 keeps its original behaviour and is also
//!   viewed as items ([`LoadedPack::items`]), format 2 is loaded by [`format2`].
//! - [`refs`]: `pack:` / `builtin:` item refs and the resolver (pure).
//! - [`item`]: one loaded item and the builtin items.
//! - [`describe`]: the catalogue that `describe_commands` / `describe_effects` print, and the
//!   per-item report of `renderly pack validate` (pure).

pub mod describe;
pub mod format2;
#[cfg(not(target_arch = "wasm32"))]
mod hash;
pub mod item;
pub mod refs;

pub use item::{builtin_items, ItemState, LoadedItem};

/// Engine features this build supports, for item `requires` (see docs/effect-pack-format.md,
/// "Feature requirements"). Items that need anything else are installed but disabled.
pub const FEATURES: &[&str] = crate::compose::runtime::FEATURES;

#[cfg(not(target_arch = "wasm32"))]
use crate::frame::RgbaFrame;
#[cfg(not(target_arch = "wasm32"))]
use crate::project::{EffectInstance, Project};
use serde::Deserialize;
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::collections::HashSet;
#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize)]
pub struct PackManifest {
    pub id: String,
    pub name: String,
    #[serde(default = "default_pack_version")]
    pub version: String,
    #[serde(default)]
    pub caption_styles: Vec<PackCaptionStyle>,
    #[serde(default)]
    pub luts: Vec<PackLut>,
    #[serde(default)]
    pub transitions: Vec<PackTransitionAlias>,
    #[serde(default)]
    pub stickers: Vec<PackSticker>,
    #[serde(default)]
    pub sfx: Vec<PackSfx>,
    /// Named command sequences applied via `ApplyTemplate` (Phase 4).
    #[serde(default)]
    pub templates: Vec<PackTemplate>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackTemplate {
    pub id: String,
    pub label: String,
    /// Serialized `Command` values; `AddClip.position_secs` is offset by the apply call.
    #[serde(default)]
    pub commands: Vec<serde_json::Value>,
}

fn default_pack_version() -> String {
    "1".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackCaptionStyle {
    pub id: String,
    pub label: String,
    #[serde(default = "default_font_scale")]
    pub font_scale: f32,
    #[serde(default = "default_fill")]
    pub fill_rgba: [u8; 4],
    #[serde(default)]
    pub stroke_rgba: Option<[u8; 4]>,
    #[serde(default)]
    pub stroke_width: f32,
    #[serde(default)]
    pub shadow_rgba: Option<[u8; 4]>,
    #[serde(default)]
    pub shadow_offset: [f32; 2],
    #[serde(default)]
    pub box_rgba: Option<[u8; 4]>,
    #[serde(default = "default_anchor")]
    pub anchor: String,
}

fn default_font_scale() -> f32 {
    0.06
}
fn default_fill() -> [u8; 4] {
    [255, 255, 255, 255]
}
fn default_anchor() -> String {
    "bottom".into()
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackLut {
    pub id: String,
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackTransitionAlias {
    pub id: String,
    pub label: String,
    /// Builtin transition kind name (`crossfade`, `wipe_left`, …).
    pub kind: String,
    #[serde(default = "default_transition_duration")]
    pub default_duration_secs: f64,
}

fn default_transition_duration() -> f64 {
    0.5
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackSticker {
    pub id: String,
    pub label: String,
    pub path: String,
    #[serde(default = "default_sticker_duration")]
    pub default_duration_secs: f64,
}

fn default_sticker_duration() -> f64 {
    3.0
}

#[derive(Debug, Clone, Deserialize)]
pub struct PackSfx {
    pub id: String,
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct LoadedPack {
    pub root: PathBuf,
    /// The format-1 view of the manifest. For format 2 it carries the id, name, release and
    /// the LUTs that loaded; `caption_styles`, `stickers`, `sfx`, `templates` and `transitions`
    /// stay empty (format 2 has items instead).
    pub manifest: PackManifest,
    /// Loaded `.cube` LUTs by item id (only items that loaded).
    pub luts: BTreeMap<String, CubeLut>,
    /// Pack format this was read from: 1 (no `format` field) or 2.
    pub format: u32,
    /// The id refs use: `manifest.id` for format 2; `local.<id>` for a format-1 id that has no
    /// publisher. Refs may name either (see [`refs::pack_matches`]).
    pub canonical_id: String,
    pub meta: PackMeta,
    /// Every item in pack order, including skipped and failed ones.
    pub items: Vec<LoadedItem>,
}

/// Pack-level fields that do not affect loading. Empty for format 1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PackMeta {
    pub release: Option<String>,
    pub license: Option<String>,
    pub authors: Vec<String>,
    pub description: String,
    pub tags: Vec<String>,
    /// `requires.renderly` as declared. A pack whose requirement this build does not meet has
    /// every item disabled.
    pub requires_renderly: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CubeLut {
    pub size: usize,
    /// Packed RGB triples, size^3 entries.
    pub data: Vec<[f32; 3]>,
}

/// Everything below that touches the filesystem (pack manifest/`.cube` loading) is
/// native-only. The wasm compositor is fed pack LUTs another way: the webview fetches the
/// `.cube` text from the host and builds [`LoadedPack::from_luts`] via [`parse_cube_text`]
/// (docs/preview-webview.md P4b). The pure LUT helpers (`CubeLut`, `parse_cube_text`,
/// `cube_lut_upload_bytes`, `find_cube_lut`, `parse_pack_lut_id`, `referenced_pack_luts`)
/// compile everywhere.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_pack(root: &Path) -> Result<LoadedPack, String> {
    let manifest_path = root.join("pack.json");
    let text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("pack.json: {e}"))?;
    match value.get("format") {
        None => load_format1(root, &text),
        Some(f) => match f.as_u64() {
            Some(1) => load_format1(root, &text),
            Some(2) => format2::load(root, &text),
            _ => Err(format!(
                "pack.json: unsupported pack format {f} (this build reads formats 1 and 2)"
            )),
        },
    }
}

/// Format 1: the original loader. A broken LUT still fails the whole pack, as it always has.
#[cfg(not(target_arch = "wasm32"))]
fn load_format1(root: &Path, text: &str) -> Result<LoadedPack, String> {
    let manifest: PackManifest =
        serde_json::from_str(text).map_err(|e| format!("pack.json: {e}"))?;
    if manifest.id.trim().is_empty() {
        return Err("pack id must be non-empty".into());
    }
    let mut luts = BTreeMap::new();
    for lut in &manifest.luts {
        let path = root.join(&lut.path);
        let cube = parse_cube(&path).map_err(|e| format!("lut '{}': {e}", lut.id))?;
        luts.insert(lut.id.clone(), cube);
    }
    let items = items_from_format1(&manifest, &luts);
    let canonical_id = if manifest.id.contains('.') {
        manifest.id.clone()
    } else {
        format!("local.{}", manifest.id)
    };
    Ok(LoadedPack {
        root: root.to_path_buf(),
        manifest,
        luts,
        format: 1,
        canonical_id,
        meta: PackMeta::default(),
        items,
    })
}

/// A format-1 manifest as items, in memory: LUTs, stickers, sfx, caption styles, templates,
/// and the `transitions` aliases of builtin transition kinds. Every item is version 1.
pub fn items_from_format1(
    manifest: &PackManifest,
    luts: &BTreeMap<String, CubeLut>,
) -> Vec<LoadedItem> {
    let ok = |kind: &str, id: &str, label: &str| {
        let mut item = LoadedItem::with_state(kind, id, 1, ItemState::Ok);
        item.label = label.to_string();
        item
    };
    let mut items = Vec::new();
    for lut in &manifest.luts {
        if luts.contains_key(&lut.id) {
            let mut item = ok("lut", &lut.id, &lut.label);
            item.path = Some(lut.path.clone());
            items.push(item);
        }
    }
    for s in &manifest.stickers {
        let mut item = ok("sticker", &s.id, &s.label);
        item.path = Some(s.path.clone());
        items.push(item);
    }
    for s in &manifest.sfx {
        let mut item = ok("sfx", &s.id, &s.label);
        item.path = Some(s.path.clone());
        items.push(item);
    }
    for s in &manifest.caption_styles {
        items.push(ok("caption_style", &s.id, &s.label));
    }
    for t in &manifest.templates {
        items.push(ok("template", &t.id, &t.label));
    }
    for t in &manifest.transitions {
        let mut item = ok("transition", &t.id, &t.label);
        item.alias_of = Some(t.kind.clone());
        items.push(item);
    }
    items
}

impl LoadedPack {
    /// A minimal in-memory pack carrying only parsed LUTs - no filesystem, no manifest
    /// beyond its id. This is what the wasm preview builds from `.cube` text the webview
    /// fetched, so `find_cube_lut` / the effect processor resolve `pack:<id>:lut:<lut>` the
    /// same way they do for a pack loaded from disk.
    pub fn from_luts(pack_id: &str, luts: BTreeMap<String, CubeLut>) -> Self {
        let manifest = PackManifest {
            id: pack_id.to_string(),
            name: pack_id.to_string(),
            version: default_pack_version(),
            caption_styles: Vec::new(),
            luts: luts
                .keys()
                .map(|id| PackLut {
                    id: id.clone(),
                    label: id.clone(),
                    path: String::new(),
                })
                .collect(),
            transitions: Vec::new(),
            stickers: Vec::new(),
            sfx: Vec::new(),
            templates: Vec::new(),
        };
        LoadedPack {
            root: PathBuf::new(),
            manifest,
            luts,
            format: 1,
            canonical_id: if pack_id.contains('.') {
                pack_id.to_string()
            } else {
                format!("local.{pack_id}")
            },
            meta: PackMeta::default(),
            items: Vec::new(),
        }
    }
}

/// Distinct `(pack_id, lut_id)` pairs of every enabled-or-not pack-LUT effect on the
/// project's media clips, in first-use order - what a host without a filesystem must fetch
/// before those effects can render. Pure data walk.
pub fn referenced_pack_luts(project: &crate::project::Project) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for track in project.all_tracks() {
        for clip in &track.clips {
            let effects = match clip {
                crate::project::Clip::Video(m) | crate::project::Clip::Audio(m) => &m.effects,
                crate::project::Clip::Adjustment(a) => &a.effects,
                crate::project::Clip::Caption(_) => continue,
            };
            for effect in effects {
                if let Some((pack, lut)) = parse_pack_lut_id(&effect.effect_id) {
                    let pair = (pack.to_string(), lut.to_string());
                    if !out.contains(&pair) {
                        out.push(pair);
                    }
                }
            }
        }
    }
    out
}

pub fn find_pack<'a>(packs: &'a [LoadedPack], pack_id: &str) -> Option<&'a LoadedPack> {
    packs.iter().find(|p| p.manifest.id == pack_id)
}

pub fn find_sticker<'a>(
    packs: &'a [LoadedPack],
    pack_id: &str,
    sticker_id: &str,
) -> Option<(&'a LoadedPack, &'a PackSticker)> {
    let pack = find_pack(packs, pack_id)?;
    let sticker = pack.manifest.stickers.iter().find(|s| s.id == sticker_id)?;
    Some((pack, sticker))
}

pub fn find_sfx<'a>(
    packs: &'a [LoadedPack],
    pack_id: &str,
    sfx_id: &str,
) -> Option<(&'a LoadedPack, &'a PackSfx)> {
    let pack = find_pack(packs, pack_id)?;
    let sfx = pack.manifest.sfx.iter().find(|s| s.id == sfx_id)?;
    Some((pack, sfx))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn pack_id_at(root: &Path) -> Option<String> {
    load_pack(root).ok().map(|p| p.manifest.id)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn load_project_packs(project: &Project) -> Vec<LoadedPack> {
    let mut out = Vec::new();
    for path in &project.asset_pack_paths {
        if let Ok(pack) = load_pack(path) {
            out.push(pack);
        }
    }
    out
}

#[cfg(not(target_arch = "wasm32"))]
pub fn known_effect_ids(project: &Project) -> HashSet<String> {
    let mut ids = HashSet::new();
    for pack in load_project_packs(project) {
        for lut in &pack.manifest.luts {
            ids.insert(format!("pack:{}:lut:{}", pack.manifest.id, lut.id));
        }
    }
    ids
}

pub fn effect_id_for_lut(pack_id: &str, lut_id: &str) -> String {
    format!("pack:{pack_id}:lut:{lut_id}")
}

pub fn find_cube_lut<'a>(
    packs: &'a [LoadedPack],
    pack_id: &str,
    lut_id: &str,
) -> Option<&'a CubeLut> {
    find_pack(packs, pack_id)?.luts.get(lut_id)
}

/// RGBA8 bytes for uploading a `.cube` LUT as a `D3` texture (R fastest, then G, then B).
pub fn cube_lut_upload_bytes(cube: &CubeLut) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(cube.data.len() * 4);
    for rgb in &cube.data {
        bytes.push((rgb[0].clamp(0.0, 1.0) * 255.0) as u8);
        bytes.push((rgb[1].clamp(0.0, 1.0) * 255.0) as u8);
        bytes.push((rgb[2].clamp(0.0, 1.0) * 255.0) as u8);
        bytes.push(255);
    }
    bytes
}

/// Apply enabled pack LUT effects (CPU) to a frame in list order.
///
/// Pack LUTs are normally applied on the GPU in [`crate::compose::effects::EffectProcessor`]
/// (improvement-plan A7). This path remains for tests and headless fallbacks. Native-only
/// (loads packs from disk via `Project::asset_pack_paths`); the wasm compositor defers pack
/// LUTs, see docs/preview-webview.md P2.
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_pack_effects(project: &Project, frame: &mut RgbaFrame, effects: &[EffectInstance]) {
    apply_pack_effects_cpu(project, frame, effects);
}

/// CPU trilinear pack LUT application (see [`apply_pack_effects`]).
#[cfg(not(target_arch = "wasm32"))]
pub fn apply_pack_effects_cpu(
    project: &Project,
    frame: &mut RgbaFrame,
    effects: &[EffectInstance],
) {
    let packs = load_project_packs(project);
    for effect in effects.iter().filter(|e| e.enabled) {
        let Some((pack_id, lut_id)) = parse_pack_lut_id(&effect.effect_id) else {
            continue;
        };
        let Some(pack) = packs.iter().find(|p| p.manifest.id == pack_id) else {
            continue;
        };
        let Some(cube) = pack.luts.get(lut_id) else {
            continue;
        };
        let intensity = effect
            .params
            .get("intensity")
            .copied()
            .unwrap_or(1.0)
            .clamp(0.0, 1.0) as f32;
        apply_cube(frame, cube, intensity);
    }
}

pub fn parse_pack_lut_id(effect_id: &str) -> Option<(&str, &str)> {
    let rest = effect_id.strip_prefix("pack:")?;
    let (pack_id, lut) = rest.split_once(":lut:")?;
    if pack_id.is_empty() || lut.is_empty() {
        return None;
    }
    Some((pack_id, lut))
}

pub fn find_caption_style<'a>(
    packs: &'a [LoadedPack],
    style_id: &str,
) -> Option<&'a PackCaptionStyle> {
    for pack in packs {
        if let Some(s) = pack
            .manifest
            .caption_styles
            .iter()
            .find(|s| s.id == style_id)
        {
            return Some(s);
        }
    }
    None
}

#[cfg(not(target_arch = "wasm32"))]
fn parse_cube(path: &Path) -> Result<CubeLut, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_cube_text(&text)
}

/// Parse the text of an Adobe/Resolve `.cube` 3D LUT. Pure (`&str` in), so the wasm preview
/// can parse LUT text the webview fetched from the host; native `load_pack` reads the file and
/// calls the same function, so both targets build identical [`CubeLut`]s.
pub fn parse_cube_text(text: &str) -> Result<CubeLut, String> {
    let mut size = 0usize;
    let mut data = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("TITLE") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("LUT_3D_SIZE") {
            size = rest
                .trim()
                .parse()
                .map_err(|_| "invalid LUT_3D_SIZE".to_string())?;
            continue;
        }
        if line.starts_with("DOMAIN_") {
            continue;
        }
        let parts: Vec<_> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let r: f32 = parts[0].parse().map_err(|_| "bad cube RGB".to_string())?;
            let g: f32 = parts[1].parse().map_err(|_| "bad cube RGB".to_string())?;
            let b: f32 = parts[2].parse().map_err(|_| "bad cube RGB".to_string())?;
            data.push([r, g, b]);
        }
    }
    if size == 0 {
        return Err("missing LUT_3D_SIZE".into());
    }
    if data.len() != size * size * size {
        return Err(format!(
            "expected {} RGB triples, got {}",
            size * size * size,
            data.len()
        ));
    }
    Ok(CubeLut { size, data })
}

#[cfg(not(target_arch = "wasm32"))]
fn apply_cube(frame: &mut RgbaFrame, cube: &CubeLut, intensity: f32) {
    if intensity <= 0.0 {
        return;
    }
    let n = cube.size;
    let max_i = (n - 1) as f32;
    for px in frame.pixels.as_chunks_mut::<4>().0 {
        let r = px[0] as f32 / 255.0;
        let g = px[1] as f32 / 255.0;
        let b = px[2] as f32 / 255.0;
        let mapped = sample_cube(cube, r, g, b, max_i);
        px[0] = ((r + (mapped[0] - r) * intensity) * 255.0).clamp(0.0, 255.0) as u8;
        px[1] = ((g + (mapped[1] - g) * intensity) * 255.0).clamp(0.0, 255.0) as u8;
        px[2] = ((b + (mapped[2] - b) * intensity) * 255.0).clamp(0.0, 255.0) as u8;
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn sample_cube(cube: &CubeLut, r: f32, g: f32, b: f32, max_i: f32) -> [f32; 3] {
    let n = cube.size;
    let rf = (r * max_i).clamp(0.0, max_i);
    let gf = (g * max_i).clamp(0.0, max_i);
    let bf = (b * max_i).clamp(0.0, max_i);
    let r0 = rf.floor() as usize;
    let g0 = gf.floor() as usize;
    let b0 = bf.floor() as usize;
    let r1 = (r0 + 1).min(n - 1);
    let g1 = (g0 + 1).min(n - 1);
    let b1 = (b0 + 1).min(n - 1);
    let tr = rf - r0 as f32;
    let tg = gf - g0 as f32;
    let tb = bf - b0 as f32;

    let c000 = cube_at(cube, r0, g0, b0);
    let c100 = cube_at(cube, r1, g0, b0);
    let c010 = cube_at(cube, r0, g1, b0);
    let c110 = cube_at(cube, r1, g1, b0);
    let c001 = cube_at(cube, r0, g0, b1);
    let c101 = cube_at(cube, r1, g0, b1);
    let c011 = cube_at(cube, r0, g1, b1);
    let c111 = cube_at(cube, r1, g1, b1);

    let c00 = lerp3(c000, c100, tr);
    let c10 = lerp3(c010, c110, tr);
    let c01 = lerp3(c001, c101, tr);
    let c11 = lerp3(c011, c111, tr);
    let c0 = lerp3(c00, c10, tg);
    let c1 = lerp3(c01, c11, tg);
    lerp3(c0, c1, tb)
}

#[cfg(not(target_arch = "wasm32"))]
fn cube_at(cube: &CubeLut, r: usize, g: usize, b: usize) -> [f32; 3] {
    let n = cube.size;
    cube.data[r + g * n + b * n * n]
}

#[cfg(not(target_arch = "wasm32"))]
fn lerp3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn cube_upload_bytes_len() {
        let cube = CubeLut {
            size: 2,
            data: vec![[0.0, 0.0, 0.0]; 8],
        };
        assert_eq!(cube_lut_upload_bytes(&cube).len(), 8 * 4);
    }

    #[test]
    fn parse_identity_cube() {
        let dir = tempfile_dir();
        let cube_path = dir.join("id.cube");
        let mut f = std::fs::File::create(&cube_path).unwrap();
        writeln!(f, "LUT_3D_SIZE 2").unwrap();
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    writeln!(f, "{} {} {}", r as f32, g as f32, b as f32).unwrap();
                }
            }
        }
        let lut = parse_cube(&cube_path).unwrap();
        assert_eq!(lut.size, 2);
        assert_eq!(lut.data.len(), 8);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("renderly-pack-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
