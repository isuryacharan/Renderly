//! Item refs (`pack:<pack>/<item>@<version>`, `builtin:<id>@<version>`) and their resolution
//! against loaded packs. Pure: no filesystem, so it builds for wasm32 too.
//!
//! Projects will store these refs with schema v8 (a later step); this module is the resolver
//! they will use. See docs/effect-pack-format.md, "Effect references and versioning".

use std::fmt;

use super::{builtin_items, ItemState, LoadedItem, LoadedPack};

/// Item ids: `[a-z0-9_-]+` (pack ids are `[a-z0-9-]` segments, see [`super::format2`]).
pub fn is_item_id(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Pack ids in refs: `[a-z0-9-]` segments joined by dots (`mira.film-looks`, or the bare
/// format-1 id `starter`).
fn is_pack_ref_id(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && !s.ends_with('.')
        && !s.contains("..")
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// A parsed item ref. Versions are integers from 1.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemRef {
    /// `pack:<pack>/<item>@<version>`. `pack` is a pack id, or a format-1 id such as `starter`
    /// (which also answers to `local.starter`).
    Pack {
        pack: String,
        item: String,
        version: u32,
    },
    /// `builtin:<id>@<version>`, compiled into the binary.
    Builtin { id: String, version: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    #[error(
        "'{0}' is not an item ref: use pack:<pack>/<item>@<version> or builtin:<id>@<version>"
    )]
    Malformed(String),
}

impl ItemRef {
    /// Parses a ref. Whitespace and upper case are rejected, not normalised.
    pub fn parse(text: &str) -> Result<ItemRef, RefError> {
        let bad = || RefError::Malformed(text.to_string());
        if let Some(rest) = text.strip_prefix("builtin:") {
            let (id, version) = split_version(rest).ok_or_else(bad)?;
            if !is_item_id(id) {
                return Err(bad());
            }
            return Ok(ItemRef::Builtin {
                id: id.to_string(),
                version,
            });
        }
        let rest = text.strip_prefix("pack:").ok_or_else(bad)?;
        let (pack, item_and_version) = rest.split_once('/').ok_or_else(bad)?;
        let (item, version) = split_version(item_and_version).ok_or_else(bad)?;
        if !is_pack_ref_id(pack) || !is_item_id(item) {
            return Err(bad());
        }
        Ok(ItemRef::Pack {
            pack: pack.to_string(),
            item: item.to_string(),
            version,
        })
    }

    pub fn version(&self) -> u32 {
        match self {
            ItemRef::Pack { version, .. } | ItemRef::Builtin { version, .. } => *version,
        }
    }
}

impl fmt::Display for ItemRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemRef::Pack {
                pack,
                item,
                version,
            } => write!(f, "pack:{pack}/{item}@{version}"),
            ItemRef::Builtin { id, version } => write!(f, "builtin:{id}@{version}"),
        }
    }
}

/// `name@version` with a plain decimal version >= 1.
fn split_version(s: &str) -> Option<(&str, u32)> {
    let (name, version) = s.rsplit_once('@')?;
    if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let version: u32 = version.parse().ok()?;
    (version >= 1).then_some((name, version))
}

/// The outcome of resolving a ref.
///
/// `Missing` means the item is not installed: a caller renders the clip **pass-through**
/// (keeps the instance's params and shows it as missing). Nothing is dropped.
/// `Disabled` means it is installed but this build cannot run it (a newer feature, or a
/// failed load); the reason is for display, and the caller also passes through.
#[derive(Debug)]
pub enum Resolution<'a> {
    /// Installed and usable. With an expected hash, the installed hash equals it.
    Ok(&'a LoadedItem),
    /// Installed and usable, but its content hash differs from the expected one. Render the
    /// installed files and mark the instance as changed. `installed_hash` is the hash of the
    /// copy that was resolved.
    Changed {
        item: &'a LoadedItem,
        installed_hash: String,
    },
    /// No installed pack or builtin has this ref.
    Missing,
    /// Installed but not runnable here.
    Disabled { reason: String },
}

/// Whether `name` (from a ref) names this pack: its declared id or its canonical id.
pub fn pack_matches(pack: &LoadedPack, name: &str) -> bool {
    pack.manifest.id == name || pack.canonical_id == name
}

/// Resolves `r` against the builtins and `packs`.
///
/// When several installed copies match (two releases of one pack, say), a copy whose hash
/// equals `expected_hash` wins; otherwise the first usable copy is used.
pub fn resolve<'a>(
    packs: &'a [LoadedPack],
    r: &ItemRef,
    expected_hash: Option<&str>,
) -> Resolution<'a> {
    let candidates: Vec<&'a LoadedItem> = match r {
        ItemRef::Builtin { id, version } => builtin_items()
            .iter()
            .filter(|i| i.id == *id && i.version == *version)
            .collect(),
        ItemRef::Pack {
            pack,
            item,
            version,
        } => packs
            .iter()
            .filter(|p| pack_matches(p, pack))
            .flat_map(|p| p.items.iter())
            .filter(|i| i.id == *item && i.version == *version)
            .collect(),
    };
    if candidates.is_empty() {
        return Resolution::Missing;
    }
    let usable: Vec<&'a LoadedItem> = candidates
        .iter()
        .copied()
        .filter(|i| matches!(i.state, ItemState::Ok))
        .collect();
    if let Some(expected) = expected_hash {
        if let Some(exact) = usable
            .iter()
            .copied()
            .find(|i| i.hash.as_deref() == Some(expected))
        {
            return Resolution::Ok(exact);
        }
    }
    if let Some(first) = usable.first().copied() {
        return match expected_hash {
            None => Resolution::Ok(first),
            Some(_) => Resolution::Changed {
                installed_hash: first.hash.clone().unwrap_or_default(),
                item: first,
            },
        };
    }
    let reason = match candidates.first().map(|i| &i.state) {
        Some(ItemState::Disabled { reason }) | Some(ItemState::Skipped { reason }) => {
            reason.clone()
        }
        Some(ItemState::Error { message }) => format!("failed to load: {message}"),
        _ => "not usable".to_string(),
    };
    Resolution::Disabled { reason }
}

/// The builtin item for a `builtin:` ref, if the ref names one.
pub fn builtin_item(r: &ItemRef) -> Option<&'static LoadedItem> {
    match r {
        ItemRef::Builtin { id, version } => builtin_items()
            .iter()
            .find(|i| i.id == *id && i.version == *version),
        ItemRef::Pack { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_formats_both_ref_kinds() {
        for text in [
            "pack:mira.film-looks/film-grain@2",
            "pack:starter/punch@1",
            "builtin:blur@1",
            "builtin:lut_filter@1",
        ] {
            let r = ItemRef::parse(text).unwrap();
            assert_eq!(r.to_string(), text);
        }
        assert_eq!(
            ItemRef::parse("pack:mira.film-looks/film-grain@2").unwrap(),
            ItemRef::Pack {
                pack: "mira.film-looks".into(),
                item: "film-grain".into(),
                version: 2,
            }
        );
    }

    #[test]
    fn rejects_malformed_refs() {
        for text in [
            "blur@1",
            "builtin:blur",
            "builtin:blur@0",
            "builtin:Blur@1",
            "builtin:blur@+1",
            "builtin:blur@x",
            "pack:mira.film-looks@1",
            "pack:mira..x/film@1",
            "pack:Mira.x/film@1",
            "pack:mira.x/film grain@1",
            "pack:/film@1",
        ] {
            assert!(ItemRef::parse(text).is_err(), "{text} should be rejected");
        }
    }

    #[test]
    fn builtin_refs_resolve_to_the_compiled_items() {
        let r = ItemRef::parse("builtin:blur@1").unwrap();
        let packs: [LoadedPack; 0] = [];
        match resolve(&packs, &r, None) {
            Resolution::Ok(item) => assert_eq!(item.id, "blur"),
            other => panic!("expected Ok, got {other:?}"),
        }
        let missing = ItemRef::parse("builtin:blur@2").unwrap();
        assert!(matches!(
            resolve(&packs, &missing, None),
            Resolution::Missing
        ));
    }

    #[test]
    fn builtin_hash_mismatch_is_changed_and_reports_the_installed_hash() {
        let r = ItemRef::parse("builtin:blur@1").unwrap();
        let packs: [LoadedPack; 0] = [];
        let installed = builtin_item(&r).unwrap().hash.clone();
        match resolve(&packs, &r, Some("0000")) {
            Resolution::Changed { installed_hash, .. } => {
                assert_eq!(Some(installed_hash), installed);
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }
}
