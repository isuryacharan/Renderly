//! Pack format 2: `pack.json` with `"format": 2` (docs/effect-pack-format.md, "Packs and items").
//!
//! [`parse`] is pure: it validates the pack-level fields and turns each `items[]` entry into an
//! [`ItemDecl`]. A pack-level problem (bad id, missing `license`, unsupported `requires`) refuses
//! the whole pack. An item-level problem only marks that item. [`load`] (native) then loads
//! each declared item and assembles the [`LoadedPack`].

use serde::Deserialize;

use super::refs::is_item_id;

/// The pack format this parser reads.
pub const FORMAT: u32 = 2;

/// The validated pack-level fields of a format-2 `pack.json`, with its item declarations.
#[derive(Debug, Clone, PartialEq)]
pub struct PackDecl {
    /// `<publisher>.<name>`.
    pub id: String,
    pub name: String,
    /// Semver of this pack release.
    pub release: String,
    pub license: String,
    pub authors: Vec<String>,
    pub description: String,
    pub tags: Vec<String>,
    /// `requires.renderly`, such as `>=0.9`.
    pub requires_renderly: Option<String>,
    pub items: Vec<ItemDecl>,
}

/// One `items[]` entry, as declared. Identity of folder items comes from their `effect.json`.
#[derive(Debug, Clone, PartialEq)]
pub enum ItemDecl {
    /// `effect`, `filter` or `transition`: a folder holding an `effect.json`.
    Folder { kind: String, path: String },
    /// An inline `lut`.
    Lut {
        id: String,
        label: String,
        path: String,
        layout: Option<String>,
        version: u32,
    },
    /// A kind this build does not load from format-2 packs. The item is listed as skipped.
    Skipped {
        kind: String,
        id: Option<String>,
        reason: String,
    },
    /// An entry that is missing required fields. The item is listed as an error.
    Invalid {
        kind: String,
        id: Option<String>,
        message: String,
    },
}

#[derive(Deserialize)]
struct RawPack {
    id: String,
    name: String,
    #[serde(default)]
    release: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    authors: Vec<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    requires: Option<serde_json::Value>,
    #[serde(default)]
    items: Vec<serde_json::Value>,
}

/// Parses and validates the text of a format-2 `pack.json`. Errors refuse the whole pack.
pub fn parse(text: &str) -> Result<PackDecl, String> {
    let raw: RawPack = serde_json::from_str(text).map_err(|e| format!("pack.json: {e}"))?;
    check_pack_id(&raw.id)?;
    if raw.name.trim().is_empty() {
        return Err("pack.json: \"name\" must not be empty".into());
    }
    let release = raw.release.ok_or_else(|| {
        "pack.json: \"release\" is required (a semver string such as \"1.0.0\")".to_string()
    })?;
    if !is_semver(&release) {
        return Err(format!(
            "pack.json: release '{release}' is not a semver string such as \"1.0.0\""
        ));
    }
    let license = raw
        .license
        .filter(|l| !l.trim().is_empty())
        .ok_or_else(|| {
            "pack.json: \"license\" is required in pack format 2 (an SPDX id, or \"user-supplied\")"
                .to_string()
        })?;
    let requires_renderly = match raw.requires.as_ref().map(|r| r.get("renderly")) {
        None | Some(None) => None,
        Some(Some(v)) => {
            let req = v
                .as_str()
                .ok_or_else(|| "pack.json: requires.renderly must be a string".to_string())?;
            // Validate the form now; whether it is met is decided at load.
            requirement_met(req, env!("CARGO_PKG_VERSION"))
                .map_err(|e| format!("pack.json: {e}"))?;
            Some(req.to_string())
        }
    };
    Ok(PackDecl {
        id: raw.id,
        name: raw.name,
        release,
        license,
        authors: raw.authors,
        description: raw.description,
        tags: raw.tags,
        requires_renderly,
        items: raw.items.iter().map(parse_item).collect(),
    })
}

fn check_pack_id(id: &str) -> Result<(), String> {
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    };
    let valid = match id.split_once('.') {
        Some((publisher, name)) => segment_ok(publisher) && segment_ok(name) && !name.contains('.'),
        None => false,
    };
    if valid {
        Ok(())
    } else {
        Err(format!(
            "pack.json: id '{id}' must be <publisher>.<name>, each part [a-z0-9-]"
        ))
    }
}

/// `x.y.z` with an optional `-prerelease` or `+build` suffix.
fn is_semver(s: &str) -> bool {
    let core = s.split(['-', '+']).next().unwrap_or("");
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Parses `x`, `x.y` or `x.y.z` (a `-`/`+` suffix is ignored) into a comparable triple.
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let core = s.trim().split(['-', '+']).next()?;
    let mut nums = [0u64; 3];
    let parts: Vec<&str> = core.split('.').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for (slot, part) in nums.iter_mut().zip(parts) {
        *slot = part.parse().ok()?;
    }
    Some((nums[0], nums[1], nums[2]))
}

/// Whether `version` satisfies `requirement`, which must be `>=<version>`.
pub fn requirement_met(requirement: &str, version: &str) -> Result<bool, String> {
    let min = requirement
        .trim()
        .strip_prefix(">=")
        .ok_or_else(|| format!("requires.renderly '{requirement}' must look like \">=0.9\""))?;
    let min = parse_version(min)
        .ok_or_else(|| format!("requires.renderly '{requirement}' is not a version"))?;
    let have =
        parse_version(version).ok_or_else(|| format!("version '{version}' is not a version"))?;
    Ok(have >= min)
}

fn parse_item(v: &serde_json::Value) -> ItemDecl {
    let kind = v
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    let id = v
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let str_field = |name: &str| v.get(name).and_then(serde_json::Value::as_str);
    match kind.as_str() {
        "" => ItemDecl::Invalid {
            kind,
            id,
            message: "item has no \"kind\"".into(),
        },
        "effect" | "filter" | "transition" => match str_field("path") {
            Some(path) => ItemDecl::Folder {
                kind: kind.clone(),
                path: path.to_string(),
            },
            None => ItemDecl::Invalid {
                message: format!("{kind} item needs \"path\" (a folder holding effect.json)"),
                kind,
                id,
            },
        },
        "lut" => {
            let Some(item_id) = id.as_deref().filter(|i| is_item_id(i)) else {
                return ItemDecl::Invalid {
                    kind,
                    id,
                    message: "lut item needs an \"id\" of [a-z0-9_-]".into(),
                };
            };
            let Some(path) = str_field("path") else {
                return ItemDecl::Invalid {
                    kind,
                    id,
                    message: "lut item needs \"path\"".into(),
                };
            };
            let version = match v.get("version") {
                None => 1,
                Some(n) => match n.as_u64().and_then(|n| u32::try_from(n).ok()) {
                    Some(n) if n >= 1 => n,
                    _ => {
                        return ItemDecl::Invalid {
                            kind,
                            id,
                            message: "lut \"version\" must be an integer from 1".into(),
                        }
                    }
                },
            };
            ItemDecl::Lut {
                id: item_id.to_string(),
                label: str_field("label").unwrap_or(item_id).to_string(),
                path: path.to_string(),
                layout: str_field("layout").map(str::to_string),
                version,
            }
        }
        "sticker" | "sfx" | "caption_style" | "template" => ItemDecl::Skipped {
            reason: format!("kind `{kind}` is not loaded from format 2 packs yet"),
            kind,
            id,
        },
        other => ItemDecl::Skipped {
            reason: format!("needs a newer Renderly (item kind `{other}`)"),
            kind: other.to_string(),
            id,
        },
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::load;

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use super::{parse, requirement_met, ItemDecl, FORMAT};
    use crate::packs::item::{load_folder, load_lut, ItemState, LoadedItem};
    use crate::packs::{CubeLut, LoadedPack, PackLut, PackManifest, PackMeta};

    /// Loads a format-2 pack from its folder. `text` is its `pack.json`.
    pub fn load(root: &Path, text: &str) -> Result<LoadedPack, String> {
        let decl = parse(text)?;
        let compat = match &decl.requires_renderly {
            Some(req) => {
                if requirement_met(req, env!("CARGO_PKG_VERSION"))? {
                    None
                } else {
                    Some(format!("needs a newer Renderly ({req})"))
                }
            }
            None => None,
        };

        let mut items: Vec<LoadedItem> = Vec::with_capacity(decl.items.len());
        let mut luts: BTreeMap<String, CubeLut> = BTreeMap::new();
        let mut seen: BTreeSet<(String, u32)> = BTreeSet::new();
        for entry in &decl.items {
            let (mut item, cube) = match entry {
                ItemDecl::Folder { kind, path } => (load_folder(root, kind, path), None),
                ItemDecl::Lut {
                    id,
                    label,
                    path,
                    layout,
                    version,
                } => load_lut(root, id, *version, label, path, layout.as_deref()),
                ItemDecl::Skipped { kind, id, reason } => (
                    LoadedItem::with_state(
                        kind,
                        id.as_deref().unwrap_or(""),
                        1,
                        ItemState::Skipped {
                            reason: reason.clone(),
                        },
                    ),
                    None,
                ),
                ItemDecl::Invalid { kind, id, message } => (
                    LoadedItem::with_state(
                        kind,
                        id.as_deref().unwrap_or(""),
                        1,
                        ItemState::Error {
                            message: message.clone(),
                        },
                    ),
                    None,
                ),
            };
            // One (id, version) per pack: a later copy is an error, the first one stands.
            let counts = !matches!(item.state, ItemState::Skipped { .. }) && !item.id.is_empty();
            if counts && !seen.insert((item.id.clone(), item.version)) {
                item = LoadedItem {
                    state: ItemState::Error {
                        message: format!(
                            "duplicate item {}@{}: an earlier item in this pack has the same id and version",
                            item.id, item.version
                        ),
                    },
                    hash: None,
                    compiled: None,
                    ..item
                };
            }
            if let (ItemState::Ok, Some(cube)) = (&item.state, cube) {
                luts.insert(item.id.clone(), cube);
            }
            items.push(item);
        }

        if let Some(reason) = compat {
            for item in &mut items {
                if item.state == ItemState::Ok {
                    item.state = ItemState::Disabled {
                        reason: reason.clone(),
                    };
                }
            }
        }

        let manifest = PackManifest {
            id: decl.id.clone(),
            name: decl.name.clone(),
            version: decl.release.clone(),
            caption_styles: Vec::new(),
            luts: items
                .iter()
                .filter(|i| i.kind == "lut" && i.state == ItemState::Ok)
                .map(|i| PackLut {
                    id: i.id.clone(),
                    label: i.label.clone(),
                    path: i.path.clone().unwrap_or_default(),
                })
                .collect(),
            transitions: Vec::new(),
            stickers: Vec::new(),
            sfx: Vec::new(),
            templates: Vec::new(),
        };
        Ok(LoadedPack {
            root: root.to_path_buf(),
            manifest,
            luts,
            format: FORMAT,
            canonical_id: decl.id.clone(),
            meta: PackMeta {
                release: Some(decl.release),
                license: Some(decl.license),
                authors: decl.authors,
                description: decl.description,
                tags: decl.tags,
                requires_renderly: decl.requires_renderly,
            },
            items,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
        "format": 2, "id": "mira.film-looks", "name": "Film Looks",
        "release": "1.3.0", "license": "CC-BY-4.0"
    }"#;

    fn with(extra: &str) -> String {
        // Splice extra top-level fields in front of the closing brace.
        let trimmed = BASE.trim_end().trim_end_matches('}');
        format!("{trimmed}, {extra} }}")
    }

    #[test]
    fn parses_a_minimal_format_2_pack() {
        let decl = parse(BASE).unwrap();
        assert_eq!(decl.id, "mira.film-looks");
        assert_eq!(decl.release, "1.3.0");
        assert_eq!(decl.license, "CC-BY-4.0");
        assert!(decl.items.is_empty());
    }

    #[test]
    fn pack_level_errors_refuse_the_pack() {
        for bad in [
            BASE.replace("mira.film-looks", "film-looks"),
            BASE.replace("mira.film-looks", "Mira.film"),
            BASE.replace("mira.film-looks", "a.b.c"),
            BASE.replace("\"1.3.0\"", "\"1.3\""),
            BASE.replace(", \"license\": \"CC-BY-4.0\"", ""),
            BASE.replace("\"license\": \"CC-BY-4.0\"", "\"license\": \" \""),
        ] {
            assert!(parse(&bad).is_err(), "should refuse: {bad}");
        }
    }

    #[test]
    fn requires_renderly_is_checked_by_form() {
        assert!(parse(&with(r#""requires": {"renderly": ">=0.1"}"#)).is_ok());
        assert!(parse(&with(r#""requires": {"renderly": "0.1"}"#)).is_err());
        assert!(parse(&with(r#""requires": {"renderly": ">=x"}"#)).is_err());
        // Unknown requirement keys are ignored.
        assert!(parse(&with(r#""requires": {"other": ">=9"}"#)).is_ok());
    }

    #[test]
    fn version_requirements_compare_numerically() {
        assert_eq!(requirement_met(">=0.9", "0.10.0"), Ok(true));
        assert_eq!(requirement_met(">=0.10", "0.9.5"), Ok(false));
        assert_eq!(requirement_met(">=1", "1.0.0"), Ok(true));
    }

    #[test]
    fn item_declarations_are_typed_by_kind() {
        let text = with(
            r#""items": [
                {"kind": "effect", "path": "effects/a/1"},
                {"kind": "transition", "path": "transitions/b/1"},
                {"kind": "lut", "id": "teal", "label": "Teal", "path": "luts/teal.cube"},
                {"kind": "lut", "id": "print", "path": "luts/print.png", "layout": "grid", "version": 2},
                {"kind": "sparkle", "id": "x"},
                {"kind": "effect"},
                {"kind": "lut", "id": "Bad Id", "path": "x.cube"}
            ]"#,
        );
        let decl = parse(&text).unwrap();
        assert!(
            matches!(&decl.items[0], ItemDecl::Folder { kind, path } if kind == "effect" && path == "effects/a/1")
        );
        assert!(
            matches!(&decl.items[2], ItemDecl::Lut { id, label, version: 1, .. } if id == "teal" && label == "Teal")
        );
        assert!(
            matches!(&decl.items[3], ItemDecl::Lut { version: 2, layout: Some(l), .. } if l == "grid")
        );
        assert!(
            matches!(&decl.items[4], ItemDecl::Skipped { reason, .. } if reason == "needs a newer Renderly (item kind `sparkle`)")
        );
        assert!(matches!(&decl.items[5], ItemDecl::Invalid { .. }));
        assert!(matches!(&decl.items[6], ItemDecl::Invalid { .. }));
    }

    #[test]
    fn semver_form_is_strict() {
        assert!(is_semver("1.0.0"));
        assert!(is_semver("1.0.0-rc.1"));
        assert!(!is_semver("1.0"));
        assert!(!is_semver("v1.0.0"));
        assert!(!is_semver("1..0"));
    }
}
