//! Typed errors for the agent edit contract (docs/command-api.md "Edit contract").
//!
//! Every write surface (core, Tauri, live bridge, MCP, in-app agent tools, CLI) reports
//! failures with one of these codes. The codes are stable strings; the `message` says what
//! went wrong and the `hint` says what to do next, phrased for a model reading it.

use crate::commands::CommandError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stable error codes. The string form (`as_str`) is what goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditErrorCode {
    /// The command JSON is malformed or its fields are invalid for the project.
    InvalidCommand,
    /// A referenced track, clip, media item or sequence does not exist.
    NotFound,
    /// `expected_revision` is not the project's current revision.
    RevisionConflict,
    /// `request_id` was already used for a different payload in this session.
    RequestIdReused,
    /// The write would delete entities and the caller did not allow exactly those ids.
    DeletionNotAllowed,
    /// Another write is in progress. Retry after a short wait.
    Busy,
    /// A batch failed part-way; nothing from the batch was applied.
    RolledBack,
    /// An unexpected failure inside the engine, file I/O or an optional backend.
    Internal,
}

impl EditErrorCode {
    /// The stable wire string, e.g. `revision_conflict`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCommand => "invalid_command",
            Self::NotFound => "not_found",
            Self::RevisionConflict => "revision_conflict",
            Self::RequestIdReused => "request_id_reused",
            Self::DeletionNotAllowed => "deletion_not_allowed",
            Self::Busy => "busy",
            Self::RolledBack => "rolled_back",
            Self::Internal => "internal",
        }
    }
}

/// A contract error: code, message, hint, and optional structured detail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}", code = .code.as_str())]
pub struct EditError {
    pub code: EditErrorCode,
    pub message: String,
    pub hint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// Hint for a stale `expected_revision`. Kept as a constant so every surface says the same thing.
pub const REVISION_HINT: &str = "re-read with timeline, then retry";

impl EditError {
    pub fn new(code: EditErrorCode, message: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: hint.into(),
            data: None,
        }
    }

    /// Attach structured detail (for example `{"current_revision": 7}`).
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// `invalid_command` with the standard hint.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(
            EditErrorCode::InvalidCommand,
            message,
            "call describe_commands for the exact command fields and types, then retry",
        )
    }

    /// `revision_conflict` carrying the revision the project is at now.
    pub fn revision_conflict(expected: u64, current: u64) -> Self {
        Self::new(
            EditErrorCode::RevisionConflict,
            format!("expected_revision {expected} is stale; the project is at revision {current}"),
            REVISION_HINT,
        )
        .with_data(serde_json::json!({ "current_revision": current }))
    }

    /// `request_id_reused`: the id was used for a different payload.
    pub fn request_id_reused(request_id: &str) -> Self {
        Self::new(
            EditErrorCode::RequestIdReused,
            format!("request_id '{request_id}' was already used for a different write in this session"),
            "use a new request_id for a different edit, or resend the identical payload to get the original result",
        )
    }

    /// `internal`.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            EditErrorCode::Internal,
            message,
            "retry once; if it repeats, report the message",
        )
    }

    /// `busy`.
    pub fn busy() -> Self {
        Self::new(
            EditErrorCode::Busy,
            "another edit is in progress on this project",
            "wait a moment, then retry the same request",
        )
    }

    /// Map a batch failure. A single command reports its own code; a batch reports
    /// `rolled_back` with the index and name of the command that failed.
    pub fn from_batch_failure(
        index: usize,
        name: &str,
        total: usize,
        cause: &CommandError,
    ) -> Self {
        let inner = Self::from_command_error(cause);
        if total <= 1 {
            return inner;
        }
        Self::new(
            EditErrorCode::RolledBack,
            format!(
                "command {index} ({name}) failed: {}; no command in this batch was applied",
                inner.message
            ),
            format!(
                "fix command {index} using its hint and resend the whole batch: {}",
                inner.hint
            ),
        )
        .with_data(serde_json::json!({
            "failed_index": index,
            "failed_command": name,
            "cause": inner.code.as_str(),
        }))
    }

    /// Map a core `CommandError` onto a contract code.
    pub fn from_command_error(err: &CommandError) -> Self {
        let message = err.to_string();
        match err {
            CommandError::TrackNotFound(_)
            | CommandError::MediaNotFound(_)
            | CommandError::ClipNotFound(_, _)
            | CommandError::SequenceNotFound(_) => Self::new(
                EditErrorCode::NotFound,
                message,
                "re-read with timeline to get current ids, then retry",
            ),
            CommandError::Media(_)
            | CommandError::Export(_)
            | CommandError::Perceive(_)
            | CommandError::Tts(_)
            | CommandError::Analysis(_)
            | CommandError::Cancelled
            | CommandError::AssetPack(_)
            | CommandError::WasmPlugin(_)
            | CommandError::Template(_)
            | CommandError::Multicam(_)
            | CommandError::ModelNotAvailable { .. }
            | CommandError::NotImplemented(_) => Self::new(
                EditErrorCode::Internal,
                message,
                "the media, model or backend this command needs is unavailable or failed; fix the input or environment, then retry",
            ),
            _ => Self::invalid(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Id;

    #[test]
    fn codes_have_stable_wire_strings() {
        let pairs = [
            (EditErrorCode::InvalidCommand, "invalid_command"),
            (EditErrorCode::NotFound, "not_found"),
            (EditErrorCode::RevisionConflict, "revision_conflict"),
            (EditErrorCode::RequestIdReused, "request_id_reused"),
            (EditErrorCode::DeletionNotAllowed, "deletion_not_allowed"),
            (EditErrorCode::Busy, "busy"),
            (EditErrorCode::RolledBack, "rolled_back"),
            (EditErrorCode::Internal, "internal"),
        ];
        for (code, text) in pairs {
            assert_eq!(code.as_str(), text);
            let json = serde_json::to_value(code).ok();
            assert_eq!(json, Some(serde_json::Value::String(text.to_string())));
        }
    }

    #[test]
    fn revision_conflict_carries_current_revision_and_hint() {
        let err = EditError::revision_conflict(3, 9);
        assert_eq!(err.code, EditErrorCode::RevisionConflict);
        assert_eq!(err.hint, REVISION_HINT);
        assert_eq!(err.data, Some(serde_json::json!({ "current_revision": 9 })));
    }

    #[test]
    fn command_errors_map_to_codes() {
        let id = Id::new_v4();
        let not_found = EditError::from_command_error(&CommandError::TrackNotFound(id));
        assert_eq!(not_found.code, EditErrorCode::NotFound);
        let overlap = EditError::from_command_error(&CommandError::Overlap(0.0, 1.0, id));
        assert_eq!(overlap.code, EditErrorCode::InvalidCommand);
        let model = EditError::from_command_error(&CommandError::ModelNotAvailable {
            model_id: "x".into(),
        });
        assert_eq!(model.code, EditErrorCode::Internal);
    }

    #[test]
    fn batch_failure_is_rolled_back_only_for_multi_command_batches() {
        let cause = CommandError::TrackNotFound(Id::new_v4());
        let single = EditError::from_batch_failure(0, "DeleteTrack", 1, &cause);
        assert_eq!(single.code, EditErrorCode::NotFound);
        let batch = EditError::from_batch_failure(2, "DeleteTrack", 3, &cause);
        assert_eq!(batch.code, EditErrorCode::RolledBack);
        assert!(batch.message.contains("command 2 (DeleteTrack)"));
        assert!(batch
            .message
            .contains("no command in this batch was applied"));
        assert_eq!(
            batch.data.as_ref().and_then(|d| d.get("cause")).cloned(),
            Some(serde_json::json!("not_found"))
        );
    }
}
