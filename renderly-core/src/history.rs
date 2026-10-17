//! Undo/redo history, shared by the app, the live bridge and the CLI (docs/architecture.md
//! "History, transactions and preview overrides"; docs/command-api.md has the command-facing
//! rules).
//!
//! `History<S>` is generic over the state type and holds no UI, clock or I/O: the caller
//! passes the current time in, so the module stays wasm-buildable and testable with synthetic
//! instants. The app instantiates it with `Arc<Project>`, so each snapshot is a cheap
//! reference-count bump.
//!
//! Each [`Record`] holds the state *before* its change. Undo and redo are the same swap between
//! two stacks, so no inverse operation is needed.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// Maximum records per stack (undo and redo each). The oldest undo record is evicted first.
pub const DEFAULT_CAP: usize = 100;

/// How long after its last change a keyed record still accepts coalesced changes, unless the
/// gesture is ended earlier with [`History::end_gesture`].
pub const COALESCE_WINDOW: Duration = Duration::from_millis(1000);

/// Command names kept per record. A long coalesced gesture or a big batch still counts every
/// command (`Record::command_count`), but only the first names are stored.
const MAX_RECORDED_COMMANDS: usize = 32;

/// One undo or redo step.
#[derive(Debug, Clone)]
pub struct Record<S> {
    label: String,
    commands: Vec<String>,
    command_count: usize,
    merge_key: Option<String>,
    /// Time of the last change folded into this record. `None` for records that never
    /// coalesce (transactions, and everything that has been undone or redone).
    touched: Option<Instant>,
    /// The state to restore when this record is undone.
    before: S,
}

impl<S> Record<S> {
    /// Display name: the first command's name, with `+N` when more commands are included.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Command names this record applied, bounded to the first few.
    pub fn commands(&self) -> &[String] {
        &self.commands
    }

    /// Total commands this record applied, including ones past the stored name list.
    pub fn command_count(&self) -> usize {
        self.command_count
    }

    /// The merge key this record still accepts coalesced changes under, if its gesture is open.
    pub fn merge_key(&self) -> Option<&str> {
        self.merge_key.as_deref()
    }

    fn absorb(&mut self, names: Vec<String>) {
        self.command_count += names.len();
        for name in names {
            if self.commands.len() >= MAX_RECORDED_COMMANDS {
                break;
            }
            self.commands.push(name);
        }
    }
}

/// A transaction in progress: changes folded into one record at the outermost commit.
#[derive(Debug)]
struct Transaction<S> {
    label: String,
    depth: usize,
    /// State before the first change in the transaction. `None` while nothing has changed.
    base: Option<S>,
    commands: Vec<String>,
    command_count: usize,
}

/// Whether undo and redo are currently available (what the UI and bridge report).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct HistoryStatus {
    pub can_undo: bool,
    pub can_redo: bool,
}

/// Bounded undo/redo stacks plus at most one open transaction. See the module docs.
#[derive(Debug)]
pub struct History<S> {
    undo: VecDeque<Record<S>>,
    redo: VecDeque<Record<S>>,
    tx: Option<Transaction<S>>,
    cap: usize,
    window: Duration,
}

impl<S> Default for History<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> History<S> {
    /// History with the default cap ([`DEFAULT_CAP`]) and coalesce window
    /// ([`COALESCE_WINDOW`]).
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_CAP, COALESCE_WINDOW)
    }

    /// History with a custom cap (at least 1) and coalesce window.
    pub fn with_limits(cap: usize, window: Duration) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            tx: None,
            cap: cap.max(1),
            window,
        }
    }

    /// Record a successful change. `before` is the state the change started from. `commands`
    /// names the commands it applied (one record for a batch). `merge_key`, when given, lets
    /// later changes with the same key amend this record while its gesture is open. Any new
    /// change clears the redo stack.
    ///
    /// Inside an open transaction the change joins the transaction, and `merge_key` is ignored.
    pub fn record<I, C>(&mut self, before: S, commands: I, merge_key: Option<&str>, now: Instant)
    where
        I: IntoIterator<Item = C>,
        C: Into<String>,
    {
        let names: Vec<String> = commands.into_iter().map(Into::into).collect();
        if let Some(tx) = self.tx.as_mut() {
            if tx.base.is_none() {
                tx.base = Some(before);
            }
            tx.command_count += names.len();
            for name in names {
                if tx.commands.len() < MAX_RECORDED_COMMANDS {
                    tx.commands.push(name);
                }
            }
            return;
        }

        self.redo.clear();
        if let Some(key) = merge_key {
            // Fields are read directly (not through `self.window()`) so the mutable borrow of
            // `self.undo` and the read of `self.window` do not conflict.
            let window = self.window;
            if let Some(top) = self.undo.back_mut() {
                let within = top
                    .touched
                    .is_some_and(|t| now.saturating_duration_since(t) <= window);
                if within && top.merge_key.as_deref() == Some(key) {
                    top.touched = Some(now);
                    top.absorb(names);
                    return;
                }
            }
        }

        let mut record = Record {
            label: label_for(&names),
            commands: Vec::new(),
            command_count: 0,
            merge_key: merge_key.map(str::to_string),
            touched: Some(now),
            before,
        };
        record.absorb(names);
        self.undo.push_back(record);
        evict_oldest(&mut self.undo, self.cap);
    }

    /// Open a transaction. Everything recorded until the matching [`commit`](Self::commit) is
    /// one record. Begins nest: an inner `begin` only increases the depth, and the label of
    /// the outermost begin is kept.
    pub fn begin(&mut self, label: &str) {
        match self.tx.as_mut() {
            Some(tx) => tx.depth += 1,
            None => {
                self.tx = Some(Transaction {
                    label: label.to_string(),
                    depth: 1,
                    base: None,
                    commands: Vec::new(),
                    command_count: 0,
                });
            }
        }
    }

    /// Close one transaction level. Only the outermost commit pushes a record, and only if
    /// something changed. Returns `true` exactly when a record was pushed.
    pub fn commit(&mut self) -> bool {
        match self.tx.as_mut() {
            None => false,
            Some(tx) if tx.depth > 1 => {
                tx.depth -= 1;
                false
            }
            Some(_) => self.finish_transaction(),
        }
    }

    /// Abort the outermost transaction, however deeply nested. Returns the state from before its
    /// first change so the caller can restore it, or `None` when nothing was open or nothing had
    /// changed. Nothing is pushed to the undo stack, and the redo stack is left as it was.
    pub fn rollback(&mut self) -> Option<S> {
        self.tx.take().and_then(|tx| tx.base)
    }

    /// Whether a transaction is open.
    pub fn in_transaction(&self) -> bool {
        self.tx.is_some()
    }

    /// Close the top record's gesture. The next change with that record's merge key starts a
    /// new record instead of amending it. The UI calls this on mouse-up or key-up.
    pub fn end_gesture(&mut self) {
        if let Some(top) = self.undo.back_mut() {
            top.merge_key = None;
        }
    }

    /// Undo one step. `current` is the state now in effect. Returns the state to install, or
    /// `None` when there is nothing to undo, in which case `current` is dropped. An open
    /// transaction is committed first.
    pub fn undo(&mut self, current: S) -> Option<S> {
        self.commit_open();
        let record = self.undo.pop_back()?;
        self.redo.push_back(Record {
            label: record.label,
            commands: record.commands,
            command_count: record.command_count,
            merge_key: None,
            touched: None,
            before: current,
        });
        evict_oldest(&mut self.redo, self.cap);
        Some(record.before)
    }

    /// Redo one step. The mirror of [`undo`](Self::undo).
    pub fn redo(&mut self, current: S) -> Option<S> {
        self.commit_open();
        let record = self.redo.pop_back()?;
        self.undo.push_back(Record {
            label: record.label,
            commands: record.commands,
            command_count: record.command_count,
            merge_key: None,
            touched: None,
            before: current,
        });
        evict_oldest(&mut self.undo, self.cap);
        Some(record.before)
    }

    /// Drop all history and any open transaction. Used when a project is opened, created or
    /// closed.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.tx = None;
    }

    /// Current undo/redo availability.
    pub fn status(&self) -> HistoryStatus {
        HistoryStatus {
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    /// Number of records on the undo stack.
    pub fn undo_depth(&self) -> usize {
        self.undo.len()
    }

    /// Number of records on the redo stack.
    pub fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// The record the next undo would restore, if any.
    pub fn peek_undo(&self) -> Option<&Record<S>> {
        self.undo.back()
    }

    fn commit_open(&mut self) {
        if self.tx.is_some() {
            self.finish_transaction();
        }
    }

    fn finish_transaction(&mut self) -> bool {
        let Some(tx) = self.tx.take() else {
            return false;
        };
        let Some(before) = tx.base else {
            return false;
        };
        self.redo.clear();
        self.undo.push_back(Record {
            label: tx.label,
            commands: tx.commands,
            command_count: tx.command_count,
            merge_key: None,
            touched: None,
            before,
        });
        evict_oldest(&mut self.undo, self.cap);
        true
    }
}

fn evict_oldest<S>(stack: &mut VecDeque<Record<S>>, cap: usize) {
    while stack.len() > cap {
        stack.pop_front();
    }
}

fn label_for(names: &[String]) -> String {
    match names {
        [] => "Edit".to_string(),
        [first] => first.clone(),
        [first, rest @ ..] => format!("{first} +{}", rest.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(ms: u64) -> Instant {
        // Anchored once per process so every test uses comparable instants.
        use std::sync::OnceLock;
        static BASE: OnceLock<Instant> = OnceLock::new();
        *BASE.get_or_init(Instant::now) + Duration::from_millis(ms)
    }

    /// Simulates the app's loop: `state` is the live value, every change records the old one.
    fn change(
        h: &mut History<u32>,
        state: &mut u32,
        to: u32,
        cmd: &str,
        key: Option<&str>,
        t: u64,
    ) {
        let before = *state;
        *state = to;
        h.record(before, [cmd], key, at(t));
    }

    #[test]
    fn record_undo_redo_round_trip() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "A", None, 0);
        change(&mut h, &mut s, 2, "B", None, 10);
        assert_eq!(
            h.status(),
            HistoryStatus {
                can_undo: true,
                can_redo: false
            }
        );

        let prev = h.undo(s).expect("undo");
        s = prev;
        assert_eq!(s, 1);
        let prev = h.undo(s).expect("undo");
        s = prev;
        assert_eq!(s, 0);
        assert!(h.undo(s).is_none());
        assert!(h.status().can_redo);

        s = h.redo(s).expect("redo");
        assert_eq!(s, 1);
        s = h.redo(s).expect("redo");
        assert_eq!(s, 2);
        assert!(h.redo(s).is_none());
    }

    #[test]
    fn a_new_change_clears_the_redo_stack() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "A", None, 0);
        s = h.undo(s).expect("undo");
        assert_eq!(h.redo_depth(), 1);
        change(&mut h, &mut s, 5, "B", None, 20);
        assert_eq!(h.redo_depth(), 0);
        assert!(h.redo(s).is_none());
    }

    #[test]
    fn transaction_commit_is_one_record() {
        let mut h = History::new();
        let mut s = 0u32;
        h.begin("Agent turn");
        change(&mut h, &mut s, 1, "A", None, 0);
        change(&mut h, &mut s, 2, "B", None, 5);
        change(&mut h, &mut s, 3, "C", None, 10);
        assert_eq!(h.undo_depth(), 0, "nothing is pushed until commit");
        assert!(h.commit());
        assert_eq!(h.undo_depth(), 1);
        let rec = h.peek_undo().expect("record");
        assert_eq!(rec.label(), "Agent turn");
        assert_eq!(rec.command_count(), 3);
        s = h.undo(s).expect("undo");
        assert_eq!(s, 0, "one undo reverts the whole transaction");
    }

    #[test]
    fn transactions_nest_and_only_the_outermost_commit_pushes() {
        let mut h = History::new();
        let mut s = 0u32;
        h.begin("outer");
        change(&mut h, &mut s, 1, "A", None, 0);
        h.begin("inner");
        change(&mut h, &mut s, 2, "B", None, 5);
        assert!(!h.commit(), "inner commit folds into the outer transaction");
        assert!(h.in_transaction());
        assert_eq!(h.undo_depth(), 0);
        assert!(h.commit(), "outer commit pushes one record");
        assert_eq!(h.undo_depth(), 1);
        assert_eq!(h.peek_undo().expect("record").label(), "outer");
        assert_eq!(h.undo(s), Some(0));
    }

    #[test]
    fn rollback_returns_the_pre_transaction_state_and_pushes_nothing() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "Before", None, 0);
        s = h.undo(s).expect("undo");
        assert_eq!(h.redo_depth(), 1);

        h.begin("tx");
        change(&mut h, &mut s, 7, "A", None, 10);
        change(&mut h, &mut s, 8, "B", None, 20);
        let base = h.rollback().expect("base");
        assert_eq!(base, 0);
        assert_eq!(h.undo_depth(), 0, "nothing pushed by rollback");
        assert_eq!(h.redo_depth(), 1, "rollback leaves the redo stack alone");
        assert!(!h.in_transaction());
    }

    #[test]
    fn rollback_aborts_the_outermost_transaction() {
        let mut h = History::new();
        let mut s = 0u32;
        h.begin("outer");
        change(&mut h, &mut s, 1, "A", None, 0);
        h.begin("inner");
        change(&mut h, &mut s, 2, "B", None, 5);
        assert_eq!(h.rollback(), Some(0));
        assert!(!h.in_transaction());
        assert!(!h.commit());
        assert_eq!(h.undo_depth(), 0);
    }

    #[test]
    fn empty_and_unchanged_transactions_push_nothing() {
        let mut h: History<u32> = History::new();
        h.begin("nothing");
        assert!(!h.commit());
        assert_eq!(h.undo_depth(), 0);
        h.begin("nothing-rolled-back");
        assert_eq!(h.rollback(), None);
        assert!(h.undo(0).is_none());
    }

    #[test]
    fn undo_commits_an_open_transaction_first() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "A", None, 0);
        h.begin("tx");
        change(&mut h, &mut s, 2, "B", None, 5);
        let prev = h.undo(s).expect("undo");
        assert_eq!(
            prev, 1,
            "the open transaction was committed and then undone"
        );
        assert!(!h.in_transaction());
    }

    #[test]
    fn same_key_within_window_coalesces_into_one_record() {
        let mut h = History::new();
        let mut s = 10u32;
        // A drag: many changes on the same key, each well within the window of the last.
        for (i, v) in [11u32, 12, 13, 14].into_iter().enumerate() {
            change(
                &mut h,
                &mut s,
                v,
                "SetClipTransform",
                Some("transform:c1"),
                (i as u64) * 100,
            );
        }
        assert_eq!(h.undo_depth(), 1, "a drag is one undo step");
        assert_eq!(h.peek_undo().expect("record").command_count(), 4);
        let prev = h.undo(s).expect("undo");
        assert_eq!(prev, 10, "undo restores the value from before the drag");
    }

    #[test]
    fn a_different_key_starts_a_new_record() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "X", Some("transform:c1"), 0);
        change(&mut h, &mut s, 2, "X", Some("effects:c1"), 10);
        assert_eq!(h.undo_depth(), 2);
    }

    #[test]
    fn a_keyless_change_breaks_the_gesture() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "X", Some("k"), 0);
        change(&mut h, &mut s, 2, "Y", None, 10);
        change(&mut h, &mut s, 3, "X", Some("k"), 20);
        assert_eq!(h.undo_depth(), 3);
    }

    #[test]
    fn an_expired_window_starts_a_new_record() {
        let mut h = History::with_limits(DEFAULT_CAP, Duration::from_millis(500));
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "X", Some("k"), 0);
        change(&mut h, &mut s, 2, "X", Some("k"), 400);
        assert_eq!(h.undo_depth(), 1, "within the window");
        change(&mut h, &mut s, 3, "X", Some("k"), 1000);
        assert_eq!(
            h.undo_depth(),
            2,
            "600 ms after the last change is past a 500 ms window"
        );
    }

    #[test]
    fn end_gesture_stops_coalescing() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "X", Some("k"), 0);
        h.end_gesture();
        change(&mut h, &mut s, 2, "X", Some("k"), 10);
        assert_eq!(h.undo_depth(), 2, "an ended gesture does not amend");
        let prev = h.undo(s).expect("undo");
        assert_eq!(prev, 1);
    }

    #[test]
    fn a_coalesced_record_reopens_after_undo_and_redo() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "X", Some("k"), 0);
        s = h.undo(s).expect("undo");
        s = h.redo(s).expect("redo");
        // The redone record must not absorb a fresh change under the old key.
        change(&mut h, &mut s, 9, "X", Some("k"), 5);
        assert_eq!(h.undo_depth(), 2);
    }

    #[test]
    fn a_transaction_never_coalesces_its_keyed_changes() {
        let mut h = History::new();
        let mut s = 0u32;
        h.begin("tx");
        change(&mut h, &mut s, 1, "X", Some("k"), 0);
        change(&mut h, &mut s, 2, "X", Some("k"), 5);
        h.commit();
        assert_eq!(
            h.undo_depth(),
            1,
            "the transaction is one record, not a merged one"
        );
        h.begin("tx2");
        change(&mut h, &mut s, 3, "X", Some("k"), 10);
        h.commit();
        assert_eq!(
            h.undo_depth(),
            2,
            "a later transaction does not merge into the earlier one"
        );
    }

    #[test]
    fn the_cap_evicts_the_oldest_undo_record() {
        let mut h = History::with_limits(3, COALESCE_WINDOW);
        let mut s = 0u32;
        for i in 1..=5u32 {
            change(&mut h, &mut s, i, "X", None, u64::from(i) * 10);
        }
        assert_eq!(h.undo_depth(), 3);
        let mut restored = Vec::new();
        while let Some(prev) = h.undo(s) {
            s = prev;
            restored.push(s);
        }
        assert_eq!(
            restored,
            vec![4, 3, 2],
            "the records for changes 1 and 2 (states 0 and 1) were evicted"
        );
    }

    #[test]
    fn the_redo_stack_is_capped_too() {
        let mut h = History::with_limits(2, COALESCE_WINDOW);
        let mut s = 0u32;
        for i in 1..=3u32 {
            change(&mut h, &mut s, i, "X", None, u64::from(i) * 10);
        }
        for _ in 0..3 {
            if let Some(prev) = h.undo(s) {
                s = prev;
            }
        }
        assert!(h.redo_depth() <= 2);
    }

    #[test]
    fn clear_drops_history_and_any_open_transaction() {
        let mut h = History::new();
        let mut s = 0u32;
        change(&mut h, &mut s, 1, "X", None, 0);
        h.begin("tx");
        h.clear();
        assert!(!h.in_transaction());
        assert!(!h.status().can_undo);
    }

    #[test]
    fn labels_summarise_batches() {
        let mut h = History::new();
        h.record(0u32, ["AddTrack", "AddClip"], None, at(0));
        assert_eq!(h.peek_undo().expect("record").label(), "AddTrack +1");
        assert_eq!(h.peek_undo().expect("record").commands().len(), 2);
    }

    #[test]
    fn stored_command_names_are_bounded_but_counted() {
        let mut h = History::new();
        h.begin("big");
        for _ in 0..(MAX_RECORDED_COMMANDS + 8) {
            h.record(0u32, ["Set"], None, at(0));
        }
        h.commit();
        let rec = h.peek_undo().expect("record");
        assert_eq!(rec.commands().len(), MAX_RECORDED_COMMANDS);
        assert_eq!(rec.command_count(), MAX_RECORDED_COMMANDS + 8);
    }
}
