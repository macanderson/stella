#!/usr/bin/env bash
#
# Did the last completed `ci.yml` run on `main` fail its test suite?
#
# ── The gap this closes ───────────────────────────────────────────────────
#
# main-canary.sh asks whether the merged tree still COMPOSES. Does it lock?
# Does it fit its size ceilings? Does it compile? Does it read clean on
# prose? None of those rows run the test suite. `ci.yml` already runs the
# suite on every push. It reports pass or fail and stops there. It files
# nothing. It blocks nothing. `main` failed the same test three times in two
# days. The `main-red` chain never fired, because nothing ever asked `ci.yml`
# what it found.
#
# This script asks that, cheaply. It does not run the suite again. It reads
# the CONCLUSION of the run `ci.yml` already made, for the newest commit that
# has a finished one. `check-main-verified.sh` already reads this same run
# history, for a different question.
#
# ── What this script does NOT ask ───────────────────────────────────────────
#
# Whether every recent commit HAS a finished run at all. That is
# `check-main-verified.sh`'s question. A missing or still-queued run is an
# absent answer, not a failing one. Treating them the same would fire this
# script on an Actions outage, not on a broken suite. A `cancelled`, a
# `timed_out`, or a `startup_failure` run is not "the suite failed" either.
# It is "nothing ran to conclusion". `check-main-verified.sh` owns all three.
#
# Only a `failure` conclusion is red. A `success`, a run still queued, or a
# conclusion this script does not know, all read as "nothing to report".
# Every monitor in this chain fails open the same way: it must never be what
# blocks a repair.
#
# ── Why a row in main-canary.sh, not a fourth workflow step ─────────────────
#
# main-canary.sh already owns the one open `main-red` issue. It opens it, it
# comments on it, and it closes it on recovery, all by one label. Adding this
# answer as another row there means a green suite closes the SAME issue a red
# suite opened. It reuses that code, so no second actor races it to open or
# close the issue. This script only says ok or FAIL, the same way
# `lockfile-sync`, `file-size`, `compile`, and `prose` already do.
#
# Usage:
#   scripts/check-ci-tests.sh [--limit N]
#   scripts/check-ci-tests.sh --fixture-commits C --fixture-runs R   # tests
#
# `CHECK_CI_TESTS_FIXTURE_COMMITS` and `CHECK_CI_TESTS_FIXTURE_RUNS` are the
# same two fixtures as environment variables. A flag wins over its matching
# variable. Either may be empty on purpose: no commits, no finished runs.
# main-canary.sh's `checks` array runs each row through `eval` on one string.
# That string cannot safely carry a fixture's newlines and spaces — a commit
# subject, several commits at once. Exporting the fixture, even empty, is how
# main-canary.sh hands it to this row without building a string `eval` would
# break. Its own `--manifest-dir` flag, hermetic for every other row, reaches
# this one the same way.
set -uo pipefail

# A decided verdict must survive a reader that closes the pipe early.
# main-canary.sh and check-main-verified.sh use this same trap.
trap '' PIPE

limit=10
fixture_commits=""
fixture_runs=""
# "Given" means a fixture was SUPPLIED, not that its value is non-empty. An
# empty fixture is a real case: no commits to check yet. It must reach the
# plain "unknown" path below, not the error for a half-supplied pair.
# `${VAR+set}` reads as `set` for a variable exported empty. That lets
# main-canary.sh's `ci-tests` row hand this script an empty pair on purpose.
commits_given=0
runs_given=0
if [ "${CHECK_CI_TESTS_FIXTURE_COMMITS+set}" = set ]; then
  fixture_commits="$CHECK_CI_TESTS_FIXTURE_COMMITS"
  commits_given=1
fi
if [ "${CHECK_CI_TESTS_FIXTURE_RUNS+set}" = set ]; then
  fixture_runs="$CHECK_CI_TESTS_FIXTURE_RUNS"
  runs_given=1
fi

while [ $# -gt 0 ]; do
  case "$1" in
  --limit)
    limit="${2:-}"
    shift 2
    ;;
  --fixture-commits)
    fixture_commits="${2:-}"
    commits_given=1
    shift 2
    ;;
  --fixture-runs)
    fixture_runs="${2:-}"
    runs_given=1
    shift 2
    ;;
  -h | --help)
    # The whole comment block after the shebang, bounded by its first
    # non-comment line — hardcoded line numbers truncate silently the first
    # time the header grows.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "check-ci-tests: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

case "$limit" in '' | *[!0-9]*)
  echo "check-ci-tests: --limit takes a whole number" >&2
  exit 2
  ;;
esac

use_fixture=0
[ "$commits_given" -eq 1 ] && use_fixture=1
[ "$runs_given" -eq 1 ] && use_fixture=1

if [ "$use_fixture" -eq 1 ] && { [ "$commits_given" -eq 0 ] || [ "$runs_given" -eq 0 ]; }; then
  echo "check-ci-tests: fixture commits and runs must be supplied together" >&2
  exit 2
fi

# A missing answer is not this script's business. Report it and exit 0. Do
# not block a repair on a gap check-main-verified.sh already owns.
unknown() {
  echo "check-ci-tests: nothing conclusive — $1"
  exit 0
}

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  unknown "gh is not installed, so no run could be looked up"
fi

if [ "$use_fixture" -eq 1 ]; then
  commits="$fixture_commits"
elif ! commits="$(git log --format='%H %h %s' -n "$limit" origin/main 2>/dev/null)"; then
  unknown "could not read origin/main — fetch it first"
fi
if [ -z "$commits" ]; then
  unknown "origin/main has no commits to check"
fi

# Each line is `<sha> <status> <conclusion>`. No `createdAt`, unlike
# check-main-verified.sh's fixture. That script needs a run's age to call a
# queue "stuck". This one only ever asks for a finished answer. There is
# nothing to age.
if [ "$use_fixture" -eq 1 ]; then
  runs="$fixture_runs"
elif ! runs="$(gh run list --workflow ci.yml --branch main --limit 60 \
  --json headSha,status,conclusion \
  --jq '.[] | "\(.headSha) \(.status) \(.conclusion // "none")"' 2>/dev/null)"; then
  unknown "could not reach the Actions API"
fi

# Walk from the tip backward. The first commit with a COMPLETED run holds
# the newest real answer ci.yml has given. A newer commit whose run is still
# queued says nothing yet, so skip it. Do not stop the walk on it. This
# reports the last VERIFIED state, not just the newest commit's open
# question.
verdict=""
match_short=""
match_subject=""

while IFS= read -r commit; do
  [ -z "$commit" ] && continue
  sha="${commit%% *}"
  rest="${commit#* }"
  short="${rest%% *}"
  subject="${rest#* }"

  this_verdict=""
  while IFS= read -r run; do
    [ -z "$run" ] && continue
    run_sha="${run%% *}"
    [ "$run_sha" = "$sha" ] || continue
    run_rest="${run#* }"
    status="${run_rest%% *}"
    conclusion="${run_rest#* }"
    if [ "$status" = "completed" ]; then
      this_verdict="$conclusion"
      break
    fi
  done <<EOF
$runs
EOF

  if [ -n "$this_verdict" ]; then
    verdict="$this_verdict"
    match_short="$short"
    match_subject="$subject"
    break
  fi
done <<EOF
$commits
EOF

if [ -z "$verdict" ]; then
  unknown "none of the last $limit commit(s) on main has a completed ci run yet"
fi

case "$verdict" in
failure)
  echo "check-ci-tests: FAIL — ci.yml's suite failed at \`${match_short}\` (${match_subject})."
  echo "  Reproduce with the failing test named in that run's log, e.g.:"
  echo "    cargo test -p <crate> <filter>"
  exit 1
  ;;
success)
  echo "check-ci-tests: ok — ci.yml's suite last passed, at \`${match_short}\`."
  exit 0
  ;;
cancelled | timed_out | startup_failure)
  echo "check-ci-tests: ok — the most recent completed run, at \`${match_short}\`," \
    "concluded '${verdict}': not a definite suite failure (see check-main-verified.sh)."
  exit 0
  ;;
*)
  echo "check-ci-tests: ok — the most recent completed run, at \`${match_short}\`," \
    "concluded '${verdict}', which this build does not treat as a definite suite failure."
  exit 0
  ;;
esac
