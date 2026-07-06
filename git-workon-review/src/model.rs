//! The diff model: [`DiffModel`]/[`FileChange`]/[`Hunk`]/[`HunkLine`] built directly from
//! git2 [`git2::Diff`]/[`git2::Patch`] structures.
//!
//! Per the M2 design decision, this is NOT a unified-diff-text parser: it walks git2's own
//! line callbacks (content bytes + origin chars, including the EOFNL origins `=`/`>`/`<`) so
//! the model can byte-exactly re-render the patches it read ([`Hunk::to_diff_bytes`]).
//!
//! ## EOFNL characterization (see `tests/diff_model.rs`)
//!
//! git2 never emits a separate pseudo-line for a missing trailing newline. Instead, when a
//! real line (context/addition/deletion) is the last line of a file lacking a trailing
//! newline, git2 emits that line's content WITHOUT the newline, immediately followed by a
//! marker line whose origin is one of:
//!
//! - `ContextEOFNL` (`=`) — the preceding CONTEXT line has no trailing newline.
//! - `AddEOFNL` (`>`)     — the preceding DELETION line's old-side content has no trailing
//!   newline (despite the name, this marks the OLD/`-` side, not the `+` side — verified
//!   empirically, do not trust the enum name).
//! - `DeleteEOFNL` (`<`)  — the preceding ADDITION line's new-side content has no trailing
//!   newline (again, the name is the mirror of what you'd expect).
//!
//! The marker's own content is `"\n\\ No newline at end of file\n"` — the leading `\n`
//! supplies the newline the preceding line omitted. [`DiffModel::from_git2`] does not push a
//! separate line for these markers; it sets [`HunkLine::missing_newline`] on the
//! most-recently-pushed line instead, matching the sketch in the plan.

use crate::error::DiffError;

/// What kind of line a [`HunkLine`] is, independent of which side of the patch it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

/// One line of hunk content, carrying EXACT bytes (no trailing `\n` normalization).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkLine {
    pub kind: LineKind,
    /// Exact bytes as git2 reported them, including the trailing `\n` when present. When
    /// [`missing_newline`](Self::missing_newline) is set, these bytes do NOT end in `\n` —
    /// the file's last line genuinely has none.
    pub content: Vec<u8>,
    pub old_lnum: Option<u32>,
    pub new_lnum: Option<u32>,
    /// Set from the EOFNL origin markers (`=`/`>`/`<`) that git2 emits immediately after this
    /// line when it is the last line of a file with no trailing newline. No pseudo-line is
    /// ever pushed for the marker itself — see the module docs.
    pub missing_newline: bool,
}

/// One `@@ ... @@` hunk, re-renderable byte-for-byte via [`Hunk::to_diff_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// Verbatim `@@ -old_start,old_count +new_start,new_count @@ ...` bytes from git2,
    /// including trailing `\n` — keeps any function-context suffix git2 attaches.
    pub header: Vec<u8>,
    pub lines: Vec<HunkLine>,
}

impl Hunk {
    /// Byte-exact re-render of this hunk: header followed by each line's origin-prefixed
    /// content, splicing in the git-canonical `\ No newline at end of file` marker (in the
    /// exact byte sequence git2 uses: a bare `\n` continuing the truncated line, then the
    /// marker text) wherever [`HunkLine::missing_newline`] is set.
    ///
    /// Fidelity is pinned against `git2::Diff::print(DiffFormat::Patch)` output in
    /// `tests/diff_model.rs`.
    pub fn to_diff_bytes(&self) -> Vec<u8> {
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
}

/// What kind of change a [`FileChange`] represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Unmerged,
}

impl From<git2::Delta> for FileStatus {
    fn from(delta: git2::Delta) -> Self {
        match delta {
            git2::Delta::Added => FileStatus::Added,
            git2::Delta::Deleted => FileStatus::Deleted,
            git2::Delta::Renamed => FileStatus::Renamed,
            git2::Delta::Copied => FileStatus::Copied,
            git2::Delta::Untracked => FileStatus::Untracked,
            git2::Delta::Conflicted => FileStatus::Unmerged,
            // Modified, Unmodified, Ignored, Typechange, Unreadable: none of these are
            // distinct routing targets in the M2 model; fall back to Modified, the ordinary
            // hunk-diffable case.
            _ => FileStatus::Modified,
        }
    }
}

/// One changed file, with its hunks (empty for binary files — see [`FileChange::is_binary`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    /// The pre-change path for [`FileStatus::Renamed`]/[`FileStatus::Copied`]; `None`
    /// otherwise.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub is_binary: bool,
    /// Raw octal file mode (e.g. `0o100644`, `0o100755`) of the pre-image, from
    /// `delta.old_file().mode()`. Carried alongside [`Self::new_mode`] so
    /// [`crate::synthesis::whole_hunk_patch`] can pick the right mode for the patch's
    /// direction — and [`crate::synthesis::PatchText::invert`] can swap them — instead of
    /// clobbering the index entry's mode with a hardcoded `100644` (a real divergence: staging
    /// any hunk of an executable file via the git2 applier used to silently reset it).
    pub old_mode: i32,
    /// Raw octal file mode of the post-image, from `delta.new_file().mode()`. See
    /// [`Self::old_mode`].
    pub new_mode: i32,
    pub hunks: Vec<Hunk>,
}

/// A diff, built from git2 structures — see the module docs for the EOFNL characterization
/// and [`Hunk::to_diff_bytes`] for the byte-fidelity contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffModel {
    pub files: Vec<FileChange>,
}

impl DiffModel {
    /// Build a [`DiffModel`] from a git2 [`git2::Diff`], iterating deltas and hunk-diffing
    /// each non-binary one via [`git2::Patch::from_diff`].
    ///
    /// Untracked deltas carry zero OIDs in git2 — this never reads blob ids off a delta;
    /// content always arrives via the patch line callbacks.
    pub fn from_git2(diff: &git2::Diff<'_>) -> Result<DiffModel, DiffError> {
        let mut files = Vec::with_capacity(diff.deltas().len());
        for i in 0..diff.deltas().len() {
            let delta = diff
                .get_delta(i)
                .expect("index within diff.deltas().len() is always valid");
            let status = FileStatus::from(delta.status());

            let old_path = delta.old_file().path().map(path_to_string);
            let new_path = delta.new_file().path().map(path_to_string);
            let path = match status {
                FileStatus::Deleted => old_path.clone(),
                _ => new_path.or_else(|| old_path.clone()),
            }
            .unwrap_or_default();
            let old_path = match status {
                FileStatus::Renamed | FileStatus::Copied => old_path,
                _ => None,
            };

            // The BINARY flag on the delta fetched via `diff.get_delta` is not yet
            // populated — libgit2 only runs the binary content check while computing the
            // patch. Build the patch unconditionally and re-check its delta's flags.
            let mut is_binary = delta.flags().contains(git2::DiffFlags::BINARY);
            let mut hunks = Vec::new();
            if let Some(patch) = git2::Patch::from_diff(diff, i)? {
                is_binary = is_binary || patch.delta().flags().contains(git2::DiffFlags::BINARY);
                if !is_binary {
                    hunks = hunks_from_patch(&patch)?;
                }
            }

            files.push(FileChange {
                path,
                old_path,
                status,
                is_binary,
                old_mode: i32::from(delta.old_file().mode()),
                new_mode: i32::from(delta.new_file().mode()),
                hunks,
            });
        }
        Ok(DiffModel { files })
    }
}

fn path_to_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

fn hunks_from_patch(patch: &git2::Patch<'_>) -> Result<Vec<Hunk>, DiffError> {
    let mut hunks = Vec::with_capacity(patch.num_hunks());
    for h in 0..patch.num_hunks() {
        let (raw_hunk, line_count) = patch.hunk(h)?;
        let mut lines: Vec<HunkLine> = Vec::with_capacity(line_count);
        for l in 0..line_count {
            let line = patch.line_in_hunk(h, l)?;
            let kind = match line.origin_value() {
                git2::DiffLineType::Context => LineKind::Context,
                git2::DiffLineType::Addition => LineKind::Addition,
                git2::DiffLineType::Deletion => LineKind::Deletion,
                git2::DiffLineType::ContextEOFNL
                | git2::DiffLineType::AddEOFNL
                | git2::DiffLineType::DeleteEOFNL => {
                    // No pseudo-line: mark the most recently pushed real line instead (see
                    // module docs for the EOFNL characterization).
                    if let Some(last) = lines.last_mut() {
                        last.missing_newline = true;
                    }
                    continue;
                }
                // FileHeader/HunkHeader/Binary never appear via `line_in_hunk`.
                _ => continue,
            };
            lines.push(HunkLine {
                kind,
                content: line.content().to_vec(),
                old_lnum: line.old_lineno(),
                new_lnum: line.new_lineno(),
                missing_newline: false,
            });
        }
        hunks.push(Hunk {
            old_start: raw_hunk.old_start(),
            old_count: raw_hunk.old_lines(),
            new_start: raw_hunk.new_start(),
            new_count: raw_hunk.new_lines(),
            header: raw_hunk.header().to_vec(),
            lines,
        });
    }
    Ok(hunks)
}
