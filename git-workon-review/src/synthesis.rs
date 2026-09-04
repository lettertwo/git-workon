//! Synthesizing invertible patch text from a [`crate::model::DiffModel`].
//!
//! git2's write side takes bytes ([`git2::Diff::from_buffer`]), and the `git apply` CLI takes
//! text on stdin — but libgit2's `Repository::apply` has NO reverse flag (plan risk #1). A
//! "reverse apply" is therefore always: synthesize the forward patch, then
//! [`PatchText::invert`] it before handing it to an applier. [`PatchText`] stays structured
//! (not opaque bytes) so that inversion is a pure, testable transform instead of a text
//! rewrite.
//!
//! This module synthesizes WHOLE hunks (`[whole_hunk_patch]`) and line-precise selections
//! (`[partial_hunk_patch]`: the direction-dependent drop rules, the no-newline-at-EOF splice).

use std::collections::BTreeSet;

use crate::error::SynthesisError;
use crate::model::{FileChange, FileStatus, Hunk, LineKind};

/// Which side of a patch is the "before" image — the direction-dependent drop rules key off
/// this. Whole-hunk patches don't drop lines, so `PatchBase` only affects
/// [`partial_hunk_patch`] (and is otherwise threaded through by [`crate::apply::StageVerb::plan`]
/// to pick which model a caller synthesizes from).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchBase {
    Old,
    New,
}

/// One line of a synthesized patch — mirrors [`crate::model::HunkLine`] minus the line-number
/// bookkeeping a patch doesn't need to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchLine {
    pub kind: LineKind,
    pub content: Vec<u8>,
    pub missing_newline: bool,
}

/// One `@@ ... @@` hunk of a [`PatchText`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchHunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Verbatim `@@ -old_start,old_count +new_start,new_count @@ ...` bytes (including
    /// trailing `\n`) for a freshly synthesized (non-inverted) hunk — reused as-is from
    /// [`crate::model::Hunk::header`] so any function-context suffix git2 attached survives.
    /// [`PatchHunk`] rebuilds this field with swapped numbers (preserving the suffix) when
    /// inverted; see [`PatchText::invert`].
    pub header: Vec<u8>,
    pub lines: Vec<PatchLine>,
}

impl PatchHunk {
    /// Render this hunk's bytes: header, then each line's origin-prefixed content, splicing
    /// in the `\ No newline at end of file` marker wherever [`PatchLine::missing_newline`] is
    /// set — byte-identical algorithm to [`crate::model::Hunk::to_diff_bytes`], since
    /// [`whole_hunk_patch`] copies a model hunk's lines verbatim.
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = self.header.clone();
        for line in &self.lines {
            let prefix: u8 = match line.kind {
                LineKind::Context => b' ',
                LineKind::Addition => b'+',
                LineKind::Deletion => b'-',
            };
            out.push(prefix);
            out.extend_from_slice(&line.content);
            if line.missing_newline {
                out.extend_from_slice(b"\n\\ No newline at end of file\n");
            }
        }
        out
    }

    /// Swap old/new starts+counts, flip Addition<->Deletion (Context stays), and rebuild the
    /// header text around the swapped numbers while preserving whatever trailing bytes
    /// followed the second `@@` marker (a function-context suffix, or just `\n`).
    fn invert(&self) -> PatchHunk {
        let suffix = header_suffix(&self.header);
        let header = format!(
            "@@ -{},{} +{},{} @@",
            self.new_start, self.new_count, self.old_start, self.old_count
        )
        .into_bytes();
        let mut header = header;
        header.extend_from_slice(&suffix);

        let lines = self
            .lines
            .iter()
            .map(|line| PatchLine {
                kind: match line.kind {
                    LineKind::Addition => LineKind::Deletion,
                    LineKind::Deletion => LineKind::Addition,
                    LineKind::Context => LineKind::Context,
                },
                content: line.content.clone(),
                missing_newline: line.missing_newline,
            })
            .collect();

        PatchHunk {
            old_start: self.new_start,
            old_count: self.new_count,
            new_start: self.old_start,
            new_count: self.old_count,
            header,
            lines,
        }
    }
}

/// Everything after the second `@@` in a hunk header, e.g. `" fn foo() {\n"` or just `"\n"`.
fn header_suffix(header: &[u8]) -> Vec<u8> {
    let find = |haystack: &[u8], needle: &[u8]| {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    };
    if let Some(first) = find(header, b"@@") {
        if let Some(second_rel) = find(&header[first + 2..], b"@@") {
            let second = first + 2 + second_rel;
            return header[second + 2..].to_vec();
        }
    }
    b"\n".to_vec()
}

/// A structured, invertible patch — the render/parse boundary between the model and the
/// appliers. `old_path`/`new_path` are `None` for a `/dev/null` side (whole-file
/// creation/deletion); [`whole_hunk_patch`] always sets both, since it only synthesizes
/// Modified/Renamed files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchText {
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    /// Raw octal mode of the pre-image (see [`FileChange::old_mode`]); swapped with
    /// [`Self::new_mode`] by [`Self::invert`].
    pub old_mode: i32,
    /// Raw octal mode of the post-image (see [`FileChange::new_mode`]) — this is the mode
    /// written into the synthesized `index` line, since a forward patch's target state is the
    /// post-image.
    pub new_mode: i32,
    pub hunks: Vec<PatchHunk>,
}

impl PatchText {
    /// Render the full patch: a `diff --git`/(`new file mode`|`deleted file mode`)?/`index`/
    /// `---`/`+++` file header, then each hunk's bytes. Always ends in `\n` (each hunk's last
    /// line is either a real line with its own trailing `\n`, or a `missing_newline` line whose
    /// marker supplies one).
    ///
    /// A one-sided patch (`old_path` or `new_path` is `None` — a whole-file creation or
    /// deletion) additionally needs a `new file mode {mode:06o}` / `deleted file mode
    /// {mode:06o}` line: without it, git parses the patch as a MODIFICATION of an existing
    /// path and rejects it against an untracked/absent preimage ("does not exist in index") —
    /// this is the exact shape `git diff --no-index /dev/null <file>` emits, and the one both
    /// `git2::Diff::from_buffer` + `Repository::apply(ApplyLocation::Index)` and `git apply
    /// --cached` accept (see the go/no-go test in `tests/suite/file_ops.rs`). The `index` line
    /// that follows omits its mode suffix for a one-sided patch — canonical git output has no
    /// mode there, since the mode line above already carries it.
    ///
    /// The `index 0000000..0000000[ <mode>]` line's OIDs are a placeholder — this crate never
    /// reads blob OIDs off the model (untracked deltas don't have them either), and `git
    /// apply` ignores them. The line exists because `git2::Diff::from_buffer` parses stricter
    /// than `git apply` and rejects a bare 3-line header (plan risk #4). For a two-sided
    /// (Modified/Renamed/Copied) patch, the MODE is load-bearing: `Repository::apply
    /// (ApplyLocation::Index, ..)` takes the new index entry's mode straight from this line, so
    /// it must be the file's real mode ([`Self::new_mode`]) — a hardcoded `100644` here used to
    /// silently clobber the exec bit of any staged `100755` file (the `git apply` CLI path
    /// never had this bug: it reads the mode from the working tree instead).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let diff_git_old = self
            .old_path
            .as_deref()
            .or(self.new_path.as_deref())
            .unwrap_or("");
        let diff_git_new = self
            .new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("");
        out.extend_from_slice(format!("diff --git a/{diff_git_old} b/{diff_git_new}\n").as_bytes());
        match (&self.old_path, &self.new_path) {
            (None, Some(_)) => {
                out.extend_from_slice(format!("new file mode {:06o}\n", self.new_mode).as_bytes());
                out.extend_from_slice(b"index 0000000..0000000\n");
            }
            (Some(_), None) => {
                out.extend_from_slice(
                    format!("deleted file mode {:06o}\n", self.old_mode).as_bytes(),
                );
                out.extend_from_slice(b"index 0000000..0000000\n");
            }
            _ => {
                out.extend_from_slice(
                    format!("index 0000000..0000000 {:06o}\n", self.new_mode).as_bytes(),
                );
            }
        }
        let old_label = match &self.old_path {
            Some(p) => format!("a/{p}"),
            None => "/dev/null".to_string(),
        };
        let new_label = match &self.new_path {
            Some(p) => format!("b/{p}"),
            None => "/dev/null".to_string(),
        };
        out.extend_from_slice(format!("--- {old_label}\n").as_bytes());
        out.extend_from_slice(format!("+++ {new_label}\n").as_bytes());
        for hunk in &self.hunks {
            out.extend_from_slice(&hunk.to_bytes());
        }
        out
    }

    /// Pure transform: swap old/new paths and invert every hunk (the direction-dependent drop
    /// rules' Old/New base swap,
    /// applied wholesale). Needed because `Repository::apply` has no reverse flag — a
    /// "reverse apply" is `invert()` then a forward apply. `invert(invert(p)) == p` (tested).
    pub fn invert(&self) -> PatchText {
        PatchText {
            old_path: self.new_path.clone(),
            new_path: self.old_path.clone(),
            old_mode: self.new_mode,
            new_mode: self.old_mode,
            hunks: self.hunks.iter().map(PatchHunk::invert).collect(),
        }
    }
}

/// Shared guard preamble for [`whole_hunk_patch`] and [`partial_hunk_patch`]: refuse binary
/// files and statuses a hunk patch can't express, look up `hunk_idx`, and derive the
/// old/new path labels — extracted so the two synthesis entry points can't drift apart on
/// these checks.
///
/// Refuses:
/// - binary files ([`SynthesisError::BinaryFile`]) — no hunks exist to synthesize from.
/// - statuses NEITHER a two-sided NOR a one-sided (creation) hunk patch can express
///   ([`SynthesisError::LineSelectionUnsupported`]): `Deleted` (a hunk patch of a deletion would
///   stage an empty blob instead of removing the file — whole-file-ops fallback) and `Unmerged`.
///   The whole-file ops and routing layer's `ops.rs` routes these statuses to `file_ops.rs`
///   before synthesis is ever reached, so
///   `LineSelectionUnsupported` is the variant callers see here — it's the closest existing
///   error to "use the whole-file op instead," which is exactly its `help` text.
///   `Copied` is treated like `Renamed` (both carry an `old_path`).
/// - `hunk_idx` out of range ([`SynthesisError::HunkOutOfRange`]).
///
/// Admits (does NOT refuse): `Modified`/`Renamed`/`Copied` (the original two-sided callers), and
/// — per the line-ops-on-one-sided-files handoff — non-binary `Untracked`/`Added`: these have no
/// `HEAD`/index preimage, but [`partial_hunk_patch`] synthesizes a one-sided (creation) patch for
/// them instead of a two-sided one (see that function's doc). [`whole_hunk_patch`] is technically
/// reachable on these statuses too (it shares this guard), but nothing calls it that way —
/// `ops::apply_hunk`'s routing (`is_hunk_patchable`) is deliberately UNCHANGED and still falls
/// back to the whole-file op for `Untracked`/`Added`, so `whole_hunk_patch` never actually
/// synthesizes a one-sided patch in practice.
fn selectable_hunk(
    file: &FileChange,
    hunk_idx: usize,
) -> Result<(&Hunk, String, String), SynthesisError> {
    if file.is_binary {
        return Err(SynthesisError::BinaryFile {
            path: file.path.clone(),
        });
    }
    match file.status {
        FileStatus::Modified
        | FileStatus::Renamed
        | FileStatus::Copied
        | FileStatus::Untracked
        | FileStatus::Added => {}
        other => {
            return Err(SynthesisError::LineSelectionUnsupported {
                path: file.path.clone(),
                status: other,
            })
        }
    }
    let hunk = file
        .hunks
        .get(hunk_idx)
        .ok_or_else(|| SynthesisError::HunkOutOfRange {
            path: file.path.clone(),
            index: hunk_idx,
        })?;

    let old_path = file.old_path.clone().unwrap_or_else(|| file.path.clone());
    let new_path = file.path.clone();
    Ok((hunk, old_path, new_path))
}

/// Synthesize a patch for the WHOLE of `file`'s hunk at `hunk_idx` — no line selection, so the
/// direction-dependent drop rules don't apply; the hunk's lines are copied verbatim.
///
/// Same refusals as [`selectable_hunk`].
pub fn whole_hunk_patch(file: &FileChange, hunk_idx: usize) -> Result<PatchText, SynthesisError> {
    let (hunk, old_path, new_path) = selectable_hunk(file, hunk_idx)?;

    let lines = hunk
        .lines
        .iter()
        .map(|line| PatchLine {
            kind: line.kind,
            content: line.content.clone(),
            missing_newline: line.missing_newline,
        })
        .collect();

    Ok(PatchText {
        old_path: Some(old_path),
        new_path: Some(new_path),
        old_mode: file.old_mode,
        new_mode: file.new_mode,
        hunks: vec![PatchHunk {
            old_start: hunk.old_start,
            old_count: hunk.old_count,
            new_start: hunk.new_start,
            new_count: hunk.new_count,
            header: hunk.header.clone(),
            lines,
        }],
    })
}

/// Which trap-2 splice (if any) an emitted line may need — see [`splice_eofnl_context_lines`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpliceNeed {
    /// No missing-newline complication: an ordinary context/addition/kept-deletion line.
    None,
    /// A dropped-DELETION-turned-context line (`base == Old`'s drop rule). Its OLD-side bytes
    /// must byte-match the real preimage exactly (marker included) — [`partial_hunk_patch`]
    /// matches both sides against real content under `base == Old` — so fixing the newline
    /// requires splitting into a delete (unchanged) + re-add (real trailing `\n`) pair rather
    /// than rewriting the line in place.
    ConvertedContext,
    /// A KEPT deletion (`sel.keep_dels` — any `base`). Unlike `ConvertedContext`, a kept
    /// deletion's bytes are never matched against anything: they're the OUTPUT written by a
    /// reverse apply (`base == New`, [`crate::apply::StageVerb::Unstage`]/`Discard`) or simply
    /// not otherwise constrained under `base == Old`. So the fix here just rewrites the line's
    /// own bytes in place (real trailing `\n`, marker dropped) instead of splitting.
    KeptDeletion,
}

/// No-newline-at-EOF splice: rewrite a deletion-shaped line that carries
/// [`crate::model::HunkLine::missing_newline`] when a LATER emitted line is [`LineKind::Context`]
/// — the shape that lets `git apply` silently corrupt content (see below). Two shapes reach
/// here, tagged by [`SpliceNeed`]: a dropped-deletion-turned-context line (`base == Old`) and a
/// KEPT deletion emitted as-is (any `base`) — both can carry the marker through to `emitted`
/// unchanged, and both are followed by a later context line under exactly the same
/// precondition: `base == New`'s drop rule converts dropped additions to context, so anything
/// emitted before those (a kept deletion, or — under `base == Old` — a different dropped
/// deletion) sits in front of context lines it was never meant to interact with.
///
/// `base == Old`'s drop rule turns a dropped deletion into a plain context line (see
/// [`partial_hunk_patch`]). That is correct UNLESS the original deletion was the file's old-side
/// EOF (no trailing newline) and the new file has more content after it (kept additions) — i.e.
/// the file transitions from "ends without a newline here" to "has more content after this
/// line," which is a real change to this line (it now needs a trailing newline) that a context
/// line — byte-identical on both sides by definition — cannot express.
///
/// A KEPT deletion under `base == New` has the identical symptom for a different reason: it
/// isn't converted to context, but a LATER dropped addition still gets converted to context
/// (the mirror rule), so the same "no-newline line, then a context line" shape results. Because
/// this deletion's bytes are pure OUTPUT (see [`SpliceNeed::KeptDeletion`]), no split is needed
/// — only its own bytes change. Only a LATER **context** line is dangerous: a later Addition
/// alone is the ordinary, well-formed "changed EOF line" diff shape (`-old\n\ No newline...\n
/// +new\n+more\n`) that `git apply` has always handled correctly; this is why the check below
/// looks for a later `Context`, not just "anything later." An earlier version of this fix only
/// handled the `ConvertedContext` case and missed `KeptDeletion`, which affects both appliers:
/// CLI silently corrupts (see below); git2 rejects the patch outright (`invalid patch hunk`) —
/// a real behavioral divergence between the two, not just a theoretical gap.
///
/// `git apply` does not reject the malformed (`ConvertedContext`) shape. Empirically (verified
/// against the system `git` binary; see `tests/line_synthesis.rs`'s tripwire), it silently
/// accepts a context line missing its newline followed by more lines and concatenates the next
/// line's bytes directly onto it with no separator — e.g. content `"last"` (no `\n`) followed by
/// an addition `"more\n"` becomes the single corrupt line `"lastmore\n"` in the applied blob,
/// exit code 0.
///
/// The `ConvertedContext` fix: emit the line as a deletion (unchanged: original content,
/// `missing_newline: true`, carrying its own `\ No newline at end of file` marker) immediately
/// followed by a re-add of the SAME line — but WITH a real trailing newline appended and no
/// marker, since in the new file this line is no longer the last one. This nets the same +1
/// old/+1 new count as a context line while giving git a real newline byte to anchor the next
/// line on.
///
/// Note this deviates from an earlier draft of this fix that also marked the re-added copy
/// `missing_newline: true` (mirroring the deletion exactly). That form was tested empirically
/// against `git apply` and found to reproduce the IDENTICAL corruption (`git apply --check`
/// reports success, but the applied content is still the concatenated `"lastmore\n"`) — the
/// marker on a non-final `+` line doesn't stop git from raw-copying the next line onto it. Only
/// giving the re-added line a genuine trailing newline (no marker) produces the correct byte
/// sequence, hence the choice codified here.
///
/// The `KeptDeletion` fix rewrites the line's own content in place (append a real `\n`, clear
/// `missing_newline`) — no companion line, and no change to the hunk's old/new counts, since a
/// kept deletion only ever contributed to the old side either way.
///
/// A dropped ADDITION converted to context (`base == New`) can never itself need either fix:
/// [`crate::model::HunkLine::missing_newline`] is only ever set on a line that is the true last
/// line of the file (see the module docs on the EOFNL characterization), and
/// [`partial_hunk_patch`] never reorders `hunk.lines` — so an addition carrying the flag is
/// always the LAST entry synthesized for its hunk, with nothing emitted after it to corrupt
/// against. `tests/line_synthesis.rs` has a test pinning this reasoning against a real apply
/// rather than asserting it blind.
fn splice_eofnl_context_lines(emitted: Vec<(PatchLine, SpliceNeed)>) -> Vec<PatchLine> {
    // Suffix scan: `later_has_context[i]` is true iff some `emitted[j]` with `j > i` is a
    // Context line — computed once up front (over the PRE-splice kinds) so the loop below can
    // ask "is anything dangerous coming after me" without repeated rescans.
    let mut later_has_context = vec![false; emitted.len()];
    let mut seen_context = false;
    for i in (0..emitted.len()).rev() {
        later_has_context[i] = seen_context;
        if emitted[i].0.kind == LineKind::Context {
            seen_context = true;
        }
    }

    let last = emitted.len().saturating_sub(1);
    let mut out = Vec::with_capacity(emitted.len());
    for (i, (line, need)) in emitted.into_iter().enumerate() {
        match need {
            SpliceNeed::ConvertedContext if line.missing_newline && i != last => {
                let mut readded = line.content.clone();
                readded.push(b'\n');
                out.push(PatchLine {
                    kind: LineKind::Deletion,
                    content: line.content,
                    missing_newline: true,
                });
                out.push(PatchLine {
                    kind: LineKind::Addition,
                    content: readded,
                    missing_newline: false,
                });
            }
            SpliceNeed::KeptDeletion if line.missing_newline && later_has_context[i] => {
                let mut content = line.content;
                content.push(b'\n');
                out.push(PatchLine {
                    kind: LineKind::Deletion,
                    content,
                    missing_newline: false,
                });
            }
            _ => out.push(line),
        }
    }
    out
}

/// Which of a hunk's addition/deletion lines to keep in a line-precise patch.
///
/// Indices are into [`crate::model::Hunk::lines`] — the `Vec` position, NOT the old/new line
/// numbers (`HunkLine::old_lnum`/`new_lnum`), which are `None` for the wrong side of an
/// add/del and therefore can't uniquely key a selection on their own.
///
/// An index that doesn't name an [`LineKind::Addition`] line in `keep_adds` (or a
/// [`LineKind::Deletion`] line in `keep_dels`) — because it's out of range, or names a
/// [`LineKind::Context`] line, or is in the wrong set — is silently ignored by
/// [`partial_hunk_patch`]. This matches the frozen prototype, whose keep-sets were predicates
/// over lines and inherently ignored anything that didn't match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineSelection {
    pub keep_adds: BTreeSet<usize>,
    pub keep_dels: BTreeSet<usize>,
}

/// Synthesize a patch for a LINE-PRECISE selection of `file`'s hunk at `hunk_idx` — the
/// direction-dependent drop rules.
///
/// Context lines are always emitted as context. For the rest, `base` decides what happens to a
/// line that ISN'T kept:
///
/// - `base == Old` (forward apply — [`crate::apply::StageVerb::Stage`], staging into an index
///   that doesn't have the change yet): a dropped addition is OMITTED (the index shouldn't
///   gain it); a dropped deletion becomes CONTEXT (the index should keep what's still there).
/// - `base == New` (reverse apply — [`crate::apply::StageVerb::Unstage`]/[`Discard`], where the
///   apply target ALREADY has the change and reverse-applying undoes the kept lines): a dropped
///   addition becomes CONTEXT (it must stay in the target, so it has to match on reverse-apply
///   just like an untouched line does); a dropped deletion is OMITTED (it's already absent from
///   the target, so it must never be matched against). This is the mirror of the `Old` rules,
///   not merely a coincidence: whichever side already contains the "dropped" line is the side
///   the patch's context has to agree with, and `base` names that side.
///
///   [`crate::apply::CliApplier`]/[`crate::apply::Git2Applier`] reverse-apply by adding
///   `--reverse` or by [`PatchText::invert`]ing before a forward apply — either way the patch
///   itself is always WRITTEN in forward orientation with the rules above; a `base == Old`
///   patch fed through a reverse apply is a different, incompatible set of drop rules and git
///   rejects it outright (see the tripwire test in `tests/line_synthesis.rs`).
///
/// [`LineSelection`] entries that don't name an add/del line in this hunk are ignored (see
/// [`LineSelection`]'s docs). If, after ignoring those, no addition and no deletion ended up
/// kept, there is nothing to synthesize a patch for: [`SynthesisError::EmptySelection`].
///
/// Counts are recomputed per emitted line (context, converted-to-context, kept-add, kept-del
/// all bump the relevant side(s)); the header is rebuilt as
/// `@@ -old_start,old_count +new_start,new_count @@` plus the source hunk's header suffix
/// (reused via [`header_suffix`]) — `new_start` is unchanged, only the counts move (and, for a
/// one-sided source, `old_start` — see below).
///
/// Same refusals as [`whole_hunk_patch`]: binary files ([`SynthesisError::BinaryFile`]),
/// unsupported statuses ([`SynthesisError::LineSelectionUnsupported`]), and an out-of-range
/// `hunk_idx` ([`SynthesisError::HunkOutOfRange`]).
///
/// ## One-sided sources (`Untracked`/`Added`, no `HEAD`/index preimage)
///
/// `hunk.lines` is ALL [`LineKind::Addition`] for these statuses (nothing pre-existed, so
/// there's nothing to have context or a deletion among) — the direction rules above still apply
/// mechanically (a dropped addition is omitted under `base == Old`, converted to context under
/// `base == New`), but the RENDERED patch's shape depends on whether any Context line ended up
/// emitted:
///
/// - `base == Old` (staging a subset of an untracked file's lines): dropped additions are always
///   OMITTED, never converted to context (there's no deletion rule to mirror them into) — so no
///   Context line is ever emitted here, and the rendered patch is always a pure creation:
///   `old_path: None`.
/// - `base == New` (unstaging an Added file's lines, or discarding an Untracked file's lines):
///   dropped additions convert to Context — the lines NOT selected for the reverse-apply must
///   stay in the target. If any survive as Context, the patch has a real (non-empty) old side —
///   `old_path: Some(path)`, so `invert()` renders it as a two-sided modification, not a
///   deletion. If EVERY addition was kept (no Context survives — a full-file selection),
///   `old_path` stays `None`: `invert()` then renders a deletion, matching "removing the whole
///   file" — though [`crate::app`]'s discard flow routes a full untracked selection to the
///   whole-file confirm (fork 2 of the handoff) rather than relying on this implicitly.
///
/// Either way, `old_start` in the rendered header is `0` when the final `old_count` is `0` (pure
/// creation/no old side), else `1` (a real, if partial, old-side region starting at the file's
/// first line) — `hunk.old_start` itself is `0` for these statuses (git2 has no old-side line
/// numbers to report), so it can't be reused verbatim once the old side gains content the way a
/// two-sided source's `old_start` can.
///
/// A deletion line carrying [`crate::model::HunkLine::missing_newline`] — whether a dropped
/// deletion converted to context (see above) or a KEPT deletion emitted verbatim — followed by
/// any other emitted line, is spliced by [`splice_eofnl_context_lines`] into git's canonical
/// delete+re-add form rather than left as a raw context/deletion line — see that function's docs
/// for why (no-newline-at-EOF splice).
pub fn partial_hunk_patch(
    file: &FileChange,
    hunk_idx: usize,
    sel: &LineSelection,
    base: PatchBase,
) -> Result<PatchText, SynthesisError> {
    let (hunk, old_path, new_path) = selectable_hunk(file, hunk_idx)?;

    let mut kept_any = false;
    // Each entry pairs the emitted line with which trap-2 splice check (if any) it needs — see
    // [`SpliceNeed`] — kept as one `push` per line so the two can't drift out of sync with each
    // other.
    let mut emitted: Vec<(PatchLine, SpliceNeed)> = Vec::with_capacity(hunk.lines.len());

    for (idx, line) in hunk.lines.iter().enumerate() {
        match line.kind {
            LineKind::Context => {
                emitted.push((
                    PatchLine {
                        kind: LineKind::Context,
                        content: line.content.clone(),
                        missing_newline: line.missing_newline,
                    },
                    SpliceNeed::None,
                ));
            }
            LineKind::Addition => {
                if sel.keep_adds.contains(&idx) {
                    kept_any = true;
                    emitted.push((
                        PatchLine {
                            kind: LineKind::Addition,
                            content: line.content.clone(),
                            missing_newline: line.missing_newline,
                        },
                        SpliceNeed::None,
                    ));
                } else if base == PatchBase::New {
                    // Dropped addition, base=New: it must remain in the (already-changed)
                    // target, so it has to match as context on reverse-apply.
                    emitted.push((
                        PatchLine {
                            kind: LineKind::Context,
                            content: line.content.clone(),
                            missing_newline: line.missing_newline,
                        },
                        SpliceNeed::None,
                    ));
                }
                // base=Old: dropped addition is omitted — the target doesn't have it yet and
                // shouldn't gain it.
            }
            LineKind::Deletion => {
                if sel.keep_dels.contains(&idx) {
                    kept_any = true;
                    emitted.push((
                        PatchLine {
                            kind: LineKind::Deletion,
                            content: line.content.clone(),
                            missing_newline: line.missing_newline,
                        },
                        SpliceNeed::KeptDeletion,
                    ));
                } else if base == PatchBase::Old {
                    // Dropped deletion, base=Old: it's still there in the target, so it has to
                    // match as context.
                    emitted.push((
                        PatchLine {
                            kind: LineKind::Context,
                            content: line.content.clone(),
                            missing_newline: line.missing_newline,
                        },
                        SpliceNeed::ConvertedContext,
                    ));
                }
                // base=New: dropped deletion is omitted — it's already absent from the target
                // and must never be matched against.
            }
        }
    }

    if !kept_any {
        return Err(SynthesisError::EmptySelection {
            path: file.path.clone(),
            hunk: hunk_idx,
        });
    }

    let lines = splice_eofnl_context_lines(emitted);

    // Counts are derived from the FINAL (post-splice) lines, not accumulated during the
    // selection loop above — the trap-2 splice can grow a single kept deletion into a
    // deletion+addition pair, which would otherwise leave the header's `new_count` short by
    // one and produce a hunk whose declared counts don't match its body.
    let old_count: u32 = lines
        .iter()
        .filter(|l| matches!(l.kind, LineKind::Context | LineKind::Deletion))
        .count() as u32;
    let new_count: u32 = lines
        .iter()
        .filter(|l| matches!(l.kind, LineKind::Context | LineKind::Addition))
        .count() as u32;

    // One-sided sources (no HEAD/index preimage) get a possibly-`None` old_path and a
    // recomputed old_start, per this function's doc comment; two-sided sources keep their
    // existing (always non-`None`, always-`hunk.old_start`) behavior unchanged.
    let one_sided_source = matches!(file.status, FileStatus::Untracked | FileStatus::Added);
    let old_path = if one_sided_source {
        if old_count == 0 {
            None
        } else {
            Some(old_path)
        }
    } else {
        Some(old_path)
    };
    let old_start = if one_sided_source {
        if old_count == 0 {
            0
        } else {
            1
        }
    } else {
        hunk.old_start
    };

    let mut header = format!(
        "@@ -{old_start},{old_count} +{},{new_count} @@",
        hunk.new_start
    )
    .into_bytes();
    header.extend_from_slice(&header_suffix(&hunk.header));

    Ok(PatchText {
        old_path,
        new_path: Some(new_path),
        old_mode: file.old_mode,
        new_mode: file.new_mode,
        hunks: vec![PatchHunk {
            old_start,
            old_count,
            new_start: hunk.new_start,
            new_count,
            header,
            lines,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Hunk, HunkLine};

    fn modified_file(hunk: Hunk) -> FileChange {
        FileChange {
            path: "f.txt".to_string(),
            old_path: None,
            status: FileStatus::Modified,
            is_binary: false,
            old_mode: 0o100644,
            new_mode: 0o100644,
            hunks: vec![hunk],
        }
    }

    fn simple_hunk() -> Hunk {
        Hunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 3,
            header: b"@@ -1,3 +1,3 @@\n".to_vec(),
            lines: vec![
                HunkLine {
                    kind: LineKind::Context,
                    content: b"line1\n".to_vec(),
                    old_lnum: Some(1),
                    new_lnum: Some(1),
                    missing_newline: false,
                },
                HunkLine {
                    kind: LineKind::Deletion,
                    content: b"line2\n".to_vec(),
                    old_lnum: Some(2),
                    new_lnum: None,
                    missing_newline: false,
                },
                HunkLine {
                    kind: LineKind::Addition,
                    content: b"CHANGED\n".to_vec(),
                    old_lnum: None,
                    new_lnum: Some(2),
                    missing_newline: false,
                },
                HunkLine {
                    kind: LineKind::Context,
                    content: b"line3\n".to_vec(),
                    old_lnum: Some(3),
                    new_lnum: Some(3),
                    missing_newline: false,
                },
            ],
        }
    }

    #[test]
    fn whole_hunk_patch_renders_exact_bytes() {
        let file = modified_file(simple_hunk());
        let patch = whole_hunk_patch(&file, 0).unwrap();

        let expected = [
            "diff --git a/f.txt b/f.txt\n",
            "index 0000000..0000000 100644\n",
            "--- a/f.txt\n",
            "+++ b/f.txt\n",
            "@@ -1,3 +1,3 @@\n",
            " line1\n",
            "-line2\n",
            "+CHANGED\n",
            " line3\n",
        ]
        .concat()
        .into_bytes();

        assert_eq!(patch.to_bytes(), expected);
    }

    #[test]
    fn whole_hunk_patch_carries_real_mode_into_index_line() {
        let mut file = modified_file(simple_hunk());
        file.old_mode = 0o100755;
        file.new_mode = 0o100755;
        let patch = whole_hunk_patch(&file, 0).unwrap();

        let rendered = patch.to_bytes();
        let rendered = String::from_utf8(rendered).unwrap();
        assert!(
            rendered.contains("index 0000000..0000000 100755\n"),
            "expected the real 100755 mode in the index line, got: {rendered}"
        );
    }

    #[test]
    fn invert_swaps_old_and_new_mode() {
        let mut file = modified_file(simple_hunk());
        file.old_mode = 0o100644;
        file.new_mode = 0o100755;
        let patch = whole_hunk_patch(&file, 0).unwrap();

        let inverted = patch.invert();
        assert_eq!(inverted.old_mode, 0o100755);
        assert_eq!(inverted.new_mode, 0o100644);
        assert!(String::from_utf8(inverted.to_bytes())
            .unwrap()
            .contains("index 0000000..0000000 100644\n"));
    }

    #[test]
    fn whole_hunk_render_body_matches_model_hunk_to_diff_bytes() {
        let hunk = simple_hunk();
        let file = modified_file(hunk.clone());
        let patch = whole_hunk_patch(&file, 0).unwrap();

        // Strip the file header (4 lines: diff --git/index/---/+++) to compare just the hunk
        // body against the model's own byte-fidelity contract.
        let rendered = patch.to_bytes();
        let body_start = rendered
            .windows(2)
            .position(|w| w == b"@@")
            .expect("hunk header present");
        let body = &rendered[body_start..];

        assert_eq!(body, hunk.to_diff_bytes().as_slice());
    }

    #[test]
    fn invert_of_invert_is_identity() {
        let file = modified_file(simple_hunk());
        let patch = whole_hunk_patch(&file, 0).unwrap();

        assert_eq!(patch.invert().invert(), patch);
    }

    #[test]
    fn invert_swaps_paths_and_line_kinds() {
        let file = modified_file(simple_hunk());
        let patch = whole_hunk_patch(&file, 0).unwrap();
        let inverted = patch.invert();

        assert_eq!(inverted.old_path, patch.new_path);
        assert_eq!(inverted.new_path, patch.old_path);
        assert_eq!(inverted.hunks[0].old_start, patch.hunks[0].new_start);
        assert_eq!(inverted.hunks[0].new_start, patch.hunks[0].old_start);
        assert_eq!(inverted.hunks[0].lines[1].kind, LineKind::Addition);
        assert_eq!(inverted.hunks[0].lines[2].kind, LineKind::Deletion);
        // Content and missing_newline travel with the line, unchanged.
        assert_eq!(inverted.hunks[0].lines[1].content, b"line2\n");
    }

    #[test]
    fn invert_moves_missing_newline_marker_with_its_line() {
        let mut hunk = simple_hunk();
        // The deletion (old side) has no trailing newline.
        hunk.lines[1].content = b"line2".to_vec();
        hunk.lines[1].missing_newline = true;
        let file = modified_file(hunk);
        let patch = whole_hunk_patch(&file, 0).unwrap();

        let inverted = patch.invert();
        // The deletion becomes an addition in the inverted patch, carrying the flag with it.
        assert_eq!(inverted.hunks[0].lines[1].kind, LineKind::Addition);
        assert!(inverted.hunks[0].lines[1].missing_newline);
        assert_eq!(inverted.hunks[0].lines[1].content, b"line2");
    }

    /// A hand-built one-sided (creation) [`PatchText`] — `whole_hunk_patch`/`partial_hunk_patch`
    /// don't synthesize these yet (that's step 3, gated on `selectable_hunk`'s status refusal);
    /// this constructs `PatchText` directly to pin `to_bytes`'s header-rendering contract on its
    /// own, matching the canonical `git diff --no-index /dev/null file` shape from the handoff.
    fn creation_patch() -> PatchText {
        PatchText {
            old_path: None,
            new_path: Some("new.txt".to_string()),
            old_mode: 0,
            new_mode: 0o100644,
            hunks: vec![PatchHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 2,
                header: b"@@ -0,0 +1,2 @@\n".to_vec(),
                lines: vec![
                    PatchLine {
                        kind: LineKind::Addition,
                        content: b"hello\n".to_vec(),
                        missing_newline: false,
                    },
                    PatchLine {
                        kind: LineKind::Addition,
                        content: b"world\n".to_vec(),
                        missing_newline: false,
                    },
                ],
            }],
        }
    }

    #[test]
    fn creation_patch_renders_new_file_mode_and_bare_index_line() {
        let patch = creation_patch();

        let expected = [
            "diff --git a/new.txt b/new.txt\n",
            "new file mode 100644\n",
            "index 0000000..0000000\n",
            "--- /dev/null\n",
            "+++ b/new.txt\n",
            "@@ -0,0 +1,2 @@\n",
            "+hello\n",
            "+world\n",
        ]
        .concat()
        .into_bytes();

        assert_eq!(patch.to_bytes(), expected);
    }

    #[test]
    fn deletion_patch_renders_deleted_file_mode_and_bare_index_line() {
        let patch = PatchText {
            old_path: Some("gone.txt".to_string()),
            new_path: None,
            old_mode: 0o100644,
            new_mode: 0,
            hunks: vec![PatchHunk {
                old_start: 1,
                old_count: 2,
                new_start: 0,
                new_count: 0,
                header: b"@@ -1,2 +0,0 @@\n".to_vec(),
                lines: vec![
                    PatchLine {
                        kind: LineKind::Deletion,
                        content: b"hello\n".to_vec(),
                        missing_newline: false,
                    },
                    PatchLine {
                        kind: LineKind::Deletion,
                        content: b"world\n".to_vec(),
                        missing_newline: false,
                    },
                ],
            }],
        };

        let expected = [
            "diff --git a/gone.txt b/gone.txt\n",
            "deleted file mode 100644\n",
            "index 0000000..0000000\n",
            "--- a/gone.txt\n",
            "+++ /dev/null\n",
            "@@ -1,2 +0,0 @@\n",
            "-hello\n",
            "-world\n",
        ]
        .concat()
        .into_bytes();

        assert_eq!(patch.to_bytes(), expected);
    }

    /// Fork 1's `invert` requirement: a reversed creation patch must render as a DELETION
    /// (`deleted file mode`), not silently keep the `new file mode` line — `invert` already
    /// swaps `old_path`/`new_path`/`old_mode`/`new_mode`, so this is a round-trip contract test
    /// on `to_bytes`'s header rendering, not new inversion logic.
    #[test]
    fn invert_of_creation_renders_as_deletion_header() {
        let patch = creation_patch();
        let inverted = patch.invert();

        assert_eq!(inverted.old_path.as_deref(), Some("new.txt"));
        assert!(inverted.new_path.is_none());

        let rendered = String::from_utf8(inverted.to_bytes()).unwrap();
        assert!(
            rendered.contains("deleted file mode 100644\n"),
            "expected a deleted file mode line, got: {rendered}"
        );
        assert!(
            rendered.contains("index 0000000..0000000\n"),
            "expected the bare (mode-suffix-free) index line, got: {rendered}"
        );
        assert!(!rendered.contains("new file mode"));

        // invert(invert(creation)) == creation (same contract as the existing modification
        // round-trip test above).
        assert_eq!(inverted.invert(), patch);
    }

    #[test]
    fn refuses_binary_file() {
        let file = FileChange {
            path: "bin.dat".to_string(),
            old_path: None,
            status: FileStatus::Modified,
            is_binary: true,
            old_mode: 0o100644,
            new_mode: 0o100644,
            hunks: vec![],
        };
        assert!(matches!(
            whole_hunk_patch(&file, 0),
            Err(SynthesisError::BinaryFile { .. })
        ));
    }

    #[test]
    fn refuses_hunk_index_out_of_range() {
        let file = modified_file(simple_hunk());
        assert!(matches!(
            whole_hunk_patch(&file, 1),
            Err(SynthesisError::HunkOutOfRange { .. })
        ));
    }

    #[test]
    fn refuses_statuses_a_hunk_patch_cannot_express() {
        // Deleted/Unmerged stay refused (fork 4): neither a two-sided nor a one-sided hunk
        // patch can express them. Added/Untracked are no longer in this list — `selectable_hunk`
        // now admits them (see its doc comment) for `partial_hunk_patch`'s one-sided path; see
        // the synthesis unit tests around `creation_patch` for their positive coverage.
        for status in [FileStatus::Deleted, FileStatus::Unmerged] {
            let file = FileChange {
                path: "f.txt".to_string(),
                old_path: None,
                status,
                is_binary: false,
                old_mode: 0o100644,
                new_mode: 0o100644,
                hunks: vec![simple_hunk()],
            };
            assert!(
                matches!(
                    whole_hunk_patch(&file, 0),
                    Err(SynthesisError::LineSelectionUnsupported { .. })
                ),
                "expected refusal for status {status:?}"
            );
        }
    }

    #[test]
    fn renamed_file_uses_old_path_in_header() {
        let mut file = modified_file(simple_hunk());
        file.status = FileStatus::Renamed;
        file.old_path = Some("old.txt".to_string());
        let patch = whole_hunk_patch(&file, 0).unwrap();

        assert_eq!(patch.old_path.as_deref(), Some("old.txt"));
        assert_eq!(patch.new_path.as_deref(), Some("f.txt"));
    }

    /// Two separate changes ("old2"->"new2" and "old4"->"new4") in one hunk, with a context
    /// line between them — the shape `partial_hunk_patch`'s direction rules are tested against:
    /// keeping only the first change should drop the second one per `base`'s rule, not just
    /// omit it uniformly.
    ///
    /// Line indices (into `hunk.lines`): 0 ctx "line1", 1 del "old2", 2 add "new2", 3 ctx
    /// "line3", 4 del "old4", 5 add "new4", 6 ctx "line5".
    fn two_change_hunk() -> Hunk {
        let line = |kind, content: &str, old_lnum, new_lnum| HunkLine {
            kind,
            content: content.as_bytes().to_vec(),
            old_lnum,
            new_lnum,
            missing_newline: false,
        };
        Hunk {
            old_start: 1,
            old_count: 5,
            new_start: 1,
            new_count: 5,
            header: b"@@ -1,5 +1,5 @@\n".to_vec(),
            lines: vec![
                line(LineKind::Context, "line1\n", Some(1), Some(1)),
                line(LineKind::Deletion, "old2\n", Some(2), None),
                line(LineKind::Addition, "new2\n", None, Some(2)),
                line(LineKind::Context, "line3\n", Some(3), Some(3)),
                line(LineKind::Deletion, "old4\n", Some(4), None),
                line(LineKind::Addition, "new4\n", None, Some(4)),
                line(LineKind::Context, "line5\n", Some(5), Some(5)),
            ],
        }
    }

    fn keep_first_change() -> LineSelection {
        LineSelection {
            keep_adds: BTreeSet::from([2]),
            keep_dels: BTreeSet::from([1]),
        }
    }

    #[test]
    fn partial_base_old_omits_dropped_add_and_contexts_dropped_del() {
        let file = modified_file(two_change_hunk());
        let patch = partial_hunk_patch(&file, 0, &keep_first_change(), PatchBase::Old).unwrap();

        let expected = [
            "diff --git a/f.txt b/f.txt\n",
            "index 0000000..0000000 100644\n",
            "--- a/f.txt\n",
            "+++ b/f.txt\n",
            "@@ -1,5 +1,5 @@\n",
            " line1\n",
            "-old2\n",
            "+new2\n",
            " line3\n",
            " old4\n",
            " line5\n",
        ]
        .concat()
        .into_bytes();

        assert_eq!(patch.to_bytes(), expected);
    }

    #[test]
    fn partial_base_new_omits_dropped_del_and_contexts_dropped_add() {
        let file = modified_file(two_change_hunk());
        let patch = partial_hunk_patch(&file, 0, &keep_first_change(), PatchBase::New).unwrap();

        let expected = [
            "diff --git a/f.txt b/f.txt\n",
            "index 0000000..0000000 100644\n",
            "--- a/f.txt\n",
            "+++ b/f.txt\n",
            "@@ -1,5 +1,5 @@\n",
            " line1\n",
            "-old2\n",
            "+new2\n",
            " line3\n",
            " new4\n",
            " line5\n",
        ]
        .concat()
        .into_bytes();

        assert_eq!(patch.to_bytes(), expected);
    }

    #[test]
    fn partial_recomputes_counts_when_kept_and_dropped_lines_differ() {
        // Keep only the addition of the first change, dropping its deletion too (base=Old
        // contexts the dropped deletion) — old_count grows relative to a hunk that dropped
        // nothing, new_count reflects only the one kept addition among the two.
        let file = modified_file(two_change_hunk());
        let sel = LineSelection {
            keep_adds: BTreeSet::from([2]),
            keep_dels: BTreeSet::new(),
        };
        let patch = partial_hunk_patch(&file, 0, &sel, PatchBase::Old).unwrap();

        // line1(ctx) old2(ctx, dropped del) new2(add, kept) line3(ctx) old4(ctx, dropped del)
        // line5(ctx): old side never sees "new2" (5 lines), new side does (6 lines); new4
        // (dropped, unkept addition) is omitted from both.
        assert_eq!(patch.hunks[0].old_count, 5);
        assert_eq!(patch.hunks[0].new_count, 6);
        assert_eq!(&patch.hunks[0].header[..], b"@@ -1,5 +1,6 @@\n".as_slice());
    }

    #[test]
    fn partial_ignores_selection_indices_that_are_not_add_or_del() {
        let file = modified_file(two_change_hunk());
        let mut sel = keep_first_change();
        // Index 0 is a context line; index 99 is out of range. Neither should change the
        // rendered patch.
        sel.keep_adds.insert(99);
        sel.keep_dels.insert(0);

        let baseline = partial_hunk_patch(&file, 0, &keep_first_change(), PatchBase::Old).unwrap();
        let with_junk = partial_hunk_patch(&file, 0, &sel, PatchBase::Old).unwrap();

        assert_eq!(with_junk.to_bytes(), baseline.to_bytes());
    }

    #[test]
    fn partial_empty_selection_errors() {
        let file = modified_file(two_change_hunk());
        let sel = LineSelection::default();

        assert!(matches!(
            partial_hunk_patch(&file, 0, &sel, PatchBase::Old),
            Err(SynthesisError::EmptySelection { .. })
        ));
    }

    #[test]
    fn partial_selection_naming_only_context_lines_is_effectively_empty() {
        let file = modified_file(two_change_hunk());
        let sel = LineSelection {
            keep_adds: BTreeSet::from([0, 3, 6]), // all context indices, none are additions
            keep_dels: BTreeSet::new(),
        };

        assert!(matches!(
            partial_hunk_patch(&file, 0, &sel, PatchBase::Old),
            Err(SynthesisError::EmptySelection { .. })
        ));
    }

    #[test]
    fn partial_refuses_binary_file() {
        let file = FileChange {
            path: "bin.dat".to_string(),
            old_path: None,
            status: FileStatus::Modified,
            is_binary: true,
            old_mode: 0o100644,
            new_mode: 0o100644,
            hunks: vec![],
        };
        assert!(matches!(
            partial_hunk_patch(&file, 0, &keep_first_change(), PatchBase::Old),
            Err(SynthesisError::BinaryFile { .. })
        ));
    }

    #[test]
    fn partial_refuses_hunk_index_out_of_range() {
        let file = modified_file(two_change_hunk());
        assert!(matches!(
            partial_hunk_patch(&file, 1, &keep_first_change(), PatchBase::Old),
            Err(SynthesisError::HunkOutOfRange { .. })
        ));
    }

    #[test]
    fn partial_refuses_statuses_a_hunk_patch_cannot_express() {
        // Deleted/Unmerged stay refused (fork 4); Added/Untracked are covered separately below
        // (they now synthesize a one-sided patch instead of refusing).
        for status in [FileStatus::Deleted, FileStatus::Unmerged] {
            let file = FileChange {
                path: "f.txt".to_string(),
                old_path: None,
                status,
                is_binary: false,
                old_mode: 0o100644,
                new_mode: 0o100644,
                hunks: vec![two_change_hunk()],
            };
            assert!(
                matches!(
                    partial_hunk_patch(&file, 0, &keep_first_change(), PatchBase::Old),
                    Err(SynthesisError::LineSelectionUnsupported { .. })
                ),
                "expected refusal for status {status:?}"
            );
        }
    }

    /// A pure-addition hunk shaped like an `Untracked` file's — `old_start`/`old_count` are `0`,
    /// every line is an `Addition`, `old_path` is `None` — the fixture for the one-sided
    /// `partial_hunk_patch` unit tests below.
    fn untracked_hunk() -> Hunk {
        let line = |content: &str, new_lnum| HunkLine {
            kind: LineKind::Addition,
            content: content.as_bytes().to_vec(),
            old_lnum: None,
            new_lnum: Some(new_lnum),
            missing_newline: false,
        };
        Hunk {
            old_start: 0,
            old_count: 0,
            new_start: 1,
            new_count: 3,
            header: b"@@ -0,0 +1,3 @@\n".to_vec(),
            lines: vec![line("one\n", 1), line("two\n", 2), line("three\n", 3)],
        }
    }

    fn untracked_file(hunk: Hunk) -> FileChange {
        FileChange {
            path: "new.txt".to_string(),
            old_path: None,
            status: FileStatus::Untracked,
            is_binary: false,
            old_mode: 0,
            new_mode: 0o100644,
            hunks: vec![hunk],
        }
    }

    /// `base == Old` (staging a subset of an untracked file's lines): dropped additions are
    /// always OMITTED, never converted to context — the rendered patch is always a pure
    /// creation, `old_path: None`, regardless of which lines are kept.
    #[test]
    fn partial_base_old_on_untracked_renders_a_pure_creation() {
        let file = untracked_file(untracked_hunk());
        let sel = LineSelection {
            keep_adds: BTreeSet::from([0, 2]), // "one" and "three"; "two" dropped
            keep_dels: BTreeSet::new(),
        };
        let patch = partial_hunk_patch(&file, 0, &sel, PatchBase::Old).unwrap();

        assert_eq!(patch.old_path, None);
        assert_eq!(patch.new_path.as_deref(), Some("new.txt"));
        assert_eq!(patch.hunks[0].old_start, 0);
        assert_eq!(patch.hunks[0].old_count, 0);

        let expected = [
            "diff --git a/new.txt b/new.txt\n",
            "new file mode 100644\n",
            "index 0000000..0000000\n",
            "--- /dev/null\n",
            "+++ b/new.txt\n",
            "@@ -0,0 +1,2 @@\n",
            "+one\n",
            "+three\n",
        ]
        .concat()
        .into_bytes();
        assert_eq!(patch.to_bytes(), expected);
    }

    /// `base == New` (unstaging/discarding a subset of lines) with a PARTIAL selection: the
    /// dropped additions survive as Context, so the patch gains a real old side —
    /// `old_path: Some(path)`, `old_start: 1` — and `invert()` (the actual apply direction for
    /// Unstage/Discard) renders a two-sided MODIFICATION, not a deletion.
    #[test]
    fn partial_base_new_on_untracked_with_partial_selection_keeps_old_path() {
        let file = untracked_file(untracked_hunk());
        let sel = LineSelection {
            keep_adds: BTreeSet::from([0]), // only "one" selected for reverse-apply
            keep_dels: BTreeSet::new(),
        };
        let patch = partial_hunk_patch(&file, 0, &sel, PatchBase::New).unwrap();

        assert_eq!(patch.old_path.as_deref(), Some("new.txt"));
        assert_eq!(patch.new_path.as_deref(), Some("new.txt"));
        assert_eq!(patch.hunks[0].old_start, 1);
        assert_eq!(patch.hunks[0].old_count, 2); // "two" and "three" survive as context

        let inverted = patch.invert();
        assert_eq!(inverted.old_path.as_deref(), Some("new.txt"));
        assert_eq!(inverted.new_path.as_deref(), Some("new.txt"));
        let rendered = String::from_utf8(inverted.to_bytes()).unwrap();
        assert!(
            !rendered.contains("deleted file mode") && !rendered.contains("new file mode"),
            "expected a two-sided modification header, got: {rendered}"
        );
    }

    /// `base == New` with a FULL selection (every addition kept): no Context survives, so
    /// `old_path` stays `None` and `invert()` renders a deletion — the shape a full-file discard
    /// would produce if it went through this path (which `app.rs`'s routing avoids per fork 2,
    /// but the synthesis-level contract still holds on its own).
    #[test]
    fn partial_base_new_on_untracked_with_full_selection_renders_as_deletion_when_inverted() {
        let file = untracked_file(untracked_hunk());
        let sel = LineSelection {
            keep_adds: BTreeSet::from([0, 1, 2]),
            keep_dels: BTreeSet::new(),
        };
        let patch = partial_hunk_patch(&file, 0, &sel, PatchBase::New).unwrap();

        assert_eq!(patch.old_path, None);
        assert_eq!(patch.hunks[0].old_start, 0);
        assert_eq!(patch.hunks[0].old_count, 0);

        let inverted = patch.invert();
        assert!(inverted.new_path.is_none());
        let rendered = String::from_utf8(inverted.to_bytes()).unwrap();
        assert!(
            rendered.contains("deleted file mode 100644\n"),
            "expected a deletion header, got: {rendered}"
        );
    }
}
