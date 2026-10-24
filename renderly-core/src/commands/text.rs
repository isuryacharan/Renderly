//! Text, caption, marker and transcript commands (schema v8): `AddCaption` / `SetCaption`,
//! `AddText` and the `SetText*` family, `SetTrackCaptionStyle`, `SetCaptionWords`,
//! `SetWordEmphasis`, `SplitCaptions`, `AddMarkers` / `RemoveMarkers`, `ApplyTranscriptCuts` /
//! `RestoreTranscriptWords`. See docs/command-api.md "Text and captions".
//!
//! Every command validates before it changes anything, so a failed command leaves the project
//! as it was, and a batch of `targets` applies to all clips or to none.

use super::{check_no_overlap, magnetic, snap_dur, CommandError, CommandOutcome, TextClipRef};
use crate::project::text::{
    validate_animation, validate_animations, validate_body, validate_style_ref,
};
use crate::project::{
    CaptionClip, CaptionWord, Clip, Id, Marker, MarkerKind, Project, TextAnimationRef,
    TextAnimations, TextBody, TextClip, TextRun, TextStyleRef, TrackKind, TranscriptRef,
};
use crate::time::{Fps, Time};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

fn invalid(message: impl Into<String>) -> CommandError {
    CommandError::InvalidText(message.into())
}

/// A word as a command takes it: times are relative to the clip start and use the `Time`
/// grammar. They are stored as given (recognition evidence is not snapped to frames).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WordInput {
    pub text: String,
    pub start: Time,
    pub end: Time,
    #[serde(default)]
    pub emphasis: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub word_id: Option<String>,
}

impl WordInput {
    fn resolve(&self, fps: Fps) -> CaptionWord {
        CaptionWord {
            text: self.text.clone(),
            start_us: self.start.resolve(fps),
            end_us: self.end.resolve(fps),
            emphasis: self.emphasis,
            word_id: self.word_id.clone(),
        }
    }
}

/// The body of a caption or text as a command takes it. `runs` and `words` omitted = empty on
/// a new clip, unchanged on an existing one.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct BodyInput {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<TextRun>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<Vec<WordInput>>,
}

impl BodyInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            runs: None,
            words: None,
        }
    }

    fn into_body(self, fps: Fps) -> TextBody {
        TextBody {
            text: self.text,
            runs: self.runs.unwrap_or_default(),
            words: self
                .words
                .unwrap_or_default()
                .iter()
                .map(|w| w.resolve(fps))
                .collect(),
        }
    }

    /// Apply onto an existing body: text always; runs / words only when given.
    fn apply_to(self, body: &mut TextBody, fps: Fps) {
        body.text = self.text;
        if let Some(runs) = self.runs {
            body.runs = runs;
        }
        if let Some(words) = self.words {
            body.words = words.iter().map(|w| w.resolve(fps)).collect();
        }
    }
}

/// A marker to add. `id` is generated when omitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct MarkerInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub time: Time,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub kind: MarkerKind,
}

fn check_style(what: &str, style: &TextStyleRef) -> Result<(), CommandError> {
    validate_style_ref(what, style).map_err(invalid)
}

fn caption_track_mut(
    project: &mut Project,
    track_id: Id,
) -> Result<&mut crate::project::Track, CommandError> {
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Caption {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Caption,
        ));
    }
    Ok(track)
}

// ---- captions -------------------------------------------------------------------------

pub(super) fn add_caption(
    project: &mut Project,
    track_id: Id,
    body: BodyInput,
    start_us: i64,
    duration_us: i64,
    style: Option<TextStyleRef>,
    animations: Option<TextAnimations>,
) -> Result<CommandOutcome, CommandError> {
    let fps = project.settings.fps;
    let track = caption_track_mut(project, track_id)?;
    if duration_us <= 0 {
        return Err(CommandError::InvalidDuration(duration_us));
    }
    check_no_overlap(track, start_us, duration_us, None)?;
    let body = body.into_body(fps);
    validate_body("body", &body, Some(duration_us)).map_err(invalid)?;
    if let Some(s) = &style {
        check_style("style", s)?;
    }
    let animations = animations.unwrap_or_default();
    validate_animations("animations", &animations).map_err(invalid)?;

    let clip_id = Id::new_v4();
    let mut clip = CaptionClip::new(start_us, duration_us, "");
    clip.id = clip_id;
    clip.body = body;
    clip.style = style;
    clip.animations = animations;
    track.clips.push(Clip::Caption(clip));
    Ok(CommandOutcome::ClipAdded { clip_id })
}

/// `SetCaption`: edit one caption's body, timing, style or animations. `style: null` makes
/// the caption inherit the track's `caption_style` again.
#[allow(clippy::too_many_arguments)]
pub(super) fn set_caption(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    body: Option<BodyInput>,
    start_us: Option<i64>,
    duration_us: Option<i64>,
    style: Option<Option<TextStyleRef>>,
    animations: Option<TextAnimations>,
) -> Result<CommandOutcome, CommandError> {
    if body.is_none()
        && start_us.is_none()
        && duration_us.is_none()
        && style.is_none()
        && animations.is_none()
    {
        return Err(CommandError::SetCaptionRequiresChange);
    }
    let fps = project.settings.fps;
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Caption {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Caption,
        ));
    }
    // Resolve the existing clip first: a missing id is ClipNotFound, not a misleading overlap.
    let existing = track
        .find_clip(clip_id)
        .ok_or(CommandError::ClipNotFound(clip_id, track_id))?;
    let Clip::Caption(existing) = existing else {
        return Err(CommandError::ClipNotFound(clip_id, track_id));
    };
    let new_start = start_us.unwrap_or(existing.start_us);
    let new_duration = duration_us.unwrap_or(existing.duration_us);
    if new_duration <= 0 {
        return Err(CommandError::InvalidDuration(new_duration));
    }
    check_no_overlap(track, new_start, new_duration, Some(clip_id))?;

    let mut new_body = existing.body.clone();
    if let Some(b) = body {
        b.apply_to(&mut new_body, fps);
    }
    validate_body("body", &new_body, Some(new_duration)).map_err(invalid)?;
    if let Some(Some(s)) = &style {
        check_style("style", s)?;
    }
    if let Some(a) = &animations {
        validate_animations("animations", a).map_err(invalid)?;
    }

    let Some(Clip::Caption(c)) = project
        .find_track_mut(track_id)
        .and_then(|t| t.clips.iter_mut().find(|c| c.id() == clip_id))
    else {
        return Err(CommandError::ClipNotFound(clip_id, track_id));
    };
    c.body = new_body;
    c.start_us = new_start;
    c.duration_us = new_duration;
    if let Some(s) = style {
        c.style = s;
    }
    if let Some(a) = animations {
        c.animations = a;
    }
    Ok(CommandOutcome::Applied)
}

// ---- text clips -----------------------------------------------------------------------

pub(super) fn add_text(
    project: &mut Project,
    track_id: Id,
    body: BodyInput,
    start_us: i64,
    duration_us: i64,
    style: Option<TextStyleRef>,
    animations: Option<TextAnimations>,
) -> Result<CommandOutcome, CommandError> {
    let fps = project.settings.fps;
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if track.kind != TrackKind::Video {
        return Err(CommandError::TrackKindMismatch(
            track_id,
            track.kind,
            TrackKind::Video,
        ));
    }
    if duration_us <= 0 {
        return Err(CommandError::InvalidDuration(duration_us));
    }
    let magnetic = track.magnetic;
    if !magnetic {
        check_no_overlap(track, start_us, duration_us, None)?;
    }
    let body = body.into_body(fps);
    validate_body("body", &body, Some(duration_us)).map_err(invalid)?;
    if let Some(s) = &style {
        check_style("style", s)?;
    }
    let animations = animations.unwrap_or_default();
    validate_animations("animations", &animations).map_err(invalid)?;

    let mut clip = TextClip::new(start_us, duration_us, body);
    if let Some(s) = style {
        clip.style = s;
    }
    clip.animations = animations;
    let clip_id = clip.id;
    let track = project
        .find_track_mut(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    if magnetic {
        magnetic::insert(track, Clip::Text(clip), fps);
    } else {
        track.clips.push(Clip::Text(clip));
    }
    Ok(CommandOutcome::ClipAdded { clip_id })
}

/// `SetTextTiming`: move or resize a text clip.
pub(super) fn set_text_timing(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    start_us: Option<i64>,
    duration_us: Option<i64>,
) -> Result<CommandOutcome, CommandError> {
    if start_us.is_none() && duration_us.is_none() {
        return Err(invalid("SetTextTiming needs start and/or duration"));
    }
    let track = project
        .find_track(track_id)
        .ok_or(CommandError::TrackNotFound(track_id))?;
    let Some(Clip::Text(existing)) = track.find_clip(clip_id) else {
        return Err(CommandError::ClipNotFound(clip_id, track_id));
    };
    let new_start = start_us.unwrap_or(existing.start_us);
    let new_duration = duration_us.unwrap_or(existing.duration_us);
    if new_duration <= 0 {
        return Err(CommandError::InvalidDuration(new_duration));
    }
    if !track.magnetic {
        check_no_overlap(track, new_start, new_duration, Some(clip_id))?;
    }
    validate_body("body", &existing.body, Some(new_duration)).map_err(invalid)?;
    let Some(Clip::Text(c)) = project
        .find_track_mut(track_id)
        .and_then(|t| t.clips.iter_mut().find(|c| c.id() == clip_id))
    else {
        return Err(CommandError::ClipNotFound(clip_id, track_id));
    };
    c.start_us = new_start;
    c.duration_us = new_duration;
    Ok(CommandOutcome::Applied)
}

// ---- shared edits on captions and text clips ------------------------------------------

/// Mutable view of the parts a caption and a text clip share.
struct TextParts<'a> {
    body: &'a mut TextBody,
    animations: &'a mut TextAnimations,
    duration_us: i64,
    style: StyleSlot<'a>,
}

enum StyleSlot<'a> {
    /// Caption clip: `None` inherits the track.
    Caption(&'a mut Option<TextStyleRef>),
    Text(&'a mut TextStyleRef),
}

fn text_parts(clip: &mut Clip) -> Option<TextParts<'_>> {
    match clip {
        Clip::Caption(c) => Some(TextParts {
            body: &mut c.body,
            animations: &mut c.animations,
            duration_us: c.duration_us,
            style: StyleSlot::Caption(&mut c.style),
        }),
        Clip::Text(c) => Some(TextParts {
            body: &mut c.body,
            animations: &mut c.animations,
            duration_us: c.duration_us,
            style: StyleSlot::Text(&mut c.style),
        }),
        _ => None,
    }
}

fn not_text(track_id: Id, clip_id: Id) -> CommandError {
    invalid(format!(
        "clip {clip_id} on track {track_id} is not a caption or text clip"
    ))
}

/// Run `edit` on every target after checking that all of them are caption or text clips and
/// that `edit` succeeds on a scratch copy of each, so a batch applies to all or none.
fn edit_text_targets(
    project: &mut Project,
    targets: &[TextClipRef],
    edit: impl Fn(&mut TextParts<'_>) -> Result<(), CommandError>,
) -> Result<CommandOutcome, CommandError> {
    let mut scratch: Vec<(TextClipRef, Clip)> = Vec::with_capacity(targets.len());
    for t in targets {
        let track = project
            .find_track(t.track_id)
            .ok_or(CommandError::TrackNotFound(t.track_id))?;
        let clip = track
            .find_clip(t.clip_id)
            .ok_or(CommandError::ClipNotFound(t.clip_id, t.track_id))?;
        scratch.push((*t, clip.clone()));
    }
    for (t, clip) in &mut scratch {
        let mut parts = text_parts(clip).ok_or_else(|| not_text(t.track_id, t.clip_id))?;
        edit(&mut parts)?;
    }
    for (t, clip) in scratch {
        if let Some(slot) = project
            .find_track_mut(t.track_id)
            .and_then(|tr| tr.clips.iter_mut().find(|c| c.id() == t.clip_id))
        {
            *slot = clip;
        }
    }
    Ok(CommandOutcome::Applied)
}

/// `SetTextBody`: change the displayed text and/or its per-range style runs. Words are
/// recognition evidence and are never touched.
pub(super) fn set_text_body(
    project: &mut Project,
    targets: Vec<TextClipRef>,
    text: Option<String>,
    runs: Option<Vec<TextRun>>,
) -> Result<CommandOutcome, CommandError> {
    if text.is_none() && runs.is_none() {
        return Err(invalid("SetTextBody needs text and/or runs"));
    }
    edit_text_targets(project, &targets, |p| {
        if let Some(t) = &text {
            p.body.text = t.clone();
        }
        if let Some(r) = &runs {
            p.body.runs = r.clone();
        }
        validate_body("body", p.body, Some(p.duration_us)).map_err(invalid)
    })
}

/// What `SetTextStyle` changes. Every field is optional: omitted = unchanged.
#[derive(Debug, Clone, Default)]
pub struct StyleChange {
    /// `Some(None)` clears the style: a caption inherits its track again, a text clip returns
    /// to the default text style.
    pub item_ref: Option<Option<String>>,
    pub hash: Option<Option<String>>,
    pub params: Option<BTreeMap<String, serde_json::Value>>,
    pub overrides: Option<BTreeMap<String, serde_json::Value>>,
}

pub(super) fn set_text_style(
    project: &mut Project,
    targets: Vec<TextClipRef>,
    change: StyleChange,
) -> Result<CommandOutcome, CommandError> {
    if change.item_ref.is_none()
        && change.hash.is_none()
        && change.params.is_none()
        && change.overrides.is_none()
    {
        return Err(invalid(
            "SetTextStyle needs at least one of ref, hash, params, overrides",
        ));
    }
    edit_text_targets(project, &targets, |p| {
        let clearing = matches!(change.item_ref, Some(None));
        let current: Option<TextStyleRef> = match &p.style {
            StyleSlot::Caption(s) => (**s).clone(),
            StyleSlot::Text(s) => Some((**s).clone()),
        };
        let next = if clearing {
            None
        } else {
            let mut style = match (&change.item_ref, current) {
                (Some(Some(r)), cur) => {
                    // A new ref keeps the params/overrides unless they are given too.
                    let mut s = cur.unwrap_or_else(|| TextStyleRef::new(r.clone()));
                    if s.item_ref != *r {
                        s.item_ref = r.clone();
                        s.hash = None;
                    }
                    s
                }
                (_, Some(cur)) => cur,
                (_, None) => {
                    return Err(invalid(
                        "this caption inherits its track's style, so it has no style to adjust; give a ref",
                    ))
                }
            };
            if let Some(h) = &change.hash {
                style.hash = h.clone();
            }
            if let Some(params) = &change.params {
                style.params = params.clone();
            }
            if let Some(overrides) = &change.overrides {
                style.overrides = overrides.clone();
            }
            check_style("style", &style)?;
            Some(style)
        };
        match (&mut p.style, next) {
            (StyleSlot::Caption(slot), next) => **slot = next,
            (StyleSlot::Text(slot), Some(next)) => **slot = next,
            (StyleSlot::Text(slot), None) => {
                **slot = TextClip::new(0, 1, TextBody::default()).style;
            }
        }
        Ok(())
    })
}

/// `SetTextAnimation`: set (ref given) or clear (`None`) the animation of one phase.
pub(super) fn set_text_animation(
    project: &mut Project,
    targets: Vec<TextClipRef>,
    phase: String,
    item_ref: Option<String>,
    hash: Option<String>,
    params: Option<BTreeMap<String, serde_json::Value>>,
    duration_us: Option<i64>,
) -> Result<CommandOutcome, CommandError> {
    if !crate::project::text::ANIMATION_PHASES.contains(&phase.as_str()) {
        return Err(invalid(format!(
            "unknown phase '{phase}' (in, out, loop, active)"
        )));
    }
    edit_text_targets(project, &targets, |p| {
        let value = match &item_ref {
            None => None,
            Some(r) => {
                let existing = p.animations.get(&phase).filter(|a| a.item_ref == *r);
                let anim = TextAnimationRef {
                    item_ref: r.clone(),
                    hash: hash
                        .clone()
                        .or_else(|| existing.and_then(|a| a.hash.clone())),
                    params: params
                        .clone()
                        .or_else(|| existing.map(|a| a.params.clone()))
                        .unwrap_or_default(),
                    duration_us: duration_us.or_else(|| existing.and_then(|a| a.duration_us)),
                };
                validate_animation("animation", &phase, &anim).map_err(invalid)?;
                Some(anim)
            }
        };
        p.animations.set(&phase, value);
        Ok(())
    })
}

/// `SetTrackCaptionStyle`: the style every caption of the track without its own inherits.
/// `reset_clips` also clears the clips' own styles, so the whole track follows it.
pub(super) fn set_track_caption_style(
    project: &mut Project,
    track_id: Id,
    style: Option<TextStyleRef>,
    reset_clips: bool,
) -> Result<CommandOutcome, CommandError> {
    let track = caption_track_mut(project, track_id)?;
    if let Some(s) = &style {
        check_style("style", s)?;
    }
    track.caption_style = style;
    if reset_clips {
        for clip in &mut track.clips {
            if let Clip::Caption(c) = clip {
                c.style = None;
            }
        }
    }
    Ok(CommandOutcome::Applied)
}

fn caption_mut(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
) -> Result<&mut CaptionClip, CommandError> {
    let track = caption_track_mut(project, track_id)?;
    match track.clips.iter_mut().find(|c| c.id() == clip_id) {
        Some(Clip::Caption(c)) => Ok(c),
        _ => Err(CommandError::ClipNotFound(clip_id, track_id)),
    }
}

/// `SetCaptionWords`: replace a caption's word timings (imports, manual timing). The displayed
/// text is not changed.
pub(super) fn set_caption_words(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    words: Vec<WordInput>,
) -> Result<CommandOutcome, CommandError> {
    let fps = project.settings.fps;
    let clip = caption_mut(project, track_id, clip_id)?;
    let mut body = clip.body.clone();
    body.words = words.iter().map(|w| w.resolve(fps)).collect();
    validate_body("words", &body, Some(clip.duration_us)).map_err(invalid)?;
    clip.body = body;
    Ok(CommandOutcome::Applied)
}

/// `SetWordEmphasis`: mark or unmark words (by index into `body.words`) as key words.
pub(super) fn set_word_emphasis(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    indices: Vec<usize>,
    emphasis: bool,
) -> Result<CommandOutcome, CommandError> {
    let clip = caption_mut(project, track_id, clip_id)?;
    if indices.is_empty() {
        return Err(invalid("SetWordEmphasis needs at least one word index"));
    }
    if let Some(bad) = indices.iter().find(|i| **i >= clip.body.words.len()) {
        return Err(invalid(format!(
            "word index {bad} is out of range: the caption has {} words",
            clip.body.words.len()
        )));
    }
    for i in indices {
        clip.body.words[i].emphasis = emphasis;
    }
    Ok(CommandOutcome::Applied)
}

// ---- SplitCaptions --------------------------------------------------------------------

/// One word on the track's timeline.
struct AbsWord {
    word: CaptionWord,
    start: i64,
    end: i64,
    style: Option<TextStyleRef>,
    animations: TextAnimations,
    transcript_id: Option<Id>,
}

/// Greedy line fill: how many lines `words` take at `max_chars` per line. A word longer than a
/// line sits alone on one.
fn lines_of(words: &[&str], max_chars: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for w in words {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + w.chars().count() <= max_chars => {
                line.push(' ');
                line.push_str(w);
            }
            _ => lines.push((*w).to_string()),
        }
    }
    lines
}

/// `SplitCaptions`: rebuild the track's word-timed captions as pages. Words from every caption
/// that has them are laid on the timeline in order and cut into pages of at most `max_lines`
/// lines of `max_chars` characters, none longer than `max_duration` (a page always holds at
/// least one word). Each page takes the style and animations of the caption its first word came
/// from; words keep their emphasis and are stored relative to the page start. Captions without
/// words (typed text) are kept as they are. Deterministic: the same words and limits give the
/// same pages.
pub(super) fn split_captions(
    project: &mut Project,
    track_id: Id,
    max_chars: usize,
    max_lines: usize,
    max_duration: Time,
) -> Result<CommandOutcome, CommandError> {
    let fps = project.settings.fps;
    if max_chars == 0 || max_lines == 0 {
        return Err(invalid("max_chars and max_lines must be >= 1"));
    }
    let max_duration_us = snap_dur(max_duration, fps);
    if max_duration_us <= 0 {
        return Err(invalid("max_duration must be > 0"));
    }
    let track = caption_track_mut(project, track_id)?;

    let mut kept: Vec<Clip> = Vec::new();
    let mut words: Vec<AbsWord> = Vec::new();
    let mut ordered: Vec<Clip> = std::mem::take(&mut track.clips);
    ordered.sort_by_key(Clip::start_us);
    for clip in ordered {
        match clip {
            Clip::Caption(c) if !c.body.words.is_empty() => {
                for w in &c.body.words {
                    words.push(AbsWord {
                        word: w.clone(),
                        start: c.start_us + w.start_us,
                        end: c.start_us + w.end_us,
                        style: c.style.clone(),
                        animations: c.animations.clone(),
                        transcript_id: c.transcript_ref.as_ref().map(|r| r.transcript_id),
                    });
                }
            }
            other => kept.push(other),
        }
    }
    words.sort_by_key(|w| (w.start, w.end));

    // Cut into pages.
    let mut pages: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    for (i, w) in words.iter().enumerate() {
        if !current.is_empty() {
            let mut texts: Vec<&str> = current
                .iter()
                .map(|&j| words[j].word.text.as_str())
                .collect();
            texts.push(w.word.text.as_str());
            let too_many_lines = lines_of(&texts, max_chars).len() > max_lines;
            let too_long = w.end - words[current[0]].start > max_duration_us;
            if too_many_lines || too_long {
                pages.push(std::mem::take(&mut current));
            }
        }
        current.push(i);
    }
    if !current.is_empty() {
        pages.push(current);
    }

    let mut previous_end = 0_i64;
    let mut built: Vec<Clip> = Vec::with_capacity(pages.len());
    for page in pages {
        let first = &words[page[0]];
        let last = &words[*page.last().unwrap_or(&page[0])];
        let start = fps.snap_to_frame(first.start).max(previous_end);
        let end = fps
            .snap_to_frame(last.end)
            .max(start + fps.frame_start(1).max(1));
        previous_end = end;
        let texts: Vec<&str> = page.iter().map(|&j| words[j].word.text.as_str()).collect();
        let text = lines_of(&texts, max_chars).join("\n");
        let page_words: Vec<CaptionWord> = page
            .iter()
            .map(|&j| {
                let w = &words[j];
                let mut cw = w.word.clone();
                cw.start_us = (w.start - start).clamp(0, end - start);
                cw.end_us = (w.end - start).clamp(cw.start_us, end - start);
                cw
            })
            .collect();
        let transcript_ref = match (first.transcript_id, &first.word.word_id, &last.word.word_id) {
            (Some(tid), Some(a), Some(b))
                if page.iter().all(|&j| words[j].transcript_id == Some(tid)) =>
            {
                Some(TranscriptRef {
                    transcript_id: tid,
                    first_word: a.clone(),
                    last_word: b.clone(),
                })
            }
            _ => None,
        };
        let mut clip = CaptionClip::new(start, end - start, "");
        clip.body = TextBody {
            text,
            runs: Vec::new(),
            words: page_words,
        };
        clip.transcript_ref = transcript_ref;
        clip.style = first.style.clone();
        clip.animations = first.animations.clone();
        built.push(Clip::Caption(clip));
    }

    let mut all = kept;
    all.extend(built);
    all.sort_by_key(Clip::start_us);
    // Restore the track only if the result is consistent; a typed caption that now overlaps a
    // page would be a hand-made collision, reported rather than hidden.
    for pair in all.windows(2) {
        if pair[0].end_us() > pair[1].start_us() {
            return Err(CommandError::Overlap(
                pair[1].start_us(),
                pair[0].end_us(),
                track_id,
            ));
        }
    }
    let track = caption_track_mut(project, track_id)?;
    track.clips = all;
    Ok(CommandOutcome::Applied)
}

// ---- markers --------------------------------------------------------------------------

pub(super) fn add_markers(
    project: &mut Project,
    markers: Vec<MarkerInput>,
) -> Result<CommandOutcome, CommandError> {
    let fps = project.settings.fps;
    if markers.is_empty() {
        return Err(CommandError::InvalidMarker(
            "AddMarkers needs at least one marker".into(),
        ));
    }
    let mut built: Vec<Marker> = Vec::with_capacity(markers.len());
    for m in markers {
        let time_us = m.time.resolve(fps);
        if time_us < 0 {
            return Err(CommandError::InvalidMarker(format!(
                "marker time {time_us} is before the start of the timeline"
            )));
        }
        if !m.color.is_empty() && !crate::project::text::is_hex_color(&m.color) {
            return Err(CommandError::InvalidMarker(format!(
                "marker colour '{}' is not #RRGGBB",
                m.color
            )));
        }
        let id = m.id.unwrap_or_else(Id::new_v4);
        if project.markers.iter().any(|x| x.id == id) || built.iter().any(|x| x.id == id) {
            return Err(CommandError::InvalidMarker(format!(
                "marker id {id} already exists"
            )));
        }
        built.push(Marker {
            id,
            time_us,
            label: m.label,
            color: m.color,
            kind: m.kind,
        });
    }
    project.markers.extend(built);
    project.markers.sort_by_key(|m| m.time_us);
    Ok(CommandOutcome::Applied)
}

pub(super) fn remove_markers(
    project: &mut Project,
    ids: Vec<Id>,
    kind: Option<MarkerKind>,
) -> Result<CommandOutcome, CommandError> {
    if ids.is_empty() && kind.is_none() {
        return Err(CommandError::InvalidMarker(
            "RemoveMarkers needs ids and/or a kind".into(),
        ));
    }
    if let Some(missing) = ids
        .iter()
        .find(|id| !project.markers.iter().any(|m| m.id == **id))
    {
        return Err(CommandError::InvalidMarker(format!("no marker {missing}")));
    }
    project.markers.retain(|m| {
        let by_id = ids.contains(&m.id);
        let by_kind = kind.is_some_and(|k| m.kind == k);
        !(by_id || by_kind)
    });
    Ok(CommandOutcome::Applied)
}

// ---- transcripts ----------------------------------------------------------------------

fn transcript_err(message: impl Into<String>) -> CommandError {
    CommandError::InvalidTranscript(message.into())
}

/// Source-time ranges of runs of consecutive words in `selected` (word indices, ascending),
/// each from the first word's start to the last word's end, snapped to the frame grid.
fn word_runs(
    words: &[crate::project::TranscriptWord],
    selected: &[usize],
    fps: Fps,
) -> Vec<(i64, i64)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &i in selected {
        match runs.last_mut() {
            Some((_, last)) if *last + 1 == i => *last = i,
            _ => runs.push((i, i)),
        }
    }
    runs.into_iter()
        .map(|(a, b)| {
            (
                fps.snap_to_frame(words[a].start_us),
                fps.snap_to_frame(words[b].end_us),
            )
        })
        .filter(|(s, e)| e > s)
        .collect()
}

/// `(track id, clip id)` of every video and audio clip of `media_id`, root timeline and
/// sequences alike, after checking each can be cut by source time.
fn clips_of_media(project: &Project, media_id: Id) -> Result<Vec<(Id, Id)>, CommandError> {
    let mut out = Vec::new();
    for track in project.all_tracks() {
        for clip in &track.clips {
            let Some(m) = clip.as_media().filter(|m| m.media_id == media_id) else {
                continue;
            };
            let retimed = (m.speed_factor() - 1.0).abs() > 1e-9
                || m.keyframes
                    .iter()
                    .any(|k| k.property == crate::project::AnimProperty::Speed);
            if retimed {
                return Err(transcript_err(format!(
                    "clip {} is retimed (speed != 1); transcript cuts work on source time, so reset its speed first",
                    m.id
                )));
            }
            out.push((track.id, m.id));
        }
    }
    Ok(out)
}

/// Cut `[lo, hi)` of source time out of the media clip `clip_id`, ripple-closing the gap on its
/// track. Splits at the ends and deletes the middle piece, so the clip's effects and keyframes
/// follow the ordinary split rules.
fn cut_source_range(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    lo: i64,
    hi: i64,
) -> Result<(), CommandError> {
    let Some(m) = project
        .find_track(track_id)
        .and_then(|t| t.find_clip(clip_id))
        .and_then(Clip::as_media)
    else {
        return Ok(());
    };
    let (a, b, s) = (m.source_in_us, m.source_out_us, m.start_us);
    let (lo, hi) = (lo.max(a), hi.min(b));
    if lo >= hi {
        return Ok(());
    }
    let (tl_lo, tl_hi) = (s + (lo - a), s + (hi - a));
    if lo == a && hi == b {
        super::delete_clip(project, track_id, clip_id, true)?;
        return Ok(());
    }
    // Split off the tail first so the clip id keeps naming the front.
    if hi < b {
        super::split_clip(project, track_id, clip_id, tl_hi)?;
    }
    let target = if lo > a {
        match super::split_clip(project, track_id, clip_id, tl_lo)? {
            CommandOutcome::ClipSplit { right_id, .. } => right_id,
            _ => clip_id,
        }
    } else {
        clip_id
    };
    super::delete_clip(project, track_id, target, true)?;
    Ok(())
}

/// `ApplyTranscriptCuts`: mark words deleted (`word_ids`, and every word carrying one of
/// `tags`), then cut every deleted range out of the clips of the transcript's media, closing
/// the gaps on those clips' tracks (a real edit: one undo step, the transcript stays the record
/// of what was removed). Already-cut ranges are a no-op, so it is safe to repeat.
pub(super) fn apply_transcript_cuts(
    project: &mut Project,
    transcript_id: Id,
    word_ids: Vec<String>,
    tags: Vec<String>,
) -> Result<CommandOutcome, CommandError> {
    let fps = project.settings.fps;
    let transcript = project
        .find_transcript(transcript_id)
        .ok_or_else(|| transcript_err(format!("no transcript {transcript_id}")))?;
    if let Some(bad) = word_ids.iter().find(|w| transcript.find_word(w).is_none()) {
        return Err(transcript_err(format!(
            "transcript {transcript_id} has no word '{bad}'"
        )));
    }
    let media_id = transcript.media_id;
    let targets = clips_of_media(project, media_id)?;

    let transcript = project
        .transcripts
        .iter_mut()
        .find(|t| t.id == transcript_id)
        .ok_or_else(|| transcript_err(format!("no transcript {transcript_id}")))?;
    for w in &mut transcript.words {
        if word_ids.contains(&w.id) || w.tags.iter().any(|t| tags.contains(t)) {
            w.deleted = true;
        }
    }
    let selected: Vec<usize> = transcript
        .words
        .iter()
        .enumerate()
        .filter(|(_, w)| w.deleted)
        .map(|(i, _)| i)
        .collect();
    if selected.is_empty() {
        return Err(transcript_err(
            "no words are deleted: pass word_ids and/or tags to choose what to cut",
        ));
    }
    let mut ranges = word_runs(&transcript.words, &selected, fps);
    // Highest range first: cutting earlier ones then never moves a later one.
    ranges.sort_by_key(|r| std::cmp::Reverse(r.0));
    for (track_id, clip_id) in targets {
        for &(lo, hi) in &ranges {
            cut_source_range(project, track_id, clip_id, lo, hi)?;
        }
    }
    Ok(CommandOutcome::Applied)
}

/// `RestoreTranscriptWords`: clear `deleted` on `word_ids` (all deleted words when empty) and
/// put their audio and picture back where it was cut: the restored ranges are re-inserted
/// between the clips on either side of the cut, ripple-opening the gap. Words whose range was
/// never cut are left alone.
pub(super) fn restore_transcript_words(
    project: &mut Project,
    transcript_id: Id,
    word_ids: Vec<String>,
) -> Result<CommandOutcome, CommandError> {
    let fps = project.settings.fps;
    let transcript = project
        .find_transcript(transcript_id)
        .ok_or_else(|| transcript_err(format!("no transcript {transcript_id}")))?;
    if let Some(bad) = word_ids.iter().find(|w| transcript.find_word(w).is_none()) {
        return Err(transcript_err(format!(
            "transcript {transcript_id} has no word '{bad}'"
        )));
    }
    let media_id = transcript.media_id;
    let targets = clips_of_media(project, media_id)?;

    let transcript = project
        .transcripts
        .iter_mut()
        .find(|t| t.id == transcript_id)
        .ok_or_else(|| transcript_err(format!("no transcript {transcript_id}")))?;
    let mut restored: Vec<usize> = Vec::new();
    for (i, w) in transcript.words.iter_mut().enumerate() {
        if w.deleted && (word_ids.is_empty() || word_ids.contains(&w.id)) {
            w.deleted = false;
            restored.push(i);
        }
    }
    if restored.is_empty() {
        return Err(transcript_err("no deleted words to restore"));
    }
    let mut ranges = word_runs(&transcript.words, &restored, fps);
    ranges.sort_by_key(|r| std::cmp::Reverse(r.0));
    let mut tracks: Vec<Id> = Vec::new();
    for (track_id, _) in targets {
        if !tracks.contains(&track_id) {
            tracks.push(track_id);
        }
    }
    for track_id in tracks {
        for &(lo, hi) in &ranges {
            reinsert_source_range(project, track_id, media_id, lo, hi);
        }
    }
    Ok(CommandOutcome::Applied)
}

/// Put `[lo, hi)` of `media_id` back on `track_id`. The range goes after the clip of that media
/// whose source ends nearest before it (or before the first clip when there is none), and
/// everything after shifts right. It extends that clip when they touch, else becomes a new
/// clip carrying the neighbour's settings (without keyframes or transition).
fn reinsert_source_range(project: &mut Project, track_id: Id, media_id: Id, lo: i64, hi: i64) {
    let fps = project.settings.fps;
    let tolerance = fps.frame_start(1).max(1);
    let Some(track) = project.find_track_mut(track_id) else {
        return;
    };
    track.clips.sort_by_key(Clip::start_us);
    // Already on the timeline? Then there is nothing to put back.
    let covered = track.clips.iter().filter_map(Clip::as_media).any(|m| {
        m.media_id == media_id
            && m.source_in_us <= lo + tolerance
            && m.source_out_us >= hi - tolerance
    });
    if covered {
        return;
    }
    let before = track
        .clips
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.as_media().map(|m| (i, m)))
        .filter(|(_, m)| m.media_id == media_id && m.source_out_us <= lo + tolerance)
        .max_by_key(|(_, m)| m.source_out_us)
        .map(|(i, m)| (i, m.clone()));
    let after = track
        .clips
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.as_media().map(|m| (i, m)))
        .filter(|(_, m)| m.media_id == media_id && m.source_in_us >= hi - tolerance)
        .min_by_key(|(_, m)| m.source_in_us)
        .map(|(i, m)| (i, m.clone()));
    let len = hi - lo;
    match (before, after) {
        (Some((i, a)), _) => {
            let insert_at = a.start_us + (a.source_out_us - a.source_in_us);
            for c in track.clips.iter_mut().skip(i + 1) {
                c.set_start_us(c.start_us() + len);
            }
            if (lo - a.source_out_us).abs() <= tolerance {
                // Touching: extend the earlier piece.
                if let Some(Clip::Video(m) | Clip::Audio(m)) = track.clips.get_mut(i) {
                    m.source_out_us = hi;
                }
            } else {
                insert_piece(track, i + 1, &a, insert_at, lo, hi);
            }
        }
        (None, Some((i, b))) => {
            for c in track.clips.iter_mut().skip(i) {
                c.set_start_us(c.start_us() + len);
            }
            if (b.source_in_us - hi).abs() <= tolerance {
                if let Some(Clip::Video(m) | Clip::Audio(m)) = track.clips.get_mut(i) {
                    m.source_in_us = lo;
                    m.start_us = b.start_us;
                }
            } else {
                insert_piece(track, i, &b, b.start_us, lo, hi);
            }
        }
        (None, None) => {}
    }
    track.clips.sort_by_key(Clip::start_us);
}

fn insert_piece(
    track: &mut crate::project::Track,
    at: usize,
    template: &crate::project::MediaClip,
    start_us: i64,
    lo: i64,
    hi: i64,
) {
    let mut piece = template.clone();
    piece.id = Id::new_v4();
    piece.start_us = start_us;
    piece.source_in_us = lo;
    piece.source_out_us = hi;
    piece.keyframes.clear();
    piece.outgoing_transition = None;
    piece.fade_in_us = 0;
    piece.fade_out_us = 0;
    piece.reid_effects();
    let clip = match track.kind {
        TrackKind::Audio => Clip::Audio(piece),
        _ => Clip::Video(piece),
    };
    track.clips.insert(at.min(track.clips.len()), clip);
}

/// Rewrite the deprecated v7 caption parameters of one command object in place:
/// `AddCaption` / `SetCaption` `text` becomes `body.text`, and `style_id` (here and on
/// `GenerateCaptions`) becomes `style: {ref: "builtin:<id>@1"}`. Returns one deprecation
/// message per rewrite.
pub fn normalize_legacy_text_params(value: &mut serde_json::Value) -> Vec<String> {
    use serde_json::{json, Value};
    let mut warnings = Vec::new();
    let Some(obj) = value.as_object_mut() else {
        return warnings;
    };
    let command = obj
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if !matches!(
        command.as_str(),
        "AddCaption" | "SetCaption" | "GenerateCaptions"
    ) {
        return warnings;
    }
    if command != "GenerateCaptions" {
        if let Some(text) = obj.remove("text") {
            if !text.is_null() && !obj.contains_key("body") {
                obj.insert("body".into(), json!({ "text": text }));
            }
            warnings.push(format!(
                "{command}: parameter `text` is deprecated; use `body: {{\"text\": ...}}`. The alias is removed in a later release."
            ));
        }
    }
    if let Some(style_id) = obj.remove("style_id") {
        if let Some(id) = style_id.as_str() {
            if !obj.contains_key("style") {
                obj.insert(
                    "style".into(),
                    crate::project::migrate::legacy_caption_style(id),
                );
            }
        }
        warnings.push(format!(
            "{command}: parameter `style_id` is deprecated; use `style: {{\"ref\": \"builtin:<id>@1\"}}` (or omit it to inherit the track's caption_style). The alias is removed in a later release."
        ));
    }
    warnings
}
