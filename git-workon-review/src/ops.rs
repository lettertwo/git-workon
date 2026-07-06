//! Routing: the ONE place (per the M2 design decision) that decides, for a given
//! [`FileChange`], whether a staging verb goes through the patch-synthesis-and-apply path
//! (`synthesis.rs`/`apply.rs`) or the whole-file path (`file_ops.rs`). The TUI (M4) calls only
//! these three functions — it never picks a path itself.
//!
//! ## The routing table (trap 3)
//!
//! - [`FileStatus::Modified`]/[`FileStatus::Renamed`]/[`FileStatus::Copied`], non-binary: a
//!   hunk patch can express both a preimage and a postimage, so `apply_hunk`/`apply_lines`
//!   synthesize one and hand it to the `Applier`.
//! - Everything else ([`FileStatus::Added`]/[`FileStatus::Deleted`]/[`FileStatus::Untracked`]/
//!   [`FileStatus::Unmerged`], or a binary file of any status): there is no two-sided hunk to
//!   patch — a hunk of one of these files IS the whole file. `apply_hunk` falls back to the
//!   file-level op for the verb. `apply_lines` does NOT fall back: line selection on a
//!   whole-file change is a different operation the caller asked for by mistake, so it must
//!   REFUSE with a typed error rather than silently widen the selection to "the whole file"
//!   behind the caller's back. The cleanest way to get that refusal is to call
//!   `partial_hunk_patch` unconditionally and propagate its `Result` — it already contains
//!   exactly this guard (see `synthesis.rs`), so `apply_lines` doesn't duplicate the status
//!   check.

use git2::Repository;

use crate::apply::{Applier, StageVerb};
use crate::error::ReviewError;
use crate::file_ops;
use crate::model::{FileChange, FileStatus};
use crate::synthesis::{partial_hunk_patch, whole_hunk_patch, LineSelection};

/// Whether `file`'s status/binary-ness can be expressed as a two-sided hunk patch (Modified,
/// Renamed, or Copied, and not binary) — the routing predicate shared by `apply_hunk` and the
/// doc comments above.
fn is_hunk_patchable(file: &FileChange) -> bool {
    !file.is_binary
        && matches!(
            file.status,
            FileStatus::Modified | FileStatus::Renamed | FileStatus::Copied
        )
}

/// Apply `verb` to the WHOLE of `file`'s hunk at `hunk_idx`.
///
/// Routes through patch synthesis when the file is hunk-patchable; otherwise falls back to the
/// file-level op for `verb` — a hunk of an Added/Deleted/Untracked/Unmerged or binary file IS
/// the whole file, so there's nothing hunk-specific left to do.
pub fn apply_hunk(
    repo: &Repository,
    applier: &dyn Applier,
    file: &FileChange,
    hunk_idx: usize,
    verb: StageVerb,
) -> Result<(), ReviewError> {
    if is_hunk_patchable(file) {
        let patch = whole_hunk_patch(file, hunk_idx)?;
        let (_, dest, dir) = verb.plan();
        applier.apply(repo, &patch, dest, dir)?;
        Ok(())
    } else {
        apply_file(repo, file, verb)
    }
}

/// Apply `verb` to a line-precise selection of `file`'s hunk at `hunk_idx`.
///
/// Unlike `apply_hunk`, this never falls back to a file-level op: line selection on a status a
/// hunk patch can't express (or a binary file) is a REFUSAL (trap 3), not a silent widening to
/// "the whole file." `partial_hunk_patch` already carries that refusal
/// ([`crate::error::SynthesisError::LineSelectionUnsupported`] /
/// [`crate::error::SynthesisError::BinaryFile`]), so calling it unconditionally and propagating
/// its `Result` is both the simplest routing and the correct one.
pub fn apply_lines(
    repo: &Repository,
    applier: &dyn Applier,
    file: &FileChange,
    hunk_idx: usize,
    sel: &LineSelection,
    verb: StageVerb,
) -> Result<(), ReviewError> {
    let (base, dest, dir) = verb.plan();
    let patch = partial_hunk_patch(file, hunk_idx, sel, base)?;
    applier.apply(repo, &patch, dest, dir)?;
    Ok(())
}

/// Apply `verb` to the WHOLE of `file`, unconditionally via `file_ops.rs` — no synthesis
/// involved. This is also `apply_hunk`'s fallback for statuses a hunk patch can't express.
///
/// `Discard` on an [`FileStatus::Untracked`] file is the one verb/status pair with no `HEAD`
/// copy to check out: [`file_ops::discard_file`]'s `checkout_head` has nothing to restore, so
/// "discard" instead means deleting the working-tree file outright
/// ([`file_ops::clean_untracked`]).
pub fn apply_file(
    repo: &Repository,
    file: &FileChange,
    verb: StageVerb,
) -> Result<(), ReviewError> {
    match verb {
        StageVerb::Stage => file_ops::stage_file(repo, &file.path)?,
        StageVerb::Unstage => file_ops::unstage_file(repo, &file.path)?,
        StageVerb::Discard => {
            if file.status == FileStatus::Untracked {
                file_ops::clean_untracked(repo, &file.path)?
            } else {
                file_ops::discard_file(repo, &file.path)?
            }
        }
    }
    Ok(())
}
