//! Keyframe types (schema v8): string property names that can address effect parameters,
//! scalar or array values, and the easing variants (docs/project-schema.md "Keyframes").

use super::{AnimProperty, Id};
use serde::{Deserialize, Serialize};
use std::fmt;

/// How the span *after* a key is interpolated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Easing {
    #[default]
    Linear,
    /// Keep this key's value until the next key.
    Hold,
    /// Cubic Bezier through this key's `handle_right` and the next key's `handle_left`
    /// (x solved for t, like CSS `cubic-bezier`).
    Bezier,
    /// Smooth monotone cubic with tangents derived from the neighbouring keys (no overshoot).
    /// No handles are stored.
    Auto,
    /// Legacy only: the v7 piecewise quadratic, kept so migrated projects render identically.
    EaseInOut,
}

/// A keyframe value: a number, or an array for `color` / `vec2` effect parameters
/// (interpolated per component).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum KeyValue {
    Number(f64),
    Vector(Vec<f64>),
}

impl KeyValue {
    /// The scalar value (first component for vectors).
    pub fn num(&self) -> f64 {
        match self {
            KeyValue::Number(n) => *n,
            KeyValue::Vector(v) => v.first().copied().unwrap_or(0.0),
        }
    }

    pub fn is_finite(&self) -> bool {
        match self {
            KeyValue::Number(n) => n.is_finite(),
            KeyValue::Vector(v) => v.iter().all(|c| c.is_finite()),
        }
    }

    /// Number of components (1 for a scalar).
    pub fn components(&self) -> usize {
        match self {
            KeyValue::Number(_) => 1,
            KeyValue::Vector(v) => v.len(),
        }
    }

    pub fn is_vector(&self) -> bool {
        matches!(self, KeyValue::Vector(_))
    }

    pub fn component(&self, i: usize) -> f64 {
        match self {
            KeyValue::Number(n) => *n,
            KeyValue::Vector(v) => v.get(i).copied().unwrap_or(0.0),
        }
    }
}

impl From<f64> for KeyValue {
    fn from(n: f64) -> Self {
        KeyValue::Number(n)
    }
}

impl From<Vec<f64>> for KeyValue {
    fn from(v: Vec<f64>) -> Self {
        KeyValue::Vector(v)
    }
}

impl PartialEq<f64> for KeyValue {
    fn eq(&self, other: &f64) -> bool {
        matches!(self, KeyValue::Number(n) if n == other)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Keyframe {
    /// Time relative to the clip's timeline start (`start_us`), in µs.
    pub time_us: i64,
    pub value: KeyValue,
    /// Interpolation of the span after this key.
    #[serde(default)]
    pub easing: Easing,
    /// Bezier handle `(time_offset_us, value_offset)` relative to this key. For array-valued
    /// keys the value offset is a fraction of the span's change (1.0 = the full change).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_left: Option<(i64, f64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_right: Option<(i64, f64)>,
}

impl Keyframe {
    /// A linear key.
    pub fn new(time_us: i64, value: impl Into<KeyValue>) -> Self {
        Self {
            time_us,
            value: value.into(),
            easing: Easing::Linear,
            handle_left: None,
            handle_right: None,
        }
    }

    pub fn with_easing(mut self, easing: Easing) -> Self {
        self.easing = easing;
        self
    }

    /// Scalar value of the key.
    pub fn num(&self) -> f64 {
        self.value.num()
    }
}

/// What a keyframe track animates: a transform/audio property, or one parameter of an effect
/// instance on the same clip. Stored as a string: `opacity`, `pos_x`, ...,
/// `effect:<instance-id>/<param>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TrackProperty {
    Anim(AnimProperty),
    Effect { instance: Id, param: String },
}

impl TrackProperty {
    pub fn anim(&self) -> Option<AnimProperty> {
        match self {
            TrackProperty::Anim(a) => Some(*a),
            TrackProperty::Effect { .. } => None,
        }
    }

    /// `(instance id, param key)` for an effect-parameter track.
    pub fn effect(&self) -> Option<(Id, &str)> {
        match self {
            TrackProperty::Effect { instance, param } => Some((*instance, param.as_str())),
            TrackProperty::Anim(_) => None,
        }
    }

    pub fn effect_param(instance: Id, param: &str) -> Self {
        TrackProperty::Effect {
            instance,
            param: param.to_string(),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        if let Some(rest) = text.strip_prefix("effect:") {
            let (id, param) = rest.split_once('/').ok_or_else(|| {
                format!("'{text}' is not a keyframe property: use effect:<instance-id>/<param>")
            })?;
            let instance = Id::parse_str(id)
                .map_err(|_| format!("'{text}': '{id}' is not an effect instance id"))?;
            if param.is_empty() {
                return Err(format!("'{text}': missing parameter name"));
            }
            return Ok(TrackProperty::Effect {
                instance,
                param: param.to_string(),
            });
        }
        AnimProperty::parse(text).map(TrackProperty::Anim).ok_or_else(|| {
            format!(
                "unknown keyframe property '{text}' (expected one of {} or effect:<instance-id>/<param>)",
                AnimProperty::ALL
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })
    }
}

impl fmt::Display for TrackProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrackProperty::Anim(a) => f.write_str(a.as_str()),
            TrackProperty::Effect { instance, param } => write!(f, "effect:{instance}/{param}"),
        }
    }
}

impl From<AnimProperty> for TrackProperty {
    fn from(a: AnimProperty) -> Self {
        TrackProperty::Anim(a)
    }
}

impl PartialEq<AnimProperty> for TrackProperty {
    fn eq(&self, other: &AnimProperty) -> bool {
        matches!(self, TrackProperty::Anim(a) if a == other)
    }
}

impl Serialize for TrackProperty {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for TrackProperty {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        TrackProperty::parse(&text).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for TrackProperty {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TrackProperty".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "pos_x | pos_y | scale_x | scale_y | rotation | opacity | volume | speed | fit_x | fit_y | effect:<instance-id>/<param>"
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct KeyframeTrack {
    pub property: TrackProperty,
    pub keys: Vec<Keyframe>,
}

impl KeyframeTrack {
    pub fn new(property: impl Into<TrackProperty>, keys: Vec<Keyframe>) -> Self {
        Self {
            property: property.into(),
            keys,
        }
    }
}

/// The value offset scale of a span: handles are `value_offset = normalised_y * scale`. For
/// scalar keys that is the span's change; array-valued keys store normalised offsets.
pub(crate) fn span_scale(a: &Keyframe, b: &Keyframe) -> f64 {
    if a.value.is_vector() || b.value.is_vector() {
        1.0
    } else {
        b.num() - a.num()
    }
}

/// Split the Bezier span `a -> b` at time `time_us` (strictly inside it) so that the motion does
/// not change (de Casteljau). Returns the new inner key (value on the curve, `Bezier` easing,
/// both handles set) and rewrites `a.handle_right` and `b.handle_left` to the outer halves.
/// Returns `None` when the span is degenerate (zero length, or a flat scalar span with
/// nothing to preserve).
pub fn split_bezier_span(a: &mut Keyframe, b: &mut Keyframe, time_us: i64) -> Option<Keyframe> {
    let span_us = b.time_us - a.time_us;
    if span_us <= 0 || time_us <= a.time_us || time_us >= b.time_us {
        return None;
    }
    let scale = span_scale(a, b);
    if !a.value.is_vector() && scale.abs() < 1e-9 {
        return None;
    }
    let span = span_us as f64;
    let (hr, hl) = default_handles(a, b);
    let p = [
        (0.0, 0.0),
        (hr.0 as f64 / span, hr.1 / scale),
        (1.0 + hl.0 as f64 / span, 1.0 + hl.1 / scale),
        (1.0, 1.0),
    ];
    let u = (time_us - a.time_us) as f64 / span;
    let s = super::anim::solve_cubic_param(u, p[1].0, p[2].0);
    let lerp = |p: (f64, f64), q: (f64, f64)| (p.0 + (q.0 - p.0) * s, p.1 + (q.1 - p.1) * s);
    let q01 = lerp(p[0], p[1]);
    let q12 = lerp(p[1], p[2]);
    let q23 = lerp(p[2], p[3]);
    let r0 = lerp(q01, q12);
    let r1 = lerp(q12, q23);
    let c = lerp(r0, r1);

    let value = interpolate_value(&a.value, &b.value, c.1);
    let to_handle = |x: f64, y: f64| ((x * span).round() as i64, y * scale);
    let inner = Keyframe {
        time_us,
        value,
        easing: Easing::Bezier,
        handle_left: Some(to_handle(r0.0 - c.0, r0.1 - c.1)),
        handle_right: Some(to_handle(r1.0 - c.0, r1.1 - c.1)),
    };
    a.easing = Easing::Bezier;
    a.handle_right = Some(to_handle(q01.0, q01.1));
    b.handle_left = Some(to_handle(q23.0 - 1.0, q23.1 - 1.0));
    Some(inner)
}

/// The handles an `a -> b` Bezier span uses when none are stored (a flat third each side).
pub(crate) fn default_handles(a: &Keyframe, b: &Keyframe) -> ((i64, f64), (i64, f64)) {
    let span = (b.time_us - a.time_us).max(1) as f64;
    let third = (span * 0.33).round() as i64;
    (
        a.handle_right.unwrap_or((third, 0.0)),
        b.handle_left.unwrap_or((-third, 0.0)),
    )
}

/// `a + (b - a) * t` per component.
pub(crate) fn interpolate_value(a: &KeyValue, b: &KeyValue, t: f64) -> KeyValue {
    match (a, b) {
        (KeyValue::Number(x), KeyValue::Number(y)) => KeyValue::Number(x + (y - x) * t),
        _ => {
            let n = a.components().max(b.components());
            KeyValue::Vector(
                (0..n)
                    .map(|i| a.component(i) + (b.component(i) - a.component(i)) * t)
                    .collect(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn track_properties_are_strings_for_transform_and_effect_params() {
        let id = Id::new_v4();
        let eff = TrackProperty::effect_param(id, "intensity");
        assert_eq!(eff.to_string(), format!("effect:{id}/intensity"));
        for text in ["pos_x", "opacity", "fit_y", &format!("effect:{id}/tint")] {
            let p = TrackProperty::parse(text).unwrap();
            assert_eq!(p.to_string(), text);
            assert_eq!(
                serde_json::from_value::<TrackProperty>(json!(text)).unwrap(),
                p
            );
        }
        assert_eq!(
            TrackProperty::parse("opacity").unwrap(),
            AnimProperty::Opacity
        );
        assert!(TrackProperty::parse("opacty")
            .unwrap_err()
            .contains("unknown keyframe property"));
        assert!(TrackProperty::parse("effect:not-a-uuid/x").is_err());
        assert!(TrackProperty::parse(&format!("effect:{id}")).is_err());
        assert!(TrackProperty::parse(&format!("effect:{id}/")).is_err());
        assert_eq!(eff.effect(), Some((id, "intensity")));
        assert_eq!(eff.anim(), None);
    }

    #[test]
    fn keyframes_serialise_numbers_and_arrays_and_skip_absent_handles() {
        let k = Keyframe::new(1_000_000, 0.5);
        assert_eq!(
            serde_json::to_value(&k).unwrap(),
            json!({ "time_us": 1000000, "value": 0.5, "easing": "linear" })
        );
        let k = Keyframe {
            easing: Easing::Bezier,
            handle_right: Some((200_000, 0.1)),
            ..Keyframe::new(0, vec![1.0, 0.0, 0.0, 1.0])
        };
        let v = serde_json::to_value(&k).unwrap();
        assert_eq!(v["value"], json!([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(v["handle_right"], json!([200000, 0.1]));
        assert_eq!(serde_json::from_value::<Keyframe>(v).unwrap(), k);
        for easing in ["linear", "hold", "bezier", "auto", "ease_in_out"] {
            let k: Keyframe =
                serde_json::from_value(json!({ "time_us": 0, "value": 1, "easing": easing }))
                    .unwrap();
            assert_eq!(serde_json::to_value(k.easing).unwrap(), easing);
        }
        // The v7 quadratics are migrated away, not accepted.
        assert!(serde_json::from_value::<Keyframe>(
            json!({ "time_us": 0, "value": 1, "easing": "ease_in" })
        )
        .is_err());
    }

    #[test]
    fn splitting_a_bezier_span_reproduces_the_curve() {
        use crate::project::sample_track_value;
        let mut a = Keyframe {
            easing: Easing::Bezier,
            handle_right: Some((400_000, 0.0)),
            ..Keyframe::new(0, 0.0)
        };
        let mut b = Keyframe {
            handle_left: Some((-400_000, 0.2)),
            ..Keyframe::new(2_000_000, 10.0)
        };
        let before = KeyframeTrack::new(AnimProperty::Opacity, vec![a.clone(), b.clone()]);
        let inner = split_bezier_span(&mut a, &mut b, 750_000).expect("splits");
        assert_eq!(inner.time_us, 750_000);
        assert_eq!(inner.easing, Easing::Bezier);
        let after = KeyframeTrack::new(AnimProperty::Opacity, vec![a, inner, b]);
        for step in 0..=100 {
            let t = step * 20_000;
            let (x, y) = (
                sample_track_value(&before, t).unwrap().num(),
                sample_track_value(&after, t).unwrap().num(),
            );
            assert!((x - y).abs() < 1e-2, "t={t}: {x} vs {y}");
        }
        // Degenerate cases do nothing.
        let mut a = Keyframe::new(0, 1.0);
        let mut b = Keyframe::new(1_000_000, 1.0);
        assert!(
            split_bezier_span(&mut a, &mut b, 500_000).is_none(),
            "flat span"
        );
        let mut b = Keyframe::new(1_000_000, 2.0);
        assert!(
            split_bezier_span(&mut a, &mut b, 0).is_none(),
            "not strictly inside"
        );
    }
}
