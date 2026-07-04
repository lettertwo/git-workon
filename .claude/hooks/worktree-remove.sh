#!/bin/sh
#
# Claude Code WorktreeRemove hook: clean up Claude-created worktrees via
# git-workon prune.
#
# Side-effect only: Claude Code ignores exit code and output (failures are
# logged in debug mode only), so this is best-effort.
#

input=$(cat)
wt_path=$(printf '%s' "$input" | jq -r '.worktree_path // empty')
main_path=$(printf '%s' "$input" | jq -r '.main_worktree_path // empty')

[ -n "$wt_path" ] || exit 0
[ -d "$wt_path" ] || exit 0

# Derive the workon name: path relative to the workon root (the parent of
# the main worktree), e.g. /root/claude/foo -> claude/foo.
root=$(dirname "${main_path:-$wt_path}")
wt_name=${wt_path#"$root"/}

# Agent worktrees are disposable: force past dirty/protection checks.
git workon prune "$wt_name" --force --yes --no-color -q >&2 2>&1 || {
  echo "worktree-remove hook: prune '$wt_name' failed" >&2
}
exit 0
