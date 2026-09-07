#!/usr/bin/env bash
#
# Wait, bounded, for a PR that has armed GitHub's native auto-merge to
# actually land.
#
#   scripts/wait-for-armed-merge.sh <branch> [--timeout-seconds N] [--poll-seconds N]
#
# ── Why this exists. ─────────────────────────────────────────────────────
#
# `auto-tag.yml` has a last resort. When the plain merge and the admin
# merge of the `bot/version-sync` PR both fail, it arms auto-merge and ends
# the job. GitHub merges the PR later, on its own. It still uses the run's
# own token. A push made with that token starts no new workflow run. The
# merge paths above hit the same wall, and already ask for the missing run
# by hand. So the commit that lands this way gets no `ci` run and no
# `main-canary` run. Only the daily canary schedule ever checks it.
#
# `scripts/dispatch-main-verification.sh` fixes "nothing asks." But it
# reads the CURRENT tip of main. Right after auto-merge is armed, that tip
# is still the old one, from before the sync PR — already checked. Calling
# the dispatcher right then asks about the wrong commit.
#
# This script is the wait in between. It polls the armed PR until GitHub
# merges it, or someone closes it. Then the caller can run the dispatcher
# on the new tip. It knows nothing about `ci` runs or dispatching — one
# job, the wait — so tests can cover it alone.
#
# ── What it prints. ──────────────────────────────────────────────────────
#
# Always exit 0. This runs at the end of a release and must never fail it.
# One line of:
#
#   MERGED   — the PR merged while this script waited.
#   CLOSED   — the PR was closed without merging.
#   TIMEOUT  — neither happened before the deadline. Only the daily canary
#              schedule checks the merged commit now, unless someone reruns
#              dispatch-main-verification.sh by hand.
#   UNKNOWN — <reason> — `gh` is missing, or the PR's state could not be
#              read even once. A caller treats this like TIMEOUT: it is
#              not yet safe to ask about main's tip.
#
# Only a missing or bad `<branch>` argument, or a bad flag value, exits
# non-zero. That is a mistake by the caller, not a fact about the PR.
#
# bash 3.2 compatible.

set -uo pipefail

branch="${1:-}"
if [ -z "$branch" ] || [ "${branch#-}" != "$branch" ]; then
  echo "wait-for-armed-merge: usage: wait-for-armed-merge.sh <branch> [--timeout-seconds N] [--poll-seconds N]" >&2
  exit 2
fi
shift

timeout_seconds=2400
poll_seconds=30

while [ $# -gt 0 ]; do
  case "$1" in
  --timeout-seconds)
    timeout_seconds="${2:-}"
    shift 2
    ;;
  # Zero polls once and gives up, which is what the tests want.
  --poll-seconds)
    poll_seconds="${2:-}"
    shift 2
    ;;
  -h | --help)
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "wait-for-armed-merge: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done
case "$timeout_seconds" in '' | *[!0-9]*)
  echo "wait-for-armed-merge: --timeout-seconds takes a whole number" >&2
  exit 2
  ;;
esac
case "$poll_seconds" in '' | *[!0-9]*)
  echo "wait-for-armed-merge: --poll-seconds takes a whole number" >&2
  exit 2
  ;;
esac

if ! command -v gh >/dev/null 2>&1; then
  echo "wait-for-armed-merge: UNKNOWN — gh is not installed, so ${branch}'s state could not be read"
  exit 0
fi

echo "wait-for-armed-merge: waiting up to ${timeout_seconds}s for ${branch} to merge or close."

deadline=$(( $(date +%s) + timeout_seconds ))
saw_any=false
state=""
while :; do
  answer="$(gh pr view "$branch" --json state --jq '.state' 2>/dev/null || true)"
  if [ -n "$answer" ]; then
    saw_any=true
    state="$answer"
  fi
  case "$state" in
  MERGED)
    echo "wait-for-armed-merge: MERGED — ${branch} merged while this waited."
    exit 0
    ;;
  CLOSED)
    echo "wait-for-armed-merge: CLOSED — ${branch} was closed without merging."
    exit 0
    ;;
  esac
  now="$(date +%s)"
  if [ "$poll_seconds" -le 0 ] || [ "$now" -ge "$deadline" ]; then
    break
  fi
  sleep "$poll_seconds"
done

if [ "$saw_any" = false ]; then
  echo "wait-for-armed-merge: UNKNOWN — could not read ${branch}'s state even once"
  exit 0
fi

echo "wait-for-armed-merge: TIMEOUT — ${branch} has not merged or closed yet; it stays armed."
exit 0
