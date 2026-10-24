//! Stepwise project migrations (docs/project-schema.md "Loading and migration").
//!
//! `Project::from_json` parses a file to a `serde_json::Value`, runs the steps below on it in
//! order, deserialises into the current types and validates. A step is a pure function on the
//! `Value`; it exists only where stored data changes. Files at schema 1..=6 need no data change
//! (serde defaults cover the fields they lack), so the first real step is v7 -> v8.
//!
//! The v7 -> v8 step is a sequence of sub-steps, one per area of the schema that changed:
//! - `time`: `*_secs` floats become integer `*_us`, `settings.fps` becomes a rational.
//! - `effects`: `effect_id` becomes a versioned `ref`; the effect list is split into the
//!   `filter` / `key` slots and `effects[]` without changing the order anything runs in.
//! - `transitions`: `kind` becomes `ref: builtin:<kind>@1`.
//! - `easing`: v7 `ease_in` / `ease_out` become the exact `bezier` handles of the same curve.
//! - `packs`: `asset_pack_paths` / `wasm_plugin_paths` become `legacy_pack_paths`.
//! - `captions`: `text` becomes `body.text`, `style_id` becomes a `builtin:<id>@1` style ref on
//!   the track (when every clip shares it) or on each clip; tracks gain `magnetic: false`, the
//!   project gains empty `markers` / `transcripts`.
//!
//! Later sub-steps are appended to [`v7_to_v8`].
//!
//! Fixtures under `renderly-core/tests/fixtures/projects/v1..v7.renderly.json` pin the expected
//! output of every step.

use serde_json::{json, Map, Value};

use super::ProjectLoadError;
use crate::time::{snap_within, Fps};

/// Schema version this build writes.
pub const CURRENT_VERSION: u32 = 8;

/// Tolerance for snapping a migrated frame-positioned time onto the frame grid: one millisecond.
const SNAP_TOLERANCE_US: i64 = 1_000;

/// What a migration did, so hosts can back the original file up before the first save.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MigrationReport {
    /// Schema version the file was written with.
    pub from_version: u32,
    /// Names of the sub-steps that ran, in order (empty when the file was already current).
    pub steps: Vec<&'static str>,
}

impl MigrationReport {
    /// True when the file was older than the current schema and was changed in memory.
    pub fn migrated(&self) -> bool {
        !self.steps.is_empty()
    }
}

/// Run every migration needed to bring `value` to [`CURRENT_VERSION`].
pub fn migrate_to_current(mut value: Value) -> Result<(Value, MigrationReport), ProjectLoadError> {
    let version = value
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            ProjectLoadError::Migration("missing or non-numeric schema_version".into())
        })?;
    let version =
        u32::try_from(version).map_err(|_| ProjectLoadError::UnsupportedVersion(u32::MAX))?;
    if !(crate::project::MIN_LOADABLE_SCHEMA_VERSION..=CURRENT_VERSION).contains(&version) {
        return Err(ProjectLoadError::UnsupportedVersion(version));
    }
    let mut report = MigrationReport {
        from_version: version,
        steps: Vec::new(),
    };
    if version <= 7 {
        v7_to_v8(&mut value, &mut report)?;
    } else if has_legacy_captions(&value) {
        // A file written by a development build between the time/effects work and the text
        // work: schema 8, but its captions still have `text` / `style_id`.
        v7_to_v8_captions(&mut value)?;
        report.steps.push("v7->v8:captions");
    }
    if let Some(obj) = value.as_object_mut() {
        obj.insert("schema_version".into(), json!(CURRENT_VERSION));
    }
    Ok((value, report))
}

/// v7 -> v8: integer time, rational fps, and (later) effect refs, captions and so on.
fn v7_to_v8(value: &mut Value, report: &mut MigrationReport) -> Result<(), ProjectLoadError> {
    v7_to_v8_time(value)?;
    report.steps.push("v7->v8:time");
    v7_to_v8_effects(value)?;
    report.steps.push("v7->v8:effects");
    v7_to_v8_transitions(value)?;
    report.steps.push("v7->v8:transitions");
    v7_to_v8_easing(value)?;
    report.steps.push("v7->v8:easing");
    v7_to_v8_packs(value)?;
    report.steps.push("v7->v8:packs");
    v7_to_v8_captions(value)?;
    report.steps.push("v7->v8:captions");
    Ok(())
}

/// The time sub-step. Frame-positioned values (clip starts, source in/out, caption and
/// adjustment spans, transition durations) are rounded to µs and snapped to the nearest frame
/// boundary when that is within 1 ms, which removes accumulated float drift. Keyframe offsets,
/// handles, fades and probed media durations are only rounded: they are not positions on the
/// frame grid.
fn v7_to_v8_time(value: &mut Value) -> Result<(), ProjectLoadError> {
    let Some(root) = value.as_object_mut() else {
        return Err(migration_err("project is not a JSON object"));
    };

    // settings.fps: f64 -> rational. Frame snapping below uses the converted rate.
    let fps = {
        let settings = root
            .get_mut("settings")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| migration_err("missing settings object"))?;
        let raw = settings
            .get("fps")
            .and_then(Value::as_f64)
            .ok_or_else(|| migration_err("settings.fps is not a number"))?;
        let fps = Fps::from_f64(raw);
        settings.insert("fps".into(), json!({ "num": fps.num, "den": fps.den }));
        fps
    };

    // Media items: probed durations.
    if let Some(media) = root.get_mut("media").and_then(Value::as_array_mut) {
        for item in media {
            if let Some(obj) = item.as_object_mut() {
                rename_secs(obj, "duration_secs", "duration_us", None)?;
            }
        }
    }

    // Root timeline and every nested sequence.
    if let Some(tracks) = root.get_mut("tracks").and_then(Value::as_array_mut) {
        for track in tracks {
            migrate_track(track, fps)?;
        }
    }
    if let Some(sequences) = root.get_mut("sequences").and_then(Value::as_array_mut) {
        for seq in sequences {
            if let Some(tracks) = seq.get_mut("tracks").and_then(Value::as_array_mut) {
                for track in tracks {
                    migrate_track(track, fps)?;
                }
            }
        }
    }

    // Multicam sync offsets (seconds per angle) -> µs.
    if let Some(groups) = root
        .get_mut("multicam_groups")
        .and_then(Value::as_array_mut)
    {
        for group in groups {
            if let Some(obj) = group.as_object_mut() {
                if let Some(offsets) = obj.remove("sync_offsets_secs") {
                    let converted: Vec<Value> = offsets
                        .as_array()
                        .ok_or_else(|| migration_err("sync_offsets_secs is not an array"))?
                        .iter()
                        .map(|v| secs_value(v.as_f64().unwrap_or(0.0)))
                        .collect();
                    obj.insert("sync_offsets_us".into(), Value::Array(converted));
                }
            }
        }
    }
    Ok(())
}

type ClipVisitor<'a> = dyn FnMut(&mut Map<String, Value>) -> Result<(), ProjectLoadError> + 'a;

/// Every clip object of the root timeline and all nested sequences.
fn for_each_clip(
    root: &mut Map<String, Value>,
    mut f: impl FnMut(&mut Map<String, Value>) -> Result<(), ProjectLoadError>,
) -> Result<(), ProjectLoadError> {
    fn walk(
        tracks: Option<&mut Vec<Value>>,
        f: &mut ClipVisitor<'_>,
    ) -> Result<(), ProjectLoadError> {
        let Some(tracks) = tracks else {
            return Ok(());
        };
        for track in tracks {
            let Some(clips) = track.get_mut("clips").and_then(Value::as_array_mut) else {
                continue;
            };
            for clip in clips {
                if let Some(obj) = clip.as_object_mut() {
                    f(obj)?;
                }
            }
        }
        Ok(())
    }
    walk(root.get_mut("tracks").and_then(Value::as_array_mut), &mut f)?;
    if let Some(sequences) = root.get_mut("sequences").and_then(Value::as_array_mut) {
        for seq in sequences {
            walk(seq.get_mut("tracks").and_then(Value::as_array_mut), &mut f)?;
        }
    }
    Ok(())
}

fn project_root(value: &mut Value) -> Result<&mut Map<String, Value>, ProjectLoadError> {
    value
        .as_object_mut()
        .ok_or_else(|| migration_err("project is not a JSON object"))
}

/// The v8 ref for a v7 `effect_id`: `builtin:<id>` -> `builtin:<id>@1`;
/// `pack:<pack>:lut:<lut>` -> `pack:local.<pack>/<lut>@1` (a pack id that already has a
/// publisher, such as `mira.looks`, keeps it); `wasm:<plugin>` and anything else unchanged.
pub fn legacy_effect_ref(effect_id: &str) -> String {
    if let Some(rest) = effect_id.strip_prefix("builtin:") {
        if rest.contains('@') {
            return effect_id.to_string();
        }
        return format!("builtin:{rest}@1");
    }
    if let Some(rest) = effect_id.strip_prefix("pack:") {
        if let Some((pack, lut)) = rest.split_once(":lut:") {
            let pack = if pack.contains('.') {
                pack.to_string()
            } else {
                format!("local.{pack}")
            };
            return format!("pack:{pack}/{lut}@1");
        }
    }
    effect_id.to_string()
}

fn is_legacy_pack_lut(effect_id: &str) -> bool {
    effect_id
        .strip_prefix("pack:")
        .is_some_and(|rest| rest.contains(":lut:"))
}

/// v7 effect objects -> v8 instances: `effect_id` becomes `ref`. Returns the instances in the
/// original order.
fn convert_effect_instances(list: &[Value]) -> Vec<Value> {
    list.iter()
        .map(|e| {
            let mut e = e.clone();
            if let Some(obj) = e.as_object_mut() {
                if let Some(Value::String(id)) = obj.remove("effect_id") {
                    obj.insert("ref".into(), Value::String(legacy_effect_ref(&id)));
                }
            }
            e
        })
        .collect()
}

/// Split a v7 effect list into the v8 slots without changing what runs when: v7 runs the list
/// in order; v8 runs `filter`, `key`, `adjust`, `effects[]`. So a pack LUT in first position
/// becomes the `filter`, a chroma key that is first (or right after that LUT) becomes the `key`
/// (not on adjustment layers, which have no key slot), and everything else stays in
/// `effects[]` in its original order. Returns `(filter, key, rest)` with v8 instances.
pub fn split_legacy_effect_slots(
    list: &[Value],
    allow_key: bool,
) -> (Option<Value>, Option<Value>, Vec<Value>) {
    let id_of = |e: &Value| {
        e.get("effect_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let converted = convert_effect_instances(list);
    let mut i = 0;
    let mut filter = None;
    let mut key = None;
    if list.first().is_some_and(|e| is_legacy_pack_lut(&id_of(e))) {
        filter = Some(converted[0].clone());
        i = 1;
    }
    if allow_key
        && list
            .get(i)
            .is_some_and(|e| id_of(e) == "builtin:chroma_key")
    {
        key = Some(converted[i].clone());
        i += 1;
    }
    (filter, key, converted[i..].to_vec())
}

/// Effects sub-step: `effect_id` -> `ref`, and the slot split of video and adjustment clips.
fn v7_to_v8_effects(value: &mut Value) -> Result<(), ProjectLoadError> {
    let root = project_root(value)?;
    for_each_clip(root, |clip| {
        let kind = clip
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !matches!(kind.as_str(), "video" | "audio" | "adjustment") {
            return Ok(());
        }
        let Some(list) = clip.get("effects").and_then(Value::as_array).cloned() else {
            return Ok(());
        };
        match kind.as_str() {
            "video" | "adjustment" => {
                let (filter, key, rest) = split_legacy_effect_slots(&list, kind == "video");
                if let Some(f) = filter {
                    clip.insert("filter".into(), f);
                }
                if let Some(k) = key {
                    clip.insert("key".into(), k);
                }
                clip.insert("effects".into(), Value::Array(rest));
            }
            _ => {
                clip.insert(
                    "effects".into(),
                    Value::Array(convert_effect_instances(&list)),
                );
            }
        }
        Ok(())
    })
}

/// Transitions sub-step: `kind: "wipe_left"` -> `ref: "builtin:wipe_left@1"`, plus the new
/// `params` and `easing` fields (`easing: null` is the transition's declared default).
fn v7_to_v8_transitions(value: &mut Value) -> Result<(), ProjectLoadError> {
    let root = project_root(value)?;
    for_each_clip(root, |clip| {
        let Some(tr) = clip
            .get_mut("outgoing_transition")
            .and_then(Value::as_object_mut)
        else {
            return Ok(());
        };
        if let Some(Value::String(kind)) = tr.remove("kind") {
            tr.insert("ref".into(), Value::String(format!("builtin:{kind}@1")));
        }
        tr.entry("params").or_insert_with(|| json!({}));
        tr.entry("easing").or_insert(Value::Null);
        Ok(())
    })
}

/// Easing sub-step over every clip's keyframe tracks.
fn v7_to_v8_easing(value: &mut Value) -> Result<(), ProjectLoadError> {
    let root = project_root(value)?;
    for_each_clip(root, |clip| {
        if let Some(tracks) = clip.get_mut("keyframes").and_then(Value::as_array_mut) {
            for track in tracks {
                if let Some(keys) = track.get_mut("keys").and_then(Value::as_array_mut) {
                    convert_legacy_easing(keys);
                }
            }
        }
        Ok(())
    })
}

/// Rewrite v7 `ease_in` / `ease_out` keys (JSON objects with `time_us`, `value`, `easing`) as
/// `bezier` keys whose handles trace the identical curve, scaled to each span in µs. The keys
/// are sorted by time first (spans are between neighbours in time order).
///
/// - `ease_in` (t^2) is the cubic Bezier with control points (1/3, 0) and (2/3, 1/3);
/// - `ease_out` (1 - (1 - t)^2) is (1/3, 2/3) and (2/3, 1);
///
/// so the handles are `(span/3, 0)` / `(-span/3, -2/3 dv)` and `(span/3, 2/3 dv)` /
/// `(-span/3, 0)`. `ease_in_out` is not one cubic and stays as the legacy easing. A last key has
/// no span after it, so its easing becomes `linear`. Returns the number of keys converted.
pub fn convert_legacy_easing(keys: &mut [Value]) -> usize {
    keys.sort_by_key(|k| k.get("time_us").and_then(Value::as_i64).unwrap_or(0));
    let mut converted = 0;
    for i in 0..keys.len() {
        let easing = keys[i]
            .get("easing")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if easing != "ease_in" && easing != "ease_out" {
            continue;
        }
        converted += 1;
        if i + 1 >= keys.len() {
            keys[i]["easing"] = json!("linear");
            continue;
        }
        let t0 = keys[i].get("time_us").and_then(Value::as_i64).unwrap_or(0);
        let t1 = keys[i + 1]
            .get("time_us")
            .and_then(Value::as_i64)
            .unwrap_or(t0);
        let v0 = keys[i].get("value").and_then(Value::as_f64).unwrap_or(0.0);
        let v1 = keys[i + 1]
            .get("value")
            .and_then(Value::as_f64)
            .unwrap_or(v0);
        let third = ((t1 - t0) as f64 / 3.0).round() as i64;
        let dv = v1 - v0;
        let (right_v, left_v) = if easing == "ease_in" {
            (0.0, -2.0 / 3.0 * dv)
        } else {
            (2.0 / 3.0 * dv, 0.0)
        };
        keys[i]["easing"] = json!("bezier");
        keys[i]["handle_right"] = json!([third, right_v]);
        keys[i + 1]["handle_left"] = json!([-third, left_v]);
    }
    converted
}

/// Packs sub-step: `asset_pack_paths` and `wasm_plugin_paths` move to the transitional
/// `legacy_pack_paths` (hosts install the pack folders into the user library on open).
fn v7_to_v8_packs(value: &mut Value) -> Result<(), ProjectLoadError> {
    let root = project_root(value)?;
    let mut paths: Vec<Value> = Vec::new();
    for key in ["asset_pack_paths", "wasm_plugin_paths"] {
        if let Some(Value::Array(list)) = root.remove(key) {
            for p in list {
                if !paths.contains(&p) {
                    paths.push(p);
                }
            }
        }
    }
    if !paths.is_empty() {
        root.insert("legacy_pack_paths".into(), Value::Array(paths));
    }
    Ok(())
}

/// True when some caption clip still has the v7 shape (no `body`).
fn has_legacy_captions(value: &Value) -> bool {
    let tracks = |v: &Value| -> Vec<Value> {
        v.get("tracks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut all = tracks(value);
    if let Some(seqs) = value.get("sequences").and_then(Value::as_array) {
        for seq in seqs {
            all.extend(tracks(seq));
        }
    }
    all.iter().any(|t| {
        t.get("clips")
            .and_then(Value::as_array)
            .is_some_and(|clips| clips.iter().any(is_legacy_caption))
    })
}

fn is_legacy_caption(clip: &Value) -> bool {
    clip.get("type").and_then(Value::as_str) == Some("caption") && clip.get("body").is_none()
}

/// The style v7 drew for a caption whose `style_id` it did not know (and the default of
/// `GenerateCaptions`).
const V7_DEFAULT_CAPTION_STYLE: &str = "tiktok-bold-yellow";

/// A `style` object (`{ref, params, overrides}`) for a v7 `style_id`. Ids that are not valid
/// ref item ids (`[a-z0-9_-]+`) are lower-cased with other characters turned into `-`.
pub fn legacy_caption_style(style_id: &str) -> Value {
    let id: String = style_id
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let id = if id.is_empty() {
        V7_DEFAULT_CAPTION_STYLE.to_string()
    } else {
        id
    };
    json!({ "ref": format!("builtin:{id}@1"), "params": {}, "overrides": {} })
}

/// Captions sub-step. Tracks gain `magnetic: false`; caption clips get `body`, `style`
/// (`null` = inherit the track) and `animations`; a caption track gets `caption_style` when every
/// clip on it had the same `style_id`, otherwise each clip keeps its own style.
fn v7_to_v8_captions(value: &mut Value) -> Result<(), ProjectLoadError> {
    let root = project_root(value)?;
    root.entry("markers").or_insert_with(|| json!([]));
    root.entry("transcripts").or_insert_with(|| json!([]));

    fn migrate_tracks(tracks: Option<&mut Vec<Value>>) {
        let Some(tracks) = tracks else {
            return;
        };
        for track in tracks {
            let Some(track) = track.as_object_mut() else {
                continue;
            };
            track.entry("magnetic").or_insert(Value::Bool(false));
            if track.get("kind").and_then(Value::as_str) != Some("caption") {
                continue;
            }
            let Some(clips) = track.get_mut("clips").and_then(Value::as_array_mut) else {
                continue;
            };
            let style_ids: Vec<String> = clips
                .iter()
                .filter(|c| is_legacy_caption(c))
                .map(|c| {
                    c.get("style_id")
                        .and_then(Value::as_str)
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or(V7_DEFAULT_CAPTION_STYLE)
                        .to_string()
                })
                .collect();
            let shared = style_ids
                .first()
                .filter(|first| style_ids.iter().all(|s| s == *first))
                .cloned();
            for (clip, style_id) in clips
                .iter_mut()
                .filter(|c| is_legacy_caption(c))
                .zip(&style_ids)
            {
                let Some(obj) = clip.as_object_mut() else {
                    continue;
                };
                let text = obj.remove("text").unwrap_or_else(|| json!(""));
                obj.remove("style_id");
                obj.insert(
                    "body".into(),
                    json!({ "text": text, "runs": [], "words": [] }),
                );
                obj.insert(
                    "style".into(),
                    if shared.is_some() {
                        Value::Null
                    } else {
                        legacy_caption_style(style_id)
                    },
                );
                obj.insert("animations".into(), json!({}));
            }
            if let Some(style_id) = shared {
                track
                    .entry("caption_style")
                    .or_insert_with(|| legacy_caption_style(&style_id));
            }
        }
    }
    migrate_tracks(root.get_mut("tracks").and_then(Value::as_array_mut));
    if let Some(sequences) = root.get_mut("sequences").and_then(Value::as_array_mut) {
        for seq in sequences {
            migrate_tracks(seq.get_mut("tracks").and_then(Value::as_array_mut));
        }
    }
    Ok(())
}

fn migrate_track(track: &mut Value, fps: Fps) -> Result<(), ProjectLoadError> {
    let Some(clips) = track.get_mut("clips").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for clip in clips {
        migrate_clip(clip, fps)?;
    }
    Ok(())
}

fn migrate_clip(clip: &mut Value, fps: Fps) -> Result<(), ProjectLoadError> {
    let kind = clip
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(obj) = clip.as_object_mut() else {
        return Err(migration_err("clip is not a JSON object"));
    };
    match kind.as_str() {
        "video" | "audio" => {
            rename_secs(obj, "position_secs", "start_us", Some(fps))?;
            rename_secs(obj, "source_in_secs", "source_in_us", Some(fps))?;
            rename_secs(obj, "source_out_secs", "source_out_us", Some(fps))?;
            rename_secs(obj, "fade_in_secs", "fade_in_us", None)?;
            rename_secs(obj, "fade_out_secs", "fade_out_us", None)?;
            migrate_keyframes(obj)?;
            if let Some(tr) = obj
                .get_mut("outgoing_transition")
                .and_then(Value::as_object_mut)
            {
                rename_secs(tr, "duration_secs", "duration_us", Some(fps))?;
            }
            if let Some(bg) = obj
                .get_mut("background_removal")
                .and_then(Value::as_object_mut)
            {
                if let Some(mf) = bg.get_mut("matte_frames").and_then(Value::as_object_mut) {
                    rename_secs(mf, "start_secs", "start_us", None)?;
                    if let Some(rate) = mf.get("fps").and_then(Value::as_f64) {
                        let f = Fps::from_f64(rate);
                        mf.insert("fps".into(), json!({ "num": f.num, "den": f.den }));
                    }
                }
            }
        }
        "caption" | "adjustment" => {
            rename_secs(obj, "position_secs", "start_us", Some(fps))?;
            rename_secs(obj, "duration_secs", "duration_us", Some(fps))?;
            if kind == "adjustment" {
                migrate_keyframes(obj)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Keyframe tracks: `time_secs` -> `time_us`; handle time offsets (first element of each
/// handle pair, in seconds) -> µs.
fn migrate_keyframes(obj: &mut Map<String, Value>) -> Result<(), ProjectLoadError> {
    let Some(tracks) = obj.get_mut("keyframes").and_then(Value::as_array_mut) else {
        return Ok(());
    };
    for track in tracks {
        let Some(keys) = track.get_mut("keys").and_then(Value::as_array_mut) else {
            continue;
        };
        for key in keys {
            let Some(k) = key.as_object_mut() else {
                return Err(migration_err("keyframe is not a JSON object"));
            };
            rename_secs(k, "time_secs", "time_us", None)?;
            for handle in ["handle_left", "handle_right"] {
                if let Some(pair) = k.get_mut(handle).and_then(Value::as_array_mut) {
                    if let Some(first) = pair.first_mut() {
                        let secs = first.as_f64().unwrap_or(0.0);
                        *first = secs_value(secs);
                    }
                }
            }
        }
    }
    Ok(())
}

/// Rename `old` (float seconds) to `new` (integer µs) in place. Missing keys are left alone and
/// `null` stays `null`. With `fps`, the value is snapped to a frame boundary when within 1 ms.
fn rename_secs(
    obj: &mut Map<String, Value>,
    old: &str,
    new: &str,
    fps: Option<Fps>,
) -> Result<(), ProjectLoadError> {
    let Some(v) = obj.remove(old) else {
        return Ok(());
    };
    if v.is_null() {
        obj.insert(new.to_string(), Value::Null);
        return Ok(());
    }
    let secs = v
        .as_f64()
        .ok_or_else(|| migration_err(&format!("{old} is not a number")))?;
    let mut us = seconds_to_us(secs);
    if let Some(f) = fps {
        us = snap_within(us, f, SNAP_TOLERANCE_US);
    }
    obj.insert(new.to_string(), json!(us));
    Ok(())
}

fn secs_value(secs: f64) -> Value {
    json!(seconds_to_us(secs))
}

fn seconds_to_us(secs: f64) -> i64 {
    crate::time::seconds_to_us(secs)
}

fn migration_err(message: &str) -> ProjectLoadError {
    ProjectLoadError::Migration(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_and_snaps_frame_positions() {
        let mut obj: Map<String, Value> =
            serde_json::from_str(r#"{"p": 1.0000004, "q": 1.02}"#).unwrap();
        let f24 = Fps::new(24, 1);
        rename_secs(&mut obj, "p", "p_us", Some(f24)).unwrap();
        rename_secs(&mut obj, "q", "q_us", Some(f24)).unwrap();
        // 1.0000004 s is 1_000_000 µs: already on the 24 fps grid.
        assert_eq!(obj["p_us"], json!(1_000_000));
        // 1.02 s is 1_020_000 µs: 20 ms from the nearest frame, so it is kept as is.
        assert_eq!(obj["q_us"], json!(1_020_000));
    }

    #[test]
    fn leaves_null_and_missing_keys_alone() {
        let mut obj: Map<String, Value> = serde_json::from_str(r#"{"d": null}"#).unwrap();
        rename_secs(&mut obj, "d", "d_us", None).unwrap();
        rename_secs(&mut obj, "absent", "absent_us", None).unwrap();
        assert_eq!(obj["d_us"], Value::Null);
        assert!(!obj.contains_key("absent_us"));
    }

    #[test]
    fn current_projects_are_not_migrated() {
        let v = json!({ "schema_version": 8, "settings": { "fps": { "num": 30, "den": 1 } } });
        let (out, report) = migrate_to_current(v).unwrap();
        assert!(!report.migrated());
        assert_eq!(out["schema_version"], json!(8));
    }

    #[test]
    fn refuses_newer_and_unknown_versions() {
        let newer = json!({ "schema_version": 9 });
        assert!(matches!(
            migrate_to_current(newer),
            Err(ProjectLoadError::UnsupportedVersion(9))
        ));
        let zero = json!({ "schema_version": 0 });
        assert!(matches!(
            migrate_to_current(zero),
            Err(ProjectLoadError::UnsupportedVersion(0))
        ));
    }
}
