//! Builtin items written in pack format 2 (`effect.json` + WGSL), embedded with `include_str!`.
//!
//! These are the format-2 ports that the design's implementation step 1 uses as its proof:
//! `blur` (multi-pass, `builtin:blur@1`) and `crossfade` (transition). They run through
//! [`super::runtime`]. The hard-coded builtin dispatch in `compose::effects` and
//! `compose::transition` still renders the live pipeline; switching it over is a separate step.
//! Tests check that these match the hard-coded output bit for bit.

use std::collections::BTreeMap;

use super::runtime::{CompiledEffect, EffectError};

const BLUR_JSON: &str = include_str!("blur/effect.json");
const BLUR_H: &str = include_str!("blur/blur_h.wgsl");
const BLUR_V: &str = include_str!("blur/blur_v.wgsl");
const CROSSFADE_JSON: &str = include_str!("crossfade/effect.json");
const CROSSFADE: &str = include_str!("crossfade/crossfade.wgsl");

/// The builtin items that are in format 2, by their `builtin:<id>@1` name.
pub const BUILTIN_FORMAT2_IDS: &[&str] = &["blur", "crossfade"];

/// Manifest and shader sources of a builtin, ready for [`CompiledEffect::load`].
pub fn builtin_sources(id: &str) -> Option<(&'static str, BTreeMap<String, String>)> {
    let (json, shaders): (&'static str, &[(&str, &str)]) = match id {
        "blur" => (
            BLUR_JSON,
            &[("blur_h.wgsl", BLUR_H), ("blur_v.wgsl", BLUR_V)],
        ),
        "crossfade" => (CROSSFADE_JSON, &[("crossfade.wgsl", CROSSFADE)]),
        _ => return None,
    };
    let shaders = shaders
        .iter()
        .map(|(name, src)| ((*name).to_string(), (*src).to_string()))
        .collect();
    Some((json, shaders))
}

/// Compiles a builtin format-2 item (for example `"blur"`).
pub fn load_builtin(id: &str) -> Result<CompiledEffect, EffectError> {
    let (json, shaders) = builtin_sources(id).ok_or_else(|| EffectError::MissingFile(id.into()))?;
    CompiledEffect::load(json, &shaders, &BTreeMap::new())
}
