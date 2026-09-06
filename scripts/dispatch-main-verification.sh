#!/usr/bin/env bash
#
# Ask for the check that main's newest commit never got.
#
#   scripts/dispatch-main-verification.sh [--ref main] [--sha SHA]
#                                         [--timeout-seconds N] [--poll-seconds N]
#
# ── The gap this closes ──────────────────────────────────────────────────────
#
# `auto-tag.yml` merges the version write-back PR itself, with the token
# GitHub hands to a workflow run. A push made with that token raises no event.
# So `ci.yml` and `main-canary.yml` never start, and every release leaves a
# `chore(release): sync versions` commit on `main` that nothing has checked.
#
# `check-main-verified.sh` asks whether such a commit is there. This script is
# the other half: it asks for the run that is missing.
#
# ── What it does ─────────────────────────────────────────────────────────────
#
# It reads the tip of `main` and counts the `ci` runs for that exact commit.
# One or more and it stops, because the commit has an answer or is getting
# one. None, and it starts `ci.yml`, waits for that run to show up, then
# starts `main-canary.yml` and waits for that one.
#
# The canary goes second on purpose. Its last step is
# `check-main-verified.sh`, which would name this same commit while the `ci`
# run was still missing.
#
# ── It claims only the runs it saw land ──────────────────────────────────────
#
# `gh workflow run` starts a run on a BRANCH. That run judges whatever the tip
# is when it lands, not the commit this script was asked about. Starting a
# workflow is not evidence that the commit got one.
#
# On 2026-09-05 one closing line named the `ci` run and spoke for both. It said
# `67a43001` "is being checked". The canary run had gone to `9455eebe`, one
# merge later. So both workflows are polled here, and each is reported alone.
#
# The two waits share one deadline. Asking the second question costs no more
# wall clock than asking the first.
#
# The run it starts cannot come back here. `auto-tag.yml` acts only on a `ci`
# run whose event was a push, and a run started this way carries the event
# `workflow_dispatch`.
#
# ── It fails open, at every unknown ──────────────────────────────────────────
#
# No `gh`, an API it cannot reach, a refused start: say so and exit 0. This
# runs at the end of a release, and it must never be the thing that turns that
# job red. Only a bad argument exits non-zero, because that is a mistake in
# the caller rather than a fact about the world.
#
# bash 3.2 compatible.

set -uo pipefail

# A decided verdict must survive a reader that closes the pipe early.
trap '' PIPE

ref=main
sha=""
timeout_seconds=180
poll_seconds=5

while [ $# -gt 0 ]; do
  case "$1" in
  --ref)
    ref="${2:-}"
    shift 2
    ;;
  --sha)
    sha="${2:-}"
    shift 2
    ;;
  --timeout-seconds)
    timeout_seconds="${2:-}"
    shift 2
    ;;
  # Zero asks once and gives up, which is what the tests want and what a
  # caller with no time to wait wants.
  --poll-seconds)
    poll_seconds="${2:-}"
    shift 2
    ;;
  -h | --help)
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "dispatch-main-verification: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

if [ -z "$ref" ]; then
  echo "dispatch-main-verification: --ref takes a branch name" >&2
  exit 2
fi
case "$timeout_seconds" in '' | *[!0-9]*)
  echo "dispatch-main-verification: --timeout-seconds takes a whole number" >&2
  exit 2
  ;;
esac
case "$poll_seconds" in '' | *[!0-9]*)
  echo "dispatch-main-verification: --poll-seconds takes a whole number" >&2
  exit 2
  ;;
esac

unknown() {
  echo "dispatch-main-verification: UNKNOWN — $1"
  echo "  Exiting 0: this runs at the end of a release, and a step that can red"
  echo "  the release is worse than the gap it watches. Nothing was started."
  exit 0
}

# Loud in the job summary when there is a summary to be loud in.
warn() {
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::warning::dispatch-main-verification: $1"
  else
    echo "dispatch-main-verification: $1" >&2
  fi
}

if ! command -v gh >/dev/null 2>&1; then
  unknown "gh is not installed, so no run could be started"
fi

repo="${GITHUB_REPOSITORY:-}"
if [ -z "$repo" ]; then
  repo="$(gh repo view --json nameWithOwner --jq '.nameWithOwner' 2>/dev/null || true)"
fi
if [ -z "$repo" ]; then
  unknown "could not tell which repository this is"
fi

# `<count> <url>` for one workflow's runs on one commit. The URL is `-` when
# there are none, so both fields are always there to read.
runs_for() { # runs_for <workflow file> <sha>
  gh api "repos/${repo}/actions/workflows/$1/runs?head_sha=$2&per_page=1" \
    --jq '"\(.total_count) \(.workflow_runs[0].html_url // "-")"' 2>/dev/null
}

# The count, or nothing at all when the answer was not a number.
count_of() { # count_of <answer>
  local n="${1%% *}"
  case "$n" in '' | *[!0-9]*) echo "" ;; *) echo "$n" ;; esac
}

if [ -z "$sha" ]; then
  sha="$(gh api "repos/${repo}/commits/${ref}" --jq '.sha' 2>/dev/null || true)"
fi
if [ -z "$sha" ]; then
  unknown "could not read the tip of ${ref}"
fi
short="$(printf '%.8s' "$sha")"

if ! answer="$(runs_for ci.yml "$sha")"; then
  unknown "could not reach the Actions API"
fi
count="$(count_of "$answer")"
if [ -z "$count" ]; then
  unknown "the Actions API answered '${answer}', which this script cannot read"
fi

if [ "$count" -gt 0 ]; then
  echo "dispatch-main-verification: OK — ${short} already has ${count} ci run(s)."
  echo "  ${answer#* }"
  exit 0
fi

echo "dispatch-main-verification: ${short} on ${ref} has no ci run — asking for one."

start() { # start <workflow file>
  if gh workflow run "$1" --repo "${repo}" --ref "${ref}" >/dev/null 2>&1; then
    echo "  started $1 on ${ref}"
    return 0
  fi
  warn "could not start $1 on ${ref}; ${short} stays unchecked until the daily canary"
  return 1
}

# One deadline for both waits, so polling the canary as well as `ci` costs no
# more wall clock than polling `ci` alone did. The answer comes back in
# `wait_url` rather than on stdout: a command substitution would run the loop
# in a subshell and lose what it spent from the budget.
remaining="$timeout_seconds"
wait_url="-"
wait_for() { # wait_for <workflow file>
  local answer found
  wait_url="-"
  while :; do
    answer="$(runs_for "$1" "$sha" || true)"
    found="$(count_of "$answer")"
    if [ -n "$found" ] && [ "$found" -gt 0 ]; then
      wait_url="${answer#* }"
      return 0
    fi
    if [ "$poll_seconds" -le 0 ] || [ "$remaining" -le 0 ]; then
      return 1
    fi
    sleep "$poll_seconds"
    remaining=$((remaining - poll_seconds))
  done
}

ci_url="-"
ci_started=0
if start ci.yml; then
  ci_started=1
  wait_for ci.yml || true
  ci_url="$wait_url"
fi

canary_url="-"
if start main-canary.yml; then
  wait_for main-canary.yml || true
  canary_url="$wait_url"
fi

# A run that never attached is reported per workflow. The two mean different
# things. `ci` is the answer being asked for. The canary is a backstop, and the
# daily schedule asks it again.
missing() { # missing <workflow file> <what it means>
  warn "no $1 run has landed on ${short}"
  cat <<TXT
  A started run judges whatever the tip of ${ref} is when it lands, so a merge
  that arrived in between takes the run and ${short} keeps none. $2

    gh run list --workflow $1 --branch ${ref}
TXT
}

if [ "$ci_url" != "-" ]; then
  echo "dispatch-main-verification: ${short} has a ci run."
  echo "  ${ci_url}"
  if [ "$canary_url" != "-" ]; then
    echo "  main-canary: ${canary_url}"
  else
    missing main-canary.yml "The daily canary asks again; nothing else does."
  fi
  exit 0
fi

if [ "$ci_started" -eq 1 ]; then
  missing ci.yml "Look for it, and start another if it went to a later commit:"
  exit 0
fi

unknown "no ci run could be started for ${short}"
