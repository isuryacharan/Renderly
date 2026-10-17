//! Preview overrides: temporary, render-only changes to clips (docs/architecture.md "History,
//! transactions and preview overrides").
//!
//! An override is not a project change. It is never saved, never recorded in undo, and never a
//! [`Command`](crate::commands). The shared render path applies overrides by building a
//! projected view of the project before evaluation, so every consumer honours them the same way:
//! the native `FrameRenderer` (live `render_frame`, MCP `look`) and the wasm compositor (webview
//! preview). Export does not apply overrides.
//!
//! Keyframes still win over the static fields an override replaces, exactly as they win over the
//! saved transform, so an override never changes how an animated property is evaluated.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::project::{Clip, ClipTransform, EffectInstance, Id, Project};

/// What a single clip renders while the override is set. Every field is optional. A field that
/// is `None` leaves the clip's saved value in place.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PreviewOverride {
    /// Replaces the clip's static transform. Opacity is clamped to 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<ClipTransform>,
    /// Replaces the clip's opacity: a media clip's `transform.opacity`, or an adjustment
    /// layer's `opacity`. Clamped to 0..=1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// Replaces the clip's whole effect stack. Hover preview sends the current stack plus one
    /// temporary instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Vec<EffectInstance>>,
}

impl PreviewOverride {
    /// True when the override changes nothing. Setting an empty override clears the target.
    pub fn is_empty(&self) -> bool {
        self.transform.is_none() && self.opacity.is_none() && self.effects.is_none()
    }
}

/// Wire shape for one override: the target clip id plus the override's fields, flattened.
/// For example `{"target": "<clip id>", "effects": [...]}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewOverrideEntry {
    pub target: Id,
    #[serde(flatten)]
    pub over: PreviewOverride,
}

/// The set of active overrides, keyed by clip id. Empty by default, which is the state a project
/// is in when nothing is being previewed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PreviewOverrides {
    entries: BTreeMap<Id, PreviewOverride>,
}

impl PreviewOverrides {
    /// No overrides.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set (or replace) the override for `target`. An empty override clears the target instead.
    pub fn set(&mut self, target: Id, over: PreviewOverride) {
        if over.is_empty() {
            self.entries.remove(&target);
        } else {
            self.entries.insert(target, over);
        }
    }

    /// Set the override from its wire form.
    pub fn set_entry(&mut self, entry: PreviewOverrideEntry) {
        self.set(entry.target, entry.over);
    }

    /// Clear the override for `target`. Returns whether one was set.
    pub fn clear(&mut self, target: Id) -> bool {
        self.entries.remove(&target).is_some()
    }

    /// Clear every override.
    pub fn clear_all(&mut self) {
        self.entries.clear();
    }

    /// True when no overrides are set.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of targets with an override set.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The override set for `target`, if any.
    pub fn get(&self, target: Id) -> Option<&PreviewOverride> {
        self.entries.get(&target)
    }

    /// The project as it should be rendered: `project` with every overridden clip's fields
    /// replaced. Borrows `project` when there are no overrides, so the common path costs nothing.
    /// The input is never mutated. Targets that do not exist in the project are ignored.
    pub fn apply<'a>(&self, project: &'a Project) -> Cow<'a, Project> {
        if self.entries.is_empty() {
            return Cow::Borrowed(project);
        }
        let mut view = project.clone();
        for track in view.all_tracks_mut() {
            for clip in track.clips.iter_mut() {
                match clip {
                    Clip::Video(m) | Clip::Audio(m) => {
                        let Some(over) = self.entries.get(&m.id) else {
                            continue;
                        };
                        if let Some(t) = &over.transform {
                            m.transform = t.clamp_opacity();
                        }
                        if let Some(op) = over.opacity {
                            m.transform.opacity = clamp_unit(op);
                        }
                        if let Some(fx) = &over.effects {
                            m.effects = fx.clone();
                        }
                    }
                    Clip::Adjustment(a) => {
                        let Some(over) = self.entries.get(&a.id) else {
                            continue;
                        };
                        if let Some(op) = over.opacity {
                            a.opacity = clamp_unit(op);
                        }
                        if let Some(fx) = &over.effects {
                            a.effects = fx.clone();
                        }
                    }
                    Clip::Caption(_) => {}
                }
            }
        }
        Cow::Owned(view)
    }
}

/// Clamp to 0..=1. Non-finite input collapses to 0 (fully transparent) rather than leaking NaN
/// into the compositor.
fn clamp_unit(v: f64) -> f64 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{
        AdjustmentClip, MediaClip, MediaItem, MediaKind, Sequence, Settings, Track, TrackKind,
    };
    use std::collections::BTreeMap as Map;

    fn effect(effect_id: &str, radius: f64) -> EffectInstance {
        let mut params = Map::new();
        params.insert("radius".to_string(), radius);
        EffectInstance {
            id: Id::new_v4(),
            effect_id: effect_id.to_string(),
            enabled: true,
            params,
        }
    }

    fn media_clip(id: Id, media_id: Id) -> MediaClip {
        MediaClip {
            id,
            media_id,
            position_secs: 0.0,
            source_in_secs: 0.0,
            source_out_secs: 2.0,
            gain_db: 0.0,
            enabled: true,
            fade_in_secs: 0.0,
            fade_out_secs: 0.0,
            speed: 1.0,
            transform: ClipTransform::default(),
            keyframes: Vec::new(),
            effects: vec![effect("builtin:blur", 2.0)],
            outgoing_transition: None,
            mask: None,
            background_removal: None,
            audio_denoise: None,
            multicam_group_id: None,
        }
    }

    fn project_with_clips() -> (Project, Id, Id, Id) {
        let mut project = Project::new("overrides", Settings::default());
        let media_id = Id::new_v4();
        project.media.push(MediaItem {
            id: media_id,
            path: "clip.mp4".into(),
            kind: MediaKind::Video,
            duration_secs: Some(2.0),
            width: Some(1920),
            height: Some(1080),
            fps: None,
            sequence_id: None,
        });
        let mut track = Track::new(TrackKind::Video, "V1");
        let media_clip_id = Id::new_v4();
        track
            .clips
            .push(Clip::Video(media_clip(media_clip_id, media_id)));
        let adj_id = Id::new_v4();
        track.clips.push(Clip::Adjustment(AdjustmentClip {
            id: adj_id,
            position_secs: 0.0,
            duration_secs: 2.0,
            enabled: true,
            opacity: 1.0,
            effects: Vec::new(),
            mask: None,
            keyframes: Vec::new(),
        }));
        project.tracks.push(track);
        (project, media_clip_id, adj_id, media_id)
    }

    /// `Project` has no `PartialEq`, so compare what would be saved.
    fn json(project: &Project) -> serde_json::Value {
        serde_json::to_value(project).expect("serialize project")
    }

    fn media_of(project: &Project, id: Id) -> &MediaClip {
        project
            .all_tracks()
            .flat_map(|t| t.clips.iter())
            .find_map(|c| match c {
                Clip::Video(m) if m.id == id => Some(m),
                _ => None,
            })
            .expect("media clip")
    }

    #[test]
    fn no_overrides_borrows_the_project() {
        let (project, _, _, _) = project_with_clips();
        let overrides = PreviewOverrides::new();
        assert!(matches!(overrides.apply(&project), Cow::Borrowed(_)));
    }

    #[test]
    fn effects_override_replaces_the_stack_and_leaves_the_project_alone() {
        let (project, clip_id, _, _) = project_with_clips();
        let mut overrides = PreviewOverrides::new();
        overrides.set(
            clip_id,
            PreviewOverride {
                effects: Some(vec![effect("builtin:color_adjust", 0.0)]),
                ..Default::default()
            },
        );
        let view = overrides.apply(&project);
        assert_eq!(
            media_of(&view, clip_id).effects[0].effect_id,
            "builtin:color_adjust"
        );
        assert_eq!(
            media_of(&project, clip_id).effects[0].effect_id,
            "builtin:blur"
        );
    }

    #[test]
    fn opacity_override_is_clamped_and_applied_to_media_and_adjustment_clips() {
        let (project, media_id, adj_id, _) = project_with_clips();
        let mut overrides = PreviewOverrides::new();
        overrides.set(
            media_id,
            PreviewOverride {
                opacity: Some(3.0),
                ..Default::default()
            },
        );
        overrides.set(
            adj_id,
            PreviewOverride {
                opacity: Some(0.25),
                ..Default::default()
            },
        );
        let view = overrides.apply(&project);
        assert_eq!(media_of(&view, media_id).transform.opacity, 1.0);
        let adj = view
            .all_tracks()
            .flat_map(|t| t.clips.iter())
            .find_map(|c| match c {
                Clip::Adjustment(a) if a.id == adj_id => Some(a),
                _ => None,
            })
            .expect("adjustment");
        assert_eq!(adj.opacity, 0.25);
    }

    #[test]
    fn transform_override_replaces_the_static_transform() {
        let (project, clip_id, _, _) = project_with_clips();
        let mut overrides = PreviewOverrides::new();
        let t = ClipTransform {
            x: 0.5,
            scale_x: 2.0,
            ..ClipTransform::default()
        };
        overrides.set(
            clip_id,
            PreviewOverride {
                transform: Some(t),
                ..Default::default()
            },
        );
        let view = overrides.apply(&project);
        let m = media_of(&view, clip_id);
        assert_eq!(m.transform.x, 0.5);
        assert_eq!(m.transform.scale_x, 2.0);
        assert_eq!(media_of(&project, clip_id).transform.x, 0.0);
    }

    #[test]
    fn clearing_restores_the_original_project() {
        let (project, clip_id, _, _) = project_with_clips();
        let mut overrides = PreviewOverrides::new();
        overrides.set(
            clip_id,
            PreviewOverride {
                effects: Some(Vec::new()),
                ..Default::default()
            },
        );
        assert!(overrides.clear(clip_id));
        assert!(
            !overrides.clear(clip_id),
            "second clear reports nothing to clear"
        );
        assert!(overrides.is_empty());
        let view = overrides.apply(&project);
        assert!(matches!(view, Cow::Borrowed(_)));
        assert_eq!(json(&view), json(&project));
    }

    #[test]
    fn an_empty_override_clears_the_target() {
        let (_, clip_id, _, _) = project_with_clips();
        let mut overrides = PreviewOverrides::new();
        overrides.set(
            clip_id,
            PreviewOverride {
                opacity: Some(0.5),
                ..Default::default()
            },
        );
        overrides.set(clip_id, PreviewOverride::default());
        assert!(overrides.is_empty());
    }

    #[test]
    fn unknown_targets_are_ignored() {
        let (project, _, _, _) = project_with_clips();
        let mut overrides = PreviewOverrides::new();
        overrides.set(
            Id::new_v4(),
            PreviewOverride {
                opacity: Some(0.0),
                ..Default::default()
            },
        );
        assert_eq!(json(&overrides.apply(&project)), json(&project));
    }

    #[test]
    fn overrides_reach_clips_inside_sequences() {
        let (mut project, _, _, media_id) = project_with_clips();
        let seq_clip_id = Id::new_v4();
        let mut seq_track = Track::new(TrackKind::Video, "Seq V1");
        seq_track
            .clips
            .push(Clip::Video(media_clip(seq_clip_id, media_id)));
        let seq_id = Id::new_v4();
        project.sequences.push(Sequence {
            id: seq_id,
            name: "inner".into(),
            width: None,
            height: None,
            tracks: vec![seq_track],
        });
        let mut overrides = PreviewOverrides::new();
        overrides.set(
            seq_clip_id,
            PreviewOverride {
                opacity: Some(0.5),
                ..Default::default()
            },
        );
        let view = overrides.apply(&project);
        assert_eq!(media_of(&view, seq_clip_id).transform.opacity, 0.5);
    }

    #[test]
    fn wire_entry_round_trips_through_json() {
        let clip_id = Id::new_v4();
        let entry: PreviewOverrideEntry = serde_json::from_value(serde_json::json!({
            "target": clip_id,
            "effects": [{ "id": Id::new_v4(), "effect_id": "builtin:blur", "params": { "radius": 4.0 } }],
        }))
        .expect("parse");
        assert_eq!(entry.target, clip_id);
        let fx = entry.over.effects.as_ref().expect("effects");
        assert_eq!(fx[0].params.get("radius"), Some(&4.0));
        assert!(fx[0].enabled, "enabled defaults to true");

        let mut overrides = PreviewOverrides::new();
        overrides.set_entry(entry);
        assert_eq!(overrides.len(), 1);
        assert!(overrides.get(clip_id).is_some());
    }

    #[test]
    fn non_finite_opacity_becomes_transparent() {
        assert_eq!(clamp_unit(f64::NAN), 0.0);
        assert_eq!(clamp_unit(f64::INFINITY), 0.0);
        assert_eq!(clamp_unit(-1.0), 0.0);
        assert_eq!(clamp_unit(0.4), 0.4);
    }
}
