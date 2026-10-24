//! The agent edit contract: one envelope around the command API, shared by every write surface
//! (docs/command-api.md "Edit contract" is the normative spec).
//!
//! A write is `plan_write(current, revision, ledger, options, caller, commands)`. It is pure:
//! it applies the commands to a scratch copy, checks them against the envelope, and returns a
//! [`WritePlan`]. The surface then either commits `plan.take_next()` (persist it, bump its
//! revision counter, emit its change event) and calls `plan.finish(Some(new_revision), ..)`,
//! or does not commit (dry run, no-op, rejected) and calls `plan.finish(None, ..)`.
//! Nothing in this module does I/O, touches undo history or reads a clock.
//!
//! Check order inside `plan_write`, which is part of the contract:
//! 1. `request_id` replay or reuse (committed writes only; dry runs never consult the ledger);
//! 2. `expected_revision` against the current revision;
//! 3. apply all commands to a scratch copy, all or nothing;
//! 4. the deletion guard, for agent callers;
//! 5. commit, unless dry run or the write changed nothing.

pub mod changes;
pub mod error;

pub use changes::{changed_ids, Changed, EntityKind};
pub use error::{EditError, EditErrorCode, REVISION_HINT};

use crate::commands::{apply_command, Command, CommandOutcome};
use crate::project::{Id, Project};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeSet, VecDeque};

/// Name of the capability set a surface reports from `describe_commands`.
pub const CAPABILITIES: &str = "renderly.capabilities.v1";

/// How many successfully committed `request_id`s a session remembers.
pub const LEDGER_CAPACITY: usize = 256;

/// Who is writing. Agent writes are subject to the deletion guard; the GUI's own actions are
/// the user acting and are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Caller {
    /// The GUI's own actions (Tauri commands invoked by the webview).
    User,
    /// Anything automated: MCP, the live bridge, in-app agent tools, the CLI. The default,
    /// so an unlabelled surface is guarded.
    #[default]
    Agent,
}

/// Explicit permission for an agent write to delete entities. Must list exactly the ids the
/// write would delete (see the deletion guard below).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllowDeletion {
    /// Why the deletion is intended. Must not be empty.
    pub reason: String,
    /// Every id the write would delete, clips, tracks, media items and sequences alike.
    pub ids: Vec<Id>,
}

/// The envelope fields a client may set on any write. Field names are the wire names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteOptions {
    /// The revision the client last read. A stale value is rejected with `revision_conflict`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
    /// Validate and report without changing the project or history.
    #[serde(default)]
    pub dry_run: bool,
    /// Client idempotency key. See `WriteLedger`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Required for an agent write that deletes anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_deletion: Option<AllowDeletion>,
    /// Deprecation messages from the parser (legacy `*_secs` parameters). Set by the surface that
    /// parsed the commands; reported in `WriteResult.warnings`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deprecations: Vec<String>,
}

/// The result every write returns on every surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteResult {
    /// True when this call changed the project. False for dry runs, no-op writes and rejected
    /// writes. True on a replayed `request_id` (the original commit).
    pub committed: bool,
    /// The project's revision after this call (unchanged unless `committed`).
    pub revision: u64,
    pub changed: Changed,
    /// Plain-language lines: one per command, then the counts of what changed.
    pub summary: Vec<String>,
    pub warnings: Vec<String>,
    /// RFC 6902 patch from the project before the write to the project after it.
    pub patch: json_patch::Patch,
    pub dry_run: bool,
    /// True when this result is the original result of an earlier committed write with the
    /// same `request_id`. Nothing was applied by this call.
    pub replayed: bool,
}

/// A committed write remembered by its `request_id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerEntry {
    request_id: String,
    fingerprint: String,
    result: WriteResult,
}

/// Bounded memory of committed writes, so a retried request is not applied twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteLedger {
    entries: VecDeque<LedgerEntry>,
    capacity: usize,
}

impl Default for WriteLedger {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteLedger {
    /// A ledger remembering the last [`LEDGER_CAPACITY`] request ids.
    pub fn new() -> Self {
        Self::with_capacity(LEDGER_CAPACITY)
    }

    /// A ledger remembering the last `capacity` request ids (at least 1).
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Number of request ids currently remembered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no request id is remembered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Forget every remembered request id. Call when a different project is opened or created.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn find(&self, request_id: &str) -> Option<&LedgerEntry> {
        self.entries.iter().find(|e| e.request_id == request_id)
    }

    fn remember(&mut self, request_id: String, fingerprint: String, result: WriteResult) {
        self.entries.retain(|e| e.request_id != request_id);
        self.entries.push_back(LedgerEntry {
            request_id,
            fingerprint,
            result,
        });
        while self.entries.len() > self.capacity {
            self.entries.pop_front();
        }
    }
}

/// A checked write, ready to commit or discard. See the module docs for the calling sequence.
#[derive(Debug)]
pub struct WritePlan {
    next: Option<Project>,
    result: WriteResult,
    outcomes: Vec<CommandOutcome>,
    /// `(request_id, fingerprint)` to remember if, and only if, the caller commits `next`.
    record: Option<(String, String)>,
}

impl WritePlan {
    /// The project to persist, when the write changes something and is not a dry run. Taking
    /// it leaves the plan without a project, so call `finish` with `Some(revision)` only after
    /// the commit has actually happened.
    pub fn take_next(&mut self) -> Option<Project> {
        self.next.take()
    }

    /// The typed outcome of each command, in order (for example the ids a command created).
    pub fn outcomes(&self) -> &[CommandOutcome] {
        &self.outcomes
    }

    /// The result so far. Its `committed` and `revision` are final only after `finish`.
    pub fn result(&self) -> &WriteResult {
        &self.result
    }

    /// Finalize the write. Pass `Some(new_revision)` only when the caller committed the project
    /// from `take_next`; pass `None` otherwise. A committed write is remembered by its
    /// `request_id`.
    pub fn finish(self, committed_at: Option<u64>, ledger: &mut WriteLedger) -> WriteResult {
        let mut result = self.result;
        if let Some(revision) = committed_at {
            result.committed = true;
            result.revision = revision;
            if let Some((request_id, fingerprint)) = self.record {
                ledger.remember(request_id, fingerprint, result.clone());
            }
        }
        result
    }
}

/// Check and apply one write. `revision` is the project's current revision; `caller` decides
/// whether the deletion guard applies. See the module docs for the order of checks.
pub fn plan_write(
    current: &Project,
    revision: u64,
    ledger: &WriteLedger,
    options: &WriteOptions,
    caller: Caller,
    commands: Vec<Command>,
) -> Result<WritePlan, EditError> {
    if commands.is_empty() {
        return Err(EditError::invalid("no commands given"));
    }
    if commands.iter().any(|c| matches!(c, Command::Export { .. })) {
        return Err(EditError::invalid(
            "Export does not edit the project; use the export tool instead of a write",
        ));
    }
    if options.dry_run {
        if let Some(command) = commands.iter().find(|c| writes_files(c)) {
            return Err(EditError::new(
                EditErrorCode::InvalidCommand,
                format!(
                    "dry_run cannot run {}: it writes files outside the project, and a dry run must change nothing",
                    command_name(command)
                ),
                "send that command without dry_run; check the rest of the batch with dry_run separately",
            ));
        }
    }
    let names: Vec<String> = commands.iter().map(command_name).collect();
    let fingerprint = fingerprint(&commands, options.allow_deletion.as_ref())?;

    if !options.dry_run {
        if let Some(request_id) = options.request_id.as_deref() {
            if let Some(entry) = ledger.find(request_id) {
                if entry.fingerprint == fingerprint {
                    let mut result = entry.result.clone();
                    result.replayed = true;
                    return Ok(WritePlan {
                        next: None,
                        result,
                        outcomes: Vec::new(),
                        record: None,
                    });
                }
                return Err(EditError::request_id_reused(request_id));
            }
        }
    }

    if let Some(expected) = options.expected_revision {
        if expected != revision {
            return Err(EditError::revision_conflict(expected, revision));
        }
    }

    let mut warnings = options.deprecations.clone();
    if caller == Caller::Agent && options.expected_revision.is_none() {
        warnings.push(format!(
            "no expected_revision was sent, so this write was not checked against your last read; pass expected_revision: {revision} (or the revision from your latest timeline/get_project) to guard against stale ids"
        ));
    }

    let mut project = current.clone();
    let mut outcomes = Vec::with_capacity(commands.len());
    let total = commands.len();
    for (index, command) in commands.into_iter().enumerate() {
        match apply_command(&mut project, command) {
            Ok(outcome) => outcomes.push(outcome),
            Err(err) => {
                return Err(EditError::from_batch_failure(
                    index,
                    &names[index],
                    total,
                    &err,
                ))
            }
        }
    }

    let detailed = changes::detailed(current, &project)?;
    if caller == Caller::Agent && !detailed.deleted.is_empty() {
        check_deletion(&detailed, options.allow_deletion.as_ref())?;
    }

    let before = to_json(current)?;
    let after = to_json(&project)?;
    let patch = json_patch::diff(&before, &after);
    let changed_anything = !patch.0.is_empty();

    let mut summary: Vec<String> = names.iter().map(|n| humanize(n)).collect();
    if changed_anything {
        summary.extend(detailed.summary_lines());
    } else {
        summary.push("Nothing changed".to_string());
    }

    let commit = changed_anything && !options.dry_run;
    let record = match (commit, options.request_id.as_ref()) {
        (true, Some(id)) => Some((id.clone(), fingerprint)),
        _ => None,
    };
    let result = WriteResult {
        committed: false,
        revision,
        changed: detailed.plain(),
        summary,
        warnings,
        patch,
        dry_run: options.dry_run,
        replayed: false,
    };
    Ok(WritePlan {
        next: commit.then_some(project),
        result,
        outcomes,
        record,
    })
}

/// Commands that write files (audio renders, generated images, transcripts, mattes) outside the
/// project file. A dry run refuses them, because it cannot undo those writes.
fn writes_files(command: &Command) -> bool {
    matches!(
        command,
        Command::GenerateVoiceover { .. }
            | Command::GenerateSticker { .. }
            | Command::GenerateBackgroundMatte { .. }
            | Command::AttachBackgroundMatte { .. }
            | Command::GenerateCaptions { .. }
    )
}

/// The deletion guard: a write that would delete anything, from an agent, must carry an
/// `allow_deletion` with a reason and exactly the deleted ids.
fn check_deletion(
    detailed: &changes::Detailed,
    allow: Option<&AllowDeletion>,
) -> Result<(), EditError> {
    let would: BTreeSet<Id> = detailed.deleted.iter().map(|(_, id)| *id).collect();
    let listed = describe_ids(detailed);
    let ids_json: Vec<String> = would.iter().map(|id| id.to_string()).collect();

    let mut problems = Vec::new();
    match allow {
        None => problems.push("no allow_deletion was given".to_string()),
        Some(a) => {
            if a.reason.trim().is_empty() {
                problems.push("allow_deletion.reason is empty".to_string());
            }
            let given: BTreeSet<Id> = a.ids.iter().copied().collect();
            if given != would {
                problems.push(
                    "allow_deletion.ids do not match the ids this write would delete exactly"
                        .to_string(),
                );
            }
        }
    }
    if problems.is_empty() {
        return Ok(());
    }

    let message = format!(
        "this write would delete {} entit{}: {listed}. Deletion is not allowed: {}.",
        would.len(),
        if would.len() == 1 { "y" } else { "ies" },
        problems.join("; ")
    );
    let hint = format!(
        "run the same write with dry_run: true to confirm the list, then resend it with \"allow_deletion\": {{\"reason\": \"<why this should be deleted>\", \"ids\": {}}}",
        json!(ids_json)
    );
    Err(
        EditError::new(EditErrorCode::DeletionNotAllowed, message, hint).with_data(json!({
            "would_delete": ids_json,
        })),
    )
}

fn describe_ids(detailed: &changes::Detailed) -> String {
    detailed
        .deleted
        .iter()
        .map(|(kind, id)| format!("{} {id}", kind_word(*kind)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn kind_word(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Media => "media",
        EntityKind::Track => "track",
        EntityKind::Clip => "clip",
        EntityKind::Sequence => "sequence",
    }
}

fn to_json(project: &Project) -> Result<Value, EditError> {
    serde_json::to_value(project)
        .map_err(|e| EditError::internal(format!("could not serialize project: {e}")))
}

/// Identity of a write's payload, for `request_id` reuse checks. Excludes the envelope fields
/// that may legitimately differ on a retry (`expected_revision`, `dry_run`).
fn fingerprint(commands: &[Command], allow: Option<&AllowDeletion>) -> Result<String, EditError> {
    serde_json::to_string(&(commands, allow))
        .map_err(|e| EditError::internal(format!("could not serialize command: {e}")))
}

/// The `command` tag of a command, e.g. `SplitClip`.
pub fn command_name(command: &Command) -> String {
    serde_json::to_value(command)
        .ok()
        .and_then(|v| v.get("command").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "Command".to_string())
}

/// `SetClipTransform` -> `Set clip transform`.
fn humanize(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The capabilities a surface reports from `describe_commands`. Stable JSON; surfaces embed it
/// unchanged so agents see one contract everywhere.
pub fn capabilities() -> Value {
    json!({
        "capabilities": CAPABILITIES,
        "project_schema_version": crate::project::SCHEMA_VERSION,
        "time": {
            "stored": "integer microseconds (`*_us` fields)",
            "grammar": [
                "integer: microseconds, e.g. 1500000",
                "\"<n>s\" or \"<n>.<f>s\": seconds, e.g. \"1.5s\"",
                "\"<n>ms\": milliseconds, e.g. \"1500ms\"",
                "\"<n>f\": frames at the project frame rate, e.g. \"90f\" (lands on the frame boundary)",
                "\"HH:MM:SS:FF\": non-drop timecode at the project frame rate, e.g. \"00:00:01:15\""
            ],
            "serialised": "integer microseconds",
            "deprecated_aliases": "the *_secs parameters (float seconds) are accepted for one release; each use adds a warning to the result",
        },
        "revisions": {
            "field": "project_revision",
            "monotonic": true,
            "scope": "per open project. The desktop app counts every committed edit, undo and redo; headless MCP and CLI sessions count their own committed writes.",
            "expected_revision": "optional on every write; if it is not the current revision the write is rejected with revision_conflict and the current revision in data.current_revision.",
        },
        "dry_run": {
            "field": "dry_run",
            "behaviour": "validates and applies the commands to a scratch copy; returns the would-be summary, changed ids and RFC 6902 patch. Changes nothing and records no history. Also runs the deletion guard, so it shows the ids a deletion would need.",
        },
        "idempotency": {
            "field": "request_id",
            "window": format!("the last {LEDGER_CAPACITY} committed request ids of the session"),
            "replay": "the same request_id with the same payload returns the original result with replayed: true and applies nothing.",
            "reuse": "the same request_id with a different payload is rejected with request_id_reused.",
            "order": "checked before expected_revision, so a replay is not a conflict.",
        },
        "results": {
            "fields": ["committed", "revision", "changed {created, modified, deleted}", "summary", "warnings", "patch", "dry_run", "replayed"],
            "changed_entities": ["media", "tracks", "clips", "sequences"],
        },
        "errors": {
            "codes": [
                EditErrorCode::InvalidCommand.as_str(),
                EditErrorCode::NotFound.as_str(),
                EditErrorCode::RevisionConflict.as_str(),
                EditErrorCode::RequestIdReused.as_str(),
                EditErrorCode::DeletionNotAllowed.as_str(),
                EditErrorCode::Busy.as_str(),
                EditErrorCode::RolledBack.as_str(),
                EditErrorCode::Internal.as_str(),
            ],
            "shape": "{code, message, hint, data?}",
        },
        "deletion_rule": {
            "applies_to": "agent callers: MCP, the live bridge, in-app agent tools and the CLI. Not the GUI's own actions.",
            "rule": "a write that would delete any clip, track, media item or sequence (including children of a deleted track or sequence) is rejected with deletion_not_allowed unless allow_deletion: {reason, ids} lists exactly those ids.",
            "field": "allow_deletion",
        },
        "concurrency": {
            "writes": "one write at a time per project. The check, apply and commit run under one lock, so expected_revision is compared against the state it is applied to.",
            "reads": "read tools return project_revision; read it before planning a write and pass it back as expected_revision.",
            "batches": "a batch applies all or nothing; a failure part-way reports rolled_back.",
        },
    })
}

#[cfg(test)]
mod tests;
