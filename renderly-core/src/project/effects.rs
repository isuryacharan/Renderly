//! Effect instances, slots, blend modes and transition references (schema v8,
//! docs/project-schema.md "Effects"). Pure data: resolving a ref against packs and computing
//! its status lives in [`crate::packs::status`].

use super::Id;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The control value of one effect parameter, typed as the effect declares it: a number (also
/// used for `int` and `choice` option indexes), a bool, a vector (`[x, y]` or `[r, g, b, a]`),
/// or a LUT ref string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum EffectParam {
    Bool(bool),
    Number(f64),
    Vector(Vec<f64>),
    Text(String),
}

impl EffectParam {
    /// The scalar value: numbers as they are, bools as 0/1. `None` for vectors and text.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            EffectParam::Number(n) => Some(*n),
            EffectParam::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// True when every number in the value is finite.
    pub fn is_finite(&self) -> bool {
        match self {
            EffectParam::Number(n) => n.is_finite(),
            EffectParam::Vector(v) => v.iter().all(|c| c.is_finite()),
            _ => true,
        }
    }
}

impl From<f64> for EffectParam {
    fn from(n: f64) -> Self {
        EffectParam::Number(n)
    }
}

impl From<bool> for EffectParam {
    fn from(b: bool) -> Self {
        EffectParam::Bool(b)
    }
}

impl From<Vec<f64>> for EffectParam {
    fn from(v: Vec<f64>) -> Self {
        EffectParam::Vector(v)
    }
}

/// Control values by param key.
pub type EffectParams = BTreeMap<String, EffectParam>;

/// One effect applied to a clip, in a slot (`filter`, `key`, `adjust`) or in `effects[]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EffectInstance {
    /// Stable id; keyframe tracks (`effect:<id>/<param>`) and hover preview address it.
    pub id: Id,
    /// `builtin:<id>@<v>`, `pack:<pack-id>/<item>@<v>`, or `wasm:<plugin>`.
    #[serde(rename = "ref")]
    pub item_ref: String,
    /// Content hash of the pack item when it was applied (pack effects only). A different
    /// installed hash marks the instance `changed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub params: EffectParams,
}

fn default_true() -> bool {
    true
}

impl EffectInstance {
    /// A new enabled instance of `item_ref` with fresh id and no params.
    pub fn new(item_ref: impl Into<String>) -> Self {
        Self {
            id: Id::new_v4(),
            item_ref: item_ref.into(),
            hash: None,
            enabled: true,
            params: EffectParams::new(),
        }
    }

    /// Builder: one numeric param.
    pub fn with_param(mut self, key: &str, value: f64) -> Self {
        self.params
            .insert(key.to_string(), EffectParam::Number(value));
        self
    }

    /// Builder: several numeric params.
    pub fn with_params(mut self, params: &[(&str, f64)]) -> Self {
        for (k, v) in params {
            self.params
                .insert((*k).to_string(), EffectParam::Number(*v));
        }
        self
    }

    /// Numeric value of param `key` (bools as 0/1).
    pub fn number(&self, key: &str) -> Option<f64> {
        self.params.get(key).and_then(EffectParam::as_f64)
    }

    /// True for `wasm:<plugin>` instances (CPU frame/audio plugins).
    pub fn is_wasm(&self) -> bool {
        self.item_ref.starts_with("wasm:")
    }
}

/// Clone effects for the right half of a split, minting fresh instance ids. Returns the new
/// instances and the `old id -> new id` pairs, so effect-param keyframe tracks can follow.
pub fn clone_effect(effect: &EffectInstance) -> EffectInstance {
    EffectInstance {
        id: Id::new_v4(),
        ..effect.clone()
    }
}

/// The 13 W3C compositing blend modes. `normal` is the default and is omitted from files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    SoftLight,
    HardLight,
    Difference,
    Exclusion,
    Add,
}

impl BlendMode {
    pub const ALL: [BlendMode; 13] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Add,
    ];

    pub fn is_normal(&self) -> bool {
        *self == BlendMode::Normal
    }

    /// The snake_case name used in project files and commands.
    pub fn as_str(self) -> &'static str {
        match self {
            BlendMode::Normal => "normal",
            BlendMode::Multiply => "multiply",
            BlendMode::Screen => "screen",
            BlendMode::Overlay => "overlay",
            BlendMode::Darken => "darken",
            BlendMode::Lighten => "lighten",
            BlendMode::ColorDodge => "color_dodge",
            BlendMode::ColorBurn => "color_burn",
            BlendMode::SoftLight => "soft_light",
            BlendMode::HardLight => "hard_light",
            BlendMode::Difference => "difference",
            BlendMode::Exclusion => "exclusion",
            BlendMode::Add => "add",
        }
    }

    /// The index the composite shader switches on (`normal` = 0).
    pub fn shader_index(self) -> u32 {
        Self::ALL.iter().position(|m| *m == self).unwrap_or(0) as u32
    }
}

/// Easing of a transition's progress. `None` on the transition uses the item's declared
/// default. Stored as `"linear"` or `{"bezier": [x1, y1, x2, y2]}`; the presets `ease_in`,
/// `ease_out` and `ease_in_out` are accepted when reading and become `bezier` control points
/// (CSS values), so a preset is a starting point a user can then adjust.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ProgressEasing {
    Linear,
    /// CSS-style cubic Bezier `(x1, y1, x2, y2)` over normalised progress.
    Bezier([f64; 4]),
}

impl<'de> Deserialize<'de> for ProgressEasing {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Name(String),
            Bezier { bezier: [f64; 4] },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Name(n) => match n.as_str() {
                "linear" => Ok(ProgressEasing::Linear),
                "ease_in" => Ok(ProgressEasing::EASE_IN),
                "ease_out" => Ok(ProgressEasing::EASE_OUT),
                "ease_in_out" => Ok(ProgressEasing::EASE_IN_OUT),
                other => Err(serde::de::Error::custom(format!(
                    "unknown transition easing '{other}' (linear, ease_in, ease_out, ease_in_out,                      or {{\"bezier\": [x1, y1, x2, y2]}})"
                ))),
            },
            Raw::Bezier { bezier } => Ok(ProgressEasing::Bezier(bezier)),
        }
    }
}

impl ProgressEasing {
    pub const EASE_IN: ProgressEasing = ProgressEasing::Bezier([0.42, 0.0, 1.0, 1.0]);
    pub const EASE_OUT: ProgressEasing = ProgressEasing::Bezier([0.0, 0.0, 0.58, 1.0]);
    pub const EASE_IN_OUT: ProgressEasing = ProgressEasing::Bezier([0.42, 0.0, 0.58, 1.0]);

    /// Eased value of normalised progress `p` (0..1).
    pub fn apply(&self, p: f64) -> f64 {
        let p = p.clamp(0.0, 1.0);
        match self {
            ProgressEasing::Linear => p,
            ProgressEasing::Bezier([x1, y1, x2, y2]) => {
                super::anim::solve_cubic_bezier(p, *x1, *y1, *x2, *y2)
            }
        }
    }
}

/// Outgoing transition at the end of a video clip (into the next clip on the same track).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ClipTransition {
    /// `builtin:<id>@<v>` or `pack:<pack>/<item>@<v>` naming a transition item.
    #[serde(rename = "ref")]
    pub item_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    pub duration_us: i64,
    #[serde(default)]
    pub params: EffectParams,
    /// `None` uses the transition's declared default easing.
    #[serde(default)]
    pub easing: Option<ProgressEasing>,
}

impl ClipTransition {
    pub fn new(item_ref: impl Into<String>, duration_us: i64) -> Self {
        Self {
            item_ref: item_ref.into(),
            hash: None,
            duration_us,
            params: EffectParams::new(),
            easing: None,
        }
    }
}

/// How a stored effect reference resolves against the installed items. Computed on load, never
/// stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectStatus {
    /// Resolved, and the hash (if any) matches.
    Ok,
    /// Resolved, but the installed item's hash differs from the one stored. Renders with the
    /// installed files.
    Changed,
    /// Not installed. Renders pass-through and keeps its params.
    Missing,
    /// Installed but this build cannot run it. Renders pass-through.
    Disabled,
}

impl EffectStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffectStatus::Ok => "ok",
            EffectStatus::Changed => "changed",
            EffectStatus::Missing => "missing",
            EffectStatus::Disabled => "disabled",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn params_keep_their_json_types() {
        let params: EffectParams = serde_json::from_value(json!({
            "radius": 4, "on": true, "tint": [1, 0, 0, 1], "pos": [0.5, 0.5], "lut": "pack:a.b/c@1"
        }))
        .unwrap();
        assert_eq!(params["radius"], EffectParam::Number(4.0));
        assert_eq!(params["on"], EffectParam::Bool(true));
        assert_eq!(
            params["tint"],
            EffectParam::Vector(vec![1.0, 0.0, 0.0, 1.0])
        );
        assert_eq!(params["lut"], EffectParam::Text("pack:a.b/c@1".into()));
        let back = serde_json::to_value(&params).unwrap();
        assert_eq!(back["on"], json!(true));
        assert_eq!(back["lut"], json!("pack:a.b/c@1"));
        assert_eq!(back["tint"], json!([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(params["on"].as_f64(), Some(1.0), "bools read as 0/1");
        assert!(!EffectParam::Vector(vec![f64::NAN]).is_finite());
    }

    #[test]
    fn an_instance_serialises_with_ref_and_omits_an_absent_hash() {
        let e = EffectInstance::new("builtin:blur@1").with_param("radius", 3.0);
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["ref"], "builtin:blur@1");
        assert!(v.get("hash").is_none() && v.get("effect_id").is_none());
        assert_eq!(v["enabled"], true);
        let back: EffectInstance = serde_json::from_value(v).unwrap();
        assert_eq!(back, e);
        // `enabled` and `params` default when a hand-written file leaves them out.
        let sparse: EffectInstance =
            serde_json::from_value(json!({ "id": e.id, "ref": "builtin:blur@1" })).unwrap();
        assert!(sparse.enabled && sparse.params.is_empty());
    }

    #[test]
    fn blend_modes_are_snake_case_and_the_shader_index_is_stable() {
        for (i, mode) in BlendMode::ALL.into_iter().enumerate() {
            let json = serde_json::to_value(mode).unwrap();
            assert_eq!(json, mode.as_str());
            assert_eq!(serde_json::from_value::<BlendMode>(json).unwrap(), mode);
            assert_eq!(mode.shader_index() as usize, i);
        }
        assert_eq!(BlendMode::ALL.len(), 13);
        assert_eq!(BlendMode::default(), BlendMode::Normal);
        assert!(serde_json::from_value::<BlendMode>(json!("burn")).is_err());
    }

    #[test]
    fn transitions_carry_a_ref_and_a_nullable_easing() {
        let t = ClipTransition::new("builtin:crossfade@1", 500_000);
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(
            v,
            json!({ "ref": "builtin:crossfade@1", "duration_us": 500000, "params": {}, "easing": null })
        );
        let t: ClipTransition = serde_json::from_value(json!({
            "ref": "builtin:wipe_left@1", "duration_us": 1, "easing": "ease_out"
        }))
        .unwrap();
        assert_eq!(t.easing, Some(ProgressEasing::EASE_OUT));
        assert_eq!(
            serde_json::to_value(t.easing).unwrap(),
            json!({ "bezier": [0.0, 0.0, 0.58, 1.0] }),
            "presets are stored as Bezier points"
        );
        assert_eq!(
            serde_json::from_value::<ProgressEasing>(json!("linear")).unwrap(),
            ProgressEasing::Linear
        );
        assert!(serde_json::from_value::<ProgressEasing>(json!("wobbly")).is_err());
    }

    #[test]
    fn progress_easing_matches_css_cubic_bezier() {
        assert_eq!(ProgressEasing::Linear.apply(0.3), 0.3);
        // CSS ease-in-out is symmetric and passes through the middle.
        assert!((ProgressEasing::EASE_IN_OUT.apply(0.5) - 0.5).abs() < 1e-6);
        assert!(ProgressEasing::EASE_IN.apply(0.25) < 0.25);
        assert!(ProgressEasing::EASE_OUT.apply(0.25) > 0.25);
        assert_eq!(ProgressEasing::EASE_IN.apply(0.0), 0.0);
        assert!((ProgressEasing::EASE_IN.apply(1.0) - 1.0).abs() < 1e-9);
        assert_eq!(
            ProgressEasing::Linear.apply(7.0),
            1.0,
            "clamped to the window"
        );
    }
}
