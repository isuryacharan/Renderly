//! `effect.json` manifest, format 1 (effect, filter and transition items).
//!
//! [`EffectManifest::parse`] reads the JSON, rejects anything this build cannot run with a
//! [`ManifestError::NeedsNewer`] (unknown kind, feature, pass type, input or parameter type),
//! and validates everything else: identifiers, parameter ranges and defaults, shaping
//! expressions (parsed and type-checked against the names they may use), pass wiring and the
//! format limits. Optional fields this build does not know are ignored, as the design says.
//! Nothing here touches the filesystem or the GPU.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use thiserror::Error;

use super::expr::{Expr, Scope, Ty};
use super::prelude::UniformValue;
use super::{HostValues, ParamValue};

/// The only manifest format this build reads.
pub const FORMAT: u32 = 1;
/// Engine features this build supports. Manifests that require anything else are disabled.
pub const FEATURES: &[&str] = &["gpu.fragment", "expr.v1"];
/// Limits from the design ("Inputs, targets and passes"; "Parameters").
pub const MAX_PARAMS: usize = 32;
pub const MAX_PASSES: usize = 16;
pub const MAX_TARGETS: usize = 8;
pub const MAX_TEXTURES: usize = 8;
pub const MAX_LUT_PARAMS: usize = 2;
/// Largest uniform block in bytes (the host block plus the params block).
pub const MAX_UNIFORM_BYTES: usize = 4096;

/// Parameter keys from the shared vocabulary (`docs/effect-params.md`). A standard key needs
/// no `label`; any other key must carry one.
pub const STANDARD_KEYS: &[&str] = &[
    "intensity",
    "speed",
    "size",
    "amount",
    "blur",
    "angle",
    "direction",
    "center",
    "color",
    "softness",
    "feather",
    "threshold",
    "offset_x",
    "offset_y",
    "scale",
    "rotation",
    "noise",
    "brightness",
    "contrast",
    "saturation",
    "hue",
    "temperature",
    "tint",
    "luminance",
    "vignette",
];

/// Names the generated prelude defines at module scope. Pass inputs, file textures and LUT
/// keys must not use them.
const RESERVED_GLOBALS: &[&str] = &[
    "host",
    "params",
    "samp_linear",
    "samp_nearest",
    "src",
    "from",
    "to",
    "prev",
    "out",
    "VsOut",
    "Host",
    "Params",
    "vs_main",
];

/// WGSL reserved words. They cannot name struct members or module-scope bindings, so param and
/// derived names must avoid them, and a pass input with one of these names is bound as
/// `in_<name>` in the prelude (see [`super::prelude::wgsl_input_name`]).
pub const WGSL_RESERVED: &[&str] = &[
    "NULL",
    "Self",
    "abstract",
    "active",
    "alignas",
    "alignof",
    "as",
    "asm",
    "asm_fragment",
    "async",
    "attribute",
    "auto",
    "await",
    "become",
    "binding_array",
    "cast",
    "catch",
    "class",
    "co_await",
    "co_return",
    "co_yield",
    "coherent",
    "column_major",
    "common",
    "compile",
    "compile_fragment",
    "concept",
    "const_cast",
    "consteval",
    "constexpr",
    "constinit",
    "crate",
    "debugger",
    "decltype",
    "delete",
    "demote",
    "demote_to_helper",
    "do",
    "dynamic_cast",
    "enum",
    "explicit",
    "export",
    "extends",
    "extern",
    "external",
    "fallthrough",
    "filter",
    "final",
    "finally",
    "friend",
    "from",
    "fxgroup",
    "get",
    "goto",
    "groupshared",
    "highp",
    "impl",
    "implements",
    "import",
    "inline",
    "instanceof",
    "interface",
    "layout",
    "lowp",
    "macro",
    "macro_rules",
    "match",
    "mediump",
    "meta",
    "mod",
    "module",
    "move",
    "mut",
    "mutable",
    "namespace",
    "new",
    "nil",
    "noexcept",
    "noinline",
    "nointerpolation",
    "noperspective",
    "null",
    "nullptr",
    "of",
    "operator",
    "package",
    "packoffset",
    "partition",
    "pass",
    "patch",
    "pixelfragment",
    "precise",
    "precision",
    "premerge",
    "priv",
    "protected",
    "pub",
    "public",
    "readonly",
    "ref",
    "regardless",
    "register",
    "reinterpret_cast",
    "require",
    "resource",
    "restrict",
    "self",
    "set",
    "shared",
    "sizeof",
    "smooth",
    "snorm",
    "static",
    "static_assert",
    "static_cast",
    "std",
    "subroutine",
    "super",
    "target",
    "template",
    "this",
    "thread_local",
    "throw",
    "trait",
    "try",
    "type",
    "typedef",
    "typeid",
    "typename",
    "typeof",
    "union",
    "unless",
    "unorm",
    "unsafe",
    "unsized",
    "use",
    "using",
    "varying",
    "virtual",
    "volatile",
    "wgsl",
    "where",
    "with",
    "writeonly",
    "yield",
];

/// Host variables usable in expressions, in the order [`HostVar`] indexes them.
const HOST_VARS: [&str; 7] = [
    "t", "duration", "progress", "width", "height", "frame", "speed",
];

#[derive(Debug, Error, Clone, PartialEq)]
pub enum ManifestError {
    #[error("effect.json is not valid JSON: {0}")]
    Json(String),
    #[error("unsupported manifest format {0} (this build reads format {FORMAT})")]
    Format(u32),
    #[error("needs a newer Renderly ({0})")]
    NeedsNewer(String),
    #[error("invalid effect.json: {0}")]
    Invalid(String),
    #[error("expression in {context}: {message}")]
    Expr { context: String, message: String },
}

fn invalid<T>(message: impl Into<String>) -> Result<T, ManifestError> {
    Err(ManifestError::Invalid(message.into()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Effect,
    Filter,
    Transition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamKind {
    Float,
    Int,
    Bool,
    Color,
    Vec2,
    Choice,
    Lut,
}

impl ParamKind {
    /// Number of components a control value of this kind has (0 for LUT references).
    pub fn components(self) -> usize {
        match self {
            ParamKind::Color => 4,
            ParamKind::Vec2 => 2,
            ParamKind::Lut => 0,
            _ => 1,
        }
    }

    fn uniform_ty(self) -> Option<UniformTy> {
        Some(match self {
            ParamKind::Float | ParamKind::Bool => UniformTy::F32,
            ParamKind::Int | ParamKind::Choice => UniformTy::I32,
            ParamKind::Vec2 => UniformTy::Vec2,
            ParamKind::Color => UniformTy::Vec4,
            ParamKind::Lut => return None,
        })
    }
}

/// Scalar type of a derived value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueTy {
    Float,
    Int,
    Bool,
}

/// WGSL types a uniform field can have (the design's table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniformTy {
    F32,
    I32,
    Vec2,
    Vec4,
}

impl UniformTy {
    pub fn wgsl(self) -> &'static str {
        match self {
            UniformTy::F32 => "f32",
            UniformTy::I32 => "i32",
            UniformTy::Vec2 => "vec2f",
            UniformTy::Vec4 => "vec4f",
        }
    }

    pub(crate) fn size(self) -> usize {
        match self {
            UniformTy::F32 | UniformTy::I32 => 4,
            UniformTy::Vec2 => 8,
            UniformTy::Vec4 => 16,
        }
    }

    pub(crate) fn align(self) -> usize {
        self.size()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChoiceOption {
    pub id: String,
    pub label: String,
}

/// Where an expression variable's value comes from when it is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Host(usize),
    /// A scalar param's control value (index into the params list).
    Scalar(usize),
    /// The component being evaluated of a vector param (index into the params list).
    Component(usize),
    /// A derived value (index into the derived list).
    Derived(usize),
    /// `x`: the component being evaluated of the param whose uniform expression this is.
    X,
}

/// A compiled expression plus the source of each of its variable slots.
#[derive(Debug, Clone)]
pub(crate) struct Compiled {
    pub(crate) expr: Expr,
    pub(crate) sources: Vec<Source>,
}

#[derive(Debug, Clone)]
pub struct ParamSpec {
    pub key: String,
    pub kind: ParamKind,
    /// Display label: the manifest's label, else the key.
    pub label: String,
    pub labels: BTreeMap<String, String>,
    pub min: f64,
    pub max: f64,
    pub step: Option<f64>,
    pub default: ParamValue,
    pub options: Vec<ChoiceOption>,
    pub animatable: bool,
    pub(crate) uniform: Option<Compiled>,
}

#[derive(Debug, Clone)]
pub struct DerivedSpec {
    pub name: String,
    pub ty: ValueTy,
    pub(crate) expr: Compiled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterMode {
    Linear,
    Nearest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapMode {
    Clamp,
    Repeat,
}

#[derive(Debug, Clone)]
pub struct TextureSpec {
    pub name: String,
    pub path: String,
    pub filter: FilterMode,
    pub wrap: WrapMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetFormat {
    Rgba8,
    Rgba16f,
}

#[derive(Debug, Clone)]
pub struct TargetSpec {
    pub name: String,
    pub scale: f64,
    pub format: TargetFormat,
}

#[derive(Debug, Clone)]
pub struct PassSpec {
    pub shader: String,
    pub entry: String,
    pub inputs: Vec<String>,
    /// `out` or the name of a target.
    pub output: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionSpec {
    pub default_duration_ms: u32,
    pub overlap: bool,
    pub easing: Easing,
}

/// A validated format-1 item manifest.
#[derive(Debug, Clone)]
pub struct EffectManifest {
    pub id: String,
    pub version: u32,
    pub kind: ItemKind,
    pub label: String,
    pub category: Option<String>,
    pub requires: Vec<String>,
    pub params: Vec<ParamSpec>,
    pub derived: Vec<DerivedSpec>,
    pub textures: Vec<TextureSpec>,
    pub targets: Vec<TargetSpec>,
    pub passes: Vec<PassSpec>,
    pub transition: Option<TransitionSpec>,
}

// ---- raw JSON shapes -------------------------------------------------------------------

#[derive(Deserialize)]
struct RawManifest {
    #[serde(default)]
    requires: Option<Vec<String>>,
    id: String,
    version: u32,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    params: Vec<RawParam>,
    #[serde(default)]
    derived: Vec<RawDerived>,
    #[serde(default)]
    textures: Vec<RawTexture>,
    #[serde(default)]
    targets: Vec<RawTarget>,
    #[serde(default)]
    passes: Vec<RawPass>,
    #[serde(default)]
    default_duration_ms: Option<u32>,
    #[serde(default)]
    overlap: Option<bool>,
    #[serde(default)]
    easing: Option<String>,
}

#[derive(Deserialize)]
struct RawParam {
    key: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    min: Option<f64>,
    #[serde(default)]
    max: Option<f64>,
    #[serde(default)]
    step: Option<f64>,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(default)]
    options: Vec<RawOption>,
    #[serde(default)]
    uniform: Option<String>,
    #[serde(default)]
    animatable: Option<bool>,
}

#[derive(Deserialize)]
struct RawOption {
    id: String,
    label: String,
}

#[derive(Deserialize)]
struct RawDerived {
    name: String,
    #[serde(rename = "type", default)]
    ty: Option<String>,
    expr: String,
}

#[derive(Deserialize)]
struct RawTexture {
    name: String,
    path: String,
    #[serde(default)]
    filter: Option<String>,
    #[serde(default)]
    wrap: Option<String>,
}

#[derive(Deserialize)]
struct RawTarget {
    name: String,
    #[serde(default)]
    scale: Option<f64>,
    #[serde(default)]
    format: Option<String>,
}

#[derive(Deserialize)]
struct RawPass {
    #[serde(rename = "type", default)]
    ty: Option<String>,
    shader: String,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    inputs: Vec<String>,
    output: String,
}

// ---- parsing ---------------------------------------------------------------------------

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn is_item_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Relative path inside the item folder: no absolute paths, drive letters or `..`.
pub fn check_relative_path(path: &str) -> Result<(), String> {
    let bad = path.is_empty()
        || path.starts_with('/')
        || path.starts_with('\\')
        || path.contains(':')
        || path.split(['/', '\\']).any(|part| part == "..");
    if bad {
        Err(format!(
            "path '{path}' must be relative and stay inside the item"
        ))
    } else {
        Ok(())
    }
}

fn number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

/// Symbols an expression of one parameter or derived value may use, in slot order.
struct ScopeBuilder {
    scope: Scope,
    sources: Vec<Source>,
}

impl ScopeBuilder {
    fn host(&mut self) {
        for (i, name) in HOST_VARS.iter().enumerate() {
            let ty = Ty::Num;
            self.scope.push(name, ty);
            self.sources.push(Source::Host(i));
        }
    }

    fn push(&mut self, name: &str, ty: Ty, source: Source) {
        self.scope.push(name, ty);
        self.sources.push(source);
    }

    fn finish(self) -> (Scope, Vec<Source>) {
        (self.scope, self.sources)
    }
}

fn compile_expr(
    context: &str,
    src: &str,
    scope: &Scope,
    forbidden: &[&str],
    sources: Vec<Source>,
) -> Result<Compiled, ManifestError> {
    let expr = Expr::compile(src, scope, forbidden).map_err(|e| ManifestError::Expr {
        context: context.to_string(),
        message: e.to_string(),
    })?;
    Ok(Compiled { expr, sources })
}

impl EffectManifest {
    /// Parses and validates `json` (the contents of an `effect.json`).
    pub fn parse(json: &str) -> Result<EffectManifest, ManifestError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| ManifestError::Json(e.to_string()))?;
        let format = value
            .get("format")
            .and_then(serde_json::Value::as_u64)
            .map_or(FORMAT, |f| u32::try_from(f).unwrap_or(u32::MAX));
        if format != FORMAT {
            return Err(ManifestError::Format(format));
        }
        let kind = match value.get("kind").and_then(serde_json::Value::as_str) {
            Some("effect") => ItemKind::Effect,
            Some("filter") => ItemKind::Filter,
            Some("transition") => ItemKind::Transition,
            Some(other) => {
                return Err(ManifestError::NeedsNewer(format!(
                    "item kind '{other}' is not supported by this build"
                )))
            }
            None => return invalid("missing \"kind\""),
        };
        let raw: RawManifest =
            serde_json::from_value(value).map_err(|e| ManifestError::Invalid(e.to_string()))?;
        Self::from_raw(raw, kind)
    }

    fn from_raw(raw: RawManifest, kind: ItemKind) -> Result<EffectManifest, ManifestError> {
        if !is_item_id(&raw.id) {
            return invalid(format!("item id '{}' must match [a-z0-9-]+", raw.id));
        }
        let requires = match raw.requires {
            Some(list) => list,
            None => FEATURES.iter().map(|f| f.to_string()).collect(),
        };
        for feature in &requires {
            if !FEATURES.contains(&feature.as_str()) {
                return Err(ManifestError::NeedsNewer(format!("feature {feature}")));
            }
        }

        let label = raw.label.clone().unwrap_or_else(|| raw.id.clone());
        let mut params = parse_params(&raw.params)?;
        compile_param_uniforms(&raw.params, &mut params)?;
        let derived = parse_derived(&raw.derived, &params)?;
        let textures = parse_textures(raw.textures)?;
        let targets = parse_targets(raw.targets)?;
        let passes = parse_passes(raw.passes, kind, &params, &textures, &targets)?;

        let transition = match kind {
            ItemKind::Transition => Some(TransitionSpec {
                default_duration_ms: raw.default_duration_ms.unwrap_or(500),
                overlap: raw.overlap.unwrap_or(true),
                easing: match raw.easing.as_deref() {
                    None | Some("linear") => Easing::Linear,
                    Some("ease_in") => Easing::EaseIn,
                    Some("ease_out") => Easing::EaseOut,
                    Some("ease_in_out") => Easing::EaseInOut,
                    Some(other) => return invalid(format!("unknown easing '{other}'")),
                },
            }),
            _ => None,
        };

        Ok(EffectManifest {
            id: raw.id,
            version: raw.version,
            kind,
            label,
            category: raw.category,
            requires,
            params,
            derived,
            textures,
            targets,
            passes,
            transition,
        })
    }

    /// Parameter keys whose values are LUT references.
    pub fn lut_params(&self) -> impl Iterator<Item = &ParamSpec> {
        self.params.iter().filter(|p| p.kind == ParamKind::Lut)
    }

    /// Resolves caller-supplied control values against the declared params: missing keys take
    /// the default, scalars are clamped to their range (ints and choices rounded), colours
    /// clamped to 0..1, and a value of the wrong shape is an error. The result is in param
    /// declaration order.
    pub fn resolve_params(
        &self,
        given: &BTreeMap<String, ParamValue>,
    ) -> Result<Vec<ParamValue>, String> {
        let mut out = Vec::with_capacity(self.params.len());
        for p in &self.params {
            let value = match given.get(&p.key) {
                None => p.default.clone(),
                Some(v) => match (p.kind, v) {
                    (ParamKind::Lut, ParamValue::Lut(r)) => ParamValue::Lut(r.clone()),
                    (ParamKind::Color | ParamKind::Vec2, ParamValue::Vector(vs))
                        if vs.len() == p.kind.components() && vs.iter().all(|c| c.is_finite()) =>
                    {
                        let mut vs = vs.clone();
                        if p.kind == ParamKind::Color {
                            for c in &mut vs {
                                *c = c.clamp(0.0, 1.0);
                            }
                        }
                        ParamValue::Vector(vs)
                    }
                    (
                        ParamKind::Float | ParamKind::Int | ParamKind::Bool | ParamKind::Choice,
                        ParamValue::Number(n),
                    ) if n.is_finite() => ParamValue::Number(match p.kind {
                        ParamKind::Bool => {
                            if *n != 0.0 {
                                1.0
                            } else {
                                0.0
                            }
                        }
                        ParamKind::Int | ParamKind::Choice => n.round().clamp(p.min, p.max),
                        _ => n.clamp(p.min, p.max),
                    }),
                    _ => {
                        return Err(format!(
                            "value for param '{}' does not match its type {:?}",
                            p.key, p.kind
                        ))
                    }
                },
            };
            out.push(value);
        }
        Ok(out)
    }

    /// Uniform values in layout order: every non-LUT param (declaration order), then every
    /// derived value. `controls` must come from [`Self::resolve_params`].
    pub fn evaluate_uniforms(
        &self,
        controls: &[ParamValue],
        host: &HostValues,
    ) -> Result<Vec<UniformValue>, ManifestError> {
        let host_vars = host.vars();
        let mut out = Vec::new();
        let mut derived_values: Vec<f64> = Vec::with_capacity(self.derived.len());
        for (pi, p) in self.params.iter().enumerate() {
            let Some(uty) = p.kind.uniform_ty() else {
                continue;
            };
            let control = &controls[pi];
            let comps = p.kind.components().max(1);
            let mut values = [0.0f64; 4];
            for (comp, slot) in values.iter_mut().enumerate().take(comps) {
                let base = match control {
                    ParamValue::Number(n) => *n,
                    ParamValue::Vector(v) => v.get(comp).copied().unwrap_or(0.0),
                    ParamValue::Lut(_) => 0.0,
                };
                *slot = match &p.uniform {
                    None => base,
                    Some(c) => eval_compiled(c, &host_vars, controls, &derived_values, comp, base)
                        .map_err(|e| expr_error(format!("uniform of param '{}'", p.key), e))?,
                };
            }
            out.push(to_uniform(uty, &values[..comps]));
        }
        for d in &self.derived {
            let v = eval_compiled(&d.expr, &host_vars, controls, &derived_values, 0, 0.0)
                .map_err(|e| expr_error(format!("derived value '{}'", d.name), e))?;
            derived_values.push(v);
            out.push(match d.ty {
                ValueTy::Float | ValueTy::Bool => UniformValue::F32(v as f32),
                ValueTy::Int => UniformValue::I32(v.trunc() as i32),
            });
        }
        Ok(out)
    }

    /// Uniform fields this item's `Params` block has, in order: `(name, type)`.
    pub fn uniform_fields(&self) -> Vec<(String, UniformTy)> {
        let mut out = Vec::new();
        for p in &self.params {
            if let Some(t) = p.kind.uniform_ty() {
                out.push((p.key.clone(), t));
            }
        }
        for d in &self.derived {
            out.push((
                d.name.clone(),
                match d.ty {
                    ValueTy::Float | ValueTy::Bool => UniformTy::F32,
                    ValueTy::Int => UniformTy::I32,
                },
            ));
        }
        out
    }
}

fn to_uniform(ty: UniformTy, comps: &[f64]) -> UniformValue {
    let c = |i: usize| comps.get(i).copied().unwrap_or(0.0) as f32;
    match ty {
        UniformTy::F32 => UniformValue::F32(c(0)),
        UniformTy::I32 => UniformValue::I32(comps.first().copied().unwrap_or(0.0).trunc() as i32),
        UniformTy::Vec2 => UniformValue::Vec2([c(0), c(1)]),
        UniformTy::Vec4 => UniformValue::Vec4([c(0), c(1), c(2), c(3)]),
    }
}

fn expr_error(context: String, e: super::expr::ExprError) -> ManifestError {
    ManifestError::Expr {
        context,
        message: e.to_string(),
    }
}

fn eval_compiled(
    c: &Compiled,
    host: &[f64; 7],
    controls: &[ParamValue],
    derived: &[f64],
    comp: usize,
    x: f64,
) -> Result<f64, super::expr::ExprError> {
    let mut slots = Vec::with_capacity(c.sources.len());
    for src in &c.sources {
        slots.push(match *src {
            Source::Host(i) => host[i],
            Source::Scalar(p) => match &controls[p] {
                ParamValue::Number(n) => *n,
                _ => 0.0,
            },
            Source::Component(p) => match &controls[p] {
                ParamValue::Vector(v) => v.get(comp).copied().unwrap_or(0.0),
                _ => 0.0,
            },
            Source::Derived(d) => derived.get(d).copied().unwrap_or(0.0),
            Source::X => x,
        });
    }
    c.expr.eval(&slots)
}

fn parse_params(raw: &[RawParam]) -> Result<Vec<ParamSpec>, ManifestError> {
    if raw.len() > MAX_PARAMS {
        return invalid(format!("{} params (limit {MAX_PARAMS})", raw.len()));
    }
    let mut seen = BTreeSet::new();
    let mut out: Vec<ParamSpec> = Vec::with_capacity(raw.len());
    for r in raw {
        if !is_ident(&r.key) || r.key.starts_with('_') {
            return invalid(format!("param key '{}' is not a valid identifier", r.key));
        }
        if matches!(r.key.as_str(), "x" | "true" | "false") || HOST_VARS.contains(&r.key.as_str()) {
            return invalid(format!("param key '{}' is reserved in expressions", r.key));
        }
        if WGSL_RESERVED.contains(&r.key.as_str()) {
            return invalid(format!("param key '{}' is a WGSL reserved word", r.key));
        }
        if !seen.insert(r.key.clone()) {
            return invalid(format!("duplicate param key '{}'", r.key));
        }
        let standard = STANDARD_KEYS.contains(&r.key.as_str());
        if !standard && r.label.is_none() {
            return invalid(format!(
                "custom param '{}' needs a \"label\" (not in the shared vocabulary)",
                r.key
            ));
        }
        let kind = match r.ty.as_str() {
            "float" => ParamKind::Float,
            "int" => ParamKind::Int,
            "bool" => ParamKind::Bool,
            "color" => ParamKind::Color,
            "vec2" => ParamKind::Vec2,
            "choice" => ParamKind::Choice,
            "lut" => ParamKind::Lut,
            other => {
                return Err(ManifestError::NeedsNewer(format!(
                    "param type '{other}' of '{}' is not supported by this build",
                    r.key
                )))
            }
        };
        let spec = parse_one_param(r, kind)?;
        out.push(spec);
    }
    let lut_count = out.iter().filter(|p| p.kind == ParamKind::Lut).count();
    if lut_count > MAX_LUT_PARAMS {
        return invalid(format!("{lut_count} lut params (limit {MAX_LUT_PARAMS})"));
    }
    Ok(out)
}

fn parse_one_param(r: &RawParam, kind: ParamKind) -> Result<ParamSpec, ManifestError> {
    let key = &r.key;
    let mut options = Vec::new();
    let (min, max, default, step);
    match kind {
        ParamKind::Float | ParamKind::Int => {
            let (Some(lo), Some(hi)) = (r.min, r.max) else {
                return invalid(format!("param '{key}' needs \"min\" and \"max\""));
            };
            if !(lo.is_finite() && hi.is_finite() && lo <= hi) {
                return invalid(format!("param '{key}' has an empty or invalid range"));
            }
            min = lo;
            max = hi;
            let d = match &r.default {
                None => lo,
                Some(v) => number(v).ok_or_else(|| {
                    ManifestError::Invalid(format!("default of '{key}' must be a number"))
                })?,
            };
            if d < lo || d > hi {
                return invalid(format!("default of '{key}' is outside its range"));
            }
            if kind == ParamKind::Int && d.fract() != 0.0 {
                return invalid(format!("default of int param '{key}' must be an integer"));
            }
            default = ParamValue::Number(d);
            step = r.step;
        }
        ParamKind::Bool => {
            min = 0.0;
            max = 1.0;
            step = None;
            let b = match &r.default {
                None => false,
                Some(v) => v.as_bool().ok_or_else(|| {
                    ManifestError::Invalid(format!("default of '{key}' must be a bool"))
                })?,
            };
            default = ParamValue::Number(if b { 1.0 } else { 0.0 });
        }
        ParamKind::Choice => {
            if r.options.is_empty() {
                return invalid(format!("choice param '{key}' needs \"options\""));
            }
            let mut ids = BTreeSet::new();
            for o in &r.options {
                if !ids.insert(o.id.clone()) {
                    return invalid(format!("duplicate option '{}' in '{key}'", o.id));
                }
                options.push(ChoiceOption {
                    id: o.id.clone(),
                    label: o.label.clone(),
                });
            }
            min = 0.0;
            max = (options.len() - 1) as f64;
            step = Some(1.0);
            let d = match &r.default {
                None => 0.0,
                Some(v) => number(v).ok_or_else(|| {
                    ManifestError::Invalid(format!("default of '{key}' must be an option index"))
                })?,
            };
            if d.fract() != 0.0 || d < min || d > max {
                return invalid(format!("default of choice '{key}' must be an option index"));
            }
            default = ParamValue::Number(d);
        }
        ParamKind::Color | ParamKind::Vec2 => {
            min = 0.0;
            max = 1.0;
            step = None;
            let n = kind.components();
            let d = match &r.default {
                None => vec![0.0; n],
                Some(v) => {
                    let arr = v.as_array().ok_or_else(|| {
                        ManifestError::Invalid(format!(
                            "default of '{key}' must be an array of {n} numbers"
                        ))
                    })?;
                    let mut vs = Vec::with_capacity(n);
                    for item in arr {
                        vs.push(number(item).ok_or_else(|| {
                            ManifestError::Invalid(format!("default of '{key}' must hold numbers"))
                        })?);
                    }
                    if vs.len() != n {
                        return invalid(format!("default of '{key}' must have {n} components"));
                    }
                    if kind == ParamKind::Color && vs.iter().any(|c| !(0.0..=1.0).contains(c)) {
                        return invalid(format!("color default of '{key}' must be in 0..1"));
                    }
                    vs
                }
            };
            default = ParamValue::Vector(d);
        }
        ParamKind::Lut => {
            min = 0.0;
            max = 0.0;
            step = None;
            default = ParamValue::Lut(String::new());
        }
    }
    if kind == ParamKind::Lut && r.uniform.is_some() {
        return invalid(format!(
            "lut param '{key}' cannot have a \"uniform\" expression"
        ));
    }
    Ok(ParamSpec {
        key: key.clone(),
        kind,
        label: r.label.clone().unwrap_or_else(|| key.clone()),
        labels: r.labels.clone(),
        min,
        max,
        step,
        default,
        options,
        animatable: r.animatable.unwrap_or(kind != ParamKind::Lut),
        uniform: None,
    })
}

/// Compiles the `uniform` expressions. Built after all params are known so each expression
/// sees every param's control value; vector params are read per component.
fn compile_param_uniforms(raw: &[RawParam], params: &mut [ParamSpec]) -> Result<(), ManifestError> {
    for (i, r) in raw.iter().enumerate() {
        let Some(src) = &r.uniform else {
            continue;
        };
        let context = format!("uniform of param '{}'", params[i].key);
        if params[i].kind.uniform_ty().is_none() {
            return invalid(format!("{context}: this param type has no uniform"));
        }
        let mut sb = ScopeBuilder {
            scope: Scope::new(),
            sources: Vec::new(),
        };
        sb.host();
        sb.push("x", Ty::Num, Source::X);
        for (pj, p) in params.iter().enumerate() {
            let source = match p.kind {
                ParamKind::Lut => continue,
                ParamKind::Vec2 | ParamKind::Color => Source::Component(pj),
                _ => Source::Scalar(pj),
            };
            let ty = if p.kind == ParamKind::Bool {
                Ty::Bool
            } else {
                Ty::Num
            };
            sb.push(&p.key, ty, source);
        }
        let (scope, sources) = sb.finish();
        let compiled = compile_expr(&context, src, &scope, &[], sources)?;
        params[i].uniform = Some(compiled);
    }
    Ok(())
}

fn parse_derived(
    raw: &[RawDerived],
    params: &[ParamSpec],
) -> Result<Vec<DerivedSpec>, ManifestError> {
    let mut out: Vec<DerivedSpec> = Vec::with_capacity(raw.len());
    let mut names: BTreeSet<&str> = params.iter().map(|p| p.key.as_str()).collect();
    for d in raw {
        if !is_ident(&d.name) || d.name.starts_with('_') {
            return invalid(format!(
                "derived name '{}' is not a valid identifier",
                d.name
            ));
        }
        if WGSL_RESERVED.contains(&d.name.as_str()) {
            return invalid(format!("derived name '{}' is a WGSL reserved word", d.name));
        }
        if !names.insert(d.name.as_str()) || HOST_VARS.contains(&d.name.as_str()) {
            return invalid(format!("derived name '{}' is already used", d.name));
        }
        let ty = match d.ty.as_deref().unwrap_or("float") {
            "float" => ValueTy::Float,
            "int" => ValueTy::Int,
            "bool" => ValueTy::Bool,
            other => {
                return Err(ManifestError::NeedsNewer(format!(
                    "derived type '{other}' is not supported by this build"
                )))
            }
        };
        let mut sb = ScopeBuilder {
            scope: Scope::new(),
            sources: Vec::new(),
        };
        sb.host();
        for (pj, p) in params.iter().enumerate() {
            match p.kind {
                ParamKind::Lut => {}
                ParamKind::Vec2 | ParamKind::Color => {}
                _ => sb.push(
                    &p.key,
                    if p.kind == ParamKind::Bool {
                        Ty::Bool
                    } else {
                        Ty::Num
                    },
                    Source::Scalar(pj),
                ),
            }
        }
        for (dj, prev) in out.iter().enumerate() {
            sb.push(
                &prev.name,
                if prev.ty == ValueTy::Bool {
                    Ty::Bool
                } else {
                    Ty::Num
                },
                Source::Derived(dj),
            );
        }
        let (scope, sources) = sb.finish();
        let forbidden: Vec<&str> = params
            .iter()
            .filter(|p| matches!(p.kind, ParamKind::Vec2 | ParamKind::Color | ParamKind::Lut))
            .map(|p| p.key.as_str())
            .collect();
        let expr = compile_expr(
            &format!("derived value '{}'", d.name),
            &d.expr,
            &scope,
            &forbidden,
            sources,
        )?;
        out.push(DerivedSpec {
            name: d.name.clone(),
            ty,
            expr,
        });
    }
    Ok(out)
}

fn parse_textures(raw: Vec<RawTexture>) -> Result<Vec<TextureSpec>, ManifestError> {
    if raw.len() > MAX_TEXTURES {
        return invalid(format!("{} textures (limit {MAX_TEXTURES})", raw.len()));
    }
    let mut out = Vec::with_capacity(raw.len());
    for t in raw {
        if !is_ident(&t.name) || RESERVED_GLOBALS.contains(&t.name.as_str()) {
            return invalid(format!("texture name '{}' is not usable", t.name));
        }
        if let Err(m) = check_relative_path(&t.path) {
            return invalid(m);
        }
        let filter = match t.filter.as_deref().unwrap_or("linear") {
            "linear" => FilterMode::Linear,
            "nearest" => FilterMode::Nearest,
            other => return invalid(format!("unknown texture filter '{other}'")),
        };
        let wrap = match t.wrap.as_deref().unwrap_or("clamp") {
            "clamp" => WrapMode::Clamp,
            "repeat" => WrapMode::Repeat,
            other => return invalid(format!("unknown texture wrap '{other}'")),
        };
        out.push(TextureSpec {
            name: t.name,
            path: t.path,
            filter,
            wrap,
        });
    }
    Ok(out)
}

fn parse_targets(raw: Vec<RawTarget>) -> Result<Vec<TargetSpec>, ManifestError> {
    if raw.len() > MAX_TARGETS {
        return invalid(format!("{} targets (limit {MAX_TARGETS})", raw.len()));
    }
    let mut out: Vec<TargetSpec> = Vec::with_capacity(raw.len());
    for t in raw {
        if !is_ident(&t.name) || RESERVED_GLOBALS.contains(&t.name.as_str()) {
            return invalid(format!("target name '{}' is not usable", t.name));
        }
        if out.iter().any(|o| o.name == t.name) {
            return invalid(format!("duplicate target '{}'", t.name));
        }
        let scale = t.scale.unwrap_or(1.0);
        if !(scale.is_finite() && scale > 0.0 && scale <= 1.0) {
            return invalid(format!("target '{}' scale must be in (0, 1]", t.name));
        }
        let format = match t.format.as_deref().unwrap_or("rgba8") {
            "rgba8" => TargetFormat::Rgba8,
            "rgba16f" => TargetFormat::Rgba16f,
            other => {
                return Err(ManifestError::NeedsNewer(format!(
                    "target format '{other}' is not supported by this build"
                )))
            }
        };
        out.push(TargetSpec {
            name: t.name,
            scale,
            format,
        });
    }
    Ok(out)
}

fn parse_passes(
    raw: Vec<RawPass>,
    kind: ItemKind,
    params: &[ParamSpec],
    textures: &[TextureSpec],
    targets: &[TargetSpec],
) -> Result<Vec<PassSpec>, ManifestError> {
    if raw.is_empty() {
        return invalid("an item needs at least one pass");
    }
    if raw.len() > MAX_PASSES {
        return invalid(format!("{} passes (limit {MAX_PASSES})", raw.len()));
    }
    let lut_keys: BTreeSet<&str> = params
        .iter()
        .filter(|p| p.kind == ParamKind::Lut)
        .map(|p| p.key.as_str())
        .collect();
    let texture_names: BTreeSet<&str> = textures.iter().map(|t| t.name.as_str()).collect();
    let target_names: BTreeSet<&str> = targets.iter().map(|t| t.name.as_str()).collect();
    for name in lut_keys.iter().chain(&texture_names).chain(&target_names) {
        if RESERVED_GLOBALS.contains(name) {
            return invalid(format!("name '{name}' is reserved"));
        }
    }
    // Shared global names must be unique: a LUT key, texture and target are all module-scope
    // bindings in the generated prelude.
    let mut globals = BTreeSet::new();
    for name in lut_keys.iter().chain(&texture_names).chain(&target_names) {
        if !globals.insert(*name) {
            return invalid(format!("name '{name}' is used twice"));
        }
    }

    let mut written: BTreeSet<&str> = BTreeSet::new();
    let mut out = Vec::with_capacity(raw.len());
    let count = raw.len();
    for (i, p) in raw.iter().enumerate() {
        match p.ty.as_deref().unwrap_or("fragment") {
            "fragment" => {}
            "compute" => return Err(ManifestError::NeedsNewer("feature gpu.compute".into())),
            "mesh" => return Err(ManifestError::NeedsNewer("feature gpu.mesh".into())),
            other => {
                return Err(ManifestError::NeedsNewer(format!(
                    "pass type '{other}' is not supported by this build"
                )))
            }
        }
        if let Err(m) = check_relative_path(&p.shader) {
            return invalid(m);
        }
        if !p.shader.ends_with(".wgsl") {
            return invalid(format!("pass shader '{}' must be a .wgsl file", p.shader));
        }
        for input in &p.inputs {
            let ok = match input.as_str() {
                "src" => matches!(kind, ItemKind::Effect | ItemKind::Filter),
                "from" | "to" => kind == ItemKind::Transition,
                "prev" => i > 0,
                "history" => return Err(ManifestError::NeedsNewer("feature gpu.feedback".into())),
                "matte" => return Err(ManifestError::NeedsNewer("feature input.matte".into())),
                "face" => return Err(ManifestError::NeedsNewer("feature input.face".into())),
                "skin" => return Err(ManifestError::NeedsNewer("feature input.skin".into())),
                "depth" => return Err(ManifestError::NeedsNewer("feature input.depth".into())),
                name => {
                    texture_names.contains(name)
                        || lut_keys.contains(name)
                        || (target_names.contains(name) && written.contains(name))
                }
            };
            if !ok {
                return invalid(format!(
                    "pass {i}: input '{input}' is not available here (src/from/to depend on the kind, prev needs an earlier pass, targets must be written by an earlier pass)"
                ));
            }
        }
        let output = p.output.as_str();
        if output != "out" && !target_names.contains(output) {
            return invalid(format!(
                "pass {i}: output '{output}' is not a target or \"out\""
            ));
        }
        if p.inputs.iter().any(|input| input == output) {
            return invalid(format!(
                "pass {i}: a pass cannot read its own output '{output}'"
            ));
        }
        if i + 1 == count && output != "out" {
            return invalid("the last pass must write \"out\"");
        }
        if i + 1 != count && output == "out" {
            return invalid(format!("pass {i} writes \"out\" but is not the last pass"));
        }
        written.insert(output);
        out.push(PassSpec {
            shader: p.shader.clone(),
            entry: p.entry.clone().unwrap_or_else(|| "fs_main".to_string()),
            inputs: p.inputs.clone(),
            output: p.output.clone(),
        });
    }
    for t in targets {
        if !written.contains(t.name.as_str()) {
            return invalid(format!("target '{}' is never written", t.name));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAIN: &str = r#"{
      "format": 1, "id": "film-grain", "version": 1, "kind": "effect", "label": "Film grain",
      "params": [
        { "key": "intensity", "type": "float", "min": 0, "max": 1, "default": 0.5 },
        { "key": "size", "type": "float", "min": 0, "max": 1, "default": 0.3,
          "uniform": "mix(0.5, 4.0, pow(x, 2.0))", "label": "Grain size" },
        { "key": "tint", "type": "color", "default": [1, 1, 1, 1] },
        { "key": "animated", "type": "bool", "default": true, "label": "Animated" }
      ],
      "derived": [ { "name": "grain_speed", "type": "float", "expr": "animated * 24.0" } ],
      "textures": [ { "name": "noise", "path": "noise.png", "filter": "linear", "wrap": "repeat" } ],
      "passes": [ { "type": "fragment", "shader": "grain.wgsl", "inputs": ["src", "noise"], "output": "out" } ]
    }"#;

    fn parse(json: &str) -> Result<EffectManifest, ManifestError> {
        EffectManifest::parse(json)
    }

    #[test]
    fn parses_the_design_example() {
        let m = parse(GRAIN).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m.id, "film-grain");
        assert_eq!(m.kind, ItemKind::Effect);
        assert_eq!(m.params.len(), 4);
        assert_eq!(m.derived.len(), 1);
        assert_eq!(m.textures[0].wrap, WrapMode::Repeat);
        assert_eq!(m.requires, vec!["gpu.fragment", "expr.v1"]);
        // "intensity" is a standard key, so no label is needed; the label falls back to key.
        assert_eq!(m.params[0].label, "intensity");
        assert_eq!(m.params[1].label, "Grain size");
    }

    #[test]
    fn unknown_optional_fields_are_ignored() {
        let json = GRAIN.replacen(
            "\"format\": 1,",
            "\"format\": 1, \"future_field\": {\"a\": 1},",
            1,
        );
        assert!(parse(&json).is_ok());
    }

    #[test]
    fn unsupported_format_and_features_need_a_newer_build() {
        let json = GRAIN.replacen("\"format\": 1", "\"format\": 2", 1);
        assert_eq!(parse(&json).err(), Some(ManifestError::Format(2)));
        let json = GRAIN.replacen(
            "\"kind\": \"effect\"",
            "\"kind\": \"effect\", \"requires\": [\"gpu.compute\"]",
            1,
        );
        assert!(
            matches!(parse(&json), Err(ManifestError::NeedsNewer(m)) if m.contains("gpu.compute"))
        );
        let json = GRAIN.replacen("\"kind\": \"effect\"", "\"kind\": \"plugin\"", 1);
        assert!(matches!(parse(&json), Err(ManifestError::NeedsNewer(_))));
    }

    #[test]
    fn unknown_param_type_disables_the_item() {
        let json = GRAIN.replacen("\"type\": \"color\"", "\"type\": \"curve\"", 1);
        assert!(matches!(parse(&json), Err(ManifestError::NeedsNewer(m)) if m.contains("curve")));
    }

    #[test]
    fn compute_pass_and_reserved_inputs_need_newer_builds() {
        let json = GRAIN.replacen("\"type\": \"fragment\"", "\"type\": \"compute\"", 1);
        assert!(
            matches!(parse(&json), Err(ManifestError::NeedsNewer(m)) if m.contains("gpu.compute"))
        );
        let json = GRAIN.replacen("[\"src\", \"noise\"]", "[\"src\", \"history\"]", 1);
        assert!(
            matches!(parse(&json), Err(ManifestError::NeedsNewer(m)) if m.contains("gpu.feedback"))
        );
    }

    #[test]
    fn validation_errors_name_the_problem() {
        // custom key without a label
        let json = GRAIN.replacen("\"key\": \"tint\"", "\"key\": \"tinty\"", 1);
        assert!(
            matches!(parse(&json), Err(ManifestError::Invalid(m)) if m.contains("needs a \"label\""))
        );
        // default outside range
        let json = GRAIN.replacen("\"default\": 0.5", "\"default\": 2.0", 1);
        assert!(
            matches!(parse(&json), Err(ManifestError::Invalid(m)) if m.contains("outside its range"))
        );
        // bad expression
        let json = GRAIN.replacen("\"animated * 24.0\"", "\"animated * \"", 1);
        assert!(matches!(parse(&json), Err(ManifestError::Expr { .. })));
        // x is only available in uniform expressions
        let json = GRAIN.replacen("\"animated * 24.0\"", "\"x * 2.0\"", 1);
        assert!(matches!(parse(&json), Err(ManifestError::Expr { .. })));
        // unknown input
        let json = GRAIN.replacen("[\"src\", \"noise\"]", "[\"src\", \"nope\"]", 1);
        assert!(matches!(parse(&json), Err(ManifestError::Invalid(_))));
        // last pass must write out
        let json = GRAIN.replacen("\"output\": \"out\"", "\"output\": \"mid\"", 1);
        assert!(matches!(parse(&json), Err(ManifestError::Invalid(_))));
        // bad path
        let json = GRAIN.replacen("\"path\": \"noise.png\"", "\"path\": \"../x.png\"", 1);
        assert!(matches!(parse(&json), Err(ManifestError::Invalid(m)) if m.contains("relative")));
        // bad id
        let json = GRAIN.replacen("\"film-grain\"", "\"Film_Grain\"", 1);
        assert!(matches!(parse(&json), Err(ManifestError::Invalid(_))));
        // not JSON
        assert!(matches!(parse("{"), Err(ManifestError::Json(_))));
    }

    #[test]
    fn passes_must_be_wired_in_order() {
        let two = r#"{
          "id": "blur", "version": 1, "kind": "effect",
          "params": [ { "key": "radius", "type": "float", "label": "Radius", "min": 0, "max": 64, "default": 0 } ],
          "derived": [ { "name": "inv_w", "expr": "1.0 / width" } ],
          "targets": [ { "name": "blurred", "scale": 1.0, "format": "rgba8" } ],
          "passes": [
            { "shader": "blur_h.wgsl", "inputs": ["src"], "output": "blurred" },
            { "shader": "blur_v.wgsl", "inputs": ["blurred"], "output": "out" }
          ]
        }"#;
        let m = parse(two).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m.passes.len(), 2);
        assert_eq!(m.passes[0].entry, "fs_main");
        // a target read before any pass writes it
        let bad = two.replacen(
            "{ \"shader\": \"blur_h.wgsl\", \"inputs\": [\"src\"], \"output\": \"blurred\" },",
            "{ \"shader\": \"blur_h.wgsl\", \"inputs\": [\"blurred\"], \"output\": \"blurred\" },",
            1,
        );
        assert!(matches!(parse(&bad), Err(ManifestError::Invalid(_))));
        // src in a transition is not available
        let trans = two.replacen("\"kind\": \"effect\"", "\"kind\": \"transition\"", 1);
        assert!(matches!(parse(&trans), Err(ManifestError::Invalid(_))));
    }

    #[test]
    fn transition_fields_have_defaults() {
        let json = r#"{
          "id": "crossfade", "version": 1, "kind": "transition",
          "passes": [ { "shader": "crossfade.wgsl", "inputs": ["from", "to"], "output": "out" } ]
        }"#;
        let m = parse(json).unwrap_or_else(|e| panic!("{e}"));
        let t = m.transition.unwrap_or_else(|| panic!("transition spec"));
        assert_eq!(t.default_duration_ms, 500);
        assert!(t.overlap);
        assert_eq!(t.easing, Easing::Linear);
    }

    #[test]
    fn resolve_params_applies_defaults_and_ranges() {
        let m = parse(GRAIN).unwrap_or_else(|e| panic!("{e}"));
        let mut given = BTreeMap::new();
        given.insert("intensity".to_string(), ParamValue::Number(7.0));
        let resolved = m.resolve_params(&given).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(resolved[0], ParamValue::Number(1.0));
        assert_eq!(resolved[1], ParamValue::Number(0.3));
        assert_eq!(resolved[2], ParamValue::Vector(vec![1.0, 1.0, 1.0, 1.0]));
        assert_eq!(resolved[3], ParamValue::Number(1.0));
        given.insert("tint".to_string(), ParamValue::Number(1.0));
        assert!(m.resolve_params(&given).is_err());
    }

    #[test]
    fn choice_and_lut_params() {
        let json = r#"{
          "id": "look", "version": 1, "kind": "filter",
          "params": [
            { "key": "mode", "type": "choice", "label": "Mode",
              "options": [ { "id": "soft", "label": "Soft" }, { "id": "hard", "label": "Hard" } ],
              "default": 1 },
            { "key": "lut", "type": "lut", "label": "LUT" }
          ],
          "passes": [ { "shader": "look.wgsl", "inputs": ["src", "lut"], "output": "out" } ]
        }"#;
        let m = parse(json).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(m.params[0].kind, ParamKind::Choice);
        assert_eq!(m.params[0].max, 1.0);
        assert_eq!(m.params[0].default, ParamValue::Number(1.0));
        assert_eq!(m.lut_params().count(), 1);
        let fields = m.uniform_fields();
        assert_eq!(fields, vec![("mode".to_string(), UniformTy::I32)]);
    }

    #[test]
    fn uniform_and_derived_values_evaluate() {
        let m = parse(GRAIN).unwrap_or_else(|e| panic!("{e}"));
        let controls = m
            .resolve_params(&BTreeMap::new())
            .unwrap_or_else(|e| panic!("{e}"));
        let host = HostValues {
            width: 320,
            height: 180,
            time: 0.0,
            duration: 1.0,
            progress: 0.0,
            frame: 0,
            seed: 0,
            speed: 1.0,
        };
        let uniforms = m
            .evaluate_uniforms(&controls, &host)
            .unwrap_or_else(|e| panic!("{e}"));
        // intensity, size (mapped), tint (vec4), animated (bool), grain_speed (derived)
        assert_eq!(uniforms.len(), 5);
        match uniforms[1] {
            // size control 0.3 -> mix(0.5, 4.0, 0.09)
            UniformValue::F32(v) => assert!((v - (0.5 + 3.5 * 0.09) as f32).abs() < 1e-6),
            ref other => panic!("unexpected {other:?}"),
        }
        match uniforms[4] {
            UniformValue::F32(v) => assert_eq!(v, 24.0),
            ref other => panic!("unexpected {other:?}"),
        }
    }
}
