#!/bin/sh
#
# Claude Code Stop hook: run clippy + tests for the crates a turn actually
# touched (plus their reverse dependents) before Claude finishes, instead of
# the whole workspace. On failure, block the stop and feed the failure
# output back to Claude so it gets a chance to fix it.
#
# Guards against infinite loops via the stop_hook_active field on stdin —
# Claude gets exactly one forced fix round per CODE STATE, then may stop
# even if still red (CI remains the hard gate; the close also runs
# `cargo-gate test --workspace` once per changeset, see CLAUDE.md).
#
# Scoping: changed files (tracked diff + untracked rs/toml, same plumbing as
# cargo-gate's fingerprint) map to crates via `cargo-gate map`'s path->crate
# dir table (longest-prefix match). The required set is those crates plus
# their workspace-internal dependents (direct edges from the map, walked to
# closure). A changed Cargo.toml/Cargo.lock, or a changed path matching no
# crate dir, widens the required set to every crate — the map itself may be
# stale relative to that change.
#
# Proof skip: `cargo-gate proven <clippy|test> <crate>` reports crates
# already green at the current fingerprint (recorded by ANY unfiltered
# `cargo-gate test|clippy -p X` run — inline, this hook, or elsewhere).
# Crates already proven both clippy and test are dropped before the gate
# runs; if the whole required set drops, this hook exits instantly.
#
# Watchdog: macOS ships no timeout(1), so the gate command group runs in the
# background and a sleeper subshell kills it if it's not done by the
# deadline. A timeout is treated as fail-open (allow the stop) with a
# warning in hook context and NO proof written, so the next stop retries.
#
# One-fix-round-per-code-state: on red, this hook writes a
# claude-stop-check.attempted marker containing the fingerprint that failed,
# instead of a proof (cargo-gate itself only ever proves green runs). If the
# marker matches the current fingerprint on a later invocation, this state
# already got its forced fix round, so exit 0 without re-running anything.
# On green, cargo-gate has just written proofs for these crates, so the
# marker (if any, from a prior red state) is removed.
#

cd "$CLAUDE_PROJECT_DIR" || exit 0

CARGO_GATE="$HOME/.claude/bin/cargo-gate"
WATCHDOG_SECS=240

input=$(cat)
stop_hook_active=$(printf '%s' "$input" | jq -r '.stop_hook_active // false')
if [ "$stop_hook_active" = "true" ]; then
  exit 0
fi

git_dir=$(git rev-parse --git-dir 2>/dev/null)
if [ -z "$git_dir" ]; then
  exit 0 # not a repo — nothing to gate
fi

attempted_file="$git_dir/claude-stop-check.attempted"

# Same fingerprint inputs cargo-gate uses: HEAD + tracked *.rs/*.toml/Cargo.lock
# diff + untracked *.rs/*.toml contents. Kept in sync deliberately — this is
# what "unchanged since the forced fix round" means on both sides.
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

if [ -n "$fingerprint" ] && [ -f "$attempted_file" ] &&
  [ "$(cat "$attempted_file" 2>/dev/null)" = "$fingerprint" ]; then
  exit 0 # this code state already got its forced fix round
fi

# Changed paths: tracked *.rs/*.toml/Cargo.lock diff names + untracked
# *.rs/*.toml paths. Same file classes as the fingerprint, but names only —
# we need paths to map to crate dirs, not contents.
changed_paths=$(
  {
    git diff HEAD --name-only -- '*.rs' '*.toml' 'Cargo.lock' 2>/dev/null
    git ls-files --others --exclude-standard -- '*.rs' '*.toml' 2>/dev/null
  } | sort -u
)

if [ -z "$changed_paths" ]; then
  exit 0 # no Rust-relevant changes at all
fi

manifest_touched=0
printf '%s\n' "$changed_paths" | grep -qE '(^|/)Cargo\.(toml|lock)$' && manifest_touched=1

map_json=""
if [ "$manifest_touched" -eq 0 ]; then
  map_json=$("$CARGO_GATE" map 2>/dev/null)
fi

all_crates=""
required=""

if [ "$manifest_touched" -eq 1 ] || [ -z "$map_json" ]; then
  # Manifest changed (map may be stale), or map generation failed — widen to
  # every crate rather than risk gating on a stale/missing map.
  map_json=$("$CARGO_GATE" map 2>/dev/null)
  if [ -n "$map_json" ]; then
    all_crates=$(printf '%s' "$map_json" | jq -r '.crates | keys[]')
  fi
  required="$all_crates"
else
  all_crates=$(printf '%s' "$map_json" | jq -r '.crates | keys[]')

  # Longest-prefix match of each changed path against crate dirs. Crate dirs
  # are absolute (from `cargo metadata`); resolve changed paths (repo-root
  # relative) the same way before comparing.
  repo_root=$(git rev-parse --show-toplevel 2>/dev/null)
  changed_crates=""
  no_match=0
  while IFS= read -r p; do
    [ -n "$p" ] || continue
    abs_p="$repo_root/$p"
    best=""
    best_len=0
    while IFS= read -r line; do
      cname=$(printf '%s' "$line" | cut -d'|' -f1)
      cdir=$(printf '%s' "$line" | cut -d'|' -f2-)
      case "$abs_p" in
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
    if [ -z "$best" ]; then
      no_match=1
    else
      changed_crates="$changed_crates $best"
    fi
  done <<EOF
$changed_paths
EOF

  if [ "$no_match" -eq 1 ]; then
    required="$all_crates"
  else
    # required = changed crates union their dependents, walked to closure
    # (the map's dependents lists are direct edges only).
    frontier=$(printf '%s' "$changed_crates" | tr ' ' '\n' | sort -u)
    required=$(printf '%s' "$frontier" | tr '\n' ' ')
    changed=1
    while [ "$changed" -eq 1 ]; do
      changed=0
      new_frontier=""
      for c in $frontier; do
        deps=$(printf '%s' "$map_json" | jq -r --arg c "$c" '.crates[$c].dependents[]? // empty')
        for d in $deps; do
          case " $required " in
            *" $d "*) : ;;
            *)
              required="$required $d"
              new_frontier="$new_frontier $d"
              changed=1
              ;;
          esac
        done
      done
      frontier="$new_frontier"
    done
  fi
fi

required=$(printf '%s' "$required" | tr ' ' '\n' | sed '/^$/d' | sort -u | tr '\n' ' ')
required=$(printf '%s' "$required" | sed 's/^ *//; s/ *$//')

if [ -z "$required" ]; then
  exit 0
fi

# Drop crates already proven both clippy and test at this fingerprint.
remaining=""
for c in $required; do
  if "$CARGO_GATE" proven clippy "$c" >/dev/null 2>&1 &&
    "$CARGO_GATE" proven test "$c" >/dev/null 2>&1; then
    : # already proven — drop
  else
    remaining="$remaining $c"
  fi
done
remaining=$(printf '%s' "$remaining" | sed 's/^ *//; s/ *$//')

if [ -z "$remaining" ]; then
  [ -f "$attempted_file" ] && rm -f "$attempted_file"
  exit 0
fi

# shellcheck disable=SC2086 # $remaining is an intentional word-split crate list
pkg_args=""
for c in $remaining; do
  pkg_args="$pkg_args -p $c"
done

gate_log=$(mktemp "${TMPDIR:-/tmp}/claude-stop-check.XXXXXX")

# Watchdog: run the gate in a background process group, poll for completion,
# and kill it if it's not done by WATCHDOG_SECS. macOS has no timeout(1), so
# this is the portable substitute — a sleeper subshell that fires `kill` on
# the gate's pid (and its process group) once the deadline passes.
(
  # shellcheck disable=SC2086 # pkg_args is an intentional word-split arg list
  "$CARGO_GATE" clippy $pkg_args >"$gate_log" 2>&1 &&
    "$CARGO_GATE" test $pkg_args >>"$gate_log" 2>&1
  echo $? >"$gate_log.status"
) &
gate_pid=$!

# Kills a process and its descendants by walking pgrep -P, leaves first.
# NOT process-group kill: a `( ... ) &` backgrounded subshell in
# non-interactive sh shares the INVOKING SHELL's process group (no job
# control means it never gets its own) — `kill -TERM -$pgid` was tried here
# first and took down the whole calling session, not just the gate, because
# of that shared group. Walking the pid tree via ppid is slower but scoped
# to exactly what this hook spawned.
kill_tree() {
  for child in $(pgrep -P "$1" 2>/dev/null); do
    kill_tree "$child"
  done
  kill -TERM "$1" 2>/dev/null
}

(
  sleep "$WATCHDOG_SECS"
  if kill -0 "$gate_pid" 2>/dev/null; then
    kill_tree "$gate_pid"
  fi
) &
watchdog_pid=$!

wait "$gate_pid" 2>/dev/null
kill "$watchdog_pid" 2>/dev/null
wait "$watchdog_pid" 2>/dev/null

if [ ! -f "$gate_log.status" ]; then
  # Watchdog fired before the gate finished — fail open, no proof written.
  rm -f "$gate_log" "$gate_log.status" 2>/dev/null
  jq -n --arg list "$remaining" '{
    hookSpecificOutput: {
      hookEventName: "Stop",
      additionalContext: ("stop gate timed out after 240s — crates " + $list + " unproven; pre-land full suite still gates")
    }
  }'
  exit 0
fi

status=$(cat "$gate_log.status" 2>/dev/null)
output=$(cat "$gate_log" 2>/dev/null)
rm -f "$gate_log" "$gate_log.status" 2>/dev/null

if [ "$status" = "0" ]; then
  # cargo-gate itself just wrote green proofs for these crates.
  [ -f "$attempted_file" ] && rm -f "$attempted_file"
  exit 0
fi

# Red: this state's one forced fix round. Record the attempted marker (not a
# proof — cargo-gate only proves green) so an immediate re-stop on the same
# tree skips straight through instead of re-running.
[ -n "$fingerprint" ] && printf '%s' "$fingerprint" >"$attempted_file"

jq -n --arg ctx "$output" '{
  decision: "block",
  hookSpecificOutput: {
    hookEventName: "Stop",
    additionalContext: ("clippy/test failed before stopping. Fix these, then continue:\n" + $ctx)
  }
}'
exit 0
