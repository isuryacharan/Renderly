//! Effect runtime for pack format 2: one generic path that runs every effect, filter and
//! transition as data (`effect.json` + WGSL + textures), the way the design in
//! `docs/effect-pack-format.md` specifies.
//!
//! Modules:
//!
//! - [`manifest`]: `effect.json` types, parsing, validation, parameter resolution and uniform
//!   evaluation. Unknown kinds, features, pass types and param types yield
//!   [`manifest::ManifestError::NeedsNewer`], which callers treat as "installed but disabled".
//! - [`expr`]: the shaping-expression language (`expr.v1`): parser, type checker, evaluator.
//! - [`prelude`]: the generated WGSL prelude (host block, `Params` struct with WGSL layout,
//!   bindings per input) and the uniform byte encoding.
//! - [`executor`]: [`executor::CompiledEffect`] (load-time compilation: manifest, expressions,
//!   prelude, naga validation) and [`executor::EffectExecutor`] (multi-pass wgpu execution with
//!   pooled targets; seek-mode, no state across frames).
//! - [`lut`]: `.cube` LUTs as 3D textures.
//!
//! Nothing here touches the filesystem or decodes images: manifests, WGSL and decoded pixels
//! come in as strings and buffers, so the module builds for wasm32 as well.
//!
//! The live compositor runs every builtin through this module: `compose::effects` and
//! `compose::transition` resolve the project's ids onto the items in `compose::builtins`.

pub mod executor;
pub mod expr;
pub mod lut;
pub mod manifest;
pub mod prelude;

pub use executor::{CompiledEffect, DecodedImage, EffectExecutor, EffectInputs, EffectOutput};
pub use manifest::{EffectManifest, ItemKind, ManifestError, FEATURES};

use thiserror::Error;

/// A resolved control value of one param, as the project stores it (`min..max` slider
/// positions for scalars, components for vectors, a LUT reference for LUT params).
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    Number(f64),
    Vector(Vec<f64>),
    Lut(String),
}

/// Per-frame values every expression and pass can see.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostValues {
    /// Size of the effect's input frame in pixels.
    pub width: u32,
    pub height: u32,
    /// Seconds since the effect started, speed-adjusted.
    pub time: f64,
    pub duration: f64,
    /// Transitions: eased 0..1. Effects: `time / duration`, supplied by the caller.
    pub progress: f64,
    pub frame: u32,
    /// Stable per effect instance.
    pub seed: u32,
    pub speed: f64,
}

impl HostValues {
    /// Expression host variables in the order `t, duration, progress, width, height, frame,
    /// speed`.
    pub(crate) fn vars(&self) -> [f64; 7] {
        [
            self.time,
            self.duration,
            self.progress,
            f64::from(self.width),
            f64::from(self.height),
            f64::from(self.frame),
            self.speed,
        ]
    }
}

#[derive(Debug, Error)]
pub enum EffectError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("invalid parameter: {0}")]
    Param(String),
    #[error("item file '{0}' is missing")]
    MissingFile(String),
    #[error("texture '{name}': {message}")]
    Texture { name: String, message: String },
    #[error("shader for pass {pass}: {message}")]
    Shader { pass: usize, message: String },
    #[error("input '{0}' is not bound")]
    MissingInput(String),
    #[error("LUT: {0}")]
    Lut(String),
    #[error("limit exceeded: {0}")]
    Limit(String),
    #[error("GPU error: {0}")]
    Gpu(String),
}
