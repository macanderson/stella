#!/usr/bin/env bash
#
# The session word a claim carries beside its author, shared by
# `scripts/main-red-claim.sh` and `scripts/issue-claim.sh`.
#
# ## Why a claim needs more than a login
#
# One person runs several agent sessions at once, so "did I write this claim?"
# cannot be answered by the login. On 2026-09-02 three sessions all running as
# one author each read their peers' claims as their own, each proceeded, and
# each opened a pull request splitting the same file the same way, eight
# minutes apart. The red-`main` claim grew a session word for that (#5265,
# #5134); the issue claim had the same hole and now reads the same word
# (#5875).
#
# The word is the first of:
#
#   - `STELLA_CLAIM_SESSION`, for a fleet that already has a run id;
#   - a random token minted on first use and kept in this clone's git dir, at
#     `$(git rev-parse --git-dir)/main-red-claim-session`.
#
# `git rev-parse --git-dir` answers per **worktree**, which is what makes the
# token tell three agent worktrees apart while a session that re-checks reads
# back the one it minted. It lives inside the git dir, so it never enters the
# work tree and cannot be committed. Two sessions sharing one worktree still
# read one token and cannot be told apart — that is what the env var is for.
#
# The file name predates the split and is kept: the word identifies the
# session, not the kind of claim, and renaming it would hand every live
# session a new identity mid-run.
#
# ## Fail-open, on both sides of the comparison
#
# `resolve_session` returns non-zero when it has no word — no git dir, a git
# dir it cannot write, or a `STELLA_CLAIM_SESSION` that is not one plain word.
# A caller with no word of its own reads every claim of its own login as its
# own, which is what both scripts did before the word existed. A claim comment
# carrying no word gets the same treatment, so a claim left by an older copy
# cannot block the author who wrote it.
#
# Sourced, never executed: it defines two functions and runs nothing.

# A session word is one plain word, so it survives a whitespace-split parse of
# a comment's first line and cannot smuggle a second column into it.
plain_word() {
  case "$1" in
  '' | *[!A-Za-z0-9._-]*) return 1 ;;
  esac
  return 0
}

# This session's own word, minted once and then read back.
resolve_session() {
  if [ -n "${STELLA_CLAIM_SESSION-}" ]; then
    if plain_word "$STELLA_CLAIM_SESSION"; then
      printf '%s' "$STELLA_CLAIM_SESSION"
      return 0
    fi
    echo "note: STELLA_CLAIM_SESSION is not one plain word, so it cannot be a" >&2
    echo "      session word. Falling back to this clone's own." >&2
  fi

  git_dir="$(git rev-parse --git-dir 2>/dev/null)" || return 1
  [ -n "$git_dir" ] || return 1
  session_file="$git_dir/main-red-claim-session"

  if [ ! -f "$session_file" ]; then
    token="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
    plain_word "$token" || token="$$-$(date +%s 2>/dev/null)"
    plain_word "$token" || return 1
    printf '%s\n' "$token" >"$session_file" 2>/dev/null || return 1
  fi

  # Read back rather than keep what was just minted, so two sessions racing to
  # create the file in one worktree end up agreeing instead of each reading the
  # other's claim as a stranger's.
  token="$(tr -d ' \t\r\n' <"$session_file" 2>/dev/null)" || return 1
  plain_word "$token" || return 1
  printf '%s' "$token"
}
