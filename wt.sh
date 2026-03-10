#!/usr/bin/env bash
# Usage:
#   ./wt.sh new <feature-name>   — create worktree + branch at .worktrees/<feature-name>
#   ./wt.sh list                 — list all worktrees
#   ./wt.sh rm <feature-name>    — remove worktree and delete its branch
#   ./wt.sh open <feature-name>  — print path (for opening a new terminal/Claude session)

set -e

WORKTREES_DIR=".worktrees"

cmd="${1:-list}"
name="$2"

case "$cmd" in
  new)
    [[ -z "$name" ]] && echo "Usage: ./wt.sh new <feature-name>" && exit 1
    branch="feature/$name"
    path="$WORKTREES_DIR/$name"
    mkdir -p "$WORKTREES_DIR"
    git worktree add -b "$branch" "$path"
    echo ""
    echo "Worktree created:"
    echo "  Path:   $(pwd)/$path"
    echo "  Branch: $branch"
    echo ""
    echo "Open a new terminal there, then run: claude"
    ;;
  list)
    git worktree list
    ;;
  rm)
    [[ -z "$name" ]] && echo "Usage: ./wt.sh rm <feature-name>" && exit 1
    path="$WORKTREES_DIR/$name"
    branch="feature/$name"
    git worktree remove "$path" --force
    git branch -D "$branch" 2>/dev/null && echo "Deleted branch $branch" || echo "Branch $branch not found (may already be merged)"
    ;;
  open)
    [[ -z "$name" ]] && echo "Usage: ./wt.sh open <feature-name>" && exit 1
    echo "$(pwd)/$WORKTREES_DIR/$name"
    ;;
  *)
    echo "Usage: ./wt.sh <new|list|rm|open> [feature-name]"
    exit 1
    ;;
esac
