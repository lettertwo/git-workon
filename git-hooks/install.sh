#!/bin/sh
#
# Install git hooks from git-hooks/ into .git/hooks/
#
# Handles worktrees correctly by using git rev-parse --git-dir.
# If a global core.hooksPath is configured, sets a local override so
# both local and global hooks run (commit-msg chains to global).
#
# Usage: ./git-hooks/install.sh
#   or:  make install-hooks
#

set -e

GIT_DIR=$(git rev-parse --git-dir)
HOOKS_DIR="$GIT_DIR/hooks"

mkdir -p "$HOOKS_DIR"

cp git-hooks/commit-msg "$HOOKS_DIR/commit-msg"
chmod +x "$HOOKS_DIR/commit-msg"

cp git-hooks/pre-push "$HOOKS_DIR/pre-push"
chmod +x "$HOOKS_DIR/pre-push"

# If global core.hooksPath is set, set a local override so our hooks run.
# The commit-msg hook chains to the global hook automatically.
GLOBAL_HOOKS_PATH=$(git config --global --get core.hooksPath 2>/dev/null || echo "")
if [ -n "$GLOBAL_HOOKS_PATH" ]; then
  git config --local core.hooksPath "$HOOKS_DIR"
  echo "Note: global core.hooksPath detected; set local override to $HOOKS_DIR"
fi

echo "✓ Installed hooks to $HOOKS_DIR"
