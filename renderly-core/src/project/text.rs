//! Text model (schema v8, docs/project-schema.md "Captions and text"): the body shared by
//! caption clips and text clips, style and animation references, project transcripts and
//! timeline markers. Pure data plus validation; rendering is in [`crate::captions`].
//!
//! Status: styles resolve to the four builtin caption styles (and installed format-1 pack
//! caption styles) rendered by the existing caption renderer. `animations`, `runs`, and
//! style `params` / most `overrides` are stored, validated and round-tripped, and are rendered
//! from the captions phase (docs/text-format.md).

use super::Id;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The style a v7 caption without a style falls back to, and the default for new text.
pub const DEFAULT_CAPTION_STYLE_REF: &str = "builtin:tiktok-bold-yellow@1";
/// Default style of a new text clip.
pub const DEFAULT_TEXT_STYLE_REF: &str = "builtin:tiktok-minimal@1";

/// Style property names an `overrides` map may carry (docs/text-format.md `text_style` items).
/// Only `fill`, `size` and `anchor` change the picture today.
pub const STYLE_OVERRIDE_KEYS: &[&str] = &[
    "font",
    "size",
    "fill",
    "strokes",
    "shadows",
    "background",
    "letter_spacing",
    "line_height",
    "align",
    "case",
    "max_width",
    "emphasis",
    "anchor",
    "margin",
];

/// Animation phases (docs/text-format.md "Phases").
pub const ANIMATION_PHASES: &[&str] = &["in", "out", "loop", "active"];

/// One range of the text with its own style properties. `start`/`end` index grapheme clusters
/// of `text`, end-exclusive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextRun {
    pub start: u32,
    pub end: u32,
    /// Style properties for the range (same vocabulary as a style's `overrides`).
    #[serde(default)]
    pub style: BTreeMap<String, Value>,
}

/// One recognised word. Times are relative to the clip start and come from speech
/// recognition; editing `text` never rewrites them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct CaptionWord {
    pub text: String,
    pub start_us: i64,
    pub end_us: i64,
    /// Key word; styles can highlight it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub emphasis: bool,
    /// Id of the transcript word this came from (`transcripts[].words[].id`), when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_id: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// What is displayed, plus the recognition evidence behind it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextBody {
    pub text: String,
    #[serde(default)]
    pub runs: Vec<TextRun>,
    #[serde(default)]
    pub words: Vec<CaptionWord>,
}

impl TextBody {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            runs: Vec::new(),
            words: Vec::new(),
        }
    }
}

/// A reference to an installed or builtin text style, with its knobs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextStyleRef {
    /// `builtin:<id>@<v>` or `pack:<pack>/<item>@<v>`.
    #[serde(rename = "ref")]
    pub item_ref: String,
    /// Content hash of the pack item when it was applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Values for the style's declared knobs.
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// Style properties set on this clip (or track) on top of the style.
    #[serde(default)]
    pub overrides: BTreeMap<String, Value>,
}

impl TextStyleRef {
    pub fn new(item_ref: impl Into<String>) -> Self {
        Self {
            item_ref: item_ref.into(),
            hash: None,
            params: BTreeMap::new(),
            overrides: BTreeMap::new(),
        }
    }

    /// The v8 ref for a v7 `style_id`.
    pub fn builtin(style_id: &str) -> Self {
        Self::new(format!("builtin:{style_id}@1"))
    }

    /// The style id the existing caption renderer resolves (`tiktok-box`, or a format-1 pack
    /// caption style id): the item part of the ref, without pack, prefix or version.
    pub fn render_style_id(&self) -> String {
        render_style_id(&self.item_ref)
    }
}

/// See [`TextStyleRef::render_style_id`].
pub fn render_style_id(item_ref: &str) -> String {
    let body = item_ref
        .strip_prefix("builtin:")
        .or_else(|| item_ref.strip_prefix("pack:"))
        .unwrap_or(item_ref);
    let item = body.rsplit('/').next().unwrap_or(body);
    item.split('@').next().unwrap_or(item).to_string()
}

/// One animation on a phase.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextAnimationRef {
    #[serde(rename = "ref")]
    pub item_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// Overrides the item's natural duration (`in` / `out`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<i64>,
}

/// At most one animation per phase.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TextAnimations {
    #[serde(rename = "in", default, skip_serializing_if = "Option::is_none")]
    pub in_: Option<TextAnimationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub out: Option<TextAnimationRef>,
    #[serde(rename = "loop", default, skip_serializing_if = "Option::is_none")]
    pub loop_: Option<TextAnimationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<TextAnimationRef>,
}

impl TextAnimations {
    pub fn is_empty(&self) -> bool {
        self.in_.is_none() && self.out.is_none() && self.loop_.is_none() && self.active.is_none()
    }

    pub fn get(&self, phase: &str) -> Option<&TextAnimationRef> {
        match phase {
            "in" => self.in_.as_ref(),
            "out" => self.out.as_ref(),
            "loop" => self.loop_.as_ref(),
            "active" => self.active.as_ref(),
            _ => None,
        }
    }

    /// Set or clear one phase; false when `phase` is not a phase name.
    pub fn set(&mut self, phase: &str, value: Option<TextAnimationRef>) -> bool {
        match phase {
            "in" => self.in_ = value,
            "out" => self.out = value,
            "loop" => self.loop_ = value,
            "active" => self.active = value,
            _ => return false,
        }
        true
    }

    /// `(phase, animation)` for every phase that has one, in phase order.
    pub fn entries(&self) -> Vec<(&'static str, &TextAnimationRef)> {
        ANIMATION_PHASES
            .iter()
            .filter_map(|p| self.get(p).map(|a| (*p, a)))
            .collect()
    }
}

/// Where a caption's words came from in a project transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TranscriptRef {
    pub transcript_id: Id,
    pub first_word: String,
    pub last_word: String,
}

/// A caption (caption track, shown over the picture).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptionClip {
    pub id: Id,
    pub start_us: i64,
    pub duration_us: i64,
    pub body: TextBody,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_ref: Option<TranscriptRef>,
    /// `None` inherits the track's `caption_style`.
    #[serde(default)]
    pub style: Option<TextStyleRef>,
    #[serde(default)]
    pub animations: TextAnimations,
}

impl CaptionClip {
    /// With its own style: `builtin:<style_id>@1`.
    pub fn with_style(mut self, style_id: &str) -> Self {
        self.style = Some(TextStyleRef::builtin(style_id));
        self
    }

    pub fn new(start_us: i64, duration_us: i64, text: impl Into<String>) -> Self {
        Self {
            id: Id::new_v4(),
            start_us,
            duration_us,
            body: TextBody::plain(text),
            transcript_ref: None,
            style: None,
            animations: TextAnimations::default(),
        }
    }
}

/// A title on a video track: a layer like a media clip whose picture is rendered text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextClip {
    pub id: Id,
    pub start_us: i64,
    pub duration_us: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub body: TextBody,
    #[serde(default = "default_text_style")]
    pub style: TextStyleRef,
    #[serde(default)]
    pub animations: TextAnimations,
    #[serde(default)]
    pub transform: super::ClipTransform,
    #[serde(default)]
    pub keyframes: Vec<super::KeyframeTrack>,
    /// Slots in render order: `filter`, `adjust`, then `effects[]` (no `key` slot).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<super::EffectInstance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjust: Option<super::EffectInstance>,
    #[serde(default)]
    pub effects: Vec<super::EffectInstance>,
    #[serde(default, skip_serializing_if = "super::BlendMode::is_normal")]
    pub blend_mode: super::BlendMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mask: Option<super::ClipMask>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outgoing_transition: Option<super::ClipTransition>,
}

fn default_true() -> bool {
    true
}

fn default_text_style() -> TextStyleRef {
    let mut style = TextStyleRef::new(DEFAULT_TEXT_STYLE_REF);
    style
        .overrides
        .insert("anchor".into(), Value::String("center".into()));
    style
}

impl TextClip {
    pub fn new(start_us: i64, duration_us: i64, body: TextBody) -> Self {
        Self {
            id: Id::new_v4(),
            start_us,
            duration_us,
            enabled: true,
            body,
            style: default_text_style(),
            animations: TextAnimations::default(),
            transform: super::ClipTransform::default(),
            keyframes: Vec::new(),
            filter: None,
            adjust: None,
            effects: Vec::new(),
            blend_mode: super::BlendMode::Normal,
            mask: None,
            outgoing_transition: None,
        }
    }

    /// Every effect instance in render order: filter, adjust, then `effects[]`.
    pub fn effect_stack(&self) -> Vec<&super::EffectInstance> {
        self.filter
            .iter()
            .chain(self.adjust.iter())
            .chain(self.effects.iter())
            .collect()
    }

    /// `(slot name, instance)` in render order.
    pub fn slot_entries(&self) -> Vec<(&'static str, &super::EffectInstance)> {
        let mut out: Vec<(&'static str, &super::EffectInstance)> = Vec::new();
        out.extend(self.filter.iter().map(|e| ("filter", e)));
        out.extend(self.adjust.iter().map(|e| ("adjust", e)));
        out.extend(self.effects.iter().map(|e| ("effects", e)));
        out
    }

    /// Mutable access to the slots (no `key` on a text clip).
    pub fn slots_mut(&mut self) -> super::SlotsMut<'_> {
        super::SlotsMut {
            filter: &mut self.filter,
            key: None,
            adjust: &mut self.adjust,
            effects: &mut self.effects,
            blend_mode: &mut self.blend_mode,
        }
    }

    /// Give every effect instance a fresh id. Returns `old -> new`.
    pub fn reid_effects(&mut self) -> std::collections::HashMap<Id, Id> {
        let mut remap = std::collections::HashMap::new();
        super::reid_slots(self.slots_mut(), &mut remap);
        remap
    }
}

/// A recognised word of a media item, in **source media time**.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TranscriptWord {
    /// Stable id within the transcript (`w-1`, `w-2`, ...).
    pub id: String,
    pub text: String,
    pub start_us: i64,
    pub end_us: i64,
    /// Removed from the edit; `ApplyTranscriptCuts` turns deleted ranges into cuts.
    #[serde(default)]
    pub deleted: bool,
    /// Detection tags: `filler`, `pause`, `repeat`.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// The words of one media item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Transcript {
    pub id: Id,
    pub media_id: Id,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub words: Vec<TranscriptWord>,
}

impl Transcript {
    pub fn find_word(&self, id: &str) -> Option<&TranscriptWord> {
        self.words.iter().find(|w| w.id == id)
    }
}

/// `user` markers are placed by hand; `beat` markers come from beat detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MarkerKind {
    #[default]
    User,
    Beat,
}

impl MarkerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MarkerKind::User => "user",
            MarkerKind::Beat => "beat",
        }
    }
}

/// A named point on the root timeline. Snapping includes markers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Marker {
    pub id: Id,
    pub time_us: i64,
    #[serde(default)]
    pub label: String,
    /// `#RRGGBB`; empty = the default marker colour.
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub kind: MarkerKind,
}

/// True when `item_ref` has the `builtin:<id>@<v>` / `pack:<pack>/<item>@<v>` shape.
pub fn check_item_ref(item_ref: &str) -> Result<(), String> {
    crate::packs::refs::ItemRef::parse(item_ref)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn check_value_tree(path: &str, v: &Value) -> Result<(), String> {
    match v {
        Value::Number(n) => {
            if n.as_f64().is_some_and(|f| !f.is_finite()) {
                return Err(format!("{path}: numbers must be finite"));
            }
            Ok(())
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                check_value_tree(&format!("{path}[{i}]"), item)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for (k, item) in map {
                check_value_tree(&format!("{path}.{k}"), item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// True for `#RRGGBB` and `#RRGGBBAA`.
pub fn is_hex_color(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    (hex.len() == 6 || hex.len() == 8) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Parse `#RRGGBB[AA]` into RGBA bytes.
pub fn parse_hex_color(s: &str) -> Option<[u8; 4]> {
    if !is_hex_color(s) {
        return None;
    }
    let hex = &s[1..];
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some([
        byte(0)?,
        byte(2)?,
        byte(4)?,
        if hex.len() == 8 { byte(6)? } else { 255 },
    ])
}

/// Validate a style property map (`overrides` or a run's `style`): known keys, finite numbers,
/// and the types of the properties the renderer reads today.
pub fn validate_style_props(what: &str, props: &BTreeMap<String, Value>) -> Result<(), String> {
    for (key, value) in props {
        if !STYLE_OVERRIDE_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "{what}: unknown style property '{key}' (known: {})",
                STYLE_OVERRIDE_KEYS.join(", ")
            ));
        }
        check_value_tree(&format!("{what}.{key}"), value)?;
        match key.as_str() {
            "fill" => {
                if let Value::String(s) = value {
                    if !is_hex_color(s) {
                        return Err(format!(
                            "{what}.fill: '{s}' is not a colour (#RRGGBB or #RRGGBBAA)"
                        ));
                    }
                }
            }
            "size" | "letter_spacing" | "line_height" | "max_width" | "margin" => {
                match value.as_f64() {
                    Some(n) if n >= 0.0 => {}
                    _ => return Err(format!("{what}.{key}: expected a number >= 0")),
                }
            }
            "anchor" if !matches!(value.as_str(), Some("top" | "center" | "bottom")) => {
                return Err(format!("{what}.anchor: expected top, center or bottom"));
            }
            "align" if !matches!(value.as_str(), Some("left" | "center" | "right")) => {
                return Err(format!("{what}.align: expected left, center or right"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Validate a style ref: ref syntax, params, overrides. Params of a known builtin are checked
/// against its (empty) declaration; packs' declarations are checked when the style kind lands.
pub fn validate_style_ref(what: &str, style: &TextStyleRef) -> Result<(), String> {
    check_item_ref(&style.item_ref).map_err(|e| format!("{what}: {e}"))?;
    for (k, v) in &style.params {
        if k.trim().is_empty() {
            return Err(format!("{what}: empty param key"));
        }
        check_value_tree(&format!("{what}.params.{k}"), v)?;
    }
    if is_known_builtin_style(&style.item_ref) && !style.params.is_empty() {
        return Err(format!(
            "{what}: '{}' declares no params (got {})",
            style.item_ref,
            style.params.keys().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    validate_style_props(&format!("{what}.overrides"), &style.overrides)
}

/// True for `builtin:<id>@1` where `<id>` is one of the builtin caption styles.
pub fn is_known_builtin_style(item_ref: &str) -> bool {
    match crate::packs::refs::ItemRef::parse(item_ref) {
        Ok(crate::packs::refs::ItemRef::Builtin { id, version }) => {
            version == 1 && crate::captions::BUILTIN_STYLES.contains(&id.as_str())
        }
        _ => false,
    }
}

/// Validate one animation: ref syntax, params, duration.
pub fn validate_animation(what: &str, phase: &str, anim: &TextAnimationRef) -> Result<(), String> {
    if !ANIMATION_PHASES.contains(&phase) {
        return Err(format!(
            "{what}: unknown phase '{phase}' (in, out, loop, active)"
        ));
    }
    check_item_ref(&anim.item_ref).map_err(|e| format!("{what}.{phase}: {e}"))?;
    for (k, v) in &anim.params {
        check_value_tree(&format!("{what}.{phase}.params.{k}"), v)?;
    }
    if let Some(d) = anim.duration_us {
        if d <= 0 {
            return Err(format!("{what}.{phase}.duration_us must be > 0"));
        }
    }
    Ok(())
}

/// Validate a set of animations.
pub fn validate_animations(what: &str, animations: &TextAnimations) -> Result<(), String> {
    for (phase, anim) in animations.entries() {
        validate_animation(what, phase, anim)?;
    }
    Ok(())
}

/// Validate a body: runs index into the text, words are ordered inside `duration_us` (when
/// given).
pub fn validate_body(what: &str, body: &TextBody, duration_us: Option<i64>) -> Result<(), String> {
    // Runs count grapheme clusters; the code point count is an upper bound for it, so a run
    // past it is always wrong. The exact bound is enforced where shaping splits graphemes.
    let max = body.text.chars().count() as u32;
    for (i, run) in body.runs.iter().enumerate() {
        if run.start >= run.end {
            return Err(format!(
                "{what}.runs[{i}]: start ({}) must be < end ({})",
                run.start, run.end
            ));
        }
        if run.end > max {
            return Err(format!(
                "{what}.runs[{i}]: end ({}) is past the end of the text ({max} characters)",
                run.end
            ));
        }
        validate_style_props(&format!("{what}.runs[{i}].style"), &run.style)?;
    }
    let mut prev_end = 0_i64;
    for (i, w) in body.words.iter().enumerate() {
        if w.start_us < 0 || w.end_us < w.start_us {
            return Err(format!(
                "{what}.words[{i}] '{}': needs 0 <= start_us <= end_us (got {}..{})",
                w.text, w.start_us, w.end_us
            ));
        }
        if w.start_us < prev_end {
            return Err(format!(
                "{what}.words[{i}] '{}' starts at {} before the previous word ends ({prev_end}); words must be in order and not overlap",
                w.text, w.start_us
            ));
        }
        if let Some(d) = duration_us {
            if w.end_us > d {
                return Err(format!(
                    "{what}.words[{i}] '{}' ends at {} after the clip ends ({d}); word times are relative to the clip start",
                    w.text, w.end_us
                ));
            }
        }
        prev_end = w.end_us;
    }
    Ok(())
}

impl super::Project {
    /// The style a caption clip is drawn with: its own, else its track's `caption_style`, else
    /// the default. `track` is the caption track holding the clip.
    pub fn caption_style_for<'a>(
        track: &'a super::Track,
        clip: &'a CaptionClip,
    ) -> std::borrow::Cow<'a, TextStyleRef> {
        match clip.style.as_ref().or(track.caption_style.as_ref()) {
            Some(s) => std::borrow::Cow::Borrowed(s),
            None => std::borrow::Cow::Owned(TextStyleRef::new(DEFAULT_CAPTION_STYLE_REF)),
        }
    }

    pub fn find_transcript(&self, id: Id) -> Option<&Transcript> {
        self.transcripts.iter().find(|t| t.id == id)
    }

    /// Times other edits snap to besides the playhead: clip edges on the root timeline and
    /// markers, sorted and de-duplicated.
    pub fn snap_points(&self) -> Vec<i64> {
        let mut points: Vec<i64> = Vec::new();
        for track in &self.tracks {
            for clip in &track.clips {
                points.push(clip.start_us());
                points.push(clip.end_us());
            }
        }
        points.extend(self.markers.iter().map(|m| m.time_us));
        points.sort_unstable();
        points.dedup();
        points
    }

    /// Validate the project-level text data: transcripts, markers.
    pub fn validate_text(&self) -> Result<(), super::ProjectError> {
        use super::ProjectError;
        let mut ids = std::collections::HashSet::new();
        for t in &self.transcripts {
            if !ids.insert(t.id) {
                return Err(ProjectError::DuplicateTranscriptId(t.id));
            }
            let mut word_ids = std::collections::HashSet::new();
            for w in &t.words {
                if !word_ids.insert(w.id.as_str()) {
                    return Err(ProjectError::DuplicateTranscriptWord(t.id, w.id.clone()));
                }
            }
        }
        let mut marker_ids = std::collections::HashSet::new();
        for m in &self.markers {
            if !marker_ids.insert(m.id) {
                return Err(ProjectError::DuplicateMarkerId(m.id));
            }
        }
        Ok(())
    }
}
