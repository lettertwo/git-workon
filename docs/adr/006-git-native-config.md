# 006 — Git-Native Config Under `workon.*` Namespace

## Context

The tool needs per-repo and per-user configuration (default branch, post-create hooks, copy patterns, protected branches, PR format, hook timeout). Options included a dedicated config file (e.g. `.workon.toml`), environment variables, or reusing git's existing config system. Users already know how to read and write git config, and git's layered precedence model (local → global → system) matches the desired behavior exactly.

## Decision

All configuration is stored under the `workon.*` key namespace in standard git config files. `WorkonConfig` reads values by calling `repo.config()` (which returns git2's layered config) and then `config.get_string()`, `config.get_bool()`, `config.get_i64()`, or `config.multivar()` as appropriate.

Multi-value keys (`workon.postCreateHook`, `workon.copyPattern`, `workon.copyExclude`, `workon.pruneProtectedBranches`) use git's native multi-value support: `git config --add` appends, `config.multivar()` returns all entries merged across layers.

CLI arguments always take precedence over config values; `WorkonConfig` stores only the config-layer values, and command implementations apply CLI overrides when combining them.

## Consequences

- No new file format; users configure with `git config --add workon.postCreateHook "npm install"`.
- Global preferences go in `~/.gitconfig`, project preferences in `.git/config` — standard git mental model.
- Config is not checked into the repository (`.git/config` is local), which is correct for hooks and paths but means teams must document their config conventions separately.
- The `workon.prFormat` value is validated by `WorkonConfig::pr_format()` and returns a `ConfigError` if invalid placeholders are present.

## References

- `docs/diagrams/config-system.md` — precedence diagram and key reference
- `git-workon-lib/src/config.rs` — `WorkonConfig` struct
