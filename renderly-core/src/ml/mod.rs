//! Local ML inference infrastructure (native only).
//!
//! * [`runtime`] — thin wrapper over `tract` (pure-Rust ONNX): load, pin shapes, optimise once
//!   per process, run `f32` tensors.
//! * [`models`] — manifest of the models Renderly knows about (licence, pinned source, SHA-256),
//!   resolution (env override -> bundled -> per-user cache) and the one explicit, checksum-
//!   verifying download entry point. Nothing here downloads implicitly.
//! * [`yunet`] — YuNet face detector built on the two modules above.
//!
//! This module is compiled out of the wasm32 build (see `lib.rs`): the browser compositor never
//! runs models.

pub mod models;
pub mod runtime;
pub mod yunet;

pub use models::{
    download_model, model_cache_dir, resolve_model, ModelError, ModelSource, ModelSpec,
    DEFAULT_MODEL_DIR_ENV, MODELS,
};
pub use runtime::{load_cached_plan, F32Tensor, InputFact, MlError, Plan};
pub use yunet::{Face, FaceDetector};
