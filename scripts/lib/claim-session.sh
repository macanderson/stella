#!/usr/bin/env bash
#
# The session word a claim carries beside its author.
#
# `scripts/main-red-claim.sh` and `scripts/issue-claim.sh` both read it.
#
# ## Why a login is not enough
#
# One person runs many agent sessions at once. A login cannot answer "did I
# write this claim?".
#
# On 2026-09-02 three sessions ran as one author. Each read the other claims as
# its own. Each went ahead. Each opened a pull request that split the same file
# the same way, eight minutes apart.
#
# The red-`main` claim grew a session word for that (`#5265`, `#5134`). The
# issue claim had the same hole (`#5875`). Both read this word now.
#
# ## Where the word comes from
#
# `STELLA_CLAIM_SESSION` first, for a fleet that has a run id.
#
# Failing that, a random token. It is minted on first use and kept in this
# clone's git dir, at `$(git rev-parse --git-dir)/main-red-claim-session`.
#
# `git rev-parse --git-dir` answers per worktree. That is what tells three
# agent worktrees apart. A session that re-checks reads back its own token.
#
# The file sits inside the git dir, so it never enters the work tree and cannot
# be committed. Two sessions in one worktree share one token and look alike.
# The env var is what tells those apart.
#
# The file name is older than this split, and it is kept. The word names the
# session, not the kind of claim. A rename would hand every live session a new
# identity mid-run.
#
# ## Fail open on both sides
#
# `resolve_session` returns non-zero when it has no word. That happens with no
# git dir, a git dir it cannot write, or a `STELLA_CLAIM_SESSION` that is not
# one plain word.
#
# A caller with no word reads every claim of its own login as its own. That is
# what both scripts did before the word existed. A claim comment with no word
# gets the same treatment, so a claim from an older copy cannot block the
# author who wrote it.
#
# Sourced, never run. It defines two functions and does nothing else.

# A session word is one plain word. That way it survives a whitespace split of
# a comment's first line, and cannot smuggle a second column into it.
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

  # Read back what is on disk, rather than keep what was just minted. Two
  # sessions racing to create the file in one worktree then agree, instead of
  # each reading the other's claim as a stranger's.
  token="$(tr -d ' \t\r\n' <"$session_file" 2>/dev/null)" || return 1
  plain_word "$token" || return 1
  printf '%s' "$token"
}
