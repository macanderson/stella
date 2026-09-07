#!/usr/bin/env bash
#
# Has every recent commit on `main` actually been verified? (#5027)
#
#   scripts/check-main-verified.sh [--limit N] [--stuck-minutes M]
#   scripts/check-main-verified.sh --announce   # ...and open/refresh/close an issue
#
# ── The gap this closes ──────────────────────────────────────────────────────
#
# Every existing mechanism here answers "is main KNOWN BROKEN". None answers
# "is main UNVERIFIED", and those are different states:
#
#   main-canary.yml   files only when its job RUNS and FAILS. A
#                     `startup_failure`, a cancellation, or a run that is never
#                     created produces no issue at all.
#   main-red-hold.yml asks the tracker whether a `main-red` issue is open. With
#                     none filed it passes, so merges keep flowing onto a tree
#                     nothing has checked.
#   gh run list       reads GREEN to a human skimming it, because a run that
#                     was never created leaves no row.
#
# On 2026-08-26 between 15:25 and 15:31 UTC, Actions stopped allocating
# runners. Four commits landed on `main` in that window and none got a
# completed `ci` run: one still `queued` with no runner ever assigned, one
# marked `failure` with all three jobs `queued` and zero steps executed, and
# two with no run created at all. `main` sat unverified for about 85 minutes
# and nothing said so. It happened to be fine — which is luck, not a signal.
# The same window would have looked identical if one of those merges had
# broken the tree.
#
# ── What counts as verified ──────────────────────────────────────────────────
#
# A `ci` run for that exact commit that reached a terminal conclusion of its
# own accord: `success`, `failure`, `cancelled`, `timed_out`. A FAILING run is
# verified — the canary and the hold deal with that, and it is not this
# script's business. What is reported is the absence of an answer:
#
#   missing          no `ci` run exists for the commit
#   queued too long  a run past --stuck-minutes with no runner
#   startup_failure  the workflow never began, so no check ran
#
# ── Three states, because a run still going is not a verdict ─────────────────
#
# A run that has not ended is not an answer. Folding one into `verified` let
# this script print "each of the last N commits on main has a completed ci
# run" over commits with no completed run. A monitor must not say that.
#
# The window fills up under plain load. On 2026-09-05 fourteen open pull
# requests fanned out across the workflows here. Every `main` run then sat 24
# to 27 minutes behind a runner. Eight were queued at once, so the eleven
# newest commits had no answer. The tree had been fixed eighteen minutes
# earlier and nothing could see it.
#
#   verified   a terminal conclusion exists for the commit
#   pending    a run exists, has not ended, and is inside the threshold
#   unverified the absences listed above
#
# `pending` exits 0, like every other unknown here, and says what it is. It
# closes no open `main-unverified` issue. A recovery is read off an answer,
# never off the lack of one.
#
# ── It fails OPEN, at every unknown ──────────────────────────────────────────
#
# No `gh`, an unreachable API, an unparseable answer: report and exit 0. This
# is a monitor, and a monitor that can itself block a merge is worse than the
# gap it watches — the same argument `main-red-claim.sh`'s header makes about
# a claim check. Every unknown is loud, never silent.
#
# ── `--announce` reaches a person, on codeql-canary.sh's shape ───────────────
#
# Before this, `main-canary.yml` ran this script and let it print into a
# workflow log and exit 1. The step above it, `main-canary.sh --announce`,
# opens a tracking issue on the same run. This one printed and stopped. Twice
# observed on a `chore(release): sync versions` commit, with `gh issue list
# --label main-red --state open` empty both days: the finding never reached
# anyone.
#
# `--announce` is the same shape `codeql-canary.sh` already uses: one open
# issue found by label, a comment while the condition recurs, closed on the
# next run that answers clean. Its label is its own (`main-unverified`, not
# `main-red`). "Nothing verified this commit" is a different state from "a
# check said no about this commit." The header above already draws that line
# for the console output, and the label carries the same line.
# `main-red-hold.yml` reads only `main-red`, so an unverified-main issue never
# arms it; blocking every open PR on an absence of information would fight the
# fail-open discipline this whole file argues for. AGENTS.md's canary section
# names which of `main-canary.yml`'s two jobs files under which label.

set -uo pipefail

# A decided verdict must survive a reader that closes the pipe early (#1815).
trap '' PIPE

limit=10
stuck_minutes=45
fixture_runs=""
fixture_commits=""
fixture_queue=""
use_fixture=0
announce=0
dry_run=0
label="main-unverified"
fixture_open_issue=""

while [ $# -gt 0 ]; do
  case "$1" in
  --limit)
    limit="${2:-}"
    shift 2
    ;;
  --stuck-minutes)
    stuck_minutes="${2:-}"
    shift 2
    ;;
  --announce)
    announce=1
    shift
    ;;
  --dry-run)
    dry_run=1
    shift
    ;;
  --label)
    [ $# -ge 2 ] || {
      echo "check-main-verified: --label needs a value" >&2
      exit 2
    }
    label="$2"
    shift 2
    ;;
  # Test-only: stand in for the `gh issue list` lookup below.
  # main-canary.sh and codeql-canary.sh use the same seam. The recovery
  # branch only runs when an issue is already open. Without this seam it
  # could never be tested offline.
  --fixture-open-issue)
    [ $# -ge 2 ] || {
      echo "check-main-verified: --fixture-open-issue needs a number" >&2
      exit 2
    }
    fixture_open_issue="$2"
    shift 2
    ;;
  # Test-only, and paired: a fixture that supplied runs but took real commits
  # would compare two different worlds and report nonsense.
  --fixture-runs)
    fixture_runs="${2:-}"
    use_fixture=1
    shift 2
    ;;
  --fixture-commits)
    fixture_commits="${2:-}"
    use_fixture=1
    shift 2
    ;;
  # Test-only: stand in for the repository-wide queue census the pending
  # report reads, so a case can pin the backlog line without an API.
  --fixture-queue)
    [ $# -ge 2 ] || {
      echo "check-main-verified: --fixture-queue needs a number" >&2
      exit 2
    }
    fixture_queue="$2"
    shift 2
    ;;
  -h | --help)
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
    exit 0
    ;;
  *)
    echo "check-main-verified: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

case "$limit" in '' | *[!0-9]*)
  echo "check-main-verified: --limit takes a whole number" >&2
  exit 2
  ;;
esac
case "$stuck_minutes" in '' | *[!0-9]*)
  echo "check-main-verified: --stuck-minutes takes a whole number" >&2
  exit 2
  ;;
esac

if [ "$use_fixture" -eq 1 ] && { [ -z "$fixture_runs" ] && [ -z "$fixture_commits" ]; }; then
  echo "check-main-verified: --fixture-runs and --fixture-commits go together" >&2
  exit 2
fi

# `--dry-run` without `--announce` would be a no-op that still reads as a
# configured monitor — the same rule main-canary.sh and codeql-canary.sh hold.
if [ "$dry_run" -eq 1 ] && [ "$announce" -eq 0 ]; then
  echo "check-main-verified: --dry-run only means something with --announce" >&2
  exit 2
fi

# `unknown` is the only exit this script takes when it cannot answer, so it is
# one function rather than a repeated pair of lines that could drift apart.
unknown() {
  echo "check-main-verified: UNKNOWN — $1"
  echo "  Exiting 0: a monitor that can block a merge is worse than the gap it"
  echo "  watches. This run established nothing; it did not establish green."
  exit 0
}

if [ "$use_fixture" -eq 0 ] && ! command -v gh >/dev/null 2>&1; then
  unknown "gh is not installed, so no run could be looked up"
fi

# ── The two reads ────────────────────────────────────────────────────────────
#
# Commits first: the question is about commits that landed, and a run list with
# no commits to match it against cannot answer anything.

if [ "$use_fixture" -eq 1 ]; then
  commits="$fixture_commits"
elif ! commits="$(git log --format='%H %h %s' -n "$limit" origin/main 2>/dev/null)"; then
  unknown "could not read origin/main — fetch it first"
fi
if [ -z "$commits" ]; then
  unknown "origin/main has no commits to check"
fi

# `<sha> <status> <conclusion> <created_at>` per line, which is the shape the
# fixture supplies too so the two paths cannot diverge.
if [ "$use_fixture" -eq 1 ]; then
  runs="$fixture_runs"
elif ! runs="$(gh run list --workflow ci.yml --branch main --limit 60 \
  --json headSha,status,conclusion,createdAt \
  --jq '.[] | "\(.headSha) \(.status) \(.conclusion // "none") \(.createdAt)"' 2>/dev/null)"; then
  unknown "could not reach the Actions API"
fi

now_epoch="$(date -u +%s 2>/dev/null || echo 0)"

# Seconds since an RFC-3339 timestamp, or 0 when this platform's `date` will
# not parse it — BSD and GNU take different flags, which is what
# `stat-portability` is about. Zero reads as "just created" and so never
# reports stuck: an unparseable timestamp must not become a finding.
age_seconds() {
  local ts="$1" at
  at="$(date -u -j -f '%Y-%m-%dT%H:%M:%SZ' "$ts" +%s 2>/dev/null ||
    date -u -d "$ts" +%s 2>/dev/null || echo 0)"
  [ "$at" -eq 0 ] && echo 0 && return
  echo $((now_epoch - at))
}

unverified=""
still_running=""
count=0

while IFS= read -r commit; do
  [ -z "$commit" ] && continue
  sha="${commit%% *}"
  rest="${commit#* }"
  short="${rest%% *}"
  subject="${rest#* }"
  count=$((count + 1))

  verdict="missing"
  while IFS= read -r run; do
    [ -z "$run" ] && continue
    run_sha="${run%% *}"
    [ "$run_sha" = "$sha" ] || continue
    run_rest="${run#* }"
    status="${run_rest%% *}"
    run_rest="${run_rest#* }"
    conclusion="${run_rest%% *}"
    created="${run_rest##* }"

    case "$conclusion" in
    success | failure | cancelled | timed_out)
      # A FAILING run is a verified commit: the canary and the hold own that
      # state, and reporting it here would be a second voice saying the same
      # thing in different words.
      verdict="verified"
      break
      ;;
    startup_failure)
      verdict="startup_failure — the workflow never began, so no check ran"
      ;;
    *)
      if [ "$status" = "completed" ]; then
        verdict="completed as '$conclusion', which is not a verdict this build recognises"
      else
        age="$(age_seconds "$created")"
        if [ "$age" -gt $((stuck_minutes * 60)) ]; then
          verdict="$status for $((age / 60))m — past the ${stuck_minutes}m threshold, so no runner is coming"
        else
          verdict="pending: $status for $((age / 60))m, inside the ${stuck_minutes}m threshold"
        fi
      fi
      ;;
    esac
  done <<EOF
$runs
EOF

  case "$verdict" in
  verified) ;;
  pending:*)
    still_running="${still_running}  $short  ${verdict#pending: }
      $subject
"
    ;;
  *)
    unverified="${unverified}  $short  $verdict
      $subject
"
    ;;
  esac
done <<EOF
$commits
EOF

# Three states, and only the first one is green. `pending` and `unverified`
# both mean no answer; they differ in whether an answer is still coming.
state=verified
[ -n "$still_running" ] && state=pending
[ -n "$unverified" ] && state=unverified

# How deep is the backlog these pending commits sit in? A commit list alone
# cannot tell "the run is a minute old" from "this repo is a hundred runs
# behind and nothing about `main` gets an answer for half an hour".
queue_depth() {
  if [ -n "$fixture_queue" ]; then
    printf '%s' "$fixture_queue"
    return
  fi
  [ "$use_fixture" -eq 1 ] && return
  command -v gh >/dev/null 2>&1 || return
  gh run list --limit 100 --json status \
    --jq '[.[] | select(.status != "completed")] | length' 2>/dev/null || true
}

# Both reports below print this block, so it is written once.
pending_report() {
  printf 'These commits have a ci run that has not concluded yet:\n\n%s' "$still_running"
  local depth
  depth="$(queue_depth)"
  case "$depth" in
  '' | *[!0-9]*) ;;
  *)
    printf '\n%s of the 100 most recent runs in this repository have not finished.\n' "$depth"
    ;;
  esac
}

# ── Announcing ───────────────────────────────────────────────────────────────
#
# Same shape as main-canary.sh and codeql-canary.sh: one open issue found by
# label, a comment on every run that still cannot answer, closed on the next
# run that can. See the header for why this is its own label rather than
# `main-red`.

gh_run() {
  if [ "$dry_run" -eq 1 ]; then
    printf 'check-main-verified: [dry-run] gh %s\n' "$*" || true
    return 0
  fi
  # Best-effort. The verdict is already decided above. A gh outage while
  # announcing must not turn it into a script failure — the same rule
  # main-canary.sh's `gh_run` follows.
  if ! gh "$@"; then
    printf 'check-main-verified: WARN — "gh %s" failed; the verdict is unaffected\n' "$*" >&2 || true
  fi
}

open_issue=""
if [ -n "$fixture_open_issue" ]; then
  open_issue="$fixture_open_issue"
elif [ "$announce" -eq 1 ] && [ "$dry_run" -eq 0 ] && command -v gh >/dev/null 2>&1; then
  open_issue="$(gh issue list --label "$label" --state open --limit 1 \
    --json number --jq '.[0].number // empty' 2>/dev/null || true)"
fi

if [ "$announce" -eq 1 ] && [ "$state" = "pending" ]; then
  # An open issue stays open. Closing it here would report a recovery this run
  # cannot see. That is the swap this script exists to refuse: no answer, read
  # as green.
  if [ -n "$open_issue" ]; then
    printf 'check-main-verified: still pending — leaving issue %s open\n' "$open_issue" || true
  else
    printf 'check-main-verified: pending, nothing to file — an answer is still coming\n' || true
  fi
elif [ "$announce" -eq 1 ]; then
  if [ "$state" = "verified" ]; then
    if [ -n "$open_issue" ]; then
      body="Every commit checked on this run has a completed \`ci\` run again.
Closing automatically — reopen if you disagree.

**Definition of done**

- [x] Every recent commit on \`main\` has a completed \`ci\` run.

<!-- main-verified -->"
      gh_run issue comment "$open_issue" --body "$body"
      gh_run issue close "$open_issue" \
        --reason completed \
        --comment "Closed by check-main-verified.sh — every recent commit is verified again."
      printf 'check-main-verified: recovered — closed #%s\n' "$open_issue" || true
    else
      printf 'check-main-verified: green, nothing open to close\n' || true
    fi
  else
    body="\`main-canary.yml\`'s \`check-main-verified.sh\` step found commit(s) on
\`main\` that **nothing has verified** — as distinct from a commit a check said
no about, which \`main-canary.sh\`'s own issue already owns.

\`\`\`
${unverified}\`\`\`

This is NOT \"main is red\". A failing run is a verified commit and the canary
owns that. These commits have no answer at all, which every other mechanism
here reads as green: the canary files only when its job runs and fails, the
red-main hold passes when no issue is open, and \`gh run list\` shows no row
for a run that was never created.

### How to fix

Re-dispatch the missing runs and let them finish before merging onto this
tree:

\`\`\`sh
gh workflow run ci.yml --ref main
\`\`\`

An Actions incident is one cause. There the runs start on their own once
capacity returns, and nothing was checking, which is the part this exists to
fix. A push made with the token a workflow run holds is the other cause, and
the release version write-back is one of those: that push raised no event at
all, so no run was ever created and none is coming. Nothing will start on its
own. Ask for it:

\`\`\`sh
./scripts/dispatch-main-verification.sh
\`\`\`

### Definition of done

- [ ] Every recent commit on \`main\` has a completed \`ci\` run.

This script closes the issue itself on its next run that can answer clean, so
a box nobody ticks costs the issue nothing.

<!-- main-verified -->"

    if [ -n "$open_issue" ]; then
      gh_run issue comment "$open_issue" --body "Still unanswered on this run:

\`\`\`
${unverified}\`\`\`"
      printf 'check-main-verified: still unverified — commented on #%s\n' "$open_issue" || true
    else
      # `gh issue create --label` fails outright on a label that does not
      # exist, which would break this at the exact moment it has something to
      # say. `--force` makes this idempotent, so it is a no-op on every run
      # after the first.
      gh_run label create "$label" \
        --color B60205 \
        --description "main carries a commit nothing verified — filed by check-main-verified.sh" \
        --force
      gh_run issue create \
        --title "main carries a commit nothing verified" \
        --label "$label" \
        --body "$body"
      printf 'check-main-verified: opened an issue\n' || true
    fi
  fi
fi

if [ "$state" = "verified" ]; then
  echo "check-main-verified: OK — each of the last $count commit(s) on main has a completed ci run."
  exit 0
fi

if [ "$state" = "pending" ]; then
  echo "check-main-verified: PENDING — main carries commits with no answer yet."
  echo
  pending_report
  cat <<'TXT'

Exiting 0: an answer is still coming, so this is not a finding. It is also not
green. Nothing here has checked these commits, and merging onto them is a bet
on a run that has not reported.

Watch for the answer rather than reading the silence:

  gh run list --workflow ci.yml --branch main --limit 5
TXT
  exit 0
fi

echo "check-main-verified: FAILED — main carries commits nothing verified."
echo
echo "$unverified"
if [ -n "$still_running" ]; then
  pending_report
  echo
fi
cat <<'TXT'
This is NOT "main is red". A failing run is a verified commit and the canary
owns that. These commits have no answer at all, which every other mechanism
here reads as green: the canary files only when its job runs and fails, the
red-main hold passes when no issue is open, and `gh run list` shows no row for
a run that was never created.

Re-dispatch the missing runs and let them finish before merging onto this tree:

  gh workflow run ci.yml --ref main

An Actions incident is one cause. There the runs start on their own once
capacity returns, and nothing was checking, which is the part this exists to
fix. A push made with the token a workflow run holds is the other cause, and
the release version write-back is one of those: that push raised no event at
all, so no run was ever created and none is coming. Nothing will start on its
own. Ask for it:

  ./scripts/dispatch-main-verification.sh
TXT
exit 1
