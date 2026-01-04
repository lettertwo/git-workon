use git_workon_fixture::prelude::*;
use std::error::Error;
use workon::WorkonConfig;

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
