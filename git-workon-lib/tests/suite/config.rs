use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::{Granularity, StackModel, WorkonConfig};

#[test]
fn read_default_branch_config() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.defaultBranch", "develop")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;
    assert_eq!(
        workon_config.default_branch(None)?,
        Some("develop".to_string())
    );
    Ok(())
}

#[test]
fn default_branch_returns_none_when_not_configured() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let workon_config = WorkonConfig::new(repo)?;
    assert_eq!(workon_config.default_branch(None)?, None);
    Ok(())
}

#[test]
fn cli_override_takes_precedence_over_config() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.defaultBranch", "develop")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;

    // Without override, returns config value
    assert_eq!(
        workon_config.default_branch(None)?,
        Some("develop".to_string())
    );

    // With override, returns override
    assert_eq!(
        workon_config.default_branch(Some("main"))?,
        Some("main".to_string())
    );
    Ok(())
}

#[test]
fn read_post_create_hooks_multi_value() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.postCreateHook", "npm install")
        .config("workon.postCreateHook", "cp .env.example .env")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;
    let hooks = workon_config.post_create_hooks()?;
    assert_eq!(hooks.len(), 2);
    assert_eq!(hooks[0], "npm install");
    assert_eq!(hooks[1], "cp .env.example .env");
    Ok(())
}

#[test]
fn empty_multivar_returns_empty_vec() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let workon_config = WorkonConfig::new(repo)?;
    assert_eq!(workon_config.post_create_hooks()?, Vec::<String>::new());
    assert_eq!(workon_config.copy_patterns()?, Vec::<String>::new());
    assert_eq!(workon_config.copy_excludes()?, Vec::<String>::new());
    assert_eq!(
        workon_config.prune_protected_branches()?,
        Vec::<String>::new()
    );
    Ok(())
}

#[test]
fn pr_format_defaults_to_pr_number() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let workon_config = WorkonConfig::new(repo)?;
    assert_eq!(workon_config.pr_format(None)?, "pr-{number}");
    Ok(())
}

#[test]
fn pr_format_reads_from_config() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.prFormat", "pull-request-{number}")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;
    assert_eq!(workon_config.pr_format(None)?, "pull-request-{number}");
    Ok(())
}

#[test]
fn pr_format_requires_number_placeholder() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.prFormat", "invalid-format")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;
    let result = workon_config.pr_format(None);
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("{number} placeholder"));
    Ok(())
}

#[test]
fn pr_format_cli_override_also_validated() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;

    let workon_config = WorkonConfig::new(repo)?;
    let result = workon_config.pr_format(Some("bad-format"));
    assert!(result.is_err());
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("{number} placeholder"));
    Ok(())
}

#[test]
fn read_copy_patterns_multi_value() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.copyPattern", ".env*")
        .config("workon.copyPattern", ".vscode/")
        .config("workon.copyPattern", "node_modules/")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;
    let patterns = workon_config.copy_patterns()?;
    assert_eq!(patterns.len(), 3);
    assert_eq!(patterns[0], ".env*");
    assert_eq!(patterns[1], ".vscode/");
    assert_eq!(patterns[2], "node_modules/");
    Ok(())
}

#[test]
fn read_copy_excludes_multi_value() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.copyExclude", ".env.production")
        .config("workon.copyExclude", "*.secret")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;
    let excludes = workon_config.copy_excludes()?;
    assert_eq!(excludes.len(), 2);
    assert_eq!(excludes[0], ".env.production");
    assert_eq!(excludes[1], "*.secret");
    Ok(())
}

#[test]
fn read_prune_protected_branches_multi_value() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.pruneProtectedBranches", "main")
        .config("workon.pruneProtectedBranches", "develop")
        .config("workon.pruneProtectedBranches", "release/*")
        .build()?;

    let repo = fixture.repo()?;
    let workon_config = WorkonConfig::new(repo)?;
    let protected = workon_config.prune_protected_branches()?;
    assert_eq!(protected.len(), 3);
    assert_eq!(protected[0], "main");
    assert_eq!(protected[1], "develop");
    assert_eq!(protected[2], "release/*");
    Ok(())
}

// ── stack_model ───────────────────────────────────────────────────────────────

#[test]
fn stack_model_none_returns_none_variant() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackModel", "none")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(cfg.stack_model(None)?, StackModel::None);
    Ok(())
}

#[test]
fn stack_model_graphite_returns_graphite_variant() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackModel", "graphite")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(cfg.stack_model(None)?, StackModel::Graphite);
    Ok(())
}

#[test]
fn stack_model_git_returns_git_variant() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackModel", "git")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(cfg.stack_model(None)?, StackModel::Git);
    Ok(())
}

#[test]
fn stack_model_cli_override_wins_over_config() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackModel", "none")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(cfg.stack_model(Some("graphite"))?, StackModel::Graphite);
    Ok(())
}

#[test]
fn stack_model_gh_stack_returns_gh_stack_variant() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackModel", "gh-stack")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(cfg.stack_model(None)?, StackModel::GhStack);
    Ok(())
}

#[test]
fn stack_model_bare_ghstack_is_rejected_as_a_different_tool() -> Result<(), Box<dyn Error>> {
    // "ghstack" (no hyphen) is Meta's Phabricator-style stacker, a different tool from
    // "gh-stack" (github/gh-stack) — it must not be silently treated as a typo.
    let fixture = FixtureBuilder::new()
        .config("workon.stackModel", "ghstack")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    let err = cfg.stack_model(None).unwrap_err();
    assert!(
        err.to_string().contains("not yet supported"),
        "expected 'not yet supported' for 'ghstack', got: {err}"
    );
    Ok(())
}

#[test]
fn stack_model_unsupported_values_return_error() -> Result<(), Box<dyn Error>> {
    for unsupported in &["branchless", "sapling", "spr"] {
        let fixture = FixtureBuilder::new()
            .config("workon.stackModel", unsupported)
            .build()?;
        let repo = fixture.repo()?;
        let cfg = WorkonConfig::new(repo)?;
        let err = cfg.stack_model(None).unwrap_err();
        assert!(
            err.to_string().contains("not yet supported"),
            "expected 'not yet supported' for model '{unsupported}', got: {err}"
        );
    }
    Ok(())
}

#[test]
fn stack_model_unknown_value_returns_error() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackModel", "unknown-tool")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    let err = cfg.stack_model(None).unwrap_err();
    assert!(err.to_string().contains("Unknown stack model"));
    Ok(())
}

// ── stack_worktree_granularity ────────────────────────────────────────────────

#[test]
fn stack_worktree_granularity_defaults_to_stack() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(cfg.stack_worktree_granularity(None)?, Granularity::Stack);
    Ok(())
}

#[test]
fn stack_worktree_granularity_stack_explicit() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackWorktreeGranularity", "stack")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(cfg.stack_worktree_granularity(None)?, Granularity::Stack);
    Ok(())
}

#[test]
fn stack_worktree_granularity_diff_is_unsupported() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackWorktreeGranularity", "diff")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    let err = cfg.stack_worktree_granularity(None).unwrap_err();
    assert!(err.to_string().contains("not yet implemented"));
    Ok(())
}

#[test]
fn stack_worktree_granularity_unknown_value_returns_error() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackWorktreeGranularity", "per-commit")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    let err = cfg.stack_worktree_granularity(None).unwrap_err();
    assert!(err.to_string().contains("Unknown worktree granularity"));
    Ok(())
}

#[test]
fn stack_worktree_granularity_cli_override_wins() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.stackWorktreeGranularity", "diff")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert_eq!(
        cfg.stack_worktree_granularity(Some("stack"))?,
        Granularity::Stack
    );
    Ok(())
}

// ── gt_auto_track ─────────────────────────────────────────────────────────────

#[test]
fn gt_auto_track_defaults_to_true() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new().build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert!(cfg.gt_auto_track(None)?);
    Ok(())
}

#[test]
fn gt_auto_track_reads_false_from_config() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.gtAutoTrack", "false")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert!(!cfg.gt_auto_track(None)?);
    Ok(())
}

#[test]
fn gt_auto_track_cli_override_wins() -> Result<(), Box<dyn Error>> {
    let fixture = FixtureBuilder::new()
        .config("workon.gtAutoTrack", "true")
        .build()?;
    let repo = fixture.repo()?;
    let cfg = WorkonConfig::new(repo)?;
    assert!(!cfg.gt_auto_track(Some(false))?);
    Ok(())
}
