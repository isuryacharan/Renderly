//! One loaded pack item (an effect, filter, transition or LUT) and the builtin items, which are
//! the same kind of thing compiled into the binary.
//!
//! The loaders are native only (they read the item folder). The types and [`builtin_items`]
//! compile everywhere, so resolution and perception work on wasm32 too.

use std::sync::{Arc, OnceLock};

use crate::compose::builtins::{load_builtin, BUILTINS};
use crate::compose::runtime::{CompiledEffect, DecodedImage, ItemKind};

/// The load state of an item. Only `Ok` items render; the rest are reported with a reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemState {
    /// Installed, validated and usable.
    Ok,
    /// Installed, but this build cannot run it (an unknown engine feature, say). It never
    /// renders half-right: the reason is the "needs a newer Renderly (...)" message.
    Disabled { reason: String },
    /// Could not be loaded. `message` says why. The rest of the pack still loads.
    Error { message: String },
    /// Not loaded: its kind is unknown to this build.
    Skipped { reason: String },
}

impl ItemState {
    /// The state name perception and the CLI report: `ok`, `disabled`, `error`, `skipped`.
    pub fn name(&self) -> &'static str {
        match self {
            ItemState::Ok => "ok",
            ItemState::Disabled { .. } => "disabled",
            ItemState::Error { .. } => "error",
            ItemState::Skipped { .. } => "skipped",
        }
    }

    /// The human-readable reason for any state other than `Ok`.
    pub fn reason(&self) -> Option<&str> {
        match self {
            ItemState::Ok => None,
            ItemState::Disabled { reason } | ItemState::Skipped { reason } => Some(reason),
            ItemState::Error { message } => Some(message),
        }
    }
}

/// One item of a loaded pack (or a builtin).
#[derive(Debug, Clone)]
pub struct LoadedItem {
    /// `effect`, `filter`, `transition`, `lut`, or a format-1 kind (`sticker`, `sfx`,
    /// `caption_style`, `template`).
    pub kind: String,
    pub id: String,
    pub version: u32,
    pub label: String,
    /// The item folder (effect, filter, transition) or `.cube` file (lut), relative to the pack
    /// root.
    pub path: Option<String>,
    pub state: ItemState,
    /// SHA-256 hex over the item manifest and the files it references. Set when the item is
    /// `Ok`; `None` otherwise.
    pub hash: Option<String>,
    /// The compiled effect, filter or transition. `None` for LUTs and for items that are not
    /// `Ok`.
    pub compiled: Option<Arc<CompiledEffect>>,
    /// For a format-1 transition alias: the builtin transition kind it names (`wipe_left`).
    pub alias_of: Option<String>,
}

impl LoadedItem {
    /// An item with no files or compiled state: the shape of every failed or skipped item.
    pub fn with_state(kind: &str, id: &str, version: u32, state: ItemState) -> Self {
        LoadedItem {
            kind: kind.to_string(),
            id: id.to_string(),
            version,
            label: id.to_string(),
            path: None,
            state,
            hash: None,
            compiled: None,
            alias_of: None,
        }
    }
}

impl LoadedItem {
    /// Compiles an effect, filter or transition from in-memory sources: its `effect.json`, the
    /// WGSL files its passes name (by path) and its decoded file textures (by path). Nothing
    /// touches the filesystem, so the wasm preview builds pack items this way from files the
    /// webview fetched. A manifest or shader that fails to load gives an `Error` item that
    /// renders pass-through, like on the native side. `hash` is the host-reported content hash.
    pub fn from_sources(
        manifest_json: &str,
        shaders: &std::collections::BTreeMap<String, String>,
        textures: &std::collections::BTreeMap<String, DecodedImage>,
        hash: Option<String>,
    ) -> LoadedItem {
        let raw: serde_json::Value =
            serde_json::from_str(manifest_json).unwrap_or(serde_json::Value::Null);
        let id = raw
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let version = raw
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(1);
        match CompiledEffect::load(manifest_json, shaders, textures) {
            Ok(compiled) => {
                let m = compiled.manifest();
                LoadedItem {
                    kind: kind_name(m.kind).to_string(),
                    id: m.id.clone(),
                    version: m.version,
                    label: m.label.clone(),
                    path: None,
                    state: ItemState::Ok,
                    hash,
                    compiled: Some(Arc::new(compiled)),
                    alias_of: None,
                }
            }
            Err(e) => LoadedItem::with_state(
                "effect",
                &id,
                version,
                ItemState::Error {
                    message: e.to_string(),
                },
            ),
        }
    }
}

pub fn kind_name(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Effect => "effect",
        ItemKind::Filter => "filter",
        ItemKind::Transition => "transition",
    }
}

/// Every builtin item (`builtin:<id>@1`), compiled once per process. Their hashes are computed
/// on native targets only (`None` on wasm32).
pub fn builtin_items() -> &'static [LoadedItem] {
    static ITEMS: OnceLock<Vec<LoadedItem>> = OnceLock::new();
    ITEMS.get_or_init(|| BUILTINS.iter().map(|b| build_builtin(b.name)).collect())
}

fn build_builtin(name: &str) -> LoadedItem {
    match load_builtin(name) {
        Ok(compiled) => {
            let kind = kind_name(compiled.manifest().kind);
            let id = compiled.manifest().id.clone();
            let version = compiled.manifest().version;
            let label = compiled.manifest().label.clone();
            let hash = builtin_hash(name);
            LoadedItem {
                kind: kind.to_string(),
                id,
                version,
                label,
                path: None,
                state: ItemState::Ok,
                hash,
                compiled: Some(Arc::new(compiled)),
                alias_of: None,
            }
        }
        Err(e) => LoadedItem::with_state(
            "effect",
            name,
            1,
            ItemState::Error {
                message: e.to_string(),
            },
        ),
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn builtin_hash(name: &str) -> Option<String> {
    use super::hash;
    let (json, shaders) = crate::compose::builtins::builtin_sources(name)?;
    let mut entries = vec![("effect.json".to_string(), hash::sha256(json.as_bytes()))];
    for (file, src) in &shaders {
        entries.push((file.clone(), hash::sha256(src.as_bytes())));
    }
    Some(hash::item_hash(&entries))
}

#[cfg(target_arch = "wasm32")]
fn builtin_hash(_name: &str) -> Option<String> {
    None
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;

    use super::{kind_name, ItemState, LoadedItem};
    use crate::compose::runtime::manifest::check_relative_path;
    use crate::compose::runtime::{
        CompiledEffect, DecodedImage, EffectError, EffectManifest, ManifestError,
    };
    use crate::packs::hash::{self, Digest32};
    use crate::packs::{parse_cube_text, CubeLut};

    fn errored(kind: &str, id: &str, version: u32, message: String) -> LoadedItem {
        LoadedItem::with_state(kind, id, version, ItemState::Error { message })
    }

    fn read_in(folder: &Path, rel: &str) -> Result<Vec<u8>, String> {
        std::fs::read(folder.join(rel)).map_err(|e| format!("'{rel}': {e}"))
    }

    /// Loads the folder item at `rel` (holding `effect.json`) declared as `kind` in pack.json.
    pub(crate) fn load_folder(root: &Path, kind: &str, rel: &str) -> LoadedItem {
        let mut item = load_folder_inner(root, kind, rel);
        item.path = Some(rel.to_string());
        item
    }

    fn load_folder_inner(root: &Path, kind: &str, rel: &str) -> LoadedItem {
        if let Err(message) = check_relative_path(rel) {
            return errored(kind, "", 1, message);
        }
        let folder = root.join(rel);
        let manifest_path = folder.join("effect.json");
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(t) => t,
            Err(e) => return errored(kind, "", 1, format!("effect.json: {e}")),
        };
        // Identity and label come from the raw JSON so an item this build cannot run still
        // shows what it is.
        let raw: serde_json::Value = match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return errored(kind, "", 1, format!("effect.json is not valid JSON: {e}")),
        };
        let id = raw
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let version = raw
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(1);
        let label = raw
            .get("label")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| id.clone(), str::to_string);

        let manifest = match EffectManifest::parse(&text) {
            Ok(m) => m,
            Err(ManifestError::NeedsNewer(msg)) => {
                return with_disabled(
                    kind,
                    &id,
                    version,
                    label,
                    format!("needs a newer Renderly ({msg})"),
                )
            }
            Err(ManifestError::Format(n)) if n > 1 => {
                return with_disabled(
                    kind,
                    &id,
                    version,
                    label,
                    format!("needs a newer Renderly (effect.json format {n})"),
                )
            }
            Err(e) => return errored(kind, &id, version, e.to_string()),
        };
        if kind_name(manifest.kind) != kind {
            return errored(
                kind,
                &id,
                version,
                format!(
                    "effect.json is a {} but pack.json lists it as {kind}",
                    kind_name(manifest.kind)
                ),
            );
        }

        let mut digests: Vec<(String, Digest32)> = vec![(
            "effect.json".to_string(),
            hash::file_digest(&manifest_path, text.as_bytes()),
        )];
        let mut shaders: BTreeMap<String, String> = BTreeMap::new();
        for pass in &manifest.passes {
            if shaders.contains_key(&pass.shader) {
                continue;
            }
            let bytes = match read_in(&folder, &pass.shader) {
                Ok(b) => b,
                Err(message) => return errored(kind, &id, version, message),
            };
            digests.push((
                pass.shader.clone(),
                hash::file_digest(&folder.join(&pass.shader), &bytes),
            ));
            match String::from_utf8(bytes) {
                Ok(src) => shaders.insert(pass.shader.clone(), src),
                Err(_) => {
                    return errored(
                        kind,
                        &id,
                        version,
                        format!("shader '{}' is not UTF-8", pass.shader),
                    )
                }
            };
        }
        let mut textures: BTreeMap<String, DecodedImage> = BTreeMap::new();
        for tex in &manifest.textures {
            let bytes = match read_in(&folder, &tex.path) {
                Ok(b) => b,
                Err(message) => return errored(kind, &id, version, message),
            };
            digests.push((
                tex.path.clone(),
                hash::file_digest(&folder.join(&tex.path), &bytes),
            ));
            let rgba = match image::load_from_memory(&bytes) {
                Ok(img) => img.to_rgba8(),
                Err(e) => {
                    return errored(kind, &id, version, format!("texture '{}': {e}", tex.name))
                }
            };
            textures.insert(
                tex.path.clone(),
                DecodedImage {
                    width: rgba.width(),
                    height: rgba.height(),
                    rgba: rgba.into_raw(),
                },
            );
        }

        let compiled = match CompiledEffect::load(&text, &shaders, &textures) {
            Ok(c) => c,
            Err(EffectError::Manifest(ManifestError::NeedsNewer(msg))) => {
                return with_disabled(
                    kind,
                    &id,
                    version,
                    label,
                    format!("needs a newer Renderly ({msg})"),
                )
            }
            Err(e) => return errored(kind, &id, version, e.to_string()),
        };
        LoadedItem {
            kind: kind.to_string(),
            id: manifest.id.clone(),
            version: manifest.version,
            label: manifest.label.clone(),
            path: None,
            state: ItemState::Ok,
            hash: Some(hash::item_hash(&digests)),
            compiled: Some(Arc::new(compiled)),
            alias_of: None,
        }
    }

    fn with_disabled(
        kind: &str,
        id: &str,
        version: u32,
        label: String,
        reason: String,
    ) -> LoadedItem {
        let mut item = LoadedItem::with_state(kind, id, version, ItemState::Disabled { reason });
        item.label = label;
        item
    }

    /// Loads an inline LUT item. Only `.cube` files load for now; PNG layouts are reported.
    pub(crate) fn load_lut(
        root: &Path,
        id: &str,
        version: u32,
        label: &str,
        rel: &str,
        layout: Option<&str>,
    ) -> (LoadedItem, Option<CubeLut>) {
        let fail = |message: String| {
            let mut item = errored("lut", id, version, message);
            item.path = Some(rel.to_string());
            (item, None)
        };
        if let Err(message) = check_relative_path(rel) {
            return fail(message);
        }
        if let Some(layout) = layout {
            if layout != "cube" {
                return fail(format!(
                    "LUT layout `{layout}` is not supported yet; use a .cube file"
                ));
            }
        }
        if !rel.to_ascii_lowercase().ends_with(".cube") {
            return fail(format!(
                "LUT '{rel}' must be a .cube file (PNG layouts are not supported yet)"
            ));
        }
        let file = root.join(rel);
        let bytes = match std::fs::read(&file) {
            Ok(b) => b,
            Err(e) => return fail(format!("'{rel}': {e}")),
        };
        let cube = match std::str::from_utf8(&bytes)
            .map_err(|e| e.to_string())
            .and_then(parse_cube_text)
        {
            Ok(c) => c,
            Err(message) => return fail(format!("LUT '{rel}': {message}")),
        };
        let declaration = format!(
            "kind=lut\nid={id}\nlabel={label}\nversion={version}\nlayout={}\n",
            layout.unwrap_or("cube")
        );
        let digests = vec![
            (
                "declaration".to_string(),
                hash::sha256(declaration.as_bytes()),
            ),
            (rel.to_string(), hash::file_digest(&file, &bytes)),
        ];
        let item = LoadedItem {
            kind: "lut".to_string(),
            id: id.to_string(),
            version,
            label: label.to_string(),
            path: Some(rel.to_string()),
            state: ItemState::Ok,
            hash: Some(hash::item_hash(&digests)),
            compiled: None,
            alias_of: None,
        };
        (item, Some(cube))
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::{load_folder, load_lut};
