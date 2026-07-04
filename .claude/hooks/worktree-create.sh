#!/bin/sh
#
# Claude Code WorktreeCreate hook: route worktree creation through git-workon.
#
# Replaces Claude Code's default `git worktree add` so that Claude-created
# worktrees (claude -w, subagent isolation) get workon's layout and autocopy
# behavior (untracked config like .cargo/config.toml, .envrc, etc.).
#
# Contract: print the created worktree path as the ONLY stdout line and exit 0.
# Any non-zero exit fails worktree creation (no git fallback), so diagnostics
# must go to stderr.
#

input=$(cat)

# Field names vary across doc examples; prefer branch, then name, then the
# basename of the proposed path.
branch=$(printf '%s' "$input" | jq -r '.branch // empty')
name=$(printf '%s' "$input" | jq -r '.name // empty')
proposed=$(printf '%s' "$input" | jq -r '.worktree_path // empty')

if [ -n "$branch" ]; then
  wt_name=$branch
elif [ -n "$name" ]; then
  wt_name=$name
elif [ -n "$proposed" ]; then
  wt_name=$(basename "$proposed")
else
  echo "worktree-create hook: no branch/name/worktree_path in input" >&2
  exit 1
fi

# Group Claude-created worktrees under claude/ unless already namespaced.
case "$wt_name" in
  claude/*) ;;
  */*) ;;
  *) wt_name="claude/$wt_name" ;;
esac

out=$(git workon new "$wt_name" --json --no-color -q) || {
  echo "worktree-create hook: git workon new '$wt_name' failed" >&2
  exit 1
}

path=$(printf '%s' "$out" | jq -r '.path // empty')
if [ -z "$path" ] || [ ! -d "$path" ]; then
  echo "worktree-create hook: no worktree path returned for '$wt_name'" >&2
  exit 1
fi

printf '%s\n' "$path"
