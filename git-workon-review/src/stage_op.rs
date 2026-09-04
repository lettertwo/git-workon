//! The concrete [`StagingOp`] the TUI enqueues for a hunk- or file-level staging verb (M4).
//!
//! ## The error seam
//!
//! [`crate::ops::apply_hunk`]/[`crate::ops::apply_file`] return [`ReviewError`], but the queue's
//! [`StagingOp::run`] contract is `Result<(), ApplyError>` — because the queue's one-shot
//! lock-retry keys off [`ApplyError::IndexLocked`]/[`crate::apply::is_lock_contention`], which
//! only lives inside the `Apply` arm. So [`FileStagingOp::run`] maps:
//!
//! - `Err(ReviewError::Apply(e))` → `Err(e)` — preserving the `ApplyError` (and thus the lock
//!   classification) so the queue's retry still fires.
//! - `Err(ReviewError::Synthesis | ReviewError::Git | ReviewError::Diff)` → a single defensive
//!   [`ApplyError::Io`] wrapping the message. These are not expected for a hunk/file verb on a
//!   pre-validated target: `apply_hunk`/`apply_file` ROUTE whole-file statuses rather than
//!   refuse, so a synthesis/git/diff error here means an assumption broke, not a normal outcome.
//!   Wrapping (rather than mapping to `Ok`) surfaces it on the footer instead of silently
//!   pretending the op succeeded.

use crate::apply::StageVerb;
use crate::error::{ApplyError, ReviewError};
use crate::model::FileChange;
use crate::ops;
use crate::queue::{OpContext, StagingOp};
use crate::synthesis::LineSelection;

/// A queued staging action over one captured [`FileChange`]: a hunk op when `hunk_idx` is
/// `Some`, a whole-file op when `None`. The `FileChange` is cloned at enqueue time (the file
/// list is rebuilt on the next refresh), but the DIRECTION is fixed by `verb` at construction —
/// M4 uses deterministic pane-role direction (locked decision #1), not the queue's live-index
/// toggle, so there's no snapshot-staleness to resolve inside `run`.
///
/// Line-precise selections do NOT use this type — see [`LineSelectionOp`], which applies a
/// (possibly multi-hunk) selection as one merged patch rather than per-hunk ops.
pub struct FileStagingOp {
    file: FileChange,
    hunk_idx: Option<usize>,
    verb: StageVerb,
}

impl FileStagingOp {
    /// A hunk-level op: applies `verb` to `file`'s hunk at `hunk_idx` (routing to the file op for
    /// non-hunk-patchable statuses — [`ops::apply_hunk`]'s own fallback).
    pub fn hunk(file: FileChange, hunk_idx: usize, verb: StageVerb) -> Self {
        Self {
            file,
            hunk_idx: Some(hunk_idx),
            verb,
        }
    }

    /// A whole-file op: applies `verb` to all of `file` via [`ops::apply_file`].
    pub fn file(file: FileChange, verb: StageVerb) -> Self {
        Self {
            file,
            hunk_idx: None,
            verb,
        }
    }
}

impl StagingOp for FileStagingOp {
    fn run(&mut self, ctx: &OpContext<'_>) -> Result<(), ApplyError> {
        let result = match self.hunk_idx {
            Some(hunk_idx) => {
                ops::apply_hunk(ctx.repo, ctx.applier, &self.file, hunk_idx, self.verb)
            }
            None => ops::apply_file(ctx.repo, &self.file, self.verb),
        };
        result.map_err(|err| map_review_error(err, &self.file.path))
    }
}

/// A queued line-selection staging action: applies `verb` to `file`'s selected lines, across
/// possibly-multiple hunks, as ONE combined patch via [`ops::apply_line_selections`].
///
/// Deliberately NOT one [`FileStagingOp`] per hunk: each per-hunk patch is synthesized from the
/// unstaged (index ↔ worktree) diff, so its line numbers assume every OTHER selected hunk's
/// change is already present in the file. Draining N independent per-hunk ops against the index
/// one at a time — which does NOT yet have the other hunks' changes — leaves each later op's
/// patch header inconsistent with the index's actual state, and libgit2 (strict, no fuzz) rejects
/// it. [`ops::apply_line_selections`] merges every selected hunk into one patch before applying,
/// so the whole selection lands in a single, internally-consistent apply.
pub struct LineSelectionOp {
    file: FileChange,
    selections: Vec<(usize, LineSelection)>,
    verb: StageVerb,
}

impl LineSelectionOp {
    pub fn new(file: FileChange, selections: Vec<(usize, LineSelection)>, verb: StageVerb) -> Self {
        Self {
            file,
            selections,
            verb,
        }
    }
}

impl StagingOp for LineSelectionOp {
    fn run(&mut self, ctx: &OpContext<'_>) -> Result<(), ApplyError> {
        ops::apply_line_selections(
            ctx.repo,
            ctx.applier,
            &self.file,
            &self.selections,
            self.verb,
        )
        .map_err(|err| map_review_error(err, &self.file.path))
    }
}

/// Preserve `ApplyError` (so lock retry still fires); wrap any other `ReviewError` in a single
/// defensive `ApplyError::Io`. See the module doc's "error seam".
fn map_review_error(err: ReviewError, path: &str) -> ApplyError {
    match err {
        ReviewError::Apply(e) => e,
        other => ApplyError::Io {
            path: path.to_string(),
            source: std::io::Error::other(other.to_string()),
        },
    }
}
