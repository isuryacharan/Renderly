//! Builtin items written in pack format 2 (`effect.json` + WGSL), embedded with `include_str!`.
//!
//! This table is the single source of truth for the builtin effects, filters and transitions:
//! the live compositor resolves every builtin through it (see `compose::effects` and
//! `compose::transition`), and the public id lists and defaults are checked against it.
//!
//! Item names are the bare ids (`blur`, `wipe_left`, ...); the project refers to them as
//! `builtin:<name>@1` (effects, schema v7 ids `builtin:<name>`) and transitions by their
//! `TransitionKind` name. `lut_filter` is what pack `.cube` LUTs resolve to.

use std::collections::BTreeMap;

use super::runtime::{CompiledEffect, EffectError, EffectManifest};

/// One embedded builtin item: its manifest and the WGSL files it names.
pub struct BuiltinSource {
    pub name: &'static str,
    pub json: &'static str,
    pub shaders: &'static [(&'static str, &'static str)],
}

macro_rules! item {
    ($name:literal, [$($shader:literal),* $(,)?]) => {
        BuiltinSource {
            name: $name,
            json: include_str!(concat!($name, "/effect.json")),
            shaders: &[$(($shader, include_str!(concat!($name, "/", $shader)))),*],
        }
    };
}

/// Every builtin item, in pack format 2.
pub const BUILTINS: &[BuiltinSource] = &[
    item!("blur", ["blur_h.wgsl", "blur_v.wgsl"]),
    item!("color_adjust", ["color_adjust.wgsl"]),
    item!("lut_contrast", ["lut_contrast.wgsl"]),
    item!("lut_warm", ["lut_warm.wgsl"]),
    item!("glitch", ["glitch.wgsl"]),
    item!("chroma_key", ["chroma_key.wgsl"]),
    item!("lut_filter", ["lut_filter.wgsl"]),
    item!("crossfade", ["crossfade.wgsl"]),
    item!("fade_black", ["fade_black.wgsl"]),
    item!("wipe_left", ["wipe_left.wgsl"]),
    item!("wipe_right", ["wipe_right.wgsl"]),
    item!("wipe_up", ["wipe_up.wgsl"]),
    item!("wipe_down", ["wipe_down.wgsl"]),
    item!("slide_left", ["slide_left.wgsl"]),
    item!("slide_right", ["slide_right.wgsl"]),
    item!("iris", ["iris.wgsl"]),
    item!("blur_dissolve", ["blur_dissolve.wgsl"]),
];

/// The embedded source of builtin `name`, if there is one.
pub fn builtin(name: &str) -> Option<&'static BuiltinSource> {
    BUILTINS.iter().find(|b| b.name == name)
}

/// Parsed manifest of builtin `name`.
pub fn manifest(name: &str) -> Option<EffectManifest> {
    EffectManifest::parse(builtin(name)?.json).ok()
}

/// Manifest and shader sources of a builtin, ready for [`CompiledEffect::load`].
pub fn builtin_sources(name: &str) -> Option<(&'static str, BTreeMap<String, String>)> {
    let b = builtin(name)?;
    let shaders = b
        .shaders
        .iter()
        .map(|(file, src)| ((*file).to_string(), (*src).to_string()))
        .collect();
    Some((b.json, shaders))
}

/// Names of the builtin transition items (`crossfade`, `wipe_left`, ...), in table order.
pub fn transition_ids() -> Vec<&'static str> {
    BUILTINS
        .iter()
        .filter(|b| {
            manifest(b.name)
                .is_some_and(|m| m.kind == super::runtime::manifest::ItemKind::Transition)
        })
        .map(|b| b.name)
        .collect()
}

/// Compiles builtin `name` (validates the manifest and every pass with naga).
pub fn load_builtin(name: &str) -> Result<CompiledEffect, EffectError> {
    let (json, shaders) =
        builtin_sources(name).ok_or_else(|| EffectError::MissingFile(name.into()))?;
    CompiledEffect::load(json, &shaders, &BTreeMap::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_loads_under_its_own_name() {
        for b in BUILTINS {
            let compiled = load_builtin(b.name).unwrap_or_else(|e| panic!("{}: {e}", b.name));
            assert_eq!(compiled.manifest().id, b.name);
        }
    }
}
