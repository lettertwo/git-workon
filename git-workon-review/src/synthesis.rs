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
//! (`[partial_hunk_patch]`, traps 1-2: direction-dependent drop rules, the EOFNL splice).

use std::collections::BTreeSet;

use crate::error::SynthesisError;
use crate::model::{FileChange, FileStatus, LineKind};

/// Which side of a patch is the "before" image — the direction-dependent drop rules (trap 1)
/// key off this. Whole-hunk patches don't drop lines, so `PatchBase` only affects
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
    /// Render the full patch: a `diff --git`/`index`/`---`/`+++` file header, then each
    /// hunk's bytes. Always ends in `\n` (each hunk's last line is either a real line with its
    /// own trailing `\n`, or a `missing_newline` line whose marker supplies one).
    ///
    /// The `index 0000000..0000000 <mode>` line's OIDs are a placeholder — this crate never
    /// reads blob OIDs off the model (untracked deltas don't have them either), and `git
    /// apply` ignores them. The line exists because `git2::Diff::from_buffer` parses stricter
    /// than `git apply` and rejects a bare 3-line header (plan risk #4). The MODE, however, is
    /// load-bearing: `Repository::apply(ApplyLocation::Index, ..)` takes the new index entry's
    /// mode straight from this line, so it must be the file's real mode
    /// ([`Self::new_mode`]) — a hardcoded `100644` here used to silently clobber the exec bit
    /// of any staged `100755` file (the `git apply` CLI path never had this bug: it reads the
    /// mode from the working tree instead).
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
        out.extend_from_slice(format!("index 0000000..0000000 {:06o}\n", self.new_mode).as_bytes());
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

    /// Pure transform: swap old/new paths and invert every hunk (trap 1's Old/New base swap,
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

/// Synthesize a patch for the WHOLE of `file`'s hunk at `hunk_idx` — no line selection, so the
/// direction-dependent drop rules (trap 1) don't apply; the hunk's lines are copied verbatim.
///
/// Refuses:
/// - binary files ([`SynthesisError::BinaryFile`]) — no hunks exist to synthesize from.
/// - `hunk_idx` out of range ([`SynthesisError::HunkOutOfRange`]).
/// - statuses a hunk patch can't express ([`SynthesisError::LineSelectionUnsupported`]):
///   `Added`/`Deleted`/`Untracked`/`Unmerged` are whole-file operations by nature — a hunk
///   patch of a deletion would stage an empty blob instead of removing the file, and a hunk
///   patch of an untracked file has no index/HEAD preimage to apply against (trap 3). CS4's
///   `ops.rs` routes these statuses to `file_ops.rs` before synthesis is ever reached, so
///   `LineSelectionUnsupported` is the variant callers see here — it's the closest existing
///   error to "use the whole-file op instead," which is exactly its `help` text.
///   `Copied` is treated like `Renamed` (both carry an `old_path`).
pub fn whole_hunk_patch(file: &FileChange, hunk_idx: usize) -> Result<PatchText, SynthesisError> {
    if file.is_binary {
        return Err(SynthesisError::BinaryFile {
            path: file.path.clone(),
        });
    }
    match file.status {
        FileStatus::Modified | FileStatus::Renamed | FileStatus::Copied => {}
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
/// direction-dependent drop rules (trap 1).
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
/// (reused via [`header_suffix`]) — the starts are unchanged, only the counts move.
///
/// Same refusals as [`whole_hunk_patch`]: binary files ([`SynthesisError::BinaryFile`]),
/// unsupported statuses ([`SynthesisError::LineSelectionUnsupported`]), and an out-of-range
/// `hunk_idx` ([`SynthesisError::HunkOutOfRange`]).
///
/// This function does not yet apply the trap-2 EOFNL splice (a dropped deletion converted to
/// context that carries [`crate::model::HunkLine::missing_newline`], followed by any kept
/// line, silently corrupts the blob under `git apply`) — see the follow-up commit.
pub fn partial_hunk_patch(
    file: &FileChange,
    hunk_idx: usize,
    sel: &LineSelection,
    base: PatchBase,
) -> Result<PatchText, SynthesisError> {
    if file.is_binary {
        return Err(SynthesisError::BinaryFile {
            path: file.path.clone(),
        });
    }
    match file.status {
        FileStatus::Modified | FileStatus::Renamed | FileStatus::Copied => {}
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

    let mut kept_any = false;
    let mut old_count = 0u32;
    let mut new_count = 0u32;
    let mut lines = Vec::with_capacity(hunk.lines.len());

    for (idx, line) in hunk.lines.iter().enumerate() {
        match line.kind {
            LineKind::Context => {
                old_count += 1;
                new_count += 1;
                lines.push(PatchLine {
                    kind: LineKind::Context,
                    content: line.content.clone(),
                    missing_newline: line.missing_newline,
                });
            }
            LineKind::Addition => {
                if sel.keep_adds.contains(&idx) {
                    kept_any = true;
                    new_count += 1;
                    lines.push(PatchLine {
                        kind: LineKind::Addition,
                        content: line.content.clone(),
                        missing_newline: line.missing_newline,
                    });
                } else if base == PatchBase::New {
                    // Dropped addition, base=New: it must remain in the (already-changed)
                    // target, so it has to match as context on reverse-apply.
                    old_count += 1;
                    new_count += 1;
                    lines.push(PatchLine {
                        kind: LineKind::Context,
                        content: line.content.clone(),
                        missing_newline: line.missing_newline,
                    });
                }
                // base=Old: dropped addition is omitted — the target doesn't have it yet and
                // shouldn't gain it.
            }
            LineKind::Deletion => {
                if sel.keep_dels.contains(&idx) {
                    kept_any = true;
                    old_count += 1;
                    lines.push(PatchLine {
                        kind: LineKind::Deletion,
                        content: line.content.clone(),
                        missing_newline: line.missing_newline,
                    });
                } else if base == PatchBase::Old {
                    // Dropped deletion, base=Old: it's still there in the target, so it has to
                    // match as context.
                    old_count += 1;
                    new_count += 1;
                    lines.push(PatchLine {
                        kind: LineKind::Context,
                        content: line.content.clone(),
                        missing_newline: line.missing_newline,
                    });
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

    let mut header = format!(
        "@@ -{},{old_count} +{},{new_count} @@",
        hunk.old_start, hunk.new_start
    )
    .into_bytes();
    header.extend_from_slice(&header_suffix(&hunk.header));

    let old_path = file.old_path.clone().unwrap_or_else(|| file.path.clone());
    let new_path = file.path.clone();

    Ok(PatchText {
        old_path: Some(old_path),
        new_path: Some(new_path),
        hunks: vec![PatchHunk {
            old_start: hunk.old_start,
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
        for status in [
            FileStatus::Added,
            FileStatus::Deleted,
            FileStatus::Untracked,
            FileStatus::Unmerged,
        ] {
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
        for status in [
            FileStatus::Added,
            FileStatus::Deleted,
            FileStatus::Untracked,
            FileStatus::Unmerged,
        ] {
            let file = FileChange {
                path: "f.txt".to_string(),
                old_path: None,
                status,
                is_binary: false,
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
}
