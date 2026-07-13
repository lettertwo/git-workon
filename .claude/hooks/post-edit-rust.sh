#!/bin/sh
#
# Claude Code PostToolUse hook (Edit|Write|MultiEdit): after any Rust file
# edit, format the edited file, then fast-fail on type errors. A cargo check
# failure exits 2 so the errors feed straight back to Claude as blocking
# feedback on the edit that introduced them — compile breakage never survives
# past a single edit. Tests still gate on Stop (stop-check.sh) and CI.
#
# Scoped check: the edited file's crate (longest path->crate-dir prefix
# match against `cargo-gate map`'s table) is checked alone instead of the
# whole workspace — a single-crate `cargo check` is the bulk of the per-edit
# cost. Falls back to an unscoped check if the file matches no crate dir or
# the map is unavailable, rather than silently checking nothing.
#

input=$(cat)
file=$(printf '%s' "$input" | jq -r '.tool_input.file_path // empty')

case "$file" in
  *.rs) ;;
  *) exit 0 ;;
esac

cd "$CLAUDE_PROJECT_DIR" || exit 0

cargo fmt -- "$file" 2>&1

CARGO_GATE="$HOME/.claude/bin/cargo-gate"

crate=""
map_json=$("$CARGO_GATE" map 2>/dev/null)
if [ -n "$map_json" ]; then
  case "$file" in
    /*) abs_file="$file" ;;
    *) abs_file="$(pwd)/$file" ;;
  esac
  best=""
  best_len=0
  while IFS= read -r line; do
    cname=$(printf '%s' "$line" | cut -d'|' -f1)
    cdir=$(printf '%s' "$line" | cut -d'|' -f2-)
    case "$abs_file" in
      "$cdir"/*)
        dlen=${#cdir}
        if [ "$dlen" -gt "$best_len" ]; then
          best="$cname"
          best_len="$dlen"
        fi
        ;;
    esac
  done <<EOF
$(printf '%s' "$map_json" | jq -r '.crates | to_entries[] | (.key + "|" + .value.dir)')
EOF
  crate="$best"
fi

if [ -n "$crate" ]; then
  out=$("$CARGO_GATE" check -p "$crate" 2>&1) || {
    echo "$out" | tail -30 >&2
    exit 2
  }
else
  out=$("$CARGO_GATE" check 2>&1) || {
    echo "$out" | tail -30 >&2
    exit 2
  }
fi
