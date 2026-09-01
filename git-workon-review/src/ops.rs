//! Routing: the ONE place (per the diff-model-and-patch-synthesis design decision) that
//! decides, for a given [`FileChange`], whether a staging verb goes through the
//! patch-synthesis-and-apply path (`synthesis.rs`/`apply.rs`) or the whole-file path
//! (`file_ops.rs`). The TUI (staging verbs) calls only
//! [`apply_hunk`]/[`apply_lines`]/[`apply_line_selections`]/[`apply_file`] — it never picks a
//! path itself.
//!
//! ## The routing table (whole-file-ops fallback)
//!
//! - [`FileStatus::Modified`]/[`FileStatus::Renamed`]/[`FileStatus::Copied`], non-binary: a
//!   hunk patch can express both a preimage and a postimage, so `apply_hunk`/`apply_lines`
//!   synthesize one and hand it to the `Applier`.
//! - [`FileStatus::Deleted`]/[`FileStatus::Unmerged`], or a binary file of any status: there is
//!   no two-sided hunk to patch — a hunk of one of these files IS the whole file. `apply_hunk`
//!   falls back to the file-level op for the verb. `apply_lines` does NOT fall back: line
//!   selection on a whole-file change is a different operation the caller asked for by mistake,
//!   so it must REFUSE with a typed error rather than silently widen the selection to "the whole
//!   file" behind the caller's back. The cleanest way to get that refusal is to call
//!   `partial_hunk_patch` unconditionally and propagate its `Result` — it already contains
//!   exactly this guard (see `synthesis.rs`), so `apply_lines` doesn't duplicate the status
//!   check.
//! - [`FileStatus::Untracked`]/[`FileStatus::Added`], non-binary: no `HEAD`/index preimage
//!   exists, but `partial_hunk_patch` synthesizes a one-sided (creation) patch for these — line
//!   ops ARE supported ([`supports_line_ops`]), just not through `apply_hunk`'s whole-hunk path:
//!   `is_hunk_patchable` (and therefore `apply_hunk`'s routing) is UNCHANGED for these statuses,
//!   since a hunk-level `s`/`d` on one of these files still means "the whole file", not "the
//!   whole hunk" — there's nothing hunk-shaped left once you're not slicing by line.

use git2::Repository;

use crate::apply::{Applier, StageVerb};
use crate::error::{ReviewError, SynthesisError};
use crate::file_ops;
use crate::model::{FileChange, FileStatus};
use crate::synthesis::{partial_hunk_patch, whole_hunk_patch, LineSelection, PatchText};

/// Whether `file`'s status/binary-ness can be expressed as a two-sided hunk patch (Modified,
/// Renamed, or Copied, and not binary) — the routing predicate shared by `apply_hunk` and the
/// doc comments above.
///
/// Also the **line-op eligibility predicate** the TUI pre-validates against before offering
/// line selection: `apply_lines` REFUSES a non-hunk-patchable file (whole-file-ops fallback, no
/// silent widening), so the TUI checks this first and shows a "use the whole-file op" notice
/// instead of enqueuing a doomed line op.
pub fn is_hunk_patchable(file: &FileChange) -> bool {
    !file.is_binary
        && matches!(
            file.status,
            FileStatus::Modified | FileStatus::Renamed | FileStatus::Copied
        )
}

/// Whether `file` supports LINE-precise stage/discard — the gate `App::stage_selection`/
/// `App::discard_selection` (m4-staging) check before offering a line selection, per the
/// line-ops-on-one-sided-files handoff. Broader than [`is_hunk_patchable`]: a non-binary
/// `Untracked`/`Added` file has no two-sided hunk (so hunk-LEVEL `s`/`d` still falls back to the
/// whole file, unchanged — see this module's doc comment), but `partial_hunk_patch` CAN
/// synthesize a one-sided (creation) patch from a selection of its lines, so line ops on it are
/// not a refusal. Deliberately does NOT touch [`is_hunk_patchable`] itself: that predicate has
/// other callers (hunk routing, zoom gating) whose semantics must not change.
pub fn supports_line_ops(file: &FileChange) -> bool {
    is_hunk_patchable(file)
        || (!file.is_binary && matches!(file.status, FileStatus::Untracked | FileStatus::Added))
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
/// hunk patch can't express (or a binary file) is a REFUSAL (whole-file-ops fallback), not a
/// silent widening to "the whole file." `partial_hunk_patch` already carries that refusal
/// ([`crate::error::SynthesisError::LineSelectionUnsupported`] /
/// [`crate::error::SynthesisError::BinaryFile`]), so calling it unconditionally and propagating
/// its `Result` is both the simplest routing and the correct one.
///
/// Single-hunk only — see [`apply_line_selections`] for a selection spanning multiple hunks,
/// which must NOT be applied as N separate calls to this function (stale-metadata head, see
/// that function's docs).
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

/// Apply `verb` to a line-precise selection spanning POSSIBLY MULTIPLE hunks of `file`, as ONE
/// combined patch (stale-metadata head).
///
/// Each `(hunk_idx, LineSelection)` synthesizes its own single-hunk [`PatchText`] via
/// [`partial_hunk_patch`] (same refusals as [`apply_lines`]: propagated from the FIRST hunk that
/// fails to synthesize — binary/unsupported-status/out-of-range/empty-selection). Their `.hunks`
/// are then merged, in ASCENDING `hunk_idx` order, into a single [`PatchText`] sharing the file's
/// paths/modes (all per-hunk patches synthesize the same ones, since they're all from the same
/// `file`) — and applied ONCE.
///
/// This is NOT equivalent to calling [`apply_lines`] once per hunk: each per-hunk patch is
/// synthesized from the unstaged (index ↔ worktree) diff, so its hunk header's line numbers
/// already assume every OTHER selected hunk's change is present too (the worktree has them all).
/// Applying any single hunk's patch standalone against the index — which has none of the other
/// hunks yet — leaves the index's actual line numbering inconsistent with what that hunk's header
/// declares, and libgit2 (strict, no fuzz) rejects the apply. Merging first keeps every selected
/// hunk's numbering internally consistent within the one patch, exactly as it is in the source
/// diff, so the whole thing applies cleanly in one shot.
///
/// `selections` empty (or every hunk's own selection resolving empty) is the caller's
/// responsibility to have already refused before enqueuing — this returns
/// [`SynthesisError::EmptySelection`] as a defensive guard rather than silently no-opping.
pub fn apply_line_selections(
    repo: &Repository,
    applier: &dyn Applier,
    file: &FileChange,
    selections: &[(usize, LineSelection)],
    verb: StageVerb,
) -> Result<(), ReviewError> {
    let (base, dest, dir) = verb.plan();

    let mut ordered: Vec<&(usize, LineSelection)> = selections.iter().collect();
    ordered.sort_by_key(|(hunk_idx, _)| *hunk_idx);

    let mut merged: Option<PatchText> = None;
    for (hunk_idx, sel) in ordered {
        let patch = partial_hunk_patch(file, *hunk_idx, sel, base)?;
        match &mut merged {
            None => merged = Some(patch),
            Some(existing) => existing.hunks.extend(patch.hunks),
        }
    }
    let merged = merged.ok_or_else(|| SynthesisError::EmptySelection {
        path: file.path.clone(),
        hunk: 0,
    })?;

    applier.apply(repo, &merged, dest, dir)?;
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
