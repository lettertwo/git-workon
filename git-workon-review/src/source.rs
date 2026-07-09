//! Classifying and resolving a `git workon review [<source>]` positional argument (ADR-030).
//!
//! [`Source::classify`] is pure — no repository access, deterministic regardless of repo
//! state — so a branch literally named `stack` only matches the keyword when spelled bare;
//! `refs/heads/stack` or `heads/stack` classify as [`Source::Ref`]. Resolution
//! ([`resolve_source`]) is where repo state comes in.
//!
//! CS3 wires real `<ref>` dispatch (shape-aware: Graphite-tracked branch, other branch,
//! bare commit-ish) and `Range` (`a..b` / `a...b`, git-diff semantics). CS4 wires `Pr`: any
//! form git-workon-lib's `parse_pr_reference` accepts (`pr-123`, `#123`, `pr#123`, GitHub URLs;
//! a bare number never matches — that spelling stays a `Ref`), reused end-to-end for
//! resolution too (`check_gh_available` → `fetch_pr_metadata` → fork-aware fetch → one
//! committed changeset).

use git2::{BranchType, Oid, Repository};
use workon::{
    assemble_changesets, get_default_branch, graphite_trunk, ChangesetError, ChangesetSpan,
    PrMetadata, StackModel, UncommittedLayer, WorkonError,
};

use crate::acquire::uncommitted_changeset;
use crate::error::SourceError;

/// Which dot form a [`Source::Range`] was spelled with — git-diff semantics differ (ADR-030).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeDots {
    /// `a..b` — base `a`, head `b` (endpoint trees, exactly a committed span).
    Two,
    /// `a...b` — base `merge-base(a, b)`, head `b` (the PR-style "what did b add").
    Three,
}

/// What a `git workon review <source>` argument was classified as (ADR-030 precedence).
///
/// `classify` only ever runs on `Some(text)` — the no-argument case stays the existing
/// auto-detect path in `main.rs` and never constructs a `Source` at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A PR reference (`pr-123`, `#123`, `pr#123`, a GitHub PR URL — any form
    /// `workon::parse_pr_reference` accepts). Carries the source text as typed, for the
    /// changeset name; the PR number is re-derived from it at resolution time.
    Pr(String),
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
    /// Classify `text` per ADR-030's precedence: PR reference first (checked via
    /// `workon::parse_pr_reference`, pure string parsing — no network, no repo access), then
    /// exact bare keyword, else a range (three-dot checked before two-dot, since `...` contains
    /// `..`), else `Ref`. A malformed near-PR spelling (`pr-`, `pr-abc`) is `Ok(Err(_))` from the
    /// lib parser, not `Ok(Some(_))` — it falls through to the normal precedence chain rather
    /// than being force-classified as a broken PR, so it ultimately resolves (or fails) as a
    /// `Ref` like any other typo. A bare number (`123`) never matches any of the lib parser's
    /// accepted spellings, so it also falls through to `Ref` — no separate digit guard needed.
    pub fn classify(text: &str) -> Source {
        if let Ok(Some(_)) = workon::parse_pr_reference(text) {
            return Source::Pr(text.to_string());
        }
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
/// auto-detect (ADR-030's "no surprise reviews" rule).
pub fn resolve_source(
    repo: &Repository,
    head_branch: &str,
    source: Source,
) -> Result<Vec<workon::Changeset>, SourceError> {
    match source {
        Source::Pr(text) => resolve_pr(repo, text),
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

/// `Pr` resolution (ADR-030): reuse git-workon-lib's `pr.rs` PR workflow end-to-end, minus the
/// worktree-creation step — review only needs the PR's base and head fetched locally so their
/// merge-base span can be computed, never a branch or worktree. Every failure here is a named,
/// hinted pre-TUI error. The network round-trip (`check_gh_available`, `fetch_pr_metadata`,
/// `fetch_branch_fresh`) lives entirely in this function so [`pr_changeset_from_metadata`] can
/// stay a pure git2 mapping, fixture-testable without gh (the real gh path is exercised manually
/// — see the CS4 changeset description).
///
/// Both refs are fetched with [`workon::fetch_branch_fresh`], not [`workon::fetch_branch`]:
/// review's whole point is freshness, and `fetch_branch`'s existence short-circuit (right for
/// its original one-time worktree-creation fetch) would leave a previously-fetched head stale
/// and — since `refs/remotes/{remote}/{base}` is virtually always already present — would never
/// refresh the base at all, corrupting the merge-base against a stale base tip.
fn resolve_pr(repo: &Repository, text: String) -> Result<Vec<workon::Changeset>, SourceError> {
    workon::check_gh_available().map_err(|source| SourceError::GhUnavailable {
        text: text.clone(),
        source,
    })?;

    // `classify` only builds `Source::Pr` from a `parse_pr_reference` `Ok(Some(_))`, so this
    // re-parse is infallible in practice; treated as unresolvable rather than unwrapped in case
    // a `Source::Pr` is ever constructed some other way.
    let pr = workon::parse_pr_reference(&text)
        .ok()
        .flatten()
        .ok_or_else(|| SourceError::UnresolvableSource { text: text.clone() })?;

    let metadata =
        workon::fetch_pr_metadata(pr.number).map_err(|source| SourceError::PrResolutionFailed {
            text: text.clone(),
            source,
        })?;

    let head_remote = if metadata.is_fork {
        workon::setup_fork_remote(repo, &metadata)
    } else {
        workon::detect_pr_remote(repo)
    }
    .map_err(|source| SourceError::PrResolutionFailed {
        text: text.clone(),
        source,
    })?;
    workon::fetch_branch_fresh(repo, &head_remote, &metadata.head_ref).map_err(|source| {
        SourceError::PrResolutionFailed {
            text: text.clone(),
            source,
        }
    })?;

    // The base branch is what the PR targets, never a fork branch — always the detected
    // upstream/origin remote, regardless of whether the head came from a fork.
    let base_remote =
        workon::detect_pr_remote(repo).map_err(|source| SourceError::PrResolutionFailed {
            text: text.clone(),
            source,
        })?;
    workon::fetch_branch_fresh(repo, &base_remote, &metadata.base_ref).map_err(|source| {
        SourceError::PrResolutionFailed {
            text: text.clone(),
            source,
        }
    })?;

    pr_changeset_from_metadata(repo, &text, &metadata, &head_remote, &base_remote)
}

/// Map fetched PR metadata to the one committed changeset review renders for it:
/// `merge-base(base tip, head tip)..head`, PR title carried through (ADR-030: "GitHub's own
/// three-dot PR diff"). Pure git2 — no gh, no fetch — assuming `head_remote`/`base_remote`
/// already have `metadata.head_ref`/`metadata.base_ref` as remote-tracking branches (true after
/// [`resolve_pr`]'s fetches, or hand-built in a fixture for testing this half without gh).
fn pr_changeset_from_metadata(
    repo: &Repository,
    text: &str,
    metadata: &PrMetadata,
    head_remote: &str,
    base_remote: &str,
) -> Result<Vec<workon::Changeset>, SourceError> {
    let unresolvable = || SourceError::UnresolvableSource {
        text: text.to_string(),
    };

    let head_oid =
        remote_branch_tip(repo, head_remote, &metadata.head_ref).ok_or_else(unresolvable)?;
    let base_tip =
        remote_branch_tip(repo, base_remote, &metadata.base_ref).ok_or_else(unresolvable)?;
    let base_oid = repo
        .merge_base(base_tip, head_oid)
        .map_err(|_| unresolvable())?;

    Ok(vec![workon::Changeset {
        name: text.to_string(),
        span: ChangesetSpan::Committed {
            base: base_oid,
            head: head_oid,
        },
        title: Some(metadata.title.clone()),
        current: true,
        needs_restack: false,
    }])
}

/// The tip commit of `refs/remotes/{remote}/{branch}`, or `None` if it isn't a remote-tracking
/// branch that resolves to a commit.
fn remote_branch_tip(repo: &Repository, remote: &str, branch: &str) -> Option<Oid> {
    repo.find_branch(&format!("{remote}/{branch}"), BranchType::Remote)
        .ok()
        .and_then(|b| b.get().target())
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

    assemble_changesets(repo, head_branch, model, UncommittedLayer::Include)
        .map_err(map_assemble_err(head_branch))
}

/// Maps an [`assemble_changesets`] failure to a [`SourceError`], for the branch named `branch`:
/// a missing upstream becomes [`SourceError::NoUpstream`], anything else
/// [`SourceError::StackResolutionFailed`].
fn map_assemble_err(branch: &str) -> impl Fn(WorkonError) -> SourceError + '_ {
    move |err| match err {
        WorkonError::Changeset(ChangesetError::NoUpstream { branch }) => {
            SourceError::NoUpstream { branch }
        }
        other => SourceError::StackResolutionFailed {
            branch: branch.to_string(),
            source: other,
        },
    }
}

/// `<ref>` resolution — shape-aware dispatch (ADR-030), checked in order:
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
            return assemble_changesets(repo, &branch_name, StackModel::Graphite, layer)
                .map_err(map_assemble_err(&branch_name));
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
/// candidate for the uncommitted layer (ADR-030: committed-only, like every source but `stack`
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

/// Rev-parse one range endpoint, peeled to a commit; an empty `text` defaults to `HEAD` (ADR-030).
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
/// (`refs/heads/<name>`, `heads/<name>`) that resolves to one — the same escapes ADR-030 gives
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
    fn classify_pr_dash_number_is_pr() {
        assert_eq!(Source::classify("pr-123"), Source::Pr("pr-123".to_string()));
    }

    #[test]
    fn classify_hash_number_is_pr() {
        assert_eq!(Source::classify("#123"), Source::Pr("#123".to_string()));
    }

    #[test]
    fn classify_pr_hash_number_is_pr() {
        assert_eq!(Source::classify("pr#123"), Source::Pr("pr#123".to_string()));
    }

    #[test]
    fn classify_github_url_is_pr() {
        let url = "https://github.com/owner/repo/pull/123";
        assert_eq!(Source::classify(url), Source::Pr(url.to_string()));
    }

    #[test]
    fn classify_bare_number_is_ref_not_pr() {
        // ADR-030 explicitly excludes a bare number — it could be a branch or an abbreviated
        // sha. `workon::parse_pr_reference` already requires a `#`/`pr-`/`pr#` prefix or a
        // GitHub URL, so this falls through to `Ref` with no extra guard needed here.
        assert_eq!(Source::classify("123"), Source::Ref("123".to_string()));
    }

    #[test]
    fn classify_malformed_pr_dash_is_ref_not_pr() {
        // `pr-` and `pr-abc` look PR-shaped but don't carry a valid number —
        // `parse_pr_reference` returns `Err`, not `Ok(Some(_))`, so classify falls through
        // rather than force-classifying a broken PR reference.
        assert_eq!(Source::classify("pr-"), Source::Ref("pr-".to_string()));
        assert_eq!(
            Source::classify("pr-abc"),
            Source::Ref("pr-abc".to_string())
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

    // ── CS4: PR metadata → changeset mapping (the gh-free half of `resolve_pr`) ─────────────

    /// [`pr_changeset_from_metadata`] is the pure git2 half of PR resolution — everything
    /// downstream of `fetch_pr_metadata`/`fetch_branch`, which the real `gh` path can't exercise
    /// in CI. This fixture stands in for "already fetched": a real (local, file-path) remote,
    /// with `fetch_branch` itself used to populate the remote-tracking refs, so the only thing
    /// not exercised here is the network round-trip to `gh` and to a non-local remote.
    #[test]
    fn pr_metadata_maps_to_merge_base_changeset_with_title(
    ) -> Result<(), Box<dyn std::error::Error>> {
        use git_workon_fixture::prelude::*;

        // `RemoteSource::from(&Fixture)` only resolves to the bare `.git` dir when
        // `fixture.repo()` itself reports bare — true for a bare fixture with NO worktree (a
        // worktree checkout is never bare, even off a bare main repo). So this "remote" fixture
        // stays worktree-free, and its two divergent branches are built directly with git2
        // rather than via `commit()` (which requires a checked-out worktree path).
        let upstream = FixtureBuilder::new()
            .bare(true)
            .default_branch("main")
            .build()?;
        let upstream_repo = upstream.repo()?;
        let base_commit = upstream_repo.head()?.peel_to_commit()?;
        let sig = git2::Signature::now("Test User", "test@example.com")?;

        let mut main_tree = upstream_repo.treebuilder(None)?;
        let a_blob = upstream_repo.blob(b"1")?;
        main_tree.insert("a.txt", a_blob, 0o100_644)?;
        let main_tree_oid = main_tree.write()?;
        let main_tree = upstream_repo.find_tree(main_tree_oid)?;
        let main_oid = upstream_repo.commit(
            Some("refs/heads/main"),
            &sig,
            &sig,
            "on main",
            &main_tree,
            &[&base_commit],
        )?;

        let mut head_tree = upstream_repo.treebuilder(None)?;
        let b_blob = upstream_repo.blob(b"1")?;
        head_tree.insert("b.txt", b_blob, 0o100_644)?;
        let head_tree_oid = head_tree.write()?;
        let head_tree = upstream_repo.find_tree(head_tree_oid)?;
        let head_oid = upstream_repo.commit(
            Some("refs/heads/pr-head"),
            &sig,
            &sig,
            "on pr-head",
            &head_tree,
            &[&base_commit],
        )?;

        let local = FixtureBuilder::new().remote("origin", &upstream).build()?;
        let repo = local.repo()?;
        workon::fetch_branch(repo, "origin", "main")?;
        workon::fetch_branch(repo, "origin", "pr-head")?;

        let metadata = PrMetadata {
            number: 123,
            title: "Add widget".to_string(),
            author: "someone".to_string(),
            head_ref: "pr-head".to_string(),
            base_ref: "main".to_string(),
            is_fork: false,
            fork_owner: None,
            fork_url: None,
        };

        let changesets = pr_changeset_from_metadata(repo, "pr-123", &metadata, "origin", "origin")?;
        assert_eq!(changesets.len(), 1);
        assert_eq!(changesets[0].name, "pr-123");
        assert_eq!(changesets[0].title.as_deref(), Some("Add widget"));
        assert!(changesets[0].current);
        match changesets[0].span {
            ChangesetSpan::Committed { base, head } => {
                assert_eq!(head, head_oid);
                let expected_base = repo.merge_base(main_oid, head_oid)?;
                assert_eq!(base, expected_base);
            }
            other => panic!("expected Committed, got {other:?}"),
        }
        Ok(())
    }

    /// A missing remote-tracking ref (nothing fetched yet for that branch) is unresolvable, not
    /// a panic — guards the "assumes already fetched" precondition documented on
    /// [`pr_changeset_from_metadata`].
    #[test]
    fn pr_metadata_with_unfetched_head_is_unresolvable() -> Result<(), Box<dyn std::error::Error>> {
        use git_workon_fixture::prelude::*;

        let fixture = FixtureBuilder::new().build()?;
        let repo = fixture.repo()?;

        let metadata = PrMetadata {
            number: 123,
            title: "Add widget".to_string(),
            author: "someone".to_string(),
            head_ref: "pr-head".to_string(),
            base_ref: "main".to_string(),
            is_fork: false,
            fork_owner: None,
            fork_url: None,
        };

        let err =
            pr_changeset_from_metadata(repo, "pr-123", &metadata, "origin", "origin").unwrap_err();
        match err {
            SourceError::UnresolvableSource { text } => assert_eq!(text, "pr-123"),
            other => panic!("expected UnresolvableSource, got {other:?}"),
        }
        Ok(())
    }
}
