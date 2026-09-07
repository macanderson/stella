#!/usr/bin/env bash
#
# Re-ask the merged-lock question for every open pull request (`#5943`).
#
# `ci.yml` asks whether `Cargo.lock` still resolves once the pull request and
# `main` are merged. That answer is true of the `main` of the moment it ran.
# `strict` is off, so nothing makes a branch catch up. The green then stands
# after `main` moves under it.
#
# That is how `main` broke on 2026-09-06. A release sync bumped every version
# and wrote a fresh lock at 09:51. A branch adding the `stella-learn` crate
# merged at 09:59. Its lock entry was at the old version, and its check was
# from before the bump. Every `--locked` build failed. Three sessions each
# wrote the same repair (`#5939`).
#
# This is the missing event. It runs when `main` moves. It asks the question
# again for each open pull request. It runs `ci` again on the ones whose
# answer changed. That new run lands under the same name on the same commit.
# So the required check goes red and the merge waits.
# `refresh-main-red-holds.sh` uses the same shape for a stale hold.
#
# ## What bounds the fan-out
#
# A pull request that writes no `Cargo.lock` cannot add a stale entry to it.
# It is skipped without asking. Of the rest, only one whose merged lock fails
# to resolve is run again. A quiet day costs one `git fetch` per open pull
# request and nothing else.
#
# ## Fails open, out loud
#
# Every unknown prints a note and exits 0. A red job here would call a pull
# request broken when nobody asked that. Its own checks are the one thing
# allowed to answer.
#
# Usage.
#
#   `scripts/recheck-lock-compositions.sh`
#   `scripts/recheck-lock-compositions.sh --dry-run`
#   `scripts/recheck-lock-compositions.sh --limit 50`
#
# Needs `gh`, `git` and a plain shell. So it runs on a bare CI runner.
set -uo pipefail

workflow="ci.yml"
limit=100
dry_run=0
repo_dir=""
fixture_open_prs=""
fixture_runs=""
use_fixture=0

while [ $# -gt 0 ]; do
  case "$1" in
  --dry-run)
    dry_run=1
    shift
    ;;
  --limit)
    [ $# -ge 2 ] || {
      echo "recheck-lock-compositions: --limit needs a number" >&2
      exit 2
    }
    limit="$2"
    shift 2
    ;;
  --repo-dir)
    [ $# -ge 2 ] || {
      echo "recheck-lock-compositions: --repo-dir needs a directory" >&2
      exit 2
    }
    repo_dir="$2"
    shift 2
    ;;
  # Test-only seams. Either one stubs every lookup and holds back the re-run.
  # So a case cannot half-reach the network. Same rule as the fixtures in
  # `dod-recheck.sh`.
  --fixture-open-prs)
    [ $# -ge 2 ] || {
      echo "recheck-lock-compositions: --fixture-open-prs needs a value" >&2
      exit 2
    }
    fixture_open_prs="$2"
    use_fixture=1
    shift 2
    ;;
  --fixture-runs)
    [ $# -ge 2 ] || {
      echo "recheck-lock-compositions: --fixture-runs needs a value" >&2
      exit 2
    }
    fixture_runs="$2"
    use_fixture=1
    shift 2
    ;;
  -h | --help)
    # The whole block after the shebang, cut off at its first line of code. A
    # line number here goes stale the first time the header grows.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "recheck-lock-compositions: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

[ "$use_fixture" -eq 1 ] && dry_run=1

script_dir="$(cd "$(dirname "$0")" && pwd)"
guard="$script_dir/check-merge-lockfile-sync.sh"
[ -n "$repo_dir" ] || repo_dir="$(cd "$script_dir/.." && pwd)"

note() { printf 'recheck-lock-compositions: %s\n' "$*" >&2 || true; }
say() {
  # Ignore SIGPIPE and drop the write. A reader that shuts the pipe must not
  # turn this into a failure. That is the `#1815` shape.
  trap '' PIPE
  printf 'recheck-lock-compositions: %s\n' "$*" || true
}

if [ ! -d "$repo_dir" ]; then
  note "no such directory: $repo_dir, so nothing was asked."
  exit 0
fi

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  note "gh is not installed, so no pull request could be read."
  exit 0
fi

# ── The open pull requests, one per line: number, then head commit ───────────
if [ "$use_fixture" -eq 1 ]; then
  open_prs="$fixture_open_prs"
elif ! open_prs="$(gh pr list --state open --limit "$limit" \
  --json number,headRefOid \
  --jq '.[] | "\(.number) \(.headRefOid)"' 2>/dev/null)"; then
  note "could not list the open pull requests, so nothing was asked."
  exit 0
fi

if [ -z "$open_prs" ]; then
  say "no open pull requests."
  exit 0
fi

# The last `ci` run on this head. Three answers, not two. "Nothing to run
# again" and "the lookup did not answer" are different facts. One empty
# string for both reads a dead lookup as a pull request with no run.
run_for() {
  head="$1"
  if [ "$use_fixture" -eq 1 ]; then
    printf '%s\n' "$fixture_runs" |
      awk -v head="$head" '$1 == head { print $2; hit = 1; exit }
                           END { if (!hit) print "none" }'
    return 0
  fi
  if ! answer="$(gh api \
    "repos/{owner}/{repo}/actions/workflows/$workflow/runs?head_sha=$head&per_page=20" \
    --jq 'if (.workflow_runs | length) == 0 then "none"
          elif .workflow_runs[0].status == "completed" then (.workflow_runs[0].id | tostring)
          else "pending" end' 2>/dev/null)"; then
    printf '?\n'
    return 0
  fi
  case "$answer" in
  "") printf '?\n' ;;
  *) printf '%s\n' "$answer" ;;
  esac
}

asked=0
rerun=0

while IFS=' ' read -r number head; do
  [ -n "${number:-}" ] || continue
  [ -n "${head:-}" ] || continue

  if [ "$use_fixture" -eq 0 ]; then
    if ! git -C "$repo_dir" fetch --no-tags --quiet origin "refs/pull/$number/head" 2>/dev/null; then
      note "#$number: could not fetch its head, so it was not asked."
      continue
    fi
  fi
  if ! git -C "$repo_dir" rev-parse --verify --quiet "${head}^{commit}" >/dev/null; then
    note "#$number: $head is not a commit here, so it was not asked."
    continue
  fi

  # A pull request that writes no lock cannot add a stale entry to one.
  if ! base="$(git -C "$repo_dir" merge-base HEAD "$head" 2>/dev/null)" || [ -z "$base" ]; then
    note "#$number: shares no history with this checkout, so it was not asked."
    continue
  fi
  files="$(git -C "$repo_dir" diff --name-only "$base" "$head" 2>/dev/null || true)"
  case $'\n'"$files"$'\n' in
  *$'\n'Cargo.lock$'\n'*) ;;
  *) continue ;;
  esac

  asked=$((asked + 1))
  verdict=0
  "$guard" --repo-dir "$repo_dir" --no-fetch "$head" >/dev/null 2>&1 || verdict=$?

  if [ "$verdict" -eq 0 ]; then
    say "#$number: its lock still resolves against this tip."
    continue
  fi
  if [ "$verdict" -ne 1 ]; then
    note "#$number: the guard could not answer, so nothing was run again."
    continue
  fi

  run_id="$(run_for "$head")"
  case "$run_id" in
  none)
    say "#$number: its lock is stale against this tip, and it has no ci run to run again."
    continue
    ;;
  pending)
    say "#$number: its lock is stale against this tip, and its ci run is still going."
    continue
    ;;
  "?")
    note "#$number: could not read its ci runs, so nothing was run again."
    continue
    ;;
  esac

  if [ "$dry_run" -eq 1 ]; then
    say "#$number: would run ci run $run_id again — its lock is stale against this tip."
    rerun=$((rerun + 1))
    continue
  fi
  if gh run rerun "$run_id" >/dev/null 2>&1; then
    say "#$number: ran ci run $run_id again — its lock is stale against this tip."
    rerun=$((rerun + 1))
  else
    note "#$number: could not run ci run $run_id again."
  fi
done <<EOF
$open_prs
EOF

say "asked $asked pull request(s) that write the lock; $rerun need ci again."
exit 0
