# Domain Glossary

Terms used throughout the `git-workon` codebase. Implementation details do not belong here.

## Worktree States

**Gone upstream** — a local branch whose upstream tracking ref (`refs/remotes/<remote>/<branch>`) no longer exists locally. Caused by the upstream branch having been deleted on the remote and the deletion not yet fetched. Detected via `Branch::upstream()` returning `Err` while `branch.<name>.remote` is still set in git config. Accurate only after a prune-fetch. See also: `WorktreeDescriptor::has_gone_upstream()`.

**Prune-fetch** — a fetch that also deletes (prunes) stale local remote-tracking refs for branches that no longer exist on the remote. Equivalent to `git fetch --prune <remote>`. Makes "gone upstream" detection trustworthy.

## Status Filters

**Status filter** — a flag (`--dirty`, `--clean`, `--ahead`, `--behind`, `--gone`) that narrows a `list` or `find` result to worktrees in a specific state. Filters select **worktrees**: each check queries the working tree or branch-tracking state of a checked-out worktree. A metadata-only stack diff (`◯`) has no working tree and can never satisfy a status filter; it is excluded from any filtered result. See also: `StatusFilter`, `WorktreeDescriptor::is_dirty()`.

## Review

**Changeset** — one reviewable unit in the review TUI: a node in a stack, a single inferred commit, or the uncommitted layer. Ordered base → head when part of a stack. See also: `workon::Changeset`.

**Changeset span** — what a changeset covers: a resolved commit range (`base..head`) or the uncommitted working tree + index. _Avoid_: "changeset source" (renamed; "source" is the review-source concept below).

**Review source** — the user's answer to "review *what?*": auto-detect (no argument), the `stack` keyword, the `uncommitted` keyword, a ref, a range, or a PR reference. Exact bare keywords win over same-named refs; a qualified spelling (`refs/heads/stack`) escapes. See also: [ADR-036](docs/adr/036-review-source-grammar.md).

**Uncommitted layer** — the synthetic changeset spanning the dirty working tree + index. Appears in a review only when the review is focused where `HEAD` actually is, since uncommitted changes diff against `HEAD`.

## Review Theming

**Wash** — a background color painted behind diff text to signal that the text changed. Washes carry the diff signal; foreground carries syntax meaning unless a theme says otherwise. _Avoid_: "tint" for the background specifically (see below), "highlight".

**Line wash** — the wash covering an entire line that contains a change. Answers "something here changed". _Avoid_: "subtle" (renamed — it named intensity, not scope).

**Edit** — the exact text that changed. On a line paired with a counterpart, the word-diff ranges within it; on a line with no counterpart, the whole line. _Avoid_: "word" (true only for the paired case), "change" (reserved for a file's change kind).

**Edit wash** — the wash covering an edit. Answers "this precisely is the change". _Avoid_: "strong" (renamed — its intensity-flavored name is what let it drift into a foreground role).

**Tint foreground** — a text color that encodes added-ness or deleted-ness rather than syntax meaning. Distinct from a wash: same fact, opposite channel. _Avoid_: "diff color" (ambiguous between the two channels).

**Slot** — one of the sixteen base16 palette positions (`base00`–`base0f`) a theme assigns colors to. A slot has a *role* only when some part of the TUI reads it; the key space accepts all sixteen regardless.

## Prune Candidate Reasons

**BranchDeleted** — the local branch ref for the worktree no longer exists in the repository. Always a prune candidate regardless of flags.

**RemoteGone** — the worktree's branch has a gone upstream. Only a candidate when `--gone` / `workon.pruneGone` is enabled.

**Merged(target)** — the worktree's branch has been merged into `target`. Only a candidate when `--merged` is passed.

**Explicit** — the worktree was named directly as a positional argument to `prune`.
