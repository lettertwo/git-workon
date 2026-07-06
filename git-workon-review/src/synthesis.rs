//! Synthesizing invertible patch text from a [`crate::model::DiffModel`].
//!
//! git2's write side takes bytes ([`git2::Diff::from_buffer`]), and the `git apply` CLI takes
//! text on stdin — but libgit2's `Repository::apply` has NO reverse flag (plan risk #1). A
//! "reverse apply" is therefore always: synthesize the forward patch, then
//! [`PatchText::invert`] it before handing it to an applier. [`PatchText`] stays structured
//! (not opaque bytes) so that inversion is a pure, testable transform instead of a text
//! rewrite.
//!
//! This module only synthesizes WHOLE hunks (`[whole_hunk_patch]`). Line-precise synthesis
//! (traps 1-2: direction-dependent drop rules, the EOFNL splice) lands in CS3
//! (`partial_hunk_patch`).

use crate::error::SynthesisError;
use crate::model::{FileChange, FileStatus, LineKind};

/// Which side of a patch is the "before" image — the direction-dependent drop rules (trap 1)
/// key off this. Whole-hunk patches (this module) don't drop lines, so `PatchBase` is
/// currently only consumed by [`crate::apply::StageVerb::plan`]; line-precise synthesis (CS3)
/// is where it drives which lines get kept vs. converted to context.
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
}
