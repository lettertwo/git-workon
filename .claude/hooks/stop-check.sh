#!/bin/sh
#
# Claude Code Stop hook: run clippy + the workspace test suite before Claude
# finishes a turn. On failure, block the stop and feed the failure output
# back to Claude so it gets a chance to fix it.
#
# Guards against infinite loops via the stop_hook_active field on stdin —
# Claude gets exactly one forced fix round, then may stop even if still red
# (CI remains the hard gate).
#
# Skip-when-unchanged: the gates only run when the Rust-relevant state — HEAD,
# tracked changes to *.rs/*.toml/Cargo.lock, and untracked *.rs/*.toml files —
# differs from the last completed run, so a conversation-only turn stops
# instantly instead of paying minutes of cargo for nothing. The fingerprint
# lives per-worktree in $GIT_DIR and is recorded on red runs too: this hook
# forces one fix round per CODE STATE, not per stop (re-running the same red
# gates on an unchanged tree can never say anything new).
#

cd "$CLAUDE_PROJECT_DIR" || exit 0

input=$(cat)
stop_hook_active=$(printf '%s' "$input" | jq -r '.stop_hook_active // false')
if [ "$stop_hook_active" = "true" ]; then
  exit 0
fi

# Fingerprint the Rust-relevant working state. Any failure (no repo, unborn
# HEAD, missing shasum) leaves it empty, which falls through to running the
# gates — the cache only ever skips work, never invents a pass.
fingerprint=""
cache_file=""
git_dir=$(git rev-parse --git-dir 2>/dev/null)
if [ -n "$git_dir" ]; then
  cache_file="$git_dir/claude-stop-check.fingerprint"
  fingerprint=$(
    {
      git rev-parse HEAD 2>/dev/null
      git diff HEAD -- '*.rs' '*.toml' 'Cargo.lock' 2>/dev/null
      untracked=$(git ls-files --others --exclude-standard -- '*.rs' '*.toml' 2>/dev/null)
      if [ -n "$untracked" ]; then
        printf '%s\n' "$untracked"
        printf '%s\n' "$untracked" | tr '\n' '\0' | xargs -0 cat 2>/dev/null
      fi
    } | shasum 2>/dev/null | cut -d' ' -f1
  )
fi

if [ -n "$fingerprint" ] && [ -f "$cache_file" ] &&
  [ "$(cat "$cache_file" 2>/dev/null)" = "$fingerprint" ]; then
  exit 0 # nothing Rust-relevant changed since the last completed run
fi

if output=$("$HOME/.claude/bin/cargo-gate" clippy 2>&1) &&
  output=$("$HOME/.claude/bin/cargo-gate" test 2>&1); then
  [ -n "$fingerprint" ] && printf '%s' "$fingerprint" >"$cache_file"
  exit 0
fi

# Red: record the state anyway — the block below is this state's one forced
# fix round; later stops of the same unchanged tree skip straight through.
[ -n "$fingerprint" ] && printf '%s' "$fingerprint" >"$cache_file"

jq -n --arg ctx "$output" '{
  decision: "block",
  hookSpecificOutput: {
    hookEventName: "Stop",
    additionalContext: ("clippy/test failed before stopping. Fix these, then continue:\n" + $ctx)
  }
}'
exit 0
