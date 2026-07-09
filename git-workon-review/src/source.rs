//! Classifying and resolving a `git workon review [<source>]` positional argument (ADR-030).
//!
//! [`Source::classify`] is pure — no repository access, deterministic regardless of repo
//! state — so a branch literally named `stack` only matches the keyword when spelled bare;
//! `refs/heads/stack` or `heads/stack` classify as [`Source::Ref`]. Resolution
//! ([`resolve_source`]) is where repo state comes in.
//!
//! CS2 ships only the variants it can resolve: [`Source::Stack`], [`Source::Uncommitted`], and
//! [`Source::Ref`] (whose resolution is a named, honest failure until CS3 wires ref/range
//! dispatch). `Range`/`Pr` variants land with their own changesets — no dead arms here yet.

use git2::Repository;
use workon::{assemble_changesets, ChangesetError, StackModel, UncommittedLayer, WorkonError};

use crate::acquire::uncommitted_changeset;
use crate::error::SourceError;

/// What a `git workon review <source>` argument was classified as (ADR-030 precedence).
///
/// `classify` only ever runs on `Some(text)` — the no-argument case stays the existing
/// auto-detect path in `main.rs` and never constructs a `Source` at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The exact bare word `stack`.
    Stack,
    /// The exact bare word `uncommitted`.
    Uncommitted,
    /// Everything else — a candidate ref, resolved (CS3) or rejected (CS2) by shape.
    Ref(String),
}

impl Source {
    /// Classify `text` per ADR-030's precedence. In CS2 that precedence is just "exact bare
    /// keyword, else `Ref`" — the PR/range arms are added ahead of the keyword check as later
    /// changesets extend this function, not this call site.
    pub fn classify(text: &str) -> Source {
        match text {
            "stack" => Source::Stack,
            "uncommitted" => Source::Uncommitted,
            other => Source::Ref(other.to_string()),
        }
    }
}

/// Resolve a classified [`Source`] to the changesets it names, for the worktree whose `HEAD`
/// is `head_branch`.
///
/// Unlike [`crate::acquire::resolve_changesets`] (the no-argument auto-detect path), every
/// arm here is an explicit ask: `Stack` never silently falls back to the uncommitted layer on
/// a missing upstream, and an unresolvable `Ref` is a named pre-TUI error, never a fallback to
/// auto-detect (ADR-030's "no surprise reviews" rule).
pub fn resolve_source(
    repo: &Repository,
    head_branch: &str,
    source: Source,
) -> Result<Vec<workon::Changeset>, SourceError> {
    match source {
        Source::Stack => resolve_stack(repo, head_branch),
        Source::Uncommitted => Ok(vec![uncommitted_changeset(head_branch)]),
        Source::Ref(text) => Err(SourceError::UnresolvableSource { text }),
    }
}

/// `stack` keyword resolution: Graphite metadata when active, otherwise the git-inference arm
/// (`StackModel::Git`, first wired into the binary here) — never a silent downgrade to
/// `StackModel::None`'s empty result, since the keyword is an explicit ask for the real stack.
/// The uncommitted layer rides along (`UncommittedLayer::Include`): `stack` always means
/// "focused on real `HEAD`."
fn resolve_stack(
    repo: &Repository,
    head_branch: &str,
) -> Result<Vec<workon::Changeset>, SourceError> {
    let model = if StackModel::detect(repo) == StackModel::Graphite {
        StackModel::Graphite
    } else {
        StackModel::Git
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
        assert_eq!(Source::classify("a..b"), Source::Ref("a..b".to_string()));
        assert_eq!(
            Source::classify("deadbeef"),
            Source::Ref("deadbeef".to_string())
        );
    }

    #[test]
    fn classify_empty_string_is_ref() {
        assert_eq!(Source::classify(""), Source::Ref(String::new()));
    }
}
