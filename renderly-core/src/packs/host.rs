//! The files of one pack item, read from disk for a host that cannot read them itself.
//!
//! The wasm preview compositor has no filesystem. The webview asks the backend for the pack items
//! a project references (`packs::referenced_pack_items`), and the backend answers with this: the
//! LUT's `.cube` text, or an effect's manifest, WGSL and texture files. The wasm side compiles
//! them with the same core code export uses (`LoadedItem::from_sources`,
//! `LoadedPack::from_parts`), so preview and export run the same items.
//!
//! Native only.

use std::collections::BTreeMap;

use super::refs::ItemRef;
use super::status::{resolve_target, Target};
use super::LoadedPack;

/// One texture file of an effect, still encoded (PNG/JPEG...): the host decodes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostTextureFile {
    /// The path the manifest names it by.
    pub path: String,
    pub bytes: Vec<u8>,
}

/// What the preview needs to run one pack item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostItem {
    /// The pack id as the ref names it, which is the canonical id (`local.starter`).
    pub pack_id: String,
    pub item_id: String,
    pub version: u32,
    /// `lut`, `effect`, `filter` or `transition`.
    pub kind: String,
    /// The installed item's content hash.
    pub hash: Option<String>,
    /// The `.cube` text, for a LUT.
    pub cube: Option<String>,
    /// `effect.json`, for effects, filters and transitions.
    pub manifest: Option<String>,
    /// The WGSL files the manifest's passes name, by path.
    pub shaders: BTreeMap<String, String>,
    pub textures: Vec<HostTextureFile>,
}

/// The files behind the pack ref `item_ref` among `packs`, or an error naming what is missing.
/// Builtin refs and refs that do not resolve to an installed, runnable item are errors: the
/// wasm build has the builtins compiled in, and a missing item renders pass-through.
pub fn host_item(
    packs: &[LoadedPack],
    item_ref: &str,
    hash: Option<&str>,
) -> Result<HostItem, String> {
    let Ok(ItemRef::Pack { .. }) = ItemRef::parse(item_ref) else {
        return Err(format!("'{item_ref}' is not a pack item ref"));
    };
    let resolved = resolve_target(packs, item_ref, hash);
    match resolved.target {
        None => Err(format!(
            "'{item_ref}' is not installed or cannot run here ({})",
            resolved.status.as_str()
        )),
        Some(Target::Lut { item, pack, .. }) => {
            let rel = item
                .path
                .as_deref()
                .ok_or_else(|| format!("LUT '{}' has no file", item.id))?;
            let cube_text = std::fs::read_to_string(pack.root.join(rel))
                .map_err(|e| format!("LUT '{rel}': {e}"))?;
            Ok(HostItem {
                pack_id: pack.canonical_id.clone(),
                item_id: item.id.clone(),
                version: item.version,
                kind: "lut".into(),
                hash: item.hash.clone(),
                cube: Some(cube_text),
                manifest: None,
                shaders: BTreeMap::new(),
                textures: Vec::new(),
            })
        }
        Some(Target::Compiled(item)) => {
            let pack = packs
                .iter()
                .find(|p| p.items.iter().any(|i| std::ptr::eq(i, item)))
                .ok_or_else(|| format!("'{item_ref}' has no pack"))?;
            let rel = item
                .path
                .as_deref()
                .ok_or_else(|| format!("item '{}' has no folder", item.id))?;
            let folder = pack.root.join(rel);
            let manifest = std::fs::read_to_string(folder.join("effect.json"))
                .map_err(|e| format!("effect.json: {e}"))?;
            let compiled = item
                .compiled
                .as_ref()
                .ok_or_else(|| format!("item '{}' did not compile", item.id))?;
            let mut shaders = BTreeMap::new();
            for pass in &compiled.manifest().passes {
                if shaders.contains_key(&pass.shader) {
                    continue;
                }
                let text = std::fs::read_to_string(folder.join(&pass.shader))
                    .map_err(|e| format!("shader '{}': {e}", pass.shader))?;
                shaders.insert(pass.shader.clone(), text);
            }
            let mut textures = Vec::new();
            for tex in &compiled.manifest().textures {
                let bytes = std::fs::read(folder.join(&tex.path))
                    .map_err(|e| format!("texture '{}': {e}", tex.path))?;
                textures.push(HostTextureFile {
                    path: tex.path.clone(),
                    bytes,
                });
            }
            Ok(HostItem {
                pack_id: pack.canonical_id.clone(),
                item_id: item.id.clone(),
                version: item.version,
                kind: item.kind.clone(),
                hash: item.hash.clone(),
                cube: None,
                manifest: Some(manifest),
                shaders,
                textures,
            })
        }
    }
}
