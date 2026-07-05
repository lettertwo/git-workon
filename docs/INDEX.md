# Context Index

Maps subsystems and topics to relevant documentation files and source paths. Used by `/context` to load targeted context.

## Subsystems

### clone

- `docs/diagrams/clone-and-init.md`
- `docs/adr/008-error-handling-guide.md`
- `docs/adr/001-bare-repo-worktrees-layout.md`
- `docs/adr/002-workon-root-discovery.md`
- Key source: `git-workon-lib/src/clone.rs`, `git-workon/src/cmd/clone.rs`

### new

- `docs/diagrams/new-worktree.md`
- `docs/diagrams/pr-workflow.md`
- `docs/adr/012-three-branch-types.md`
- Key source: `git-workon/src/cmd/new.rs`, `git-workon-lib/src/worktree.rs`

### find

- `docs/diagrams/find-flow.md`
- `docs/adr/013-find-three-mode-strategy.md`
- `docs/adr/023-unified-stack-tree-views.md`
- Key source: `git-workon/src/cmd/find.rs`, `git-workon-lib/src/worktree.rs`

### prune

- `docs/diagrams/prune-flow.md`
- `docs/adr/014-prune-three-phase-safety.md`
- Key source: `git-workon/src/cmd/prune.rs`

### move

- `docs/diagrams/move-flow.md`
- `docs/adr/015-atomic-move-with-rollback.md`
- Key source: `git-workon/src/cmd/move.rs`

### doctor

- `docs/diagrams/doctor-flow.md`
- Key source: `git-workon/src/cmd/doctor.rs`

### stack / stacked-diffs / graphite

- `docs/recipes/stacked-diffs.md` — user guide: setup, worktree-per-stack pattern, list/find/new stack-aware behavior, config reference
- `docs/rfc/stacked-diffs.md` — research on stacked diff tools (Graphite, branchless, spr, Sapling, git-stack)
- `docs/adr/023-unified-stack-tree-views.md` — unified tree rendering for list and find
- Key source: `git-workon-lib/src/stack.rs`, `git-workon-lib/src/stack/graphite.rs`, `git-workon-lib/src/config.rs` (`stack_model`, `gt_auto_track`)

### pr / pull-request

- `docs/diagrams/pr-workflow.md`
- `docs/adr/009-pr-workflow-gh-cli.md`
- Key source: `git-workon-lib/src/pr.rs`, `git-workon/src/cmd/new.rs`

### review / workon-review

- `docs/rfc/workon-review.md`
- `docs/adr/027-review-crate-workspace-placement.md`
- Key source: `git-workon-review/src/`

## Cross-cutting Concerns

### errors / error-handling / miette

- `docs/diagrams/error-hierarchy.md`
- `docs/adr/008-error-handling-guide.md`
- `docs/adr/008-error-handling-strategy.md`
- Key source: `git-workon-lib/src/error.rs`

### testing / tests / fixture

- `docs/adr/017-fixture-based-testing-guide.md`
- `docs/adr/017-fixture-based-testing.md`
- Key source: `git-workon-fixture/src/fixture_builder.rs`, `git-workon-fixture/src/predicates/`

### config / configuration

- `docs/diagrams/config-system.md`
- `docs/adr/006-git-native-config.md`
- Key source: `git-workon-lib/src/config.rs`

### architecture / overview

- `docs/diagrams/architecture.md`
- `docs/diagrams/command-dispatch.md`
- `docs/adr/003-three-crate-workspace.md`
- `docs/adr/004-smart-routing-default-command.md`
- `docs/adr/005-run-trait-command-dispatch.md`

### hooks / copy

- `docs/adr/007-hybrid-hook-system.md`
- `docs/adr/010-platform-copy-on-write.md`
- `docs/adr/011-file-copy-two-mode-design.md`
- Key source: `git-workon/src/hooks.rs`, `git-workon/src/copy.rs`

### implementation / principles / code-quality

- `docs/adr/018-implementation-principles-guide.md`
- `docs/adr/018-implementation-principles.md`
- `docs/adr/016-self-documenting-code.md`

## RFCs and Research

### stacked-diffs / stacked

- `docs/rfc/stacked-diffs.md` — pre-implementation research (see `stack` subsystem above for current docs)
