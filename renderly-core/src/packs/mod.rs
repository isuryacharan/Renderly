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
pub(crate) mod hash;
#[cfg(not(target_arch = "wasm32"))]
pub mod host;
pub mod item;
pub mod refs;
#[cfg(not(target_arch = "wasm32"))]
pub mod rpack;
pub mod status;

#[cfg(not(target_arch = "wasm32"))]
pub use rpack::build;

pub use item::{builtin_items, ItemState, LoadedItem};

/// Engine features this build supports, for item `requires` (see docs/effect-pack-format.md,
/// "Feature requirements"). Items that need anything else are installed but disabled.
pub const FEATURES: &[&str] = crate::compose::runtime::FEATURES;

use serde::Deserialize;
use std::collections::BTreeMap;
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
    /// Serialized `Command` values; `AddClip.start_us` is offset by the apply call.
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

/// Duration of a pack item in integer µs. Packs written before schema v8 used the float
/// `default_duration_secs`; it is still read (deprecated alias) and converted.
fn pack_duration_us(us: Option<i64>, legacy_secs: Option<f64>, default_us: i64) -> i64 {
    match (us, legacy_secs) {
        (Some(us), _) => us,
        (None, Some(secs)) => crate::time::seconds_to_us(secs),
        (None, None) => default_us,
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RawPackTransitionAlias")]
pub struct PackTransitionAlias {
    pub id: String,
    pub label: String,
    /// Builtin transition kind name (`crossfade`, `wipe_left`, …).
    pub kind: String,
    pub default_duration_us: i64,
}

#[derive(Deserialize)]
struct RawPackTransitionAlias {
    id: String,
    label: String,
    kind: String,
    #[serde(default)]
    default_duration_us: Option<i64>,
    #[serde(default)]
    default_duration_secs: Option<f64>,
}

impl From<RawPackTransitionAlias> for PackTransitionAlias {
    fn from(r: RawPackTransitionAlias) -> Self {
        Self {
            id: r.id,
            label: r.label,
            kind: r.kind,
            default_duration_us: pack_duration_us(
                r.default_duration_us,
                r.default_duration_secs,
                default_transition_duration(),
            ),
        }
    }
}

fn default_transition_duration() -> i64 {
    500_000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(from = "RawPackSticker")]
pub struct PackSticker {
    pub id: String,
    pub label: String,
    pub path: String,
    pub default_duration_us: i64,
}

#[derive(Deserialize)]
struct RawPackSticker {
    id: String,
    label: String,
    path: String,
    #[serde(default)]
    default_duration_us: Option<i64>,
    #[serde(default)]
    default_duration_secs: Option<f64>,
}

impl From<RawPackSticker> for PackSticker {
    fn from(r: RawPackSticker) -> Self {
        Self {
            id: r.id,
            label: r.label,
            path: r.path,
            default_duration_us: pack_duration_us(
                r.default_duration_us,
                r.default_duration_secs,
                default_sticker_duration(),
            ),
        }
    }
}

fn default_sticker_duration() -> i64 {
    3_000_000
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
    /// Loaded `.cube` LUTs by [`lut_key`] (only items that loaded).
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

/// The key a LUT item has in [`LoadedPack::luts`]: its id for version 1, `id@version` for later
/// versions, so two versions of one LUT live side by side.
pub fn lut_key(id: &str, version: u32) -> String {
    if version == 1 {
        id.to_string()
    } else {
        format!("{id}@{version}")
    }
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
    // `license` is optional in format 1 and read only here (the library records it).
    let license = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("license")?.as_str().map(str::to_string));
    Ok(LoadedPack {
        root: root.to_path_buf(),
        manifest,
        luts,
        format: 1,
        canonical_id,
        meta: PackMeta {
            license,
            ..PackMeta::default()
        },
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
    /// The release this pack reports: `meta.release` for format 2, the manifest `version` for
    /// format 1.
    pub fn release(&self) -> String {
        self.meta
            .release
            .clone()
            .unwrap_or_else(|| self.manifest.version.clone())
    }

    /// A minimal in-memory pack carrying only parsed LUTs (keyed by [`lut_key`]) - no
    /// filesystem, no manifest beyond its id. This is what the wasm preview builds from `.cube` text the webview
    /// fetched, so `find_cube_lut` / the effect processor resolve `pack:<id>:lut:<lut>` the
    /// same way they do for a pack loaded from disk.
    pub fn from_luts(pack_id: &str, luts: BTreeMap<String, CubeLut>) -> Self {
        Self::from_parts(pack_id, luts, Vec::new())
    }

    /// An in-memory pack built from host-supplied parts: parsed `.cube` LUTs (each becomes a
    /// version-1 `lut` item) and already compiled effect items (see
    /// [`LoadedItem::from_sources`]). This is how the wasm preview receives pack content: the
    /// webview fetches the files from the host and core compiles them, so preview and export
    /// run the same items. `pack_id` is the id refs use (`local.starter`, `mira.film-looks`).
    pub fn from_parts(
        pack_id: &str,
        luts: BTreeMap<String, CubeLut>,
        effect_items: Vec<LoadedItem>,
    ) -> Self {
        let mut items: Vec<LoadedItem> = luts
            .keys()
            .map(|key| {
                // Keys are `id` (version 1) or `id@version` (see `lut_key`).
                let (id, version) = match key.rsplit_once('@') {
                    Some((id, v)) => (id, v.parse().unwrap_or(1)),
                    None => (key.as_str(), 1),
                };
                let mut item = LoadedItem::with_state("lut", id, version, ItemState::Ok);
                item.label = id.to_string();
                item
            })
            .collect();
        items.extend(effect_items);
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
            items,
        }
    }
}

/// A `pack:` ref a project uses, with the hash stored where it was applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRefUse {
    pub item_ref: String,
    pub hash: Option<String>,
}

/// Distinct `pack:` refs used by the project's effects and transitions (enabled or not), in
/// first-use order - what a host without a filesystem must fetch before those items can
/// render. Pure data walk.
pub fn referenced_pack_items(project: &crate::project::Project) -> Vec<PackRefUse> {
    use crate::project::Clip;
    let mut out: Vec<PackRefUse> = Vec::new();
    let mut push = |item_ref: &str, hash: &Option<String>| {
        if item_ref.starts_with("pack:") && !out.iter().any(|u| u.item_ref == item_ref) {
            out.push(PackRefUse {
                item_ref: item_ref.to_string(),
                hash: hash.clone(),
            });
        }
    };
    for track in project.all_tracks() {
        for clip in &track.clips {
            match clip {
                Clip::Video(m) | Clip::Audio(m) => {
                    for e in m.effect_stack() {
                        push(&e.item_ref, &e.hash);
                    }
                    if let Some(t) = &m.outgoing_transition {
                        push(&t.item_ref, &t.hash);
                    }
                }
                Clip::Adjustment(a) => {
                    for e in a.effect_stack() {
                        push(&e.item_ref, &e.hash);
                    }
                }
                Clip::Text(t) => {
                    for e in t.effect_stack() {
                        push(&e.item_ref, &e.hash);
                    }
                    if let Some(tr) = &t.outgoing_transition {
                        push(&tr.item_ref, &tr.hash);
                    }
                }
                // Text styles and animations are not effect-pack items yet (captions phase).
                Clip::Caption(_) => {}
            }
        }
    }
    out
}

pub fn find_pack<'a>(packs: &'a [LoadedPack], pack_id: &str) -> Option<&'a LoadedPack> {
    packs.iter().find(|p| refs::pack_matches(p, pack_id))
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

/// The packs installed in the user library (`RENDERLY_LIBRARY_DIR` or the default folder),
/// newest release first: what an item ref resolves against. Empty when the library cannot be
/// read. Native only; hosts pass the result to the renderer, so the compositor itself stays
/// pure.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_installed_packs() -> Vec<LoadedPack> {
    crate::library::Library::open(&crate::library::default_root())
        .map(|library| library.load_all())
        .unwrap_or_default()
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

pub fn find_cube_lut<'a>(
    packs: &'a [LoadedPack],
    pack_id: &str,
    lut_id: &str,
) -> Option<&'a CubeLut> {
    find_pack(packs, pack_id)?.luts.get(lut_id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn pack_durations_accept_micros_and_the_legacy_seconds_alias() {
        let new: PackTransitionAlias = serde_json::from_str(
            r#"{"id":"a","label":"A","kind":"crossfade","default_duration_us":350000}"#,
        )
        .unwrap();
        assert_eq!(new.default_duration_us, 350_000);
        let old: PackTransitionAlias = serde_json::from_str(
            r#"{"id":"a","label":"A","kind":"crossfade","default_duration_secs":0.35}"#,
        )
        .unwrap();
        assert_eq!(old.default_duration_us, 350_000);
        let none: PackSticker =
            serde_json::from_str(r#"{"id":"s","label":"S","path":"s.png"}"#).unwrap();
        assert_eq!(none.default_duration_us, 3_000_000);
        let sticker: PackSticker = serde_json::from_str(
            r#"{"id":"s","label":"S","path":"s.png","default_duration_secs":1.5}"#,
        )
        .unwrap();
        assert_eq!(sticker.default_duration_us, 1_500_000);
    }

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
