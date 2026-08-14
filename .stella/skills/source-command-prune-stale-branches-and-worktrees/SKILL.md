---
name: "source-command-prune-stale-branches-and-worktrees"
description: "Prune stale git branches (merged, abandoned, or older than 30 days with no open PR) and stale git worktrees. Shows a plan, confirms, then executes."
---

# source-command-prune-stale-branches-and-worktrees

Use this skill when the user asks to run the migrated source command `prune-stale-branches-and-worktrees`.

## Command Template

# /prune-stale-branches-and-worktrees

Clean up stale git branches (local + remote) and git worktrees. Always show the plan before deleting anything. Never delete `main`, `develop`, or any branch with an open PR.

## Phase 0 — Gather state

```bash
# Sync remote tracking refs
git fetch --all --prune

# List all local branches with last commit date and merge status
git for-each-ref --sort=-committerdate refs/heads/ --format='%(refname:short)|%(committerdate:iso8601)|%(upstream:short)|%(upstream:track)' 2>/dev/null

# List all remote branches (origin)
git for-each-ref --sort=-committerdate refs/remotes/origin/ --format='%(refname:short)|%(committerdate:iso8601)' 2>/dev/null | grep -v 'HEAD'

# List all git worktrees
git worktree list --porcelain 2>/dev/null
```

Pull open PRs from GitHub to protect branches that have an open PR:
- Use `mcp__plugin_github_github__list_pull_requests` with state `open` to get the list. Extract `headRefName` (branch name) for each open PR.

## Phase 1 — Classify branches

For each branch (local and remote), classify as one of:

| Status | Criteria |
|---|---|
| **Protected** | `main`, `develop`, `master`, `release/*`, or has an open PR |
| **Merged** | `git branch --merged main` lists it |
| **Stale** | Last commit > 30 days ago, no open PR, not merged into main |
| **Active** | Last commit ≤ 30 days ago, not merged |

Rules:
- Protected → never touch.
- Merged → safe to delete (already in main).
- Stale → propose deletion; show last commit message and date so the user can override.
- Active → keep; report only (so the user has a full picture).

## Phase 2 — Classify worktrees

For each worktree from `git worktree list --porcelain`:
- **Main worktree** (the repo root) → never touch.
- **Linked worktree with no branch** (detached HEAD) → STALE, propose `git worktree remove --force`.
- **Linked worktree whose branch no longer exists** → STALE, propose remove.
- **Linked worktree whose branch is merged** → STALE, propose remove + branch delete.
- **Linked worktree with active branch** → keep; report path and branch.

## Phase 3 — Show the plan (always — do not skip)

Print the full deletion plan in a clear table before touching anything:

```
BRANCHES TO DELETE
──────────────────────────────────────────────────────────────────────────
Branch                     | Reason    | Last commit        | Local | Remote
feature/old-auth           | merged    | 2026-04-12 Fix ... | yes   | yes
experiment/unused-widget   | stale 45d | 2026-04-20 WIP ... | yes   | no
...

WORKTREES TO REMOVE
──────────────────────────────────────────────────────────────────────────
Path                                | Branch        | Reason
/tmp/Codex-worktrees/feat-x        | feat/x        | branch merged
/tmp/Codex-worktrees/detached-123  | (detached)    | no branch

KEPT (active or protected)
──────────────────────────────────────────────────────────────────────────
main          — protected
feat/current  — active (last commit 2026-06-05)
```

**Ask for confirmation before proceeding.** Say:
> "Ready to delete N branches and remove M worktrees. Reply 'yes' to proceed, or name specific branches to skip."

Wait for the user's reply. If they say yes or provide exclusions, proceed. If no reply within the turn, stop.

## Phase 4 — Execute (only after confirmation)

Delete merged/stale branches:

```bash
# Delete local merged branches (never main/develop/master)
git branch --merged main | grep -vE '^\*|main|develop|master' | xargs -r git branch -d

# Delete stale local branches (force-delete since not merged)
# Only branches explicitly listed in the stale plan
git branch -D <stale_branch_1> <stale_branch_2> ...

# Delete remote branches
git push origin --delete <branch1> <branch2> ...

# Remove stale worktrees
git worktree remove --force <path1>
git worktree remove --force <path2>

# Prune stale remote-tracking refs
git fetch --prune

# Compact the repo
git gc --auto
```

## Phase 5 — Report

Print a final summary:

```
✓ Deleted N local branches
✓ Deleted N remote branches
✓ Removed N worktrees
✓ Kept N branches (active or protected)

Freed approximately X MB (from git gc output).
```

If any deletion fails (e.g. branch not fully merged, worktree has uncommitted changes), report the error with the exact branch/path and the reason — do not force-delete without explicit user approval.
