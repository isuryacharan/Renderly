//! Project -> OTIO. See docs/otio.md for the mapping table.

use super::time::{frames, frames_ceil, rational_time, time_range};
use super::url::{self, is_absolute_path, path_to_file_url};
use super::{
    ExportOptions, OtioError, CAPTION_GENERATOR_KIND, RENDERLY_METADATA_FORMAT,
    RENDERLY_METADATA_KEY,
};
use crate::project::{
    AnimProperty, CaptionClip, Clip, ClipTransition, EffectInstance, Id, MediaClip, MediaItem,
    MediaKind, Project, Track, TrackKind, TransitionKind, MAX_SEQUENCE_DEPTH, SCHEMA_VERSION,
};
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use std::path::Path;

const DEFAULT_MEDIA_KEY: &str = "DEFAULT_MEDIA";

/// Export `project` with default options.
pub fn export_otio(project: &Project) -> Result<String, OtioError> {
    export_otio_with_options(project, &ExportOptions::default())
}

/// Export `project` as pretty-printed OTIO JSON (a `Timeline.1`).
pub fn export_otio_with_options(
    project: &Project,
    options: &ExportOptions,
) -> Result<String, OtioError> {
    let fps = project.settings.fps;
    if !(fps.is_finite() && fps > 0.0 && fps <= 1000.0) {
        return Err(OtioError::BadRate(fps));
    }
    let mut ex = Exporter {
        project,
        fps,
        base_dir: options.base_dir.as_deref(),
        placed: HashSet::new(),
    };
    let tracks = ex.tracks(&project.tracks, 0)?;
    let meta = ex.timeline_metadata()?;
    let timeline = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": project.name,
        "global_start_time": rational_time(0, fps),
        "metadata": { RENDERLY_METADATA_KEY: meta },
        "tracks": {
            "OTIO_SCHEMA": "Stack.1",
            "name": "tracks",
            "children": tracks,
            "effects": [],
            "markers": [],
            "enabled": true,
            "source_range": null,
            "metadata": {},
        },
    });
    serde_json::to_string_pretty(&timeline).map_err(OtioError::Serialize)
}

struct Exporter<'a> {
    project: &'a Project,
    fps: f64,
    base_dir: Option<&'a Path>,
    /// Sequences written as a nested stack (the rest go to `unplaced_sequences`).
    placed: HashSet<Id>,
}

fn renderly_meta(value: Value) -> Value {
    json!({ RENDERLY_METADATA_KEY: value })
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, OtioError> {
    serde_json::to_value(v).map_err(OtioError::Serialize)
}

impl Exporter<'_> {
    fn tracks(&mut self, tracks: &[Track], depth: usize) -> Result<Vec<Value>, OtioError> {
        tracks.iter().map(|t| self.track(t, depth)).collect()
    }

    fn track(&mut self, t: &Track, depth: usize) -> Result<Value, OtioError> {
        let mut order: Vec<&Clip> = t.clips.iter().collect();
        order.sort_by(|a, b| {
            a.position_secs()
                .partial_cmp(&b.position_secs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Layout in whole frames. Ends are snapped (not durations), so clips that touch in
        // the project still touch in OTIO; a clip that would start before the previous one
        // ends after snapping is pushed to the cursor, and every clip is at least a frame.
        let mut cursor = 0_i64;
        let mut layout: Vec<(i64, i64)> = Vec::with_capacity(order.len());
        for c in &order {
            let mut start = frames(c.position_secs(), self.fps);
            let end = frames(c.end_secs(), self.fps);
            if start < cursor {
                start = cursor;
            }
            let dur = (end - start).max(1);
            layout.push((start, dur));
            cursor = start + dur;
        }

        let mut children: Vec<Value> = Vec::new();
        let mut cursor = 0_i64;
        for (i, clip) in order.iter().enumerate() {
            let (start, dur) = layout[i];
            if start > cursor {
                children.push(self.gap(start - cursor));
            }
            children.push(self.item(clip, start, dur, depth)?);
            cursor = start + dur;
            if t.kind == TrackKind::Video {
                if let Some(tr) = clip.as_media().and_then(|m| m.outgoing_transition.as_ref()) {
                    // A native Transition only exists between two touching items.
                    if layout.get(i + 1).is_some_and(|&(next, _)| next == cursor) {
                        children.push(self.transition(tr, dur)?);
                    }
                }
            }
        }

        let kind = match t.kind {
            TrackKind::Audio => "Audio",
            TrackKind::Video | TrackKind::Caption => "Video",
        };
        let hides = match t.kind {
            TrackKind::Audio => t.muted,
            TrackKind::Video | TrackKind::Caption => t.hidden,
        };
        Ok(json!({
            "OTIO_SCHEMA": "Track.1",
            "name": t.name,
            "kind": kind,
            "children": children,
            "effects": [],
            "markers": [],
            "enabled": !hides,
            "source_range": null,
            "metadata": renderly_meta(json!({
                "track": {
                    "id": t.id,
                    "kind": t.kind,
                    "audio_role": t.audio_role,
                    "muted": t.muted,
                    "locked": t.locked,
                    "hidden": t.hidden,
                }
            })),
        }))
    }

    fn gap(&self, dur: i64) -> Value {
        json!({
            "OTIO_SCHEMA": "Gap.1",
            "name": "",
            "source_range": time_range(0, dur, self.fps),
            "effects": [],
            "markers": [],
            "enabled": true,
            "metadata": {},
        })
    }

    fn item(
        &mut self,
        clip: &Clip,
        start: i64,
        dur: i64,
        depth: usize,
    ) -> Result<Value, OtioError> {
        match clip {
            Clip::Video(m) | Clip::Audio(m) => self.media_item(m, start, dur, depth),
            Clip::Caption(c) => self.caption_item(c, dur),
            Clip::Adjustment(a) => Ok(json!({
                "OTIO_SCHEMA": "Gap.1",
                "name": "Adjustment layer",
                "source_range": time_range(0, dur, self.fps),
                "effects": self.effects(&a.effects),
                "markers": [],
                "enabled": true,
                "metadata": renderly_meta(json!({ "adjustment": to_json(a)? })),
            })),
        }
    }

    fn effects(&self, effects: &[EffectInstance]) -> Vec<Value> {
        effects
            .iter()
            .map(|e| {
                json!({
                    "OTIO_SCHEMA": "Effect.1",
                    "name": e.effect_id,
                    "effect_name": e.effect_id,
                    "enabled": e.enabled,
                    "metadata": renderly_meta(json!({ "id": e.id, "params": e.params })),
                })
            })
            .collect()
    }

    /// The time warp that stands for a clip's speed, if any: the constant `speed`, or for
    /// speed keyframes the average (source span / timeline span).
    fn time_warp(&self, m: &MediaClip) -> Option<Value> {
        let keyed = m
            .keyframes
            .iter()
            .any(|k| k.property == AnimProperty::Speed && !k.keys.is_empty());
        let scalar = if keyed {
            let tl = m.timeline_duration_secs();
            (tl > 0.0).then(|| (m.source_out_secs - m.source_in_secs) / tl)?
        } else {
            m.speed_factor()
        };
        ((scalar - 1.0).abs() > 1e-9).then(|| {
            json!({
                "OTIO_SCHEMA": "LinearTimeWarp.1",
                "name": "",
                "effect_name": "LinearTimeWarp",
                "time_scalar": scalar,
                "enabled": true,
                "metadata": renderly_meta(json!({ "speed_keyframed": keyed })),
            })
        })
    }

    fn media_item(
        &mut self,
        m: &MediaClip,
        start: i64,
        dur: i64,
        depth: usize,
    ) -> Result<Value, OtioError> {
        let media = self.project.find_media(m.media_id).ok_or_else(|| {
            OtioError::Invalid(format!(
                "clip {} references unknown media {}",
                m.id, m.media_id
            ))
        })?;
        let _ = start;
        let mut effects: Vec<Value> = self.time_warp(m).into_iter().collect();
        effects.extend(self.effects(&m.effects));
        let source_range = time_range(frames(m.source_in_secs, self.fps), dur, self.fps);
        let metadata = renderly_meta(json!({ "clip": to_json(m)? }));

        if media.kind == MediaKind::Sequence {
            let seq_id = media.sequence_id.ok_or_else(|| {
                OtioError::Invalid(format!("sequence media {} has no sequence_id", media.id))
            })?;
            if depth >= MAX_SEQUENCE_DEPTH {
                return Err(OtioError::Invalid(format!(
                    "sequence {seq_id} is nested deeper than {MAX_SEQUENCE_DEPTH}"
                )));
            }
            let seq = self
                .project
                .find_sequence(seq_id)
                .ok_or_else(|| OtioError::Invalid(format!("unknown sequence {seq_id}")))?;
            self.placed.insert(seq_id);
            let children = self.tracks(&seq.tracks, depth + 1)?;
            let mut meta = metadata;
            meta[RENDERLY_METADATA_KEY]["sequence"] = json!({
                "id": seq.id, "name": seq.name, "width": seq.width, "height": seq.height,
            });
            return Ok(json!({
                "OTIO_SCHEMA": "Stack.1",
                "name": seq.name,
                "children": children,
                "source_range": source_range,
                "effects": effects,
                "markers": [],
                "enabled": m.enabled,
                "metadata": meta,
            }));
        }

        Ok(json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": file_name(&media.path.to_string_lossy()),
            "source_range": source_range,
            "media_references": { DEFAULT_MEDIA_KEY: self.external_reference(media) },
            "active_media_reference_key": DEFAULT_MEDIA_KEY,
            "effects": effects,
            "markers": [],
            "enabled": m.enabled,
            "metadata": metadata,
        }))
    }

    fn external_reference(&self, media: &MediaItem) -> Value {
        let path = media.path.to_string_lossy().into_owned();
        let abs = match self.base_dir {
            Some(base) if !is_absolute_path(&path) => url::join(&base.to_string_lossy(), &path),
            _ => path.clone(),
        };
        let available = media
            .duration_secs
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| time_range(0, frames_ceil(d, self.fps).max(1), self.fps));
        json!({
            "OTIO_SCHEMA": "ExternalReference.1",
            "name": file_name(&path),
            "target_url": path_to_file_url(&abs),
            "available_range": available,
            "available_image_bounds": null,
            "metadata": {},
        })
    }

    /// A caption has no OTIO primitive. It is written as a clip on a `Video` track whose
    /// media is a `GeneratorReference` (OTIO's own notion of synthesised media such as
    /// titles) with the text and style as generator parameters — so other tools see a
    /// correctly timed, named title clip — and the exact caption under `metadata`.
    fn caption_item(&self, c: &CaptionClip, dur: i64) -> Result<Value, OtioError> {
        Ok(json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": c.text,
            "source_range": time_range(0, dur, self.fps),
            "media_references": { DEFAULT_MEDIA_KEY: {
                "OTIO_SCHEMA": "GeneratorReference.1",
                "name": "caption",
                "generator_kind": CAPTION_GENERATOR_KIND,
                "parameters": { "text": c.text, "style_id": c.style_id },
                "available_range": time_range(0, dur, self.fps),
                "available_image_bounds": null,
                "metadata": {},
            }},
            "active_media_reference_key": DEFAULT_MEDIA_KEY,
            "effects": [],
            "markers": [],
            "enabled": true,
            "metadata": renderly_meta(json!({ "caption": to_json(c)? })),
        }))
    }

    /// `crossfade` is OTIO's `SMPTE_Dissolve`; every other kind is `Custom_Transition` with
    /// the Renderly kind in metadata. Renderly's window ends at the cut, so the whole length
    /// is `in_offset` (the part of the outgoing clip it overlaps) and `out_offset` is 0.
    fn transition(&self, tr: &ClipTransition, prev_dur: i64) -> Result<Value, OtioError> {
        let in_offset = frames(tr.duration_secs, self.fps).clamp(1, prev_dur.max(1));
        let transition_type = if tr.kind == TransitionKind::Crossfade {
            "SMPTE_Dissolve"
        } else {
            "Custom_Transition"
        };
        Ok(json!({
            "OTIO_SCHEMA": "Transition.1",
            "name": tr.kind.as_str(),
            "transition_type": transition_type,
            "in_offset": rational_time(in_offset, self.fps),
            "out_offset": rational_time(0, self.fps),
            "metadata": renderly_meta(json!({ "transition": to_json(tr)? })),
        }))
    }

    fn timeline_metadata(&self) -> Result<Value, OtioError> {
        let p = self.project;
        let unplaced: Vec<&crate::project::Sequence> = p
            .sequences
            .iter()
            .filter(|s| !self.placed.contains(&s.id))
            .collect();
        let mut meta = Map::new();
        meta.insert("format".into(), json!(RENDERLY_METADATA_FORMAT));
        meta.insert("schema_version".into(), json!(SCHEMA_VERSION));
        meta.insert("project_id".into(), to_json(&p.id)?);
        meta.insert("settings".into(), to_json(&p.settings)?);
        meta.insert("media".into(), to_json(&p.media)?);
        meta.insert("asset_pack_paths".into(), to_json(&p.asset_pack_paths)?);
        meta.insert("wasm_plugin_paths".into(), to_json(&p.wasm_plugin_paths)?);
        meta.insert("multicam_groups".into(), to_json(&p.multicam_groups)?);
        meta.insert(
            "segmentation_model_path".into(),
            to_json(&p.segmentation_model_path)?,
        );
        meta.insert("unplaced_sequences".into(), to_json(&unplaced)?);
        Ok(Value::Object(meta))
    }
}

fn file_name(path: &str) -> String {
    let p = path.replace('\\', "/");
    p.rsplit('/').next().unwrap_or("").to_string()
}
