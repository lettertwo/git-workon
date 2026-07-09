//! Fixture tests for the M7 CS2 `Source` classifier + resolver (ADR-030): the `stack` and
//! `uncommitted` keywords, in both a Graphite-managed repo and a plain-git repo. `Ref`
//! resolution is a named interim failure until CS3 wires ref/range dispatch — see
//! `unresolvable_source_prints_error` in `tests/cli.rs`-style output assertions below (pinned
//! `NO_COLOR=1`, per the FORCE_COLOR trap this environment sets).

use assert_cmd::cargo_bin_cmd;
use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::ChangesetSpan;
use workon_review::error::SourceError;
use workon_review::source::{resolve_source, Source};

macro_rules! both_formats {
    ($($name:ident),+ $(,)?) => {$(
        mod $name {
            use super::*;
            #[test] fn refs()   { super::$name(MetadataFormat::Refs).unwrap() }
            #[test] fn sqlite() { super::$name(MetadataFormat::Sqlite).unwrap() }
        }
    )+};
}

both_formats!(
    stack_keyword_in_graphite_repo_returns_full_stack,
    uncommitted_keyword_in_graphite_repo_returns_single_uncommitted_changeset,
);

fn stack_keyword_in_graphite_repo_returns_full_stack(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .branch_metadata("c", "b")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "b", Source::classify("stack"))?;
    let names: Vec<&str> = changesets.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);

    let current: Vec<&str> = changesets
        .iter()
        .filter(|c| c.current)
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(current, vec!["b"], "exactly the focused branch is current");
    Ok(())
}

fn uncommitted_keyword_in_graphite_repo_returns_single_uncommitted_changeset(
    format: MetadataFormat,
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .metadata_format(format)
        .graphite_config(&["main"])
        .branch_metadata("a", "main")
        .branch_metadata("b", "a")
        .build()?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "b", Source::classify("uncommitted"))?;
    assert_eq!(changesets.len(), 1, "always exactly one changeset");
    assert_eq!(changesets[0].span, ChangesetSpan::Uncommitted);
    assert!(changesets[0].current);
    assert_eq!(
        changesets[0].name, "b",
        "the uncommitted changeset is named after the focused branch, not the stack"
    );
    Ok(())
}

#[test]
fn stack_keyword_in_plain_git_repo_with_upstream_returns_per_commit_changesets(
) -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()?;
    fixture.commit("main").file("a.txt", "1").create("first")?;
    fixture.commit("main").file("b.txt", "2").create("second")?;
    let repo = fixture.repo()?;

    let changesets = resolve_source(repo, "main", Source::classify("stack"))?;
    assert_eq!(changesets.len(), 2, "one changeset per commit");
    assert_eq!(changesets[0].title.as_deref(), Some("first"));
    assert_eq!(changesets[1].title.as_deref(), Some("second"));
    assert!(changesets[1].current);
    Ok(())
}

#[test]
fn stack_keyword_with_no_upstream_errors() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let err = resolve_source(repo, "main", Source::classify("stack")).unwrap_err();
    match err {
        SourceError::NoUpstream { branch } => assert_eq!(branch, "main"),
        other => panic!("expected NoUpstream, got {other:?}"),
    }
    Ok(())
}

/// `stack` on a branch that's caught up with its upstream and has a clean tree resolves to
/// zero changesets (`assemble_git`'s empty-vec arm, `git_inference_caught_up_and_clean_returns_empty`
/// in `git-workon-lib/tests/changeset.rs`) — end-to-end through the binary this must print
/// "nothing to review" and exit 0, exactly like the no-argument auto-detect path, not panic.
#[test]
fn stack_keyword_caught_up_and_clean_prints_nothing_to_review() {
    let fixture = FixtureBuilder::new()
        .remote("origin", "https://example.com/origin.git")
        .upstream("main", "origin/main")
        .build()
        .unwrap();
    let repo = fixture.repo().unwrap();
    let workdir = repo.workdir().unwrap();

    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.current_dir(workdir)
        .env("NO_COLOR", "1")
        .arg("stack")
        .assert()
        .success()
        .stderr(predicate::str::contains("nothing to review"));
}

/// The classifier/resolver seam CS2 introduces resolves `Ref` to a named, hinted pre-TUI
/// failure (real ref resolution is CS3) — end-to-end through the binary, so this doubles as
/// the CS2 manual smoke check ("a source shape renders or errors honestly"). Color is pinned
/// off: `FORCE_COLOR=3` is set in this dev environment and would otherwise leak ANSI codes
/// into the assertion.
#[test]
fn unresolvable_ref_source_prints_named_error_and_exits_nonzero() {
    let fixture = FixtureBuilder::new().build().unwrap();
    let repo = fixture.repo().unwrap();
    let workdir = repo.workdir().unwrap();

    let mut cmd = cargo_bin_cmd!("git-workon-review");
    cmd.current_dir(workdir)
        .env("NO_COLOR", "1")
        .arg("no-such-thing")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "cannot resolve 'no-such-thing' as a review source",
        ));
}
