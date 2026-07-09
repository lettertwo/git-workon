//! Classifying and resolving a `git workon review [<source>]` positional argument (ADR-036).
//!
//! [`Source::classify`] is pure — no repository access, deterministic regardless of repo
//! state — so a branch literally named `stack` only matches the keyword when spelled bare;
//! `refs/heads/stack` or `heads/stack` classify as [`Source::Ref`]. Resolution
//! ([`resolve_source`]) is where repo state comes in.
//!
//! CS3 wires real `<ref>` dispatch (shape-aware: Graphite-tracked branch, other branch,
//! bare commit-ish) and `Range` (`a..b` / `a...b`, git-diff semantics). `Pr` still lands with
//! its own changeset — no dead arm here yet (CS4).

use git2::{BranchType, Oid, Repository};
use workon::{
    assemble_changesets, get_default_branch, graphite_trunk, ChangesetError, ChangesetSpan,
    StackModel, UncommittedLayer, WorkonError,
};

use crate::acquire::uncommitted_changeset;
use crate::error::SourceError;

/// Which dot form a [`Source::Range`] was spelled with — git-diff semantics differ (ADR-036).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeDots {
    /// `a..b` — base `a`, head `b` (endpoint trees, exactly a committed span).
    Two,
    /// `a...b` — base `merge-base(a, b)`, head `b` (the PR-style "what did b add").
    Three,
}

/// What a `git workon review <source>` argument was classified as (ADR-036 precedence).
///
/// `classify` only ever runs on `Some(text)` — the no-argument case stays the existing
/// auto-detect path in `main.rs` and never constructs a `Source` at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The exact bare word `stack`.
    Stack,
    /// The exact bare word `uncommitted`.
    Uncommitted,
    /// `<base>..<head>` or `<base>...<head>` — either side may be empty, defaulting to `HEAD`
    /// at resolution time (classification stays pure/repo-state-free).
    Range {
        base_text: String,
        head_text: String,
        dots: RangeDots,
    },
    /// Everything else — a candidate ref, resolved by shape (CS3).
    Ref(String),
}

impl Source {
    /// Classify `text` per ADR-036's precedence: exact bare keyword, else a range (three-dot
    /// checked before two-dot, since `...` contains `..`), else `Ref`. Keywords are checked
    /// first — they're exact-bare and contain no dots, so the order between "keyword" and
    /// "range" never actually competes, but reading top-to-bottom matches the ADR's precedence
    /// list.
    pub fn classify(text: &str) -> Source {
        match text {
            "stack" => return Source::Stack,
            "uncommitted" => return Source::Uncommitted,
            _ => {}
        }
        if let Some((base_text, head_text)) = text.split_once("...") {
            return Source::Range {
                base_text: base_text.to_string(),
                head_text: head_text.to_string(),
                dots: RangeDots::Three,
            };
        }
        if let Some((base_text, head_text)) = text.split_once("..") {
            return Source::Range {
                base_text: base_text.to_string(),
                head_text: head_text.to_string(),
                dots: RangeDots::Two,
            };
        }
        Source::Ref(text.to_string())
    }
}

/// Resolve a classified [`Source`] to the changesets it names, for the worktree whose `HEAD`
/// is `head_branch`.
///
/// Unlike [`crate::acquire::resolve_changesets`] (the no-argument auto-detect path), every
/// arm here is an explicit ask: `Stack` never silently falls back to the uncommitted layer on
/// a missing upstream, and an unresolvable `Ref` is a named pre-TUI error, never a fallback to
/// auto-detect (ADR-036's "no surprise reviews" rule).
pub fn resolve_source(
    repo: &Repository,
    head_branch: &str,
    source: Source,
) -> Result<Vec<workon::Changeset>, SourceError> {
    match source {
        Source::Stack => resolve_stack(repo, head_branch),
        Source::Uncommitted => Ok(vec![uncommitted_changeset(head_branch)]),
        Source::Range {
            base_text,
            head_text,
            dots,
        } => resolve_range(repo, &base_text, &head_text, dots),
        Source::Ref(text) => resolve_ref(repo, head_branch, text),
    }
}

/// `stack` keyword resolution: stack metadata (Graphite or gh-stack) when active, otherwise the
/// git-inference arm (`StackModel::Git`, first wired into the binary here) — never a silent
/// downgrade to `StackModel::None`'s empty result, since the keyword is an explicit ask for the
/// real stack.
/// The uncommitted layer rides along (`UncommittedLayer::Include`): `stack` always means
/// "focused on real `HEAD`."
fn resolve_stack(
    repo: &Repository,
    head_branch: &str,
) -> Result<Vec<workon::Changeset>, SourceError> {
    let model = match StackModel::detect(repo) {
        StackModel::None | StackModel::Git => StackModel::Git,
        metadata @ (StackModel::Graphite | StackModel::GhStack) => metadata,
    };

    assemble_changesets(repo, head_branch, model, UncommittedLayer::Include).map_err(
        |err| match err {
            WorkonError::Changeset(ChangesetError::NoUpstream { branch }) => {
                SourceError::NoUpstream { branch }
            }
            other => SourceError::StackResolutionFailed {
                branch: head_branch.to_string(),
                source: other,
            },
        },
    )
}

/// `<ref>` resolution — shape-aware dispatch (ADR-036), checked in order:
///
/// 1. A Graphite-tracked LOCAL branch (`text` names a local branch, qualified spellings like
///    `refs/heads/<name>`/`heads/<name>` included, AND that branch has a Graphite metadata row)
///    → the whole stack focused there, exactly like `stack` but pinned to `text`'s branch
///    instead of real `HEAD`. The uncommitted layer rides along only when the resolved branch
///    IS `head_branch` — this is the first caller to pass [`UncommittedLayer::Omit`].
/// 2. Any other branch (an untracked local branch, or a remote-tracking branch like
///    `origin/foo`) → one committed changeset, "what this branch adds": base =
///    `merge-base(upstream, branch)` when a local branch has an upstream, else
///    `merge-base(trunk, branch)`.
/// 3. A bare commit-ish (sha, tag, `HEAD~2`) that rev-parses to a commit but isn't a branch →
///    one changeset spanning just that commit (`parent..ref`, or [`ChangesetSpan::CommittedRoot`]
///    for a parentless root commit).
/// 4. Nothing rev-parses → [`SourceError::UnresolvableSource`].
fn resolve_ref(
    repo: &Repository,
    head_branch: &str,
    text: String,
) -> Result<Vec<workon::Changeset>, SourceError> {
    if let Some(branch_name) = resolve_local_branch_name(repo, &text) {
        if workon::current_stack(repo, &branch_name, StackModel::Graphite)
            .ok()
            .flatten()
            .is_some()
        {
            let layer = if branch_name == head_branch {
                UncommittedLayer::Include
            } else {
                UncommittedLayer::Omit
            };
            return assemble_changesets(repo, &branch_name, StackModel::Graphite, layer).map_err(
                |err| match err {
                    WorkonError::Changeset(ChangesetError::NoUpstream { branch }) => {
                        SourceError::NoUpstream { branch }
                    }
                    other => SourceError::StackResolutionFailed {
                        branch: branch_name.clone(),
                        source: other,
                    },
                },
            );
        }

        // Untracked local branch: "what this branch adds" vs its upstream, else the trunk.
        let branch = repo
            .find_branch(&branch_name, BranchType::Local)
            .map_err(|_| SourceError::UnresolvableSource { text: text.clone() })?;
        let head_oid = branch
            .get()
            .target()
            .ok_or_else(|| SourceError::UnresolvableSource { text: text.clone() })?;
        let upstream_oid = branch.upstream().ok().and_then(|u| u.get().target());
        return one_changeset_from_branch(repo, &text, head_oid, upstream_oid);
    }

    // Remote-tracking branch (e.g. "origin/foo"): no upstream concept of its own, base always
    // comes from the trunk.
    if let Ok(branch) = repo.find_branch(&text, BranchType::Remote) {
        let head_oid = branch
            .get()
            .target()
            .ok_or_else(|| SourceError::UnresolvableSource { text: text.clone() })?;
        return one_changeset_from_branch(repo, &text, head_oid, None);
    }

    // Bare commit-ish: sha, tag, `HEAD~2`, etc. — rev-parses to a commit but isn't a branch.
    if let Some(head_oid) = revparse_to_commit(repo, &text) {
        let commit = repo
            .find_commit(head_oid)
            .map_err(|_| SourceError::UnresolvableSource { text: text.clone() })?;
        let span = match commit.parent_id(0) {
            Ok(base) => ChangesetSpan::Committed {
                base,
                head: head_oid,
            },
            // A root commit has no parent to diff against — the empty tree stands in.
            Err(_) => ChangesetSpan::CommittedRoot { head: head_oid },
        };
        return Ok(vec![workon::Changeset {
            name: text,
            span,
            title: None,
            current: true,
            needs_restack: false,
        }]);
    }

    Err(SourceError::UnresolvableSource { text })
}

/// `Range` resolution: rev-parse each endpoint (an empty side defaults to `HEAD`), then combine
/// per [`RangeDots`] — `a..b` spans the endpoints directly; `a...b` bases off their merge-base.
/// One committed changeset either way, named after the source text exactly as typed. Never a
/// candidate for the uncommitted layer (ADR-036: committed-only, like every source but `stack`
/// and a `<ref>` on real `HEAD`).
fn resolve_range(
    repo: &Repository,
    base_text: &str,
    head_text: &str,
    dots: RangeDots,
) -> Result<Vec<workon::Changeset>, SourceError> {
    let base_oid = resolve_endpoint(repo, base_text)?;
    let head_oid = resolve_endpoint(repo, head_text)?;

    let base_oid = match dots {
        RangeDots::Two => base_oid,
        RangeDots::Three => {
            repo.merge_base(base_oid, head_oid)
                .map_err(|_| SourceError::UnresolvableSource {
                    text: format!("{base_text}...{head_text}"),
                })?
        }
    };

    let name = match dots {
        RangeDots::Two => format!("{base_text}..{head_text}"),
        RangeDots::Three => format!("{base_text}...{head_text}"),
    };

    Ok(vec![workon::Changeset {
        name,
        span: ChangesetSpan::Committed {
            base: base_oid,
            head: head_oid,
        },
        title: None,
        current: true,
        needs_restack: false,
    }])
}

/// Rev-parse one range endpoint, peeled to a commit; an empty `text` defaults to `HEAD` (ADR-036).
fn resolve_endpoint(repo: &Repository, text: &str) -> Result<Oid, SourceError> {
    if text.is_empty() {
        return repo
            .head()
            .and_then(|h| h.peel_to_commit())
            .map(|c| c.id())
            .map_err(|_| SourceError::UnresolvableSource {
                text: "HEAD".to_string(),
            });
    }
    revparse_to_commit(repo, text).ok_or_else(|| SourceError::UnresolvableSource {
        text: text.to_string(),
    })
}

/// Rev-parse `text` and peel it to a commit, or `None` if it doesn't resolve to one.
fn revparse_to_commit(repo: &Repository, text: &str) -> Option<Oid> {
    repo.revparse_single(text)
        .ok()
        .and_then(|obj| obj.peel_to_commit().ok())
        .map(|c| c.id())
}

/// The resolved local branch name for `text`: an exact bare match, or a qualified spelling
/// (`refs/heads/<name>`, `heads/<name>`) that resolves to one — the same escapes ADR-036 gives
/// for the `stack`/`uncommitted` keywords, so `refs/heads/<head_branch>` still counts as
/// "focused on real `HEAD`" (compares equal to `head_branch` after unwrapping).
fn resolve_local_branch_name(repo: &Repository, text: &str) -> Option<String> {
    if repo.find_branch(text, BranchType::Local).is_ok() {
        return Some(text.to_string());
    }
    for prefix in ["refs/heads/", "heads/"] {
        if let Some(name) = text.strip_prefix(prefix) {
            if repo.find_branch(name, BranchType::Local).is_ok() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// One committed changeset spanning "what `branch` (named `text`) adds": base =
/// `merge-base(upstream_oid, head_oid)` when `upstream_oid` is `Some` (a local branch with an
/// upstream), else `merge-base(trunk, head_oid)` where trunk is the Graphite trunk if known,
/// else the repo's default branch. Neither resolving is [`SourceError::NoBaseForBranch`].
fn one_changeset_from_branch(
    repo: &Repository,
    text: &str,
    head_oid: Oid,
    upstream_oid: Option<Oid>,
) -> Result<Vec<workon::Changeset>, SourceError> {
    let no_base = || SourceError::NoBaseForBranch {
        branch: text.to_string(),
    };

    let base_target = match upstream_oid {
        Some(oid) => oid,
        None => trunk_commit_oid(repo).ok_or_else(no_base)?,
    };
    let base_oid = repo
        .merge_base(base_target, head_oid)
        .map_err(|_| no_base())?;

    Ok(vec![workon::Changeset {
        name: text.to_string(),
        span: ChangesetSpan::Committed {
            base: base_oid,
            head: head_oid,
        },
        title: None,
        current: true,
        needs_restack: false,
    }])
}

/// The trunk branch's tip commit: the Graphite trunk if known, else the repo's default branch
/// (`init.defaultBranch`/`main`/`master`) — `None` if neither resolves to a real commit.
fn trunk_commit_oid(repo: &Repository) -> Option<Oid> {
    let name = graphite_trunk(repo).or_else(|| get_default_branch(repo).ok())?;
    revparse_to_commit(repo, &name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_bare_stack_is_stack_keyword() {
        assert_eq!(Source::classify("stack"), Source::Stack);
    }

    #[test]
    fn classify_bare_uncommitted_is_uncommitted_keyword() {
        assert_eq!(Source::classify("uncommitted"), Source::Uncommitted);
    }

    #[test]
    fn classify_qualified_stack_ref_is_ref_not_keyword() {
        assert_eq!(
            Source::classify("refs/heads/stack"),
            Source::Ref("refs/heads/stack".to_string())
        );
        assert_eq!(
            Source::classify("heads/stack"),
            Source::Ref("heads/stack".to_string())
        );
    }

    #[test]
    fn classify_qualified_uncommitted_ref_is_ref_not_keyword() {
        assert_eq!(
            Source::classify("refs/heads/uncommitted"),
            Source::Ref("refs/heads/uncommitted".to_string())
        );
    }

    #[test]
    fn classify_is_case_sensitive() {
        assert_eq!(Source::classify("Stack"), Source::Ref("Stack".to_string()));
        assert_eq!(
            Source::classify("Uncommitted"),
            Source::Ref("Uncommitted".to_string())
        );
        assert_eq!(Source::classify("STACK"), Source::Ref("STACK".to_string()));
    }

    #[test]
    fn classify_arbitrary_text_is_ref() {
        assert_eq!(Source::classify("main"), Source::Ref("main".to_string()));
        assert_eq!(
            Source::classify("deadbeef"),
            Source::Ref("deadbeef".to_string())
        );
    }

    #[test]
    fn classify_empty_string_is_ref() {
        assert_eq!(Source::classify(""), Source::Ref(String::new()));
    }

    #[test]
    fn classify_two_dot_range() {
        assert_eq!(
            Source::classify("a..b"),
            Source::Range {
                base_text: "a".to_string(),
                head_text: "b".to_string(),
                dots: RangeDots::Two,
            }
        );
    }

    #[test]
    fn classify_three_dot_range() {
        assert_eq!(
            Source::classify("a...b"),
            Source::Range {
                base_text: "a".to_string(),
                head_text: "b".to_string(),
                dots: RangeDots::Three,
            }
        );
    }

    #[test]
    fn classify_range_empty_sides() {
        assert_eq!(
            Source::classify("..main"),
            Source::Range {
                base_text: String::new(),
                head_text: "main".to_string(),
                dots: RangeDots::Two,
            }
        );
        assert_eq!(
            Source::classify("main.."),
            Source::Range {
                base_text: "main".to_string(),
                head_text: String::new(),
                dots: RangeDots::Two,
            }
        );
        assert_eq!(
            Source::classify(".."),
            Source::Range {
                base_text: String::new(),
                head_text: String::new(),
                dots: RangeDots::Two,
            }
        );
    }

    #[test]
    fn classify_dotted_text_mixed_with_keyword_text_is_range() {
        assert_eq!(
            Source::classify("stack..main"),
            Source::Range {
                base_text: "stack".to_string(),
                head_text: "main".to_string(),
                dots: RangeDots::Two,
            }
        );
    }
}
