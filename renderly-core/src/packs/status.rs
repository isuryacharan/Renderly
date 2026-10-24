//! Resolving stored effect refs against loaded packs, and the load-time status of every effect
//! instance in a project (`ok`, `changed`, `missing`, `disabled`). Pure: packs come in as a
//! slice, so this builds for wasm32 too.
//!
//! The status is never stored. A `missing` or `disabled` instance renders pass-through and keeps
//! its params (docs/effect-pack-format.md "Effect references and versioning").

use serde::Serialize;

use super::refs::{self, ItemRef, Resolution};
use super::{CubeLut, LoadedItem, LoadedPack};
use crate::project::{Clip, EffectInstance, EffectStatus, Id, Project};

/// What a stored ref resolved to, with the pack it came from.
pub enum Target<'a> {
    /// A compiled effect, filter or transition (builtin or pack).
    Compiled(&'a LoadedItem),
    /// A pack LUT, applied through `builtin:lut_filter@1`.
    Lut {
        item: &'a LoadedItem,
        pack: &'a LoadedPack,
        cube: &'a CubeLut,
    },
}

/// The outcome of [`resolve_target`].
pub struct Resolved<'a> {
    pub status: EffectStatus,
    /// `None` when missing or disabled (render pass-through).
    pub target: Option<Target<'a>>,
}

/// Resolve `item_ref` (with the hash stored on the instance, if any) against the builtins and
/// `packs`.
pub fn resolve_target<'a>(
    packs: &'a [LoadedPack],
    item_ref: &str,
    hash: Option<&str>,
) -> Resolved<'a> {
    let Ok(parsed) = ItemRef::parse(item_ref) else {
        return Resolved {
            status: EffectStatus::Missing,
            target: None,
        };
    };
    let (status, item) = match refs::resolve(packs, &parsed, hash) {
        Resolution::Ok(item) => (EffectStatus::Ok, item),
        Resolution::Changed { item, .. } => (EffectStatus::Changed, item),
        Resolution::Missing => {
            return Resolved {
                status: EffectStatus::Missing,
                target: None,
            }
        }
        Resolution::Disabled { .. } => {
            return Resolved {
                status: EffectStatus::Disabled,
                target: None,
            }
        }
    };
    // A format-1 transition alias (`pack:local.starter/whip@1`) runs the builtin it names.
    let item = match &item.alias_of {
        Some(kind) => {
            let builtin = ItemRef::Builtin {
                id: kind.clone(),
                version: 1,
            };
            match refs::builtin_item(&builtin) {
                Some(b) => b,
                None => {
                    return Resolved {
                        status: EffectStatus::Disabled,
                        target: None,
                    }
                }
            }
        }
        None => item,
    };
    let target = if item.kind == "lut" {
        let pack = packs
            .iter()
            .find(|p| p.items.iter().any(|i| std::ptr::eq(i, item)));
        pack.and_then(|p| {
            p.luts
                .get(&super::lut_key(&item.id, item.version))
                .map(|cube| (p, cube))
        })
        .map(|(pack, cube)| Target::Lut { item, pack, cube })
    } else if item.compiled.is_some() {
        Some(Target::Compiled(item))
    } else {
        None
    };
    match target {
        Some(t) => Resolved {
            status,
            target: Some(t),
        },
        None => Resolved {
            status: EffectStatus::Disabled,
            target: None,
        },
    }
}

/// Status of one stored ref. `wasm:*` refs are reported `ok` (plugins are checked by the host
/// that loads them).
pub fn effect_status(packs: &[LoadedPack], item_ref: &str, hash: Option<&str>) -> EffectStatus {
    if item_ref.starts_with("wasm:") {
        return EffectStatus::Ok;
    }
    resolve_target(packs, item_ref, hash).status
}

/// One effect instance or transition that is not `ok`, for perception.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusEntry {
    pub clip_id: Id,
    /// The effect instance id, or `None` for a transition.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<Id>,
    #[serde(rename = "ref")]
    pub item_ref: String,
    pub status: EffectStatus,
}

/// The status of a caption / text style ref. Styles are the builtin caption styles
/// (`builtin:<id>@1`) and installed format-1 pack caption styles; anything else is `missing`
/// (the caption then renders with the track's or the default style).
pub fn text_style_status(packs: &[LoadedPack], item_ref: &str) -> EffectStatus {
    let known = |id: &str| {
        crate::captions::BUILTIN_STYLES.contains(&id)
            || super::find_caption_style(packs, id).is_some()
    };
    match ItemRef::parse(item_ref) {
        Ok(ItemRef::Builtin { id, version }) if version == 1 && known(&id) => EffectStatus::Ok,
        Ok(ItemRef::Pack { item, .. }) if super::find_caption_style(packs, &item).is_some() => {
            EffectStatus::Ok
        }
        _ => EffectStatus::Missing,
    }
}

/// Every effect instance and transition in the project whose status is not `ok`, in timeline
/// order (root tracks, then sequences). Compact on purpose: a healthy project returns nothing.
pub fn problem_effects(project: &Project, packs: &[LoadedPack]) -> Vec<StatusEntry> {
    let mut out = Vec::new();
    for track in project.all_tracks() {
        if let Some(style) = &track.caption_style {
            let status = text_style_status(packs, &style.item_ref);
            if status != EffectStatus::Ok {
                out.push(StatusEntry {
                    clip_id: track.id,
                    instance_id: None,
                    item_ref: style.item_ref.clone(),
                    status,
                });
            }
        }
        for clip in &track.clips {
            let (clip_id, stack): (Id, Vec<&EffectInstance>) = match clip {
                Clip::Video(m) | Clip::Audio(m) => (m.id, m.effect_stack()),
                Clip::Adjustment(a) => (a.id, a.effect_stack()),
                Clip::Text(t) => (t.id, t.effect_stack()),
                Clip::Caption(c) => {
                    if let Some(style) = c.style.as_ref() {
                        let status = text_style_status(packs, &style.item_ref);
                        if status != EffectStatus::Ok {
                            out.push(StatusEntry {
                                clip_id: c.id,
                                instance_id: None,
                                item_ref: style.item_ref.clone(),
                                status,
                            });
                        }
                    }
                    continue;
                }
            };
            for e in stack {
                let status = effect_status(packs, &e.item_ref, e.hash.as_deref());
                if status != EffectStatus::Ok {
                    out.push(StatusEntry {
                        clip_id,
                        instance_id: Some(e.id),
                        item_ref: e.item_ref.clone(),
                        status,
                    });
                }
            }
            let style = match clip {
                Clip::Text(t) => Some(&t.style),
                _ => None,
            };
            if let Some(style) = style {
                let status = text_style_status(packs, &style.item_ref);
                if status != EffectStatus::Ok {
                    out.push(StatusEntry {
                        clip_id: clip.id(),
                        instance_id: None,
                        item_ref: style.item_ref.clone(),
                        status,
                    });
                }
            }
            let transition = match clip {
                Clip::Text(t) => t.outgoing_transition.as_ref(),
                other => other
                    .as_media()
                    .and_then(|m| m.outgoing_transition.as_ref()),
            };
            if let Some(tr) = transition {
                let status = effect_status(packs, &tr.item_ref, tr.hash.as_deref());
                if status != EffectStatus::Ok {
                    out.push(StatusEntry {
                        clip_id,
                        instance_id: None,
                        item_ref: tr.item_ref.clone(),
                        status,
                    });
                }
            }
        }
    }
    out
}

/// [`problem_effects`] against the packs installed in the user library. The library is only
/// read when the project uses a `pack:` ref (a builtin-only project needs no packs).
#[cfg(not(target_arch = "wasm32"))]
pub fn installed_problem_effects(project: &Project) -> Vec<StatusEntry> {
    let styles_need_packs = project.all_tracks().any(|t| {
        t.caption_style
            .iter()
            .chain(t.clips.iter().filter_map(|c| match c {
                Clip::Caption(c) => c.style.as_ref(),
                Clip::Text(x) => Some(&x.style),
                _ => None,
            }))
            .any(|s| text_style_status(&[], &s.item_ref) != EffectStatus::Ok)
    });
    let packs = if super::referenced_pack_items(project).is_empty() && !styles_need_packs {
        Vec::new()
    } else {
        super::load_installed_packs()
    };
    problem_effects(project, &packs)
}

/// The status of every instance on one clip, by instance id (all of them, ok included).
pub fn clip_effect_statuses(clip: &Clip, packs: &[LoadedPack]) -> Vec<(Id, EffectStatus)> {
    let stack: Vec<&EffectInstance> = match clip {
        Clip::Video(m) | Clip::Audio(m) => m.effect_stack(),
        Clip::Adjustment(a) => a.effect_stack(),
        Clip::Text(t) => t.effect_stack(),
        Clip::Caption(_) => Vec::new(),
    };
    stack
        .into_iter()
        .map(|e| (e.id, effect_status(packs, &e.item_ref, e.hash.as_deref())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_refs_are_ok_and_unknown_are_missing() {
        let packs: [LoadedPack; 0] = [];
        assert_eq!(
            effect_status(&packs, "builtin:blur@1", None),
            EffectStatus::Ok
        );
        assert_eq!(
            effect_status(&packs, "builtin:blur@2", None),
            EffectStatus::Missing
        );
        assert_eq!(
            effect_status(&packs, "pack:mira.film/grain@1", Some("sha256:x")),
            EffectStatus::Missing
        );
        assert_eq!(
            effect_status(&packs, "not a ref", None),
            EffectStatus::Missing
        );
        assert_eq!(effect_status(&packs, "wasm:gain", None), EffectStatus::Ok);
    }
}
