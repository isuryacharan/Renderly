//! Effect, transition, keyframe-easing and batch commands (schema v8): `SetClipEffects` with
//! slots, `SetClipTransition` with a ref, `SetKeyframeEasing`, and the validation behind keyframes
//! on effect parameters. See docs/command-api.md.
//!
//! Every command here validates all of its targets before changing any of them, so a batch
//! (`targets`) either applies to every clip or to none, and is one undo step.

use super::{snap_dur, CommandError, CommandOutcome};
use crate::compose::effects::{manifest_params, validate_params};
use crate::compose::runtime::manifest::ParamKind;
use crate::compose::runtime::EffectManifest;
use crate::packs::refs::ItemRef;
use crate::packs::status::{resolve_target, Target};
use crate::packs::LoadedPack;
use crate::project::{
    keys, BlendMode, Clip, ClipTransition, Easing, EffectInstance, EffectParam, EffectParams,
    EffectStatus, Id, KeyValue, KeyframeTrack, ProgressEasing, Project, TrackKind, TrackProperty,
};
use crate::time::Time;
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashSet;

pub use super::sequences::ClipRef;

fn default_true() -> bool {
    true
}

/// An effect to put in a slot, as a command takes it. `ref` is the only required field.
///
/// - `id`: keep an existing instance (so its keyframes stay) or choose the id of a new one;
///   generated when omitted. Ignored when a command targets several clips (each gets its own).
/// - `params`: control values by key. Omitted on an existing instance keeps its params; omitted
///   on a new instance writes the effect's `initial` values (so a new blur is visible).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EffectSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    #[serde(rename = "ref")]
    pub item_ref: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<EffectParams>,
}

impl From<EffectInstance> for EffectSpec {
    fn from(e: EffectInstance) -> Self {
        Self {
            id: Some(e.id),
            item_ref: e.item_ref,
            hash: e.hash,
            enabled: e.enabled,
            params: Some(e.params),
        }
    }
}

impl EffectSpec {
    /// A spec for `item_ref` with no params (the effect's initial values are used).
    pub fn new(item_ref: impl Into<String>) -> Self {
        Self {
            id: None,
            item_ref: item_ref.into(),
            hash: None,
            enabled: true,
            params: None,
        }
    }

    pub fn with_params(mut self, params: &[(&str, f64)]) -> Self {
        self.params = Some(
            params
                .iter()
                .map(|(k, v)| ((*k).to_string(), EffectParam::Number(*v)))
                .collect(),
        );
        self
    }
}

/// `Some(None)` when the field is present and `null` (clear), `Some(Some(v))` when set,
/// `None` when absent (unchanged).
pub fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Some(Option::<T>::deserialize(deserializer)?))
}

/// A field that must be present but may be `null`.
pub fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

/// How a key's easing is chosen by `SetKeyframeEasing`: a name, or CSS Bezier control points.
/// Named presets other than `linear`, `hold` and `auto` are written as `bezier` handles (CSS
/// values: ease-in 0.42,0,1,1; ease-out 0,0,0.58,1; ease-in-out 0.42,0,0.58,1), so a preset is a
/// starting point the user can then drag.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EasingChoice {
    Linear,
    Hold,
    Auto,
    EaseIn,
    EaseOut,
    EaseInOut,
    /// `[x1, y1, x2, y2]` control points over the normalised span.
    Bezier([f64; 4]),
}

impl EasingChoice {
    fn bezier_points(self) -> Option<[f64; 4]> {
        match self {
            EasingChoice::EaseIn => Some([0.42, 0.0, 1.0, 1.0]),
            EasingChoice::EaseOut => Some([0.0, 0.0, 0.58, 1.0]),
            EasingChoice::EaseInOut => Some([0.42, 0.0, 0.58, 1.0]),
            EasingChoice::Bezier(p) => Some(p),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for EasingChoice {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Name(String),
            Bezier { bezier: [f64; 4] },
        }
        match Raw::deserialize(deserializer)? {
            Raw::Name(n) => match n.as_str() {
                "linear" => Ok(EasingChoice::Linear),
                "hold" => Ok(EasingChoice::Hold),
                "auto" => Ok(EasingChoice::Auto),
                "ease_in" => Ok(EasingChoice::EaseIn),
                "ease_out" => Ok(EasingChoice::EaseOut),
                "ease_in_out" => Ok(EasingChoice::EaseInOut),
                other => Err(serde::de::Error::custom(format!(
                    "unknown easing '{other}' (linear, hold, auto, ease_in, ease_out, \
                     ease_in_out, or {{\"bezier\": [x1, y1, x2, y2]}})"
                ))),
            },
            Raw::Bezier { bezier } => Ok(EasingChoice::Bezier(bezier)),
        }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for EasingChoice {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EasingChoice".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "linear | hold | auto | ease_in | ease_out | ease_in_out | {\"bezier\": [x1, y1, x2, y2]}",
            "oneOf": [
                { "type": "string", "enum": ["linear", "hold", "auto", "ease_in", "ease_out", "ease_in_out"] },
                { "type": "object", "properties": { "bezier": { "type": "array", "items": { "type": "number" }, "minItems": 4, "maxItems": 4 } }, "required": ["bezier"] }
            ]
        })
    }
}

/// Resolve a command's target selector: either `track_id` + `clip_id` (one clip) or `targets`
/// (one or more), not both.
pub(super) fn resolve_targets(
    command: &str,
    track_id: Option<Id>,
    clip_id: Option<Id>,
    targets: Vec<ClipRef>,
) -> Result<Vec<ClipRef>, CommandError> {
    match (track_id, clip_id, targets.is_empty()) {
        (Some(track_id), Some(clip_id), true) => Ok(vec![ClipRef { track_id, clip_id }]),
        (None, None, false) => Ok(targets),
        (None, None, true) => Err(CommandError::InvalidEffects(format!(
            "{command} needs track_id and clip_id, or targets: [{{track_id, clip_id}}, ...]"
        ))),
        _ => Err(CommandError::InvalidEffects(format!(
            "{command}: give either track_id + clip_id or targets, not both"
        ))),
    }
}

/// What a command needs to know about the installed items. Packs load lazily: builtin-only
/// commands never touch the library.
pub(super) struct Ctx {
    wasm_ids: HashSet<String>,
    packs: Option<Vec<LoadedPack>>,
}

impl Ctx {
    pub(super) fn new(project: &Project) -> Self {
        Self {
            wasm_ids: crate::plugins::known_plugin_refs(project),
            packs: None,
        }
    }

    fn packs(&mut self) -> &[LoadedPack] {
        self.packs
            .get_or_insert_with(crate::packs::load_installed_packs)
    }
}

/// An installed item a command applies.
pub(super) struct ItemInfo {
    /// `effect`, `filter`, `lut`, `transition`, or `wasm`.
    kind: String,
    /// The manifest that types its params (a pack LUT uses the LUT filter's).
    manifest: Option<EffectManifest>,
    hash: Option<String>,
    category: Option<String>,
}

fn lut_filter_manifest() -> Option<EffectManifest> {
    crate::compose::builtins::manifest("lut_filter")
}

/// Look `item_ref` up. `Err` carries a message for the agent: malformed ref, not installed, or
/// installed but not runnable here.
pub(super) fn item_info(ctx: &mut Ctx, item_ref: &str) -> Result<ItemInfo, String> {
    if let Some(plugin) = item_ref.strip_prefix("wasm:") {
        if !ctx.wasm_ids.contains(item_ref) {
            return Err(format!(
                "WASM plugin '{plugin}' is not loaded (LoadWasmPlugin first)"
            ));
        }
        return Ok(ItemInfo {
            kind: "wasm".into(),
            manifest: None,
            hash: None,
            category: None,
        });
    }
    let parsed = ItemRef::parse(item_ref).map_err(|e| e.to_string())?;
    let resolved = match &parsed {
        ItemRef::Builtin { .. } => resolve_target(&[], item_ref, None),
        ItemRef::Pack { .. } => resolve_target(ctx.packs(), item_ref, None),
    };
    match (resolved.status, resolved.target) {
        (EffectStatus::Missing, _) => Err(match parsed {
            ItemRef::Builtin { id, version } => {
                format!("unknown builtin 'builtin:{id}@{version}' (describe_effects lists them)")
            }
            ItemRef::Pack { .. } => {
                format!("'{item_ref}' is not installed (describe_effects lists installed items)")
            }
        }),
        (EffectStatus::Disabled, _) | (_, None) => Err(format!(
            "'{item_ref}' is installed but this build cannot run it"
        )),
        (_, Some(Target::Compiled(item))) => {
            let manifest = item.compiled.as_ref().map(|c| c.manifest().clone());
            Ok(ItemInfo {
                kind: item.kind.clone(),
                category: manifest.as_ref().and_then(|m| m.category.clone()),
                manifest,
                hash: match parsed {
                    ItemRef::Builtin { .. } => None,
                    ItemRef::Pack { .. } => item.hash.clone(),
                },
            })
        }
        (_, Some(Target::Lut { item, .. })) => Ok(ItemInfo {
            kind: "lut".into(),
            manifest: lut_filter_manifest(),
            hash: item.hash.clone(),
            category: Some("look".into()),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Filter,
    Key,
    Adjust,
    Effects,
}

/// Whether an item may sit in `slot`. `filter` takes filters and LUTs, `key` the chroma key,
/// `adjust` the colour adjustment; `effects[]` takes any single-input item (a second LUT
/// stacked on the first is fine) and `wasm:*` plugins.
fn slot_accepts(slot: Slot, item_ref: &str, info: &ItemInfo) -> Result<(), String> {
    let bare = item_ref.split('@').next().unwrap_or(item_ref);
    let ok = match slot {
        Slot::Filter => matches!(info.kind.as_str(), "filter" | "lut"),
        Slot::Key => bare == "builtin:chroma_key" || info.category.as_deref() == Some("key"),
        Slot::Adjust => {
            matches!(bare, "builtin:color_adjust" | "builtin:adjust")
                || info.category.as_deref() == Some("adjust")
        }
        Slot::Effects => matches!(info.kind.as_str(), "effect" | "filter" | "lut" | "wasm"),
    };
    if ok {
        return Ok(());
    }
    Err(match slot {
        Slot::Filter => format!("'{item_ref}' is a {} and cannot go in the filter slot (filters and LUTs only)", info.kind),
        Slot::Key => format!("'{item_ref}' cannot go in the key slot (keying effects only, such as builtin:chroma_key@1)"),
        Slot::Adjust => format!("'{item_ref}' cannot go in the adjust slot (builtin:color_adjust@1)"),
        Slot::Effects => format!("'{item_ref}' is a {} and cannot be applied as an effect", info.kind),
    })
}

/// Validate one param map against a manifest and resolve LUT params to installed LUTs.
fn check_params(
    ctx: &mut Ctx,
    manifest: &EffectManifest,
    params: &mut EffectParams,
) -> Result<(), String> {
    validate_params(manifest, params)?;
    for spec in manifest.params.iter().filter(|p| p.kind == ParamKind::Lut) {
        if let Some(EffectParam::Text(r)) = params.get(&spec.key) {
            let resolved = resolve_target(ctx.packs(), r, None);
            if !matches!(resolved.target, Some(Target::Lut { .. })) {
                return Err(format!(
                    "param '{}' needs an installed LUT ref (pack:<pack>/<lut>@<v>), got '{r}'",
                    spec.key
                ));
            }
        }
    }
    Ok(())
}

/// Build the instance a spec describes. `existing` is the instance of the same id already on
/// the clip, if any.
fn build_instance(
    ctx: &mut Ctx,
    spec: &EffectSpec,
    slot: Slot,
    existing: Option<&EffectInstance>,
    keep_ids: bool,
) -> Result<EffectInstance, CommandError> {
    let invalid = |m: String| CommandError::InvalidEffects(m);
    if spec.item_ref.trim().is_empty() {
        return Err(invalid("empty ref".into()));
    }
    let same_ref = existing.filter(|e| e.item_ref == spec.item_ref);
    let id = match (keep_ids, spec.id, same_ref) {
        (true, Some(id), _) => id,
        (_, _, Some(e)) if keep_ids => e.id,
        _ => Id::new_v4(),
    };
    let info = match item_info(ctx, &spec.item_ref) {
        Ok(info) => info,
        Err(message) => {
            // An item that was applied before and has since gone missing stays editable: the
            // instance keeps its params and renders pass-through.
            return match same_ref {
                Some(e) => Ok(EffectInstance {
                    id,
                    item_ref: spec.item_ref.clone(),
                    hash: spec.hash.clone().or_else(|| e.hash.clone()),
                    enabled: spec.enabled,
                    params: spec.params.clone().unwrap_or_else(|| e.params.clone()),
                }),
                None => Err(invalid(message)),
            };
        }
    };
    slot_accepts(slot, &spec.item_ref, &info).map_err(invalid)?;
    let params = match (&spec.params, &info.manifest) {
        (Some(p), Some(manifest)) => {
            let mut p = p.clone();
            check_params(ctx, manifest, &mut p).map_err(invalid)?;
            p
        }
        (Some(p), None) => {
            // wasm:* plugins take plain numbers (`intensity`).
            if !p
                .values()
                .all(|v| matches!(v, EffectParam::Number(n) if n.is_finite()))
            {
                return Err(invalid(format!(
                    "'{}' takes numeric params only",
                    spec.item_ref
                )));
            }
            p.clone()
        }
        (None, Some(manifest)) => match same_ref {
            Some(e) => e.params.clone(),
            None => manifest_params(manifest, true),
        },
        (None, None) => same_ref.map(|e| e.params.clone()).unwrap_or_default(),
    };
    Ok(EffectInstance {
        id,
        item_ref: spec.item_ref.clone(),
        hash: same_ref
            .and_then(|e| e.hash.clone())
            .filter(|h| info.hash.as_ref().is_none_or(|installed| installed == h))
            .or(info.hash),
        enabled: spec.enabled,
        params,
    })
}

/// The instance of the clip's current stack that `spec` names by id.
fn find_current<'a>(
    current: &[&'a EffectInstance],
    spec: &EffectSpec,
) -> Option<&'a EffectInstance> {
    let id = spec.id?;
    current.iter().copied().find(|e| e.id == id)
}

fn build_slot(
    ctx: &mut Ctx,
    slot: Slot,
    change: &Option<Option<EffectSpec>>,
    current: &[&EffectInstance],
    keep_ids: bool,
) -> Result<Option<Option<EffectInstance>>, CommandError> {
    Ok(match change {
        None => None,
        Some(None) => Some(None),
        Some(Some(spec)) => Some(Some(build_instance(
            ctx,
            spec,
            slot,
            find_current(current, spec),
            keep_ids,
        )?)),
    })
}

/// The slot changes of `SetClipEffects`.
#[derive(Debug, Clone, Default)]
pub struct EffectsChange {
    pub filter: Option<Option<EffectSpec>>,
    pub key: Option<Option<EffectSpec>>,
    pub adjust: Option<Option<EffectSpec>>,
    pub effects: Option<Vec<EffectSpec>>,
    pub blend_mode: Option<BlendMode>,
}

struct PreparedEffects {
    target: ClipRef,
    filter: Option<Option<EffectInstance>>,
    key: Option<Option<EffectInstance>>,
    adjust: Option<Option<EffectInstance>>,
    effects: Option<Vec<EffectInstance>>,
    blend_mode: Option<BlendMode>,
}

pub(super) fn set_clip_effects(
    project: &mut Project,
    targets: Vec<ClipRef>,
    change: EffectsChange,
) -> Result<CommandOutcome, CommandError> {
    if change.filter.is_none()
        && change.key.is_none()
        && change.adjust.is_none()
        && change.effects.is_none()
        && change.blend_mode.is_none()
    {
        return Err(CommandError::InvalidEffects(
            "SetClipEffects needs at least one of filter, key, adjust, effects, blend_mode \
             (omitted = unchanged, null = cleared)"
                .into(),
        ));
    }
    let keep_ids = targets.len() == 1;
    let mut ctx = Ctx::new(project);
    let mut prepared: Vec<PreparedEffects> = Vec::with_capacity(targets.len());
    for target in targets {
        let track = project
            .find_track(target.track_id)
            .ok_or(CommandError::TrackNotFound(target.track_id))?;
        let clip = track
            .clips
            .iter()
            .find(|c| c.id() == target.clip_id)
            .ok_or(CommandError::ClipNotFound(target.clip_id, target.track_id))?;
        let (current, is_adjustment, is_audio): (Vec<&EffectInstance>, bool, bool) = match clip {
            Clip::Video(m) => (m.effect_stack(), false, false),
            Clip::Audio(m) => (m.effect_stack(), false, true),
            Clip::Adjustment(a) => (a.effect_stack(), true, false),
            // A text clip has no key slot and no decoded frame for CPU plugins, like an
            // adjustment layer.
            Clip::Text(t) => (t.effect_stack(), true, false),
            Clip::Caption(_) => return Err(CommandError::NotMediaClip(target.clip_id)),
        };
        if is_audio
            && (change.filter.is_some()
                || change.key.is_some()
                || change.adjust.is_some()
                || change.blend_mode.is_some())
        {
            return Err(CommandError::InvalidEffects(
                "audio clips take only effects[] (wasm:* plugins); filter, key, adjust and \
                 blend_mode are for video clips and adjustment layers"
                    .into(),
            ));
        }
        if is_adjustment && matches!(change.key, Some(Some(_))) {
            return Err(CommandError::InvalidEffects(
                "adjustment layers and text clips have no key slot".into(),
            ));
        }
        let filter = build_slot(&mut ctx, Slot::Filter, &change.filter, &current, keep_ids)?;
        let key = build_slot(&mut ctx, Slot::Key, &change.key, &current, keep_ids)?;
        let adjust = build_slot(&mut ctx, Slot::Adjust, &change.adjust, &current, keep_ids)?;
        let effects = match &change.effects {
            None => None,
            Some(list) => {
                let mut out = Vec::with_capacity(list.len());
                for spec in list {
                    let inst = build_instance(
                        &mut ctx,
                        spec,
                        Slot::Effects,
                        find_current(&current, spec),
                        keep_ids,
                    )?;
                    if is_adjustment && inst.is_wasm() {
                        // WASM frame plugins run on CPU pixels of a decoded source frame; an
                        // adjustment layer's input exists only on the GPU.
                        return Err(CommandError::InvalidEffects(format!(
                            "WASM plugin effect '{}' is not supported on adjustment layers or text clips",
                            inst.item_ref
                        )));
                    }
                    out.push(inst);
                }
                Some(out)
            }
        };
        // Ids must be unique across the clip's whole resulting stack.
        let keep =
            |slot: &Option<Option<EffectInstance>>, old: Option<&EffectInstance>| -> Vec<Id> {
                match slot {
                    None => old.map(|e| e.id).into_iter().collect(),
                    Some(s) => s.iter().map(|e| e.id).collect(),
                }
            };
        let (old_f, old_k, old_a, old_effects): (_, _, _, &[EffectInstance]) = match clip {
            Clip::Video(m) | Clip::Audio(m) => (
                m.filter.as_ref(),
                m.key.as_ref(),
                m.adjust.as_ref(),
                &m.effects,
            ),
            Clip::Adjustment(a) => (a.filter.as_ref(), None, a.adjust.as_ref(), &a.effects),
            Clip::Text(t) => (t.filter.as_ref(), None, t.adjust.as_ref(), &t.effects),
            Clip::Caption(_) => (None, None, None, &[]),
        };
        let mut resulting: Vec<Id> = keep(&filter, old_f);
        resulting.extend(keep(&key, old_k));
        resulting.extend(keep(&adjust, old_a));
        match &effects {
            Some(list) => resulting.extend(list.iter().map(|e| e.id)),
            None => resulting.extend(old_effects.iter().map(|e| e.id)),
        }
        let mut seen = HashSet::new();
        for id in &resulting {
            if !seen.insert(*id) {
                return Err(CommandError::InvalidEffects(format!(
                    "duplicate effect id {id}"
                )));
            }
        }
        prepared.push(PreparedEffects {
            target,
            filter,
            key,
            adjust,
            effects,
            blend_mode: change.blend_mode,
        });
    }

    for p in prepared {
        let clip = super::any_clip_mut(project, p.target.track_id, p.target.clip_id)?;
        match clip {
            Clip::Video(m) | Clip::Audio(m) => {
                let before = stack_ids(&m.effect_stack());
                apply_prepared(m.slots_mut(), &p);
                let after = stack_ids(&m.effect_stack());
                purge_effect_tracks(&mut m.keyframes, &before, &after);
            }
            Clip::Adjustment(a) => {
                let before = stack_ids(&a.effect_stack());
                apply_prepared(a.slots_mut(), &p);
                let after = stack_ids(&a.effect_stack());
                purge_effect_tracks(&mut a.keyframes, &before, &after);
            }
            Clip::Text(t) => {
                let before = stack_ids(&t.effect_stack());
                apply_prepared(t.slots_mut(), &p);
                let after = stack_ids(&t.effect_stack());
                purge_effect_tracks(&mut t.keyframes, &before, &after);
            }
            Clip::Caption(_) => return Err(CommandError::NotMediaClip(p.target.clip_id)),
        }
    }
    Ok(CommandOutcome::Applied)
}

fn stack_ids(stack: &[&EffectInstance]) -> HashSet<Id> {
    stack.iter().map(|e| e.id).collect()
}

fn apply_prepared(slots: crate::project::SlotsMut<'_>, p: &PreparedEffects) {
    if let Some(f) = &p.filter {
        *slots.filter = f.clone();
    }
    if let (Some(k), Some(slot)) = (&p.key, slots.key) {
        *slot = k.clone();
    }
    if let Some(a) = &p.adjust {
        *slots.adjust = a.clone();
    }
    if let Some(list) = &p.effects {
        *slots.effects = list.clone();
    }
    if let Some(mode) = p.blend_mode {
        *slots.blend_mode = mode;
    }
}

/// Deleting an effect instance deletes its keyframe tracks.
fn purge_effect_tracks(tracks: &mut Vec<KeyframeTrack>, before: &HashSet<Id>, after: &HashSet<Id>) {
    tracks.retain(|t| match t.property.effect() {
        Some((id, _)) => !(before.contains(&id) && !after.contains(&id)),
        None => true,
    });
}

// ---- transitions ----------------------------------------------------------------------

/// `SetClipTransition`: set (`item_ref` given) or clear (`None`) the outgoing transition of
/// every target.
#[allow(clippy::too_many_arguments)]
pub(super) fn set_clip_transition(
    project: &mut Project,
    targets: Vec<ClipRef>,
    item_ref: Option<String>,
    duration: Option<Time>,
    params: Option<EffectParams>,
    easing: Option<Option<ProgressEasing>>,
) -> Result<CommandOutcome, CommandError> {
    let invalid = |m: String| CommandError::InvalidTransition(m);
    let fps = project.settings.fps;
    let mut ctx = Ctx::new(project);
    let info = match &item_ref {
        None => None,
        Some(r) => {
            let info = item_info(&mut ctx, r).map_err(invalid)?;
            if info.kind != "transition" {
                return Err(invalid(format!(
                    "'{r}' is a {}, not a transition",
                    info.kind
                )));
            }
            Some(info)
        }
    };
    let mut new_values: Vec<(ClipRef, Option<ClipTransition>)> = Vec::with_capacity(targets.len());
    for target in targets {
        let track = project
            .find_track(target.track_id)
            .ok_or(CommandError::TrackNotFound(target.track_id))?;
        if track.kind != TrackKind::Video {
            return Err(invalid(
                "transitions are only supported on video tracks".into(),
            ));
        }
        // A video clip or a text clip: (id, start, timeline duration, current transition).
        let (clip_id, clip_start, clip_dur, existing) = match track
            .clips
            .iter()
            .find(|c| c.id() == target.clip_id)
            .ok_or(CommandError::ClipNotFound(target.clip_id, target.track_id))?
        {
            Clip::Video(m) => (
                m.id,
                m.start_us,
                m.timeline_duration_us(),
                m.outgoing_transition.as_ref(),
            ),
            Clip::Text(t) => (
                t.id,
                t.start_us,
                t.duration_us,
                t.outgoing_transition.as_ref(),
            ),
            _ => return Err(CommandError::NotMediaClip(target.clip_id)),
        };
        let (Some(item_ref), Some(info)) = (&item_ref, &info) else {
            new_values.push((target, None));
            continue;
        };
        let duration_us = match (duration, existing) {
            (Some(d), _) => snap_dur(d, fps),
            (None, Some(e)) => e.duration_us,
            (None, None) => return Err(invalid("duration is required".into())),
        };
        if duration_us <= 0 {
            return Err(invalid("duration must be > 0".into()));
        }
        if duration_us > clip_dur / 2 {
            return Err(invalid("duration must be <= half the clip duration".into()));
        }
        let end = clip_start + clip_dur;
        let mut nexts: Vec<(i64, i64)> = track
            .clips
            .iter()
            .filter(|c| matches!(c, Clip::Video(_) | Clip::Text(_)))
            .filter(|c| c.id() != clip_id && c.is_enabled() && c.start_us() >= end)
            .map(|c| (c.start_us(), c.duration_us()))
            .collect();
        nexts.sort_by_key(|n| n.0);
        let Some(next) = nexts.first() else {
            return Err(invalid("no following clip on this track".into()));
        };
        if duration_us > next.1 / 2 {
            return Err(invalid(
                "duration must be <= half the next clip duration".into(),
            ));
        }
        let same_ref = existing.filter(|e| &e.item_ref == item_ref);
        let mut params_out = match (&params, same_ref) {
            (Some(p), _) => p.clone(),
            (None, Some(e)) => e.params.clone(),
            (None, None) => info
                .manifest
                .as_ref()
                .map(|m| manifest_params(m, true))
                .unwrap_or_default(),
        };
        if let Some(manifest) = &info.manifest {
            check_params(&mut ctx, manifest, &mut params_out).map_err(invalid)?;
        }
        let easing_out = match (&easing, existing) {
            (Some(e), _) => *e,
            (None, Some(e)) => e.easing,
            (None, None) => None,
        };
        new_values.push((
            target,
            Some(ClipTransition {
                item_ref: item_ref.clone(),
                hash: info.hash.clone(),
                duration_us,
                params: params_out,
                easing: easing_out,
            }),
        ));
    }
    for (target, transition) in new_values {
        match super::any_clip_mut(project, target.track_id, target.clip_id)? {
            Clip::Video(m) => m.outgoing_transition = transition,
            Clip::Text(t) => t.outgoing_transition = transition,
            _ => return Err(CommandError::NotMediaClip(target.clip_id)),
        }
    }
    Ok(CommandOutcome::Applied)
}

// ---- keyframes ------------------------------------------------------------------------

/// Validate a clip's keyframe tracks and return them sorted. `stack` is the clip's effect
/// instances (effect-parameter tracks must address one of them, and a param that can be
/// animated); `adjustment` limits the transform properties to opacity.
pub(super) fn validate_keyframes(
    project: &Project,
    stack: &[&EffectInstance],
    adjustment: bool,
    tracks: Vec<KeyframeTrack>,
) -> Result<Vec<KeyframeTrack>, CommandError> {
    use crate::project::AnimProperty;
    let invalid = |m: String| CommandError::InvalidKeyframes(m);
    let mut ctx = Ctx::new(project);
    let mut seen = HashSet::new();
    let mut out = Vec::with_capacity(tracks.len());
    for mut track in tracks {
        if !seen.insert(track.property.clone()) {
            return Err(invalid(format!("duplicate property {}", track.property)));
        }
        let mut times = HashSet::new();
        for key in &track.keys {
            if key.time_us < 0 || !key.value.is_finite() {
                return Err(invalid("non-finite or negative keyframe".into()));
            }
            if !times.insert(key.time_us) {
                return Err(invalid(format!(
                    "{}: two keys at {} us",
                    track.property, key.time_us
                )));
            }
        }
        match &track.property {
            TrackProperty::Anim(prop) => {
                if adjustment && *prop != AnimProperty::Opacity {
                    return Err(invalid(
                        "adjustment layers support opacity and effect-parameter keyframes only"
                            .into(),
                    ));
                }
                for key in &track.keys {
                    if key.value.is_vector() {
                        return Err(invalid(format!("{prop:?} keyframes take a number")));
                    }
                    let v = key.value.num();
                    if *prop == AnimProperty::Opacity && !(0.0..=1.0).contains(&v) {
                        return Err(invalid("opacity keyframe must be in 0..=1".into()));
                    }
                    if matches!(prop, AnimProperty::FitX | AnimProperty::FitY)
                        && !(-1.0..=1.0).contains(&v)
                    {
                        return Err(invalid("fit keyframe must be in -1..=1".into()));
                    }
                    if *prop == AnimProperty::Speed && !(0.25..=4.0).contains(&v) {
                        return Err(invalid("speed keyframe must be in 0.25..=4".into()));
                    }
                }
            }
            TrackProperty::Effect { instance, param } => {
                let effect = stack.iter().find(|e| e.id == *instance).ok_or_else(|| {
                    invalid(format!(
                        "no effect instance {instance} on this clip (add it with SetClipEffects first)"
                    ))
                })?;
                let info = item_info(&mut ctx, &effect.item_ref).map_err(invalid)?;
                let manifest = info.manifest.ok_or_else(|| {
                    invalid(format!("'{}' has no animatable params", effect.item_ref))
                })?;
                let spec = manifest
                    .params
                    .iter()
                    .find(|p| &p.key == param)
                    .ok_or_else(|| {
                        let animatable: Vec<&str> = manifest
                            .params
                            .iter()
                            .filter(|p| animatable(p.kind, p.animatable))
                            .map(|p| p.key.as_str())
                            .collect();
                        invalid(format!(
                            "'{}' has no param '{param}' (animatable: {})",
                            effect.item_ref,
                            animatable.join(", ")
                        ))
                    })?;
                if !animatable(spec.kind, spec.animatable) {
                    return Err(invalid(format!(
                        "param '{param}' of '{}' is not animatable (bool, choice and LUT params \
                         and params marked animatable:false cannot be keyframed)",
                        effect.item_ref
                    )));
                }
                let want = spec.kind.components().max(1);
                for key in &track.keys {
                    let ok = match (&key.value, spec.kind) {
                        (KeyValue::Number(_), ParamKind::Float | ParamKind::Int) => true,
                        (KeyValue::Vector(v), ParamKind::Color | ParamKind::Vec2) => {
                            v.len() == want
                        }
                        _ => false,
                    };
                    if !ok {
                        return Err(invalid(format!(
                            "param '{param}' keys must be {}",
                            match spec.kind {
                                ParamKind::Color => "[r, g, b, a]".to_string(),
                                ParamKind::Vec2 => "[x, y]".to_string(),
                                _ => "numbers".to_string(),
                            }
                        )));
                    }
                }
            }
        }
        track.keys.sort_by_key(|k| k.time_us);
        out.push(track);
    }
    Ok(out)
}

fn animatable(kind: ParamKind, declared: bool) -> bool {
    declared
        && matches!(
            kind,
            ParamKind::Float | ParamKind::Int | ParamKind::Color | ParamKind::Vec2
        )
}

/// Inserting a key inside a Bezier span splits the curve so the motion does not change: when
/// `new` has a key the `old` track did not, strictly inside an `old` Bezier span, with both
/// handles omitted and a value on that curve, the key becomes a Bezier key and the span's
/// handles are re-cut (de Casteljau). Everything else is taken as written.
pub(super) fn complete_inserted_keys(old: &[KeyframeTrack], new: &mut [KeyframeTrack]) {
    for track in new.iter_mut() {
        let Some(old_track) = old.iter().find(|t| t.property == track.property) else {
            continue;
        };
        let mut old_keys = old_track.keys.clone();
        old_keys.sort_by_key(|k| k.time_us);
        let inserted: Vec<i64> = track
            .keys
            .iter()
            .filter(|k| !old_keys.iter().any(|o| o.time_us == k.time_us))
            .map(|k| k.time_us)
            .collect();
        for time in inserted {
            let Some(span) = old_keys
                .windows(2)
                .find(|w| w[0].time_us < time && time < w[1].time_us)
            else {
                continue;
            };
            if span[0].easing != Easing::Bezier {
                continue;
            }
            let Some(idx) = track.keys.iter().position(|k| k.time_us == time) else {
                continue;
            };
            let k = &track.keys[idx];
            if k.handle_left.is_some()
                || k.handle_right.is_some()
                || !matches!(k.easing, Easing::Linear | Easing::Bezier)
            {
                continue;
            }
            let (mut a, mut b) = (span[0].clone(), span[1].clone());
            let Some(inner) = keys::split_bezier_span(&mut a, &mut b, time) else {
                continue;
            };
            let on_curve = match (&inner.value, &k.value) {
                (KeyValue::Number(x), KeyValue::Number(y)) => {
                    (x - y).abs() <= 1e-6 * (1.0 + (b.num() - a.num()).abs())
                }
                (KeyValue::Vector(x), KeyValue::Vector(y)) => {
                    x.len() == y.len() && x.iter().zip(y).all(|(p, q)| (p - q).abs() <= 1e-6)
                }
                _ => false,
            };
            if !on_curve {
                continue;
            }
            track.keys[idx].easing = Easing::Bezier;
            track.keys[idx].handle_left = inner.handle_left;
            track.keys[idx].handle_right = inner.handle_right;
            if let Some(left) = track.keys.iter_mut().find(|k| k.time_us == a.time_us) {
                left.easing = Easing::Bezier;
                left.handle_right = a.handle_right;
            }
            if let Some(right) = track.keys.iter_mut().find(|k| k.time_us == b.time_us) {
                right.handle_left = b.handle_left;
            }
        }
    }
}

/// `SetKeyframeEasing`: set how the span after the key at `time` is interpolated. Presets that
/// are curves are written as `bezier` handles; `linear`, `hold` and `auto` clear the span's
/// handles.
pub(super) fn set_keyframe_easing(
    project: &mut Project,
    track_id: Id,
    clip_id: Id,
    property: TrackProperty,
    time: Time,
    easing: EasingChoice,
) -> Result<CommandOutcome, CommandError> {
    let invalid = |m: String| CommandError::InvalidKeyframes(m);
    let fps = project.settings.fps;
    let at = time.resolve(fps);
    let half_frame = (fps.frame_start(1) / 2).max(1);
    let tracks: &mut Vec<KeyframeTrack> = match super::any_clip_mut(project, track_id, clip_id)? {
        Clip::Video(m) | Clip::Audio(m) => &mut m.keyframes,
        Clip::Adjustment(a) => &mut a.keyframes,
        Clip::Text(t) => &mut t.keyframes,
        Clip::Caption(_) => return Err(CommandError::NotMediaClip(clip_id)),
    };
    let track = tracks
        .iter_mut()
        .find(|t| t.property == property)
        .ok_or_else(|| invalid(format!("the clip has no keyframe track '{property}'")))?;
    track.keys.sort_by_key(|k| k.time_us);
    let idx = track
        .keys
        .iter()
        .enumerate()
        .min_by_key(|(_, k)| (k.time_us - at).abs())
        .filter(|(_, k)| (k.time_us - at).abs() <= half_frame)
        .map(|(i, _)| i)
        .ok_or_else(|| {
            let times: Vec<String> = track.keys.iter().map(|k| k.time_us.to_string()).collect();
            invalid(format!(
                "no key of '{property}' at {at} us (keys at: {} us)",
                times.join(", ")
            ))
        })?;
    if idx + 1 >= track.keys.len() {
        return Err(invalid(
            "that is the last key: easing describes the span after a key, and there is none".into(),
        ));
    }
    let (left, right) = track.keys.split_at_mut(idx + 1);
    let a = &mut left[idx];
    let b = &mut right[0];
    match easing.bezier_points() {
        Some([x1, y1, x2, y2]) => {
            let span = (b.time_us - a.time_us) as f64;
            let scale = keys::span_scale(a, b);
            a.easing = Easing::Bezier;
            a.handle_right = Some(((x1 * span).round() as i64, y1 * scale));
            b.handle_left = Some((((x2 - 1.0) * span).round() as i64, (y2 - 1.0) * scale));
        }
        None => {
            a.easing = match easing {
                EasingChoice::Hold => Easing::Hold,
                EasingChoice::Auto => Easing::Auto,
                _ => Easing::Linear,
            };
            a.handle_right = None;
            b.handle_left = None;
        }
    }
    Ok(CommandOutcome::Applied)
}

// ---- deprecated v7 shapes ---------------------------------------------------------------

/// Rewrite the deprecated v7 shapes of one command object (a JSON object with a `command` key)
/// to their v8 form, in place; returns one deprecation message per rewrite. Accepted for one
/// release, like the `*_secs` time aliases:
///
/// - `SetClipEffects { effects: [{ effect_id, params }] }` (the whole effect list) becomes the
///   slots: a LUT in first position goes to `filter`, `key` and `adjust` are cleared, the rest
///   is `effects[]` in the same order, so what runs and in which order does not change.
///   `effect_id` is mapped to a versioned `ref` (`builtin:blur` -> `builtin:blur@1`,
///   `pack:<pack>:lut:<lut>` -> `pack:local.<pack>/<lut>@1`).
/// - `SetClipTransition { transition: { kind, duration_us } | null }` becomes
///   `{ ref: "builtin:<kind>@1", duration }` / `{ ref: null }`.
/// - `SetClipKeyframes` keys with `easing: "ease_in" | "ease_out"` become the exact `bezier`
///   handles of the same curve.
pub fn normalize_legacy_effect_params(value: &mut serde_json::Value) -> Vec<String> {
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
    match command.as_str() {
        "SetClipEffects" => {
            let legacy = obj
                .get("effects")
                .and_then(Value::as_array)
                .is_some_and(|list| {
                    list.iter()
                        .any(|e| e.get("effect_id").is_some() && e.get("ref").is_none())
                });
            if legacy {
                let list = obj
                    .get("effects")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let (filter, _key, rest) =
                    crate::project::migrate::split_legacy_effect_slots(&list, false);
                obj.insert("filter".into(), filter.unwrap_or(Value::Null));
                obj.insert("key".into(), Value::Null);
                obj.insert("adjust".into(), Value::Null);
                obj.insert("effects".into(), Value::Array(rest));
                warnings.push(
                    "SetClipEffects: `effects` entries with `effect_id` are deprecated; use \
                     `ref` (builtin:blur@1, pack:<pack>/<item>@1) and the `filter` / `key` / \
                     `adjust` / `effects` slots. `effects` now replaces only effects[]; the \
                     legacy form replaced the whole stack and was converted to do the same."
                        .to_string(),
                );
            }
        }
        "SetClipTransition" => {
            if let Some(tr) = obj.remove("transition") {
                match tr {
                    Value::Null => {
                        obj.insert("ref".into(), Value::Null);
                    }
                    Value::Object(t) => {
                        if let Some(kind) = t.get("kind").and_then(Value::as_str) {
                            obj.insert("ref".into(), json!(format!("builtin:{kind}@1")));
                        }
                        if let Some(us) = t.get("duration_us").and_then(Value::as_i64) {
                            obj.entry("duration").or_insert(json!(us));
                        } else if let Some(secs) = t.get("duration_secs").and_then(Value::as_f64) {
                            obj.entry("duration").or_insert(json!(format!("{secs}s")));
                        }
                    }
                    other => {
                        obj.insert("transition".into(), other);
                    }
                }
                warnings.push(
                    "SetClipTransition: `transition: { kind, duration_us }` is deprecated; use \
                     `ref` (builtin:crossfade@1, null to clear) and `duration`."
                        .to_string(),
                );
            }
        }
        "SetClipKeyframes" => {
            let mut converted = 0;
            if let Some(tracks) = obj.get_mut("keyframes").and_then(Value::as_array_mut) {
                for track in tracks {
                    if let Some(keys) = track.get_mut("keys").and_then(Value::as_array_mut) {
                        converted += crate::project::migrate::convert_legacy_easing(keys);
                    }
                }
            }
            if converted > 0 {
                warnings.push(
                    "SetClipKeyframes: easing `ease_in` / `ease_out` is deprecated; use \
                     `bezier` with handles, or SetKeyframeEasing with a preset."
                        .to_string(),
                );
            }
        }
        "LoadAssetPack" | "UnloadAssetPack" => warnings.push(format!(
            "{command}: packs live in the user library, not in the project. This installs / \
             uninstalls in the library (the same as `renderly pack install`); apply items by ref."
        )),
        _ => {}
    }
    warnings
}
