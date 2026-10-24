//! Perception for effects and packs: the catalogue that `describe_commands` and
//! `describe_effects` print, and the per-item report behind `renderly pack validate`.
//!
//! Pure (no filesystem), so it builds for wasm32. Output is kept compact: agents read it with
//! token budgets.

use std::fmt::Write as _;

use serde::Serialize;
use serde_json::{json, Value};

use super::item::builtin_items;
use super::{LoadedItem, LoadedPack};
use crate::compose::runtime::manifest::{ParamKind, ParamSpec};
use crate::compose::runtime::ParamValue;

/// One effect, filter, transition or LUT the engine can apply, builtin or from a pack.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CatalogueEntry {
    #[serde(rename = "ref")]
    pub reference: String,
    pub kind: String,
    pub label: String,
    /// `builtin`, or the pack's canonical id.
    pub source: String,
    /// `ok`, `disabled`, `error` or `skipped`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Where `SetClipEffects` puts it by default: `filter`, `key`, `adjust` or `effects`
    /// (`transition` for `SetClipTransition`). Filters and LUTs may also go in `effects`.
    pub slot: &'static str,
    /// Empty for LUTs and for items that did not compile.
    pub params: Vec<ParamEntry>,
}

/// One control of an item.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ParamEntry {
    pub key: String,
    #[serde(rename = "type")]
    pub ty: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// The value when the param is absent from a stored instance.
    pub default: Value,
    /// The value written when the effect is added.
    pub initial: Value,
    /// Choice ids, in index order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// True when the control can be keyframed (`effect:<id>/<key>`): numeric, colour and vec2
    /// params not marked `animatable: false`.
    pub animatable: bool,
}

/// Every builtin item, then every effect, filter, transition and LUT of the loaded packs, in
/// pack order. Format-1 stickers, sfx, caption styles and templates are not effects and are
/// left out.
pub fn catalogue(packs: &[LoadedPack]) -> Vec<CatalogueEntry> {
    let mut out: Vec<CatalogueEntry> = builtin_items()
        .iter()
        .map(|item| {
            entry(
                item,
                format!("builtin:{}@{}", item.id, item.version),
                "builtin".to_string(),
            )
        })
        .collect();
    for pack in packs {
        for item in &pack.items {
            if !is_catalogued_kind(&item.kind) {
                continue;
            }
            out.push(entry(
                item,
                format!("pack:{}/{}@{}", pack.canonical_id, item.id, item.version),
                pack.canonical_id.clone(),
            ));
        }
    }
    out
}

fn is_catalogued_kind(kind: &str) -> bool {
    matches!(kind, "effect" | "filter" | "transition" | "lut")
}

fn entry(item: &LoadedItem, reference: String, source: String) -> CatalogueEntry {
    let params = item
        .compiled
        .as_ref()
        .map(|c| c.manifest().params.iter().map(param_entry).collect())
        .unwrap_or_default();
    CatalogueEntry {
        reference,
        kind: item.kind.clone(),
        label: item.label.clone(),
        source,
        state: item.state.name(),
        reason: item.state.reason().map(str::to_string),
        slot: default_slot(item),
        params,
    }
}

/// The slot an item belongs in unless the caller says otherwise (docs/effect-pack-format.md
/// "Stack slots"): filters and LUTs in `filter`, keying effects in `key`, the colour adjustment
/// in `adjust`, transitions on the clip junction, everything else in `effects`.
fn default_slot(item: &LoadedItem) -> &'static str {
    let category = item
        .compiled
        .as_ref()
        .and_then(|c| c.manifest().category.clone());
    match item.kind.as_str() {
        "transition" => "transition",
        "filter" | "lut" => "filter",
        _ if category.as_deref() == Some("key") => "key",
        _ if category.as_deref() == Some("adjust") || item.id == "color_adjust" => "adjust",
        _ => "effects",
    }
}

fn param_entry(p: &ParamSpec) -> ParamEntry {
    let numeric = p.kind != ParamKind::Lut;
    ParamEntry {
        key: p.key.clone(),
        ty: param_type(p.kind),
        min: numeric.then_some(p.min),
        max: numeric.then_some(p.max),
        default: value_json(&p.default),
        initial: value_json(&p.initial),
        options: p.options.iter().map(|o| o.id.clone()).collect(),
        animatable: p.animatable
            && matches!(
                p.kind,
                ParamKind::Float | ParamKind::Int | ParamKind::Color | ParamKind::Vec2
            ),
    }
}

fn param_type(kind: ParamKind) -> &'static str {
    match kind {
        ParamKind::Float => "float",
        ParamKind::Int => "int",
        ParamKind::Bool => "bool",
        ParamKind::Color => "color",
        ParamKind::Vec2 => "vec2",
        ParamKind::Choice => "choice",
        ParamKind::Lut => "lut",
    }
}

fn value_json(v: &ParamValue) -> Value {
    match v {
        ParamValue::Number(n) => json!(n),
        ParamValue::Vector(vs) => json!(vs),
        ParamValue::Lut(s) => json!(s),
    }
}

/// The catalogue as compact text, one line per item:
/// `ref kind "label" state[: reason] | key:type lo..hi =default [init=v], ...`.
pub fn catalogue_text(packs: &[LoadedPack]) -> String {
    let mut out = String::from(
        "Effects, filters, transitions and LUTs. Refs: builtin:<id>@<version>, \
         pack:<pack>/<item>@<version>. Apply with SetClipEffects (into the filter / key / adjust \
         slot or the effects list; slot= is the usual one) or SetClipTransition. Slots run in \
         the order filter, key, adjust, effects.\n",
    );
    for e in catalogue(packs) {
        let _ = write!(
            out,
            "{} {} \"{}\" {}",
            e.reference, e.kind, e.label, e.state
        );
        if e.state == "ok" && e.kind != "transition" {
            let _ = write!(out, " slot={}", e.slot);
        }
        if let Some(reason) = &e.reason {
            let _ = write!(out, ": {reason}");
        }
        if !e.params.is_empty() {
            out.push_str(" |");
            let params: Vec<String> = e.params.iter().map(param_text).collect();
            out.push(' ');
            out.push_str(&params.join(", "));
        }
        out.push('\n');
    }
    out
}

fn param_text(p: &ParamEntry) -> String {
    let mut s = format!("{}:{}", p.key, p.ty);
    if let (Some(min), Some(max)) = (p.min, p.max) {
        if matches!(p.ty, "float" | "int" | "choice") {
            let _ = write!(s, " {min}..{max}");
        }
    }
    if !p.options.is_empty() {
        let _ = write!(s, " [{}]", p.options.join("|"));
    }
    let _ = write!(s, " ={}", p.default);
    if p.initial != p.default {
        let _ = write!(s, " init={}", p.initial);
    }
    s
}

/// One pack's items with their states, for `renderly pack validate` and JSON output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PackReport {
    pub id: String,
    pub name: String,
    pub format: u32,
    pub release: Option<String>,
    pub license: Option<String>,
    pub items: Vec<ItemReport>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ItemReport {
    pub kind: String,
    pub id: String,
    pub version: u32,
    pub label: String,
    pub path: Option<String>,
    /// `ok`, `disabled`, `error` or `skipped`.
    pub state: &'static str,
    pub reason: Option<String>,
    /// SHA-256 hex, for items that loaded.
    pub hash: Option<String>,
}

pub fn pack_report(pack: &LoadedPack) -> PackReport {
    PackReport {
        id: pack.canonical_id.clone(),
        name: pack.manifest.name.clone(),
        format: pack.format,
        release: pack.meta.release.clone(),
        license: pack.meta.license.clone(),
        items: pack
            .items
            .iter()
            .map(|i| ItemReport {
                kind: i.kind.clone(),
                id: i.id.clone(),
                version: i.version,
                label: i.label.clone(),
                path: i.path.clone(),
                state: i.state.name(),
                reason: i.state.reason().map(str::to_string),
                hash: i.hash.clone(),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_catalogued_with_their_params() {
        let entries = catalogue(&[]);
        let blur = entries
            .iter()
            .find(|e| e.reference == "builtin:blur@1")
            .expect("blur is catalogued");
        assert_eq!(blur.state, "ok");
        assert_eq!(blur.kind, "effect");
        let radius = blur.params.iter().find(|p| p.key == "radius").unwrap();
        assert_eq!(radius.ty, "float");
        assert_eq!(radius.max, Some(64.0));
        assert_eq!(radius.default, json!(0.0));
        // A freshly added blur is visible: `initial` differs from the identity `default`.
        assert_eq!(radius.initial, json!(8.0));
        assert!(radius.animatable, "numeric params can be keyframed");
        let text = catalogue_text(&[]);
        assert!(text.contains(
            "builtin:blur@1 effect \"Blur\" ok slot=effects | radius:float 0..64 =0.0 init=8.0"
        ));
        assert!(
            text.contains("builtin:chroma_key@1 effect \"Chroma key\" ok slot=key"),
            "{text}"
        );
        assert!(text.contains("builtin:color_adjust@1 effect \"Color adjust\" ok slot=adjust"));
        assert!(text.contains("builtin:lut_filter@1 filter"));
    }
}
