//! OpenTimelineIO (`.otio`) interop — pure Rust, no dependency on the OTIO C++/Python
//! libraries. Spec and mapping tables: docs/otio.md.
//!
//! * [`export_otio`] writes a project as an OTIO `Timeline`; everything OTIO cannot express is
//!   carried under `metadata["renderly"]` so Renderly → OTIO → Renderly is lossless.
//! * [`import_otio`] builds a **new** [`Project`] from any OTIO file (ours or a foreign one,
//!   e.g. from Premiere/Resolve/Hiero exports) and reports what it could not carry over as
//!   [`OtioWarning`]s — unknown schemas and fields are tolerated, never a hard error.
//!
//! Importing is a load-path operation ("create a project from a file"), not a `Command`: it
//! does not mutate an open project (see docs/command-api.md next to `Export`).

use crate::project::ProjectError;
use serde::Serialize;
use std::path::PathBuf;

mod export;
mod import;
pub mod time;
pub mod url;

pub use export::{export_otio, export_otio_with_options};
pub use import::import_otio;
#[cfg(not(target_arch = "wasm32"))]
pub use import::probe_media;

/// Version of the `metadata["renderly"]` layout this build writes.
pub const RENDERLY_METADATA_FORMAT: u32 = 1;
/// Key under which Renderly stores its own data on OTIO objects.
pub const RENDERLY_METADATA_KEY: &str = "renderly";
/// `generator_kind` of the `GeneratorReference` used for caption clips.
pub const CAPTION_GENERATOR_KIND: &str = "renderly.caption";

/// Why an OTIO export or import failed outright. Anything recoverable is an [`OtioWarning`]
/// instead.
#[derive(Debug, thiserror::Error)]
pub enum OtioError {
    #[error("parsing OTIO JSON: {0}")]
    Parse(serde_json::Error),
    #[error("not an OpenTimelineIO timeline: {0}")]
    NotATimeline(String),
    #[error("invalid OTIO document: {0}")]
    Invalid(String),
    #[error("project frame rate {0} cannot be written as an OTIO rate")]
    BadRate(f64),
    #[error("writing OTIO JSON: {0}")]
    Serialize(serde_json::Error),
    #[error("the imported project is invalid: {0}")]
    InvalidProject(#[from] ProjectError),
}

/// What a warning is about; stable ids so callers can filter them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    /// A schema (or schema version) this build does not know; the object was skipped or
    /// treated as a gap.
    UnknownSchema,
    /// A clip without usable media (`MissingReference`, an unsupported reference type, a
    /// non-`file:` URL); imported as a gap so every later item keeps its position.
    MissingMedia,
    /// An OTIO effect with no Renderly equivalent; dropped.
    UnsupportedEffect,
    /// A time warp Renderly cannot express (`FreezeFrame`, `time_scalar` of 0).
    TimeWarp,
    /// A time scalar outside Renderly's `0.25..=4` speed range; clamped.
    SpeedClamped,
    /// A transition that was approximated, shortened or dropped.
    Transition,
    /// Markers are not imported.
    MarkersIgnored,
    /// A track trimmed or offset by its own `source_range` (content moved or cut).
    TrackTrimmed,
    /// Frame-rate choice, mixed rates, or an unusable rate.
    Rate,
    /// Anything else (global start time, nesting depth, clamped media ranges, ...).
    Other,
}

/// A non-fatal observation made while importing.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OtioWarning {
    pub kind: WarningKind,
    /// Where in the document, e.g. `tracks.children[1].children[4]`.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for OtioWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Options for [`export_otio_with_options`].
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Directory relative media paths are resolved against (normally the project file's
    /// directory) so every `target_url` is an absolute `file://` URL. Without it a relative
    /// media path is written as a relative URL reference.
    pub base_dir: Option<PathBuf>,
}

/// Options for [`import_otio`].
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Directory relative `target_url`s are resolved against (the `.otio` file's directory).
    pub base_dir: Option<PathBuf>,
    /// Project width when the file carries no Renderly metadata (default 1920).
    pub width: Option<u32>,
    /// Project height when the file carries no Renderly metadata (default 1080).
    pub height: Option<u32>,
    /// Force the project frame rate instead of choosing the dominant one.
    pub fps: Option<f64>,
    /// Audio sample rate when the file carries no Renderly metadata (default 48000).
    pub sample_rate: Option<u32>,
    /// Project name; defaults to the timeline's name.
    pub name: Option<String>,
}
