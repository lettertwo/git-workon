#!/bin/sh
#
# Claude Code PreToolUse hook (matcher: Bash): deny raw `cargo test` /
# `cargo clippy` invocations and nudge toward `cargo-gate test`/`clippy`
# instead — cargo-gate serializes cargo machine-wide, filters output, and
# records green proofs the Stop gate honors (skipping crates already
# proven at the current fingerprint). `cargo fmt`/`check`/`build`/`metadata`,
# cargo-gate itself, and non-cargo commands are unaffected.
#
# Matching: looks for `cargo test`/`cargo clippy` at a plausible command
# position — start of string, or right after `&&`, `;`, `|`, `(` (optional
# whitespace) — so a `cd x && cargo test` or piped form is still caught,
# while incidental substrings like `grep 'cargo test'` are not. Not
# preceded by `cargo-gate` (or a path ending in `/cargo-gate`), so
# `cargo-gate test -p x` passes through untouched. `cargo test --no-run` is
# still `cargo test` and is denied like any other form.
#

input=$(cat)
command=$(printf '%s' "$input" | jq -r '.tool_input.command // empty')

if [ -z "$command" ]; then
  exit 0
fi

if printf '%s' "$command" | grep -Eq '(^|&&|;|\||\()[[:space:]]*cargo(-gate)?[[:space:]]+(test|clippy)([[:space:]]|$)' &&
  ! printf '%s' "$command" | grep -Eq '(^|&&|;|\||\()[[:space:]]*(([A-Za-z0-9_./-]*/)?cargo-gate)[[:space:]]+(test|clippy)([[:space:]]|$)'; then
  jq -n '{
    hookSpecificOutput: {
      hookEventName: "PreToolUse",
      permissionDecision: "deny",
      permissionDecisionReason: "use cargo-gate test/clippy [args] instead — it serializes cargo machine-wide, filters output, and records green proofs the Stop gate honors (skips re-running)"
    }
  }'
  exit 0
fi

exit 0
