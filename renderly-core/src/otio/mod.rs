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
/// `generator_kind` of the `GeneratorReference` used for text clips (titles on video tracks).
pub const TEXT_GENERATOR_KIND: &str = "renderly.text";

/// The OTIO marker colours, with the RGB each stands for (nearest match wins when a Renderly
/// marker colour is written; the exact colour is kept in the marker's metadata).
const MARKER_COLORS: &[(&str, [u8; 3])] = &[
    ("PINK", [0xff, 0x69, 0xb4]),
    ("RED", [0xe5, 0x39, 0x35]),
    ("ORANGE", [0xff, 0x98, 0x00]),
    ("YELLOW", [0xff, 0xeb, 0x3b]),
    ("GREEN", [0x43, 0xa0, 0x47]),
    ("CYAN", [0x00, 0xbc, 0xd4]),
    ("BLUE", [0x1e, 0x88, 0xe5]),
    ("PURPLE", [0x8e, 0x24, 0xaa]),
    ("MAGENTA", [0xd8, 0x1b, 0x60]),
    ("BLACK", [0x00, 0x00, 0x00]),
    ("WHITE", [0xff, 0xff, 0xff]),
];

/// OTIO marker colour name for a Renderly marker colour (`#RRGGBB`); empty = the default for
/// the kind (red for user markers, cyan for beats).
pub(crate) fn otio_marker_color(hex: &str, kind: crate::project::MarkerKind) -> &'static str {
    let Some([r, g, b, _]) = crate::project::text::parse_hex_color(hex) else {
        return match kind {
            crate::project::MarkerKind::User => "RED",
            crate::project::MarkerKind::Beat => "CYAN",
        };
    };
    let dist = |c: &[u8; 3]| {
        let d = |a: u8, b: u8| (i32::from(a) - i32::from(b)).pow(2);
        d(r, c[0]) + d(g, c[1]) + d(b, c[2])
    };
    MARKER_COLORS
        .iter()
        .min_by_key(|(_, c)| dist(c))
        .map_or("RED", |(n, _)| n)
}

/// Renderly marker colour (`#RRGGBB`) for an OTIO colour name; unknown names give `""`.
pub(crate) fn marker_color_from_otio(name: &str) -> String {
    MARKER_COLORS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, c)| format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]))
        .unwrap_or_default()
}

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
