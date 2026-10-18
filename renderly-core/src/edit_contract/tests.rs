//! Unit tests for the edit contract. Each rule in docs/command-api.md "Edit contract" has a
//! test here, run against real commands on a real project.

use super::*;
use crate::commands::Command;
use crate::project::{Id, Project, Settings};
use serde_json::json;

fn command(value: Value) -> Command {
    serde_json::from_value(value).unwrap_or_else(|e| panic!("bad test command: {e}"))
}

fn project() -> Project {
    Project::new("contract", Settings::default())
}

/// `Project` has no `PartialEq`; its JSON is the comparison.
fn snap(project: &Project) -> Value {
    serde_json::to_value(project).unwrap_or(Value::Null)
}

/// Plan, commit (when the plan says so) and finish, the way every surface does.
fn run(
    project: &mut Project,
    revision: &mut u64,
    ledger: &mut WriteLedger,
    options: &WriteOptions,
    caller: Caller,
    commands: Vec<Command>,
) -> Result<WriteResult, EditError> {
    let mut plan = plan_write(project, *revision, ledger, options, caller, commands)?;
    let committed_at = match plan.take_next() {
        Some(next) => {
            *project = next;
            *revision += 1;
            Some(*revision)
        }
        None => None,
    };
    Ok(plan.finish(committed_at, ledger))
}

fn add_track(
    project: &mut Project,
    revision: &mut u64,
    ledger: &mut WriteLedger,
    kind: &str,
) -> Id {
    let result = run(
        project,
        revision,
        ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(
            json!({ "command": "AddTrack", "kind": kind, "name": "T" }),
        )],
    );
    assert!(result.is_ok(), "AddTrack failed: {result:?}");
    project.tracks.last().map(|t| t.id).unwrap_or_default()
}

/// A project with one caption track holding one caption clip. Caption clips need no media.
fn caption_fixture() -> (Project, u64, WriteLedger, Id, Id) {
    let mut p = project();
    let mut rev = 0;
    let mut ledger = WriteLedger::new();
    let track = add_track(&mut p, &mut rev, &mut ledger, "caption");
    run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(json!({
            "command": "AddCaption", "track_id": track, "text": "hi",
            "position_secs": 0.0, "duration_secs": 1.0, "style_id": "tiktok-minimal"
        }))],
    )
    .unwrap_or_else(|e| panic!("AddCaption failed: {e}"));
    let clip = p.tracks[0].clips[0].id();
    (p, rev, ledger, track, clip)
}

fn delete_clip(track: Id, clip: Id) -> Vec<Command> {
    vec![command(json!({
        "command": "DeleteClip", "track_id": track, "clip_id": clip, "ripple": false
    }))]
}

fn options_with_deletion(ids: Vec<Id>) -> WriteOptions {
    WriteOptions {
        allow_deletion: Some(AllowDeletion {
            reason: "test cleanup".into(),
            ids,
        }),
        ..WriteOptions::default()
    }
}

#[test]
fn committed_write_bumps_revision_and_reports_changes() {
    let mut p = project();
    let mut rev = 0;
    let mut ledger = WriteLedger::new();
    let result = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(
            json!({ "command": "AddTrack", "kind": "video", "name": "V" }),
        )],
    );
    let result = result.unwrap_or_else(|e| panic!("{e}"));
    assert!(result.committed);
    assert_eq!(result.revision, 1);
    assert_eq!(result.changed.created.len(), 1);
    assert!(!result.dry_run && !result.replayed);
    assert_eq!(
        result.summary.first().map(String::as_str),
        Some("Add track")
    );
}

#[test]
fn stale_expected_revision_is_rejected_with_current_revision() {
    let (mut p, mut rev, mut ledger, track, _) = caption_fixture();
    let before = p.clone();
    let options = WriteOptions {
        expected_revision: Some(rev - 1),
        ..WriteOptions::default()
    };
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::Agent,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "X" }),
        )],
    )
    .err()
    .unwrap_or_else(|| panic!("stale revision was accepted"));
    assert_eq!(err.code, EditErrorCode::RevisionConflict);
    assert_eq!(err.hint, REVISION_HINT);
    assert_eq!(err.data, Some(json!({ "current_revision": rev })));
    assert_eq!(
        snap(&p),
        snap(&before),
        "a rejected write must not change the project"
    );
}

#[test]
fn matching_expected_revision_is_accepted() {
    let (mut p, mut rev, mut ledger, track, _) = caption_fixture();
    let options = WriteOptions {
        expected_revision: Some(rev),
        ..WriteOptions::default()
    };
    let result = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::Agent,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "X" }),
        )],
    );
    assert!(result.is_ok_and(|r| r.committed && r.warnings.is_empty()));
}

#[test]
fn agent_write_without_expected_revision_gets_a_warning() {
    let (mut p, mut rev, mut ledger, track, _) = caption_fixture();
    let result = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::Agent,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "Y" }),
        )],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(result.warnings.len(), 1);
    assert!(result.warnings[0].contains("expected_revision"));
}

#[test]
fn dry_run_returns_patch_and_changes_nothing() {
    let (mut p, mut rev, mut ledger, track, _) = caption_fixture();
    let before = p.clone();
    let options = WriteOptions {
        dry_run: true,
        ..WriteOptions::default()
    };
    let mut plan = plan_write(
        &p,
        rev,
        &ledger,
        &options,
        Caller::User,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "Dry" }),
        )],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(
        plan.take_next().is_none(),
        "a dry run must not produce a project to commit"
    );
    let result = plan.finish(None, &mut ledger);
    assert!(!result.committed);
    assert!(result.dry_run);
    assert_eq!(result.revision, rev);
    assert!(!result.patch.0.is_empty(), "dry run returns the patch");
    assert_eq!(result.changed.modified, vec![track]);
    assert_eq!(snap(&p), snap(&before));
    assert!(ledger.is_empty(), "a dry run records nothing");
    // The same write for real still works afterwards, at the same revision.
    let real = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "Dry" }),
        )],
    );
    assert!(real.is_ok_and(|r| r.committed && r.revision == rev));
}

#[test]
fn request_id_replay_returns_original_result_without_reapplying() {
    let (mut p, mut rev, mut ledger, track, _) = caption_fixture();
    let options = WriteOptions {
        request_id: Some("req-1".into()),
        ..WriteOptions::default()
    };
    let rename = |name: &str| {
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": name }),
        )]
    };
    let first = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::User,
        rename("Once"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(first.committed);
    let after_first = p.clone();
    let rev_after_first = rev;

    // Retry the identical request later: nothing is applied again, the original result comes back.
    let replay = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::User,
        rename("Once"),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(replay.replayed);
    assert!(replay.committed);
    assert_eq!(replay.revision, first.revision);
    assert_eq!(rev, rev_after_first, "a replay must not bump the revision");
    assert_eq!(snap(&p), snap(&after_first));
}

#[test]
fn request_id_replay_is_not_a_revision_conflict() {
    let (mut p, mut rev, mut ledger, track, _) = caption_fixture();
    let rename = || {
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "R" }),
        )]
    };
    let options = WriteOptions {
        request_id: Some("req-2".into()),
        expected_revision: Some(rev),
        ..WriteOptions::default()
    };
    run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::User,
        rename(),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    // The client's expected_revision is now stale, but the replay is still answered.
    let replay = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::User,
        rename(),
    );
    assert!(replay.is_ok_and(|r| r.replayed));
}

#[test]
fn request_id_reused_with_a_different_payload_is_rejected() {
    let (mut p, mut rev, mut ledger, track, _) = caption_fixture();
    let options = WriteOptions {
        request_id: Some("req-3".into()),
        ..WriteOptions::default()
    };
    run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::User,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "A" }),
        )],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::User,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": track, "name": "B" }),
        )],
    )
    .err()
    .unwrap_or_else(|| panic!("reused request_id was accepted"));
    assert_eq!(err.code, EditErrorCode::RequestIdReused);
}

#[test]
fn ledger_forgets_the_oldest_request_ids_beyond_its_capacity() {
    let mut p = project();
    let mut rev = 0;
    let mut ledger = WriteLedger::with_capacity(2);
    for i in 0..3 {
        let options = WriteOptions {
            request_id: Some(format!("r{i}")),
            ..WriteOptions::default()
        };
        run(
            &mut p,
            &mut rev,
            &mut ledger,
            &options,
            Caller::User,
            vec![command(
                json!({ "command": "CreateSequence", "name": format!("S{i}") }),
            )],
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }
    assert_eq!(ledger.len(), 2);
    // r0 was evicted, so reusing it with a different payload is a fresh write, not a reuse error.
    let options = WriteOptions {
        request_id: Some("r0".into()),
        ..WriteOptions::default()
    };
    let fresh = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::User,
        vec![command(
            json!({ "command": "CreateSequence", "name": "Other" }),
        )],
    );
    assert!(fresh.is_ok_and(|r| r.committed && !r.replayed));
}

#[test]
fn batch_failure_rolls_back_and_names_the_command() {
    let (mut p, mut rev, mut ledger, track, clip) = caption_fixture();
    let before = p.clone();
    let missing = Id::new_v4();
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![
            command(json!({ "command": "RenameTrack", "track_id": track, "name": "Half" })),
            command(json!({ "command": "DeleteClip", "track_id": track, "clip_id": missing, "ripple": false })),
        ],
    )
    .err()
    .unwrap_or_else(|| panic!("batch with a bad command was accepted"));
    assert_eq!(err.code, EditErrorCode::RolledBack);
    assert!(err.message.contains("command 1 (DeleteClip)"));
    assert_eq!(
        snap(&p),
        snap(&before),
        "nothing from a failed batch may be applied"
    );
    assert!(p.tracks[0].clips.iter().any(|c| c.id() == clip));
}

#[test]
fn single_command_failure_reports_its_own_code() {
    let (mut p, mut rev, mut ledger, _, _) = caption_fixture();
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(
            json!({ "command": "RenameTrack", "track_id": Id::new_v4(), "name": "X" }),
        )],
    )
    .err()
    .unwrap_or_else(|| panic!("missing track was accepted"));
    assert_eq!(err.code, EditErrorCode::NotFound);
}

#[test]
fn export_is_not_a_write() {
    let mut p = project();
    let mut rev = 0;
    let mut ledger = WriteLedger::new();
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(json!({
            "command": "Export", "output_path": "x.mp4", "preset": "tik_tok9x16"
        }))],
    )
    .err()
    .unwrap_or_else(|| panic!("Export was accepted as a write"));
    assert_eq!(err.code, EditErrorCode::InvalidCommand);
}

#[test]
fn dry_run_refuses_commands_that_write_files() {
    let p = project();
    let rev = 0;
    let ledger = WriteLedger::new();
    let options = WriteOptions {
        dry_run: true,
        ..WriteOptions::default()
    };
    let err = plan_write(
        &p,
        rev,
        &ledger,
        &options,
        Caller::User,
        vec![command(json!({
            "command": "GenerateVoiceover", "text": "hi", "track_id": Id::new_v4(),
            "position_secs": 0.0, "output_path": "/tmp/x.wav",
            "provider": { "provider": "piper_local" }
        }))],
    )
    .err()
    .unwrap_or_else(|| panic!("dry run accepted a file-writing command"));
    assert_eq!(err.code, EditErrorCode::InvalidCommand);
    assert!(err.message.contains("GenerateVoiceover"));
}

#[test]
fn empty_batch_is_invalid() {
    let mut p = project();
    let mut rev = 0;
    let mut ledger = WriteLedger::new();
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![],
    )
    .err()
    .unwrap_or_else(|| panic!("empty batch was accepted"));
    assert_eq!(err.code, EditErrorCode::InvalidCommand);
}

// ---- deletion guard ------------------------------------------------------------------------

#[test]
fn agent_delete_is_rejected_without_allow_deletion() {
    let (mut p, mut rev, mut ledger, track, clip) = caption_fixture();
    let before = p.clone();
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::Agent,
        delete_clip(track, clip),
    )
    .err()
    .unwrap_or_else(|| panic!("agent delete was accepted without allow_deletion"));
    assert_eq!(err.code, EditErrorCode::DeletionNotAllowed);
    assert!(
        err.message.contains(&clip.to_string()),
        "message lists the ids"
    );
    assert!(
        err.hint.contains("allow_deletion"),
        "hint shows the field to add"
    );
    assert_eq!(snap(&p), snap(&before));
}

#[test]
fn agent_delete_is_rejected_with_wrong_ids() {
    let (mut p, mut rev, mut ledger, track, clip) = caption_fixture();
    let before = p.clone();
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options_with_deletion(vec![track]),
        Caller::Agent,
        delete_clip(track, clip),
    )
    .err()
    .unwrap_or_else(|| panic!("agent delete was accepted with the wrong ids"));
    assert_eq!(err.code, EditErrorCode::DeletionNotAllowed);
    assert_eq!(snap(&p), snap(&before));
}

#[test]
fn agent_delete_is_rejected_with_empty_reason() {
    let (mut p, mut rev, mut ledger, track, clip) = caption_fixture();
    let options = WriteOptions {
        allow_deletion: Some(AllowDeletion {
            reason: "   ".into(),
            ids: vec![clip],
        }),
        ..WriteOptions::default()
    };
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options,
        Caller::Agent,
        delete_clip(track, clip),
    )
    .err()
    .unwrap_or_else(|| panic!("empty reason was accepted"));
    assert_eq!(err.code, EditErrorCode::DeletionNotAllowed);
    assert!(err.message.contains("reason is empty"));
}

#[test]
fn agent_delete_is_accepted_with_exact_ids() {
    let (mut p, mut rev, mut ledger, track, clip) = caption_fixture();
    let result = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options_with_deletion(vec![clip]),
        Caller::Agent,
        delete_clip(track, clip),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(result.committed);
    assert_eq!(result.changed.deleted, vec![clip]);
    assert!(p.tracks[0].clips.is_empty());
}

#[test]
fn dry_run_shows_the_ids_a_deletion_needs() {
    let (p, rev, ledger, track, clip) = caption_fixture();
    let options = WriteOptions {
        dry_run: true,
        allow_deletion: Some(AllowDeletion {
            reason: "preview".into(),
            ids: vec![clip],
        }),
        ..WriteOptions::default()
    };
    let plan = plan_write(
        &p,
        rev,
        &ledger,
        &options,
        Caller::Agent,
        delete_clip(track, clip),
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(plan.result().changed.deleted, vec![clip]);
    assert!(!plan.result().committed);
}

#[test]
fn gui_user_deletes_without_allow_deletion() {
    let (mut p, mut rev, mut ledger, track, clip) = caption_fixture();
    let result = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        delete_clip(track, clip),
    )
    .unwrap_or_else(|e| panic!("GUI delete was blocked: {e}"));
    assert!(result.committed);
    assert!(p.tracks[0].clips.is_empty());
}

#[test]
fn deleting_a_sequence_requires_every_child_id() {
    let mut p = project();
    let mut rev = 0;
    let mut ledger = WriteLedger::new();
    run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(
            json!({ "command": "CreateSequence", "name": "Inner" }),
        )],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let sequence = p.sequences[0].id;
    run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::User,
        vec![command(json!({
            "command": "AddTrack", "kind": "video", "name": "V", "sequence_id": sequence
        }))],
    )
    .unwrap_or_else(|e| panic!("{e}"));
    let inner_track = p.sequences[0].tracks[0].id;
    let delete = vec![command(
        json!({ "command": "DeleteSequence", "sequence_id": sequence }),
    )];

    // Only the sequence id: rejected, because its track goes too.
    let err = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options_with_deletion(vec![sequence]),
        Caller::Agent,
        delete.clone(),
    )
    .err()
    .unwrap_or_else(|| panic!("partial id list was accepted"));
    assert_eq!(err.code, EditErrorCode::DeletionNotAllowed);

    // The exact set, including the nested track and the sequence's media item: accepted.
    let media = p
        .media
        .iter()
        .find(|m| m.sequence_id == Some(sequence))
        .map(|m| m.id);
    let mut ids = vec![sequence, inner_track];
    ids.extend(media);
    let result = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &options_with_deletion(ids),
        Caller::Agent,
        delete,
    )
    .unwrap_or_else(|e| panic!("{e}"));
    assert!(result.committed);
    assert!(p.sequences.is_empty());
}

#[test]
fn moving_a_clip_to_another_track_is_not_a_deletion() {
    // A move keeps the clip's id, so the guard must not treat it as a deletion.
    let (mut p, mut rev, mut ledger, track, clip) = caption_fixture();
    let other = add_track(&mut p, &mut rev, &mut ledger, "caption");
    let result = run(
        &mut p,
        &mut rev,
        &mut ledger,
        &WriteOptions::default(),
        Caller::Agent,
        vec![command(json!({
            "command": "MoveClip", "track_id": track, "clip_id": clip,
            "new_position_secs": 2.0, "new_track_id": other
        }))],
    )
    .unwrap_or_else(|e| panic!("MoveClip was blocked by the guard: {e}"));
    assert!(result.changed.deleted.is_empty());
    assert_eq!(result.changed.modified, vec![clip]);
}

#[test]
fn capabilities_name_the_contract_version_and_rules() {
    let caps = capabilities();
    assert_eq!(
        caps.get("capabilities").and_then(Value::as_str),
        Some(CAPABILITIES)
    );
    assert!(caps.get("deletion_rule").is_some());
    assert!(caps.get("idempotency").is_some());
    assert!(caps.get("errors").is_some());
}

#[test]
fn humanize_splits_command_names() {
    assert_eq!(humanize("SetClipTransform"), "Set clip transform");
    assert_eq!(humanize("AddTrack"), "Add track");
}
