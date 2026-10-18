//! Generic change detection for the edit contract: which media items, tracks, clips and
//! sequences were created, modified or deleted between two project states.
//!
//! The diff is computed from the projects' JSON, keyed by id, so it does not depend on which
//! command ran. Clips are found wherever they live: on the root timeline and in every nested
//! sequence. A track's or sequence's own entry excludes its children, so changing one clip
//! marks that clip as modified and nothing else. A moved clip keeps its id, so it shows as
//! modified rather than deleted.

use super::error::EditError;
use crate::project::{Id, Project};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The kinds of entity the contract reports on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityKind {
    Media,
    Track,
    Clip,
    Sequence,
}

impl EntityKind {
    fn label(self, count: usize) -> String {
        let (one, many) = match self {
            Self::Media => ("media item", "media items"),
            Self::Track => ("track", "tracks"),
            Self::Clip => ("clip", "clips"),
            Self::Sequence => ("sequence", "sequences"),
        };
        format!("{count} {}", if count == 1 { one } else { many })
    }
}

/// The `changed` block of a write result: entity ids by change type. Clips, tracks, media and
/// sequences are all UUIDs, so the ids alone identify them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changed {
    pub created: Vec<Id>,
    pub modified: Vec<Id>,
    pub deleted: Vec<Id>,
}

impl Changed {
    pub fn is_empty(&self) -> bool {
        self.created.is_empty() && self.modified.is_empty() && self.deleted.is_empty()
    }
}

/// A change with its kind kept, for summaries and the deletion guard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Detailed {
    pub created: Vec<(EntityKind, Id)>,
    pub modified: Vec<(EntityKind, Id)>,
    pub deleted: Vec<(EntityKind, Id)>,
}

impl Detailed {
    pub fn plain(&self) -> Changed {
        let ids = |v: &[(EntityKind, Id)]| v.iter().map(|(_, id)| *id).collect();
        Changed {
            created: ids(&self.created),
            modified: ids(&self.modified),
            deleted: ids(&self.deleted),
        }
    }

    /// Plain-language lines, e.g. `Deleted 2 clips, 1 track`.
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for (verb, list) in [
            ("Created", &self.created),
            ("Modified", &self.modified),
            ("Deleted", &self.deleted),
        ] {
            let parts = counts_by_kind(list);
            if !parts.is_empty() {
                lines.push(format!("{verb} {}", parts.join(", ")));
            }
        }
        lines
    }
}

fn counts_by_kind(list: &[(EntityKind, Id)]) -> Vec<String> {
    let mut counts: BTreeMap<EntityKind, usize> = BTreeMap::new();
    for (kind, _) in list {
        *counts.entry(*kind).or_default() += 1;
    }
    counts.into_iter().map(|(k, n)| k.label(n)).collect()
}

type Entities = BTreeMap<(EntityKind, String), Value>;

/// Changed ids between two project states, in a stable order.
pub fn changed_ids(before: &Project, after: &Project) -> Result<Changed, EditError> {
    Ok(detailed(before, after)?.plain())
}

pub(crate) fn detailed(before: &Project, after: &Project) -> Result<Detailed, EditError> {
    let before = entities(&project_json(before)?);
    let after = entities(&project_json(after)?);
    let mut out = Detailed::default();

    for ((kind, id), value) in &after {
        match before.get(&(*kind, id.clone())) {
            None => out.created.push((*kind, parse_id(id)?)),
            Some(old) if old != value => out.modified.push((*kind, parse_id(id)?)),
            Some(_) => {}
        }
    }
    for (kind, id) in before.keys() {
        if !after.contains_key(&(*kind, id.clone())) {
            out.deleted.push((*kind, parse_id(id)?));
        }
    }
    Ok(out)
}

fn project_json(project: &Project) -> Result<Value, EditError> {
    serde_json::to_value(project)
        .map_err(|e| EditError::internal(format!("could not serialize project: {e}")))
}

fn parse_id(raw: &str) -> Result<Id, EditError> {
    raw.parse::<Id>()
        .map_err(|e| EditError::internal(format!("project holds a malformed id '{raw}': {e}")))
}

/// Flatten a project's JSON into one entry per media item, track, clip and sequence.
fn entities(project: &Value) -> Entities {
    let mut out = Entities::new();
    for media in array(project, "media") {
        insert(&mut out, EntityKind::Media, media, &[]);
    }
    walk_tracks(array(project, "tracks"), &mut out);
    for sequence in array(project, "sequences") {
        insert(&mut out, EntityKind::Sequence, sequence, &["tracks"]);
        walk_tracks(array(sequence, "tracks"), &mut out);
    }
    out
}

fn walk_tracks<'a>(tracks: impl Iterator<Item = &'a Value>, out: &mut Entities) {
    for track in tracks {
        insert(out, EntityKind::Track, track, &["clips"]);
        for clip in array(track, "clips") {
            insert(out, EntityKind::Clip, clip, &[]);
        }
    }
}

fn array<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn insert(out: &mut Entities, kind: EntityKind, value: &Value, strip: &[&str]) {
    let Some(id) = value.get("id").and_then(Value::as_str) else {
        return;
    };
    let mut stored = value.clone();
    if let Value::Object(map) = &mut stored {
        for key in strip {
            map.remove(*key);
        }
    }
    out.insert((kind, id.to_string()), stored);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{apply_command, Command};
    use crate::project::Settings;

    fn project() -> Project {
        Project::new("t", Settings::default())
    }

    fn apply(p: &mut Project, json: Value) {
        let cmd: Command = serde_json::from_value(json).unwrap_or_else(|e| panic!("{e}"));
        apply_command(p, cmd).unwrap_or_else(|e| panic!("{e}"));
    }

    #[test]
    fn identical_projects_have_no_changes() {
        let p = project();
        assert!(changed_ids(&p, &p.clone())
            .map(|c| c.is_empty())
            .unwrap_or(false));
    }

    #[test]
    fn creating_a_track_reports_it_as_created() {
        let before = project();
        let mut after = before.clone();
        apply(
            &mut after,
            serde_json::json!({ "command": "AddTrack", "kind": "video", "name": "V1" }),
        );
        let changed = changed_ids(&before, &after).unwrap_or_default();
        assert_eq!(changed.created.len(), 1);
        assert!(changed.modified.is_empty() && changed.deleted.is_empty());
    }

    #[test]
    fn deleting_a_track_reports_the_track_as_deleted() {
        let mut before = project();
        apply(
            &mut before,
            serde_json::json!({ "command": "AddTrack", "kind": "video", "name": "V1" }),
        );
        let track_id = before.tracks[0].id;
        let mut after = before.clone();
        apply(
            &mut after,
            serde_json::json!({ "command": "DeleteTrack", "track_id": track_id }),
        );
        let changed = changed_ids(&before, &after).unwrap_or_default();
        assert_eq!(changed.deleted, vec![track_id]);
        assert!(changed.created.is_empty());
    }

    #[test]
    fn renaming_a_track_marks_only_the_track_modified() {
        let mut before = project();
        apply(
            &mut before,
            serde_json::json!({ "command": "AddTrack", "kind": "audio", "name": "A1" }),
        );
        let track_id = before.tracks[0].id;
        let mut after = before.clone();
        apply(
            &mut after,
            serde_json::json!({ "command": "RenameTrack", "track_id": track_id, "name": "Music" }),
        );
        let changed = changed_ids(&before, &after).unwrap_or_default();
        assert_eq!(changed.modified, vec![track_id]);
        assert!(changed.created.is_empty() && changed.deleted.is_empty());
    }

    #[test]
    fn nested_sequence_tracks_and_clips_are_found() {
        let mut before = project();
        apply(
            &mut before,
            serde_json::json!({ "command": "CreateSequence", "name": "Inner" }),
        );
        let sequence_id = before.sequences[0].id;
        let mut after = before.clone();
        apply(
            &mut after,
            serde_json::json!({ "command": "AddTrack", "kind": "video", "name": "V", "sequence_id": sequence_id }),
        );
        let changed = detailed(&before, &after).unwrap_or_default();
        assert_eq!(changed.created.len(), 1);
        assert_eq!(changed.created[0].0, EntityKind::Track);
        // The sequence itself changed only through its children, which are excluded from its own entry.
        assert!(changed.modified.is_empty());
    }

    #[test]
    fn deleting_a_sequence_reports_its_media_item_and_tracks() {
        let mut before = project();
        apply(
            &mut before,
            serde_json::json!({ "command": "CreateSequence", "name": "Inner", "id": null }),
        );
        let sequence_id = before.sequences[0].id;
        let mut after = before.clone();
        apply(
            &mut after,
            serde_json::json!({ "command": "DeleteSequence", "sequence_id": sequence_id }),
        );
        let changed = detailed(&before, &after).unwrap_or_default();
        assert!(changed
            .deleted
            .contains(&(EntityKind::Sequence, sequence_id)));
        assert!(changed.deleted.iter().any(|(k, _)| *k == EntityKind::Media));
        assert!(changed.created.is_empty());
    }

    #[test]
    fn summary_lines_count_by_kind() {
        let detailed = Detailed {
            created: vec![],
            modified: vec![],
            deleted: vec![
                (EntityKind::Clip, Id::new_v4()),
                (EntityKind::Clip, Id::new_v4()),
                (EntityKind::Track, Id::new_v4()),
            ],
        };
        // Kinds are listed in EntityKind order: media, track, clip, sequence.
        assert_eq!(detailed.summary_lines(), vec!["Deleted 1 track, 2 clips"]);
    }
}
