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

cd "$CLAUDE_PROJECT_DIR" || exit 0

input=$(cat)
stop_hook_active=$(printf '%s' "$input" | jq -r '.stop_hook_active // false')
if [ "$stop_hook_active" = "true" ]; then
  exit 0
fi

output=$(cargo clippy --all-targets --all-features -- -D warnings 2>&1) \
  && output=$(cargo test --workspace 2>&1) \
  && exit 0

jq -n --arg ctx "$output" '{
  decision: "block",
  hookSpecificOutput: {
    hookEventName: "Stop",
    additionalContext: ("clippy/test failed before stopping. Fix these, then continue:\n" + $ctx)
  }
}'
exit 0
