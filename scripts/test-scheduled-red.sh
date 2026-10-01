#!/usr/bin/env bash
#
# Hermetic tests for `scripts/scheduled-red.sh` and
# `scripts/check-scheduled-red-coverage.sh`.
#
#   ./scripts/test-scheduled-red.sh    (or: make scheduled-red-test)
#
# No network and no real `gh`. The reporter runs with a `PATH` that holds
# `bash` and a stub `gh`, and nothing else. The stub holds the open issues a
# case asks for, as JSON, and logs each call it gets. So a case checks the
# write the reporter sent, and not only what it printed.
#
# That `PATH` has no `jq` in it. Every reporter case below shows the script
# needs none. The stub runs the `--jq` filter the script sends, with the real
# `jq` by its full path. That is what `gh` does with its own copy. So the
# cases test the filter itself, on bodies with tabs and line breaks.
#
# A reporter that fails open looks the same as one that never ran. So each
# negative control checks for the warning it prints.
#
# The coverage check runs twice. It runs on fixtures, to show it can fail.
# It also runs on this tree, so a new scheduled workflow that is not in
# `scheduled-red.yml` turns this suite red.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
REPORTER="$repo_root/scripts/scheduled-red.sh"
COVERAGE="$repo_root/scripts/check-scheduled-red-coverage.sh"

pass=0
fail=0

ok() {
  printf '  \033[32m✓\033[0m %s\n' "$*"
  pass=$((pass + 1))
}
bad() {
  printf '  \033[31m✗\033[0m %s\n' "$*"
  fail=$((fail + 1))
}

for subject in "$REPORTER" "$COVERAGE"; do
  if [ ! -x "$subject" ]; then
    bad "$subject is missing or not executable"
    exit 1
  fi
done

# The stub needs `jq` to run the filter. The reporter does not.
jq_path="$(command -v jq || true)"
if [ -z "$jq_path" ]; then
  bad "jq is not installed, so the stub cannot run the reporter's --jq filter"
  exit 1
fi

sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT

bash_path="$(command -v bash)"

# `bash` and the stub, and nothing else.
mkdir -p "$sandbox/with-gh" "$sandbox/no-gh"
ln -s "$bash_path" "$sandbox/with-gh/bash"
ln -s "$bash_path" "$sandbox/no-gh/bash"

# The stub. `GH_STUB_ISSUES` is the JSON list of open issues. `gh issue list`
# keeps only the fields `--json` names, then runs the `--jq` filter on them.
# So a body reaches the filter only when the script asks for it. A call that
# contains `GH_STUB_FAIL_MATCH` exits 1, the way a tracker outage does.
cat >"$sandbox/with-gh/gh" <<'GH_STUB'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$GH_STUB_CALLS_LOG"
if [ -n "${GH_STUB_FAIL_MATCH:-}" ]; then
  case "$*" in
  *"$GH_STUB_FAIL_MATCH"*)
    echo "gh stub: failing on purpose" >&2
    exit 1
    ;;
  esac
fi
case "$1 $2" in
"issue list")
  fields=""
  filter="."
  prev=""
  for arg in "$@"; do
    case "$prev" in
    --json) fields="$arg" ;;
    --jq) filter="$arg" ;;
    esac
    prev="$arg"
  done
  printf '%s' "${GH_STUB_ISSUES:-[]}" |
    "$GH_STUB_JQ" -c --arg fields "$fields" \
      '[.[] | with_entries(select(.key as $k | ($fields | split(",") | index($k)) != null))]' |
    "$GH_STUB_JQ" -r "$filter"
  ;;
esac
GH_STUB
chmod +x "$sandbox/with-gh/gh"

calls_log="$sandbox/calls.log"
export GH_STUB_CALLS_LOG="$calls_log"
export GH_STUB_JQ="$jq_path"

# One issue as JSON, in the shape `gh issue list --json` gives.
issue() { # issue <number> <title> [body]
  # shellcheck disable=SC2016 # `$number` and the rest are jq variables, not shell ones
  "$jq_path" -cn --argjson number "$1" --arg title "$2" --arg body "${3:-}" \
    '{number: $number, title: $title, body: $body}'
}

# The open issues for one case, as one JSON list.
open_issues() { # open_issues [<issue json>...]
  if [ $# -eq 0 ]; then
    printf '[]'
    return
  fi
  printf '%s\n' "$@" | "$jq_path" -cs .
}

# A body like the one the reporter writes. The tabs and the line breaks are
# there on purpose: the listing must survive them.
marked_body() { # marked_body <workflow>
  # shellcheck disable=SC2016 # the backticks are literal markdown
  printf 'The scheduled run of `%s` failed.\n\n\tcol one\tcol two\n<!-- scheduled-red -->\n<!-- scheduled-red: workflow=%s -->' \
    "$1" "$1"
}

title="P1 T2 S DevOps (CI): The scheduled workflow nightly-bench is failing"
old_title="Scheduled workflow failing: nightly-bench"
marker="<!-- scheduled-red: workflow=nightly-bench -->"

# run_reporter <path dir> <issues json> <fail match> <args...>
# Sets `out`, `rc` and `calls`.
run_reporter() {
  local path_dir="$1" issues="$2" fail_match="$3"
  shift 3
  : >"$calls_log"
  out="$(env -u SCHEDULED_RED_WORKFLOW -u SCHEDULED_RED_CONCLUSION \
    -u SCHEDULED_RED_RUN_URL \
    PATH="$sandbox/$path_dir" GH_STUB_ISSUES="$issues" \
    GH_STUB_FAIL_MATCH="$fail_match" "$REPORTER" "$@" 2>&1)"
  rc=$?
  calls="$(cat "$calls_log")"
}

has() { # has <name> <haystack> <needle>
  case "$2" in
  *"$3"*) ok "$1" ;;
  *) bad "$1 — wanted '$3' in: $2" ;;
  esac
}

lacks() { # lacks <name> <haystack> <needle>
  case "$2" in
  *"$3"*) bad "$1 — did not want '$3' in: $2" ;;
  *) ok "$1" ;;
  esac
}

exits() { # exits <name> <want>
  if [ "$rc" -eq "$2" ]; then
    ok "$1"
  else
    bad "$1 — exit $rc, wanted $2: $out"
  fi
}

red=(--workflow nightly-bench --conclusion failure
  --run-url "https://example.invalid/runs/7")
green=(--workflow nightly-bench --conclusion success
  --run-url "https://example.invalid/runs/8")

# Fixtures used by more than one case.
none="$(open_issues)"
marked="$(open_issues "$(issue 7 "$title" "$(marked_body nightly-bench)")")"
titled="$(open_issues "$(issue 7 "$old_title" "$(printf 'An early body.\n\tIt has a tab.')")")"

echo "scheduled-red — the reporter"

# ── A red run with nothing open opens one issue ─────────────────────────────

run_reporter with-gh "$none" "" "${red[@]}"
exits "a red run exits 0" 0
has "a red run with nothing open creates an issue" "$calls" \
  "issue create --title $title --label SCHEDULED-RED"
has "...whose body carries the marker for its workflow" "$calls" "$marker"
has "...after it makes sure the label exists" "$calls" \
  "label create SCHEDULED-RED"
has "...in the colour and words the label manifest sets" "$calls" \
  "--color D5584D --description A scheduled workflow is failing. Filed by scheduled-red.yml. --force"
has "...and the issue carries the run URL" "$calls" \
  "https://example.invalid/runs/7"
has "...and a Definition of done box" "$calls" \
  "- [ ] The cause of the failing scheduled run is fixed"
has "...and the listing asks for the bodies it reads" "$calls" \
  "--json number,title,body"
lacks "...and it comments on nothing" "$calls" "issue comment"

# An issue for another workflow shares the label. It is not this one.
run_reporter with-gh \
  "$(open_issues "$(issue 7 "P1 T2 S DevOps (CI): The scheduled workflow msrv is failing" "$(marked_body msrv)")")" \
  "" "${red[@]}"
has "another workflow's issue does not count as open" "$calls" "issue create"
lacks "...and gets no comment" "$calls" "issue comment 7"

# The marker must match in full, never as a prefix.
run_reporter with-gh \
  "$(open_issues "$(issue 7 "$title" "$(marked_body nightly-bench-extra)")")" \
  "" "${red[@]}"
has "a marker that only starts with the name does not match" "$calls" \
  "issue create"

# A marker quoted inside a sentence is not the marker line.
run_reporter with-gh \
  "$(open_issues "$(issue 7 "$title" "It reads $marker in the middle.")")" \
  "" "${red[@]}"
has "a marker inside a sentence does not match" "$calls" "issue create"

# The fallback title must match in full too.
run_reporter with-gh "$(open_issues "$(issue 7 "$old_title-extra")")" "" \
  "${red[@]}"
has "a title that only starts with the name does not match" "$calls" \
  "issue create"

# A body line shaped like a listing line must not read as an issue. Only
# the marker leaves the body, so this stays one issue for another workflow.
fake_line="$(printf 'Notes.\n8\t%s\tnightly-bench\n8\t%s' "$old_title" "$title")"
run_reporter with-gh "$(open_issues "$(issue 9 "Another issue" "$fake_line")")" \
  "" "${red[@]}"
has "a body line shaped like a listing line is not read as an issue" "$calls" \
  "issue create"
lacks "...and the issue it names gets no comment" "$calls" "issue comment 8"

# A run that timed out did not finish its work.
run_reporter with-gh "$none" "" --workflow nightly-bench --conclusion timed_out
has "a timed-out run counts as red" "$calls" "issue create"

# ── A second red run comments on the open issue ─────────────────────────────

run_reporter with-gh "$marked" "" "${red[@]}"
exits "a repeat red run exits 0" 0
has "a repeat red run finds the issue by its marker and comments" "$calls" \
  "issue comment 7"
lacks "...and opens no second issue" "$calls" "issue create"

# The triage agent may retitle the issue. The marker still finds it.
run_reporter with-gh \
  "$(open_issues "$(issue 7 "P2 T1 XS DevOps (CI): Bench job times out" "$(marked_body nightly-bench)")")" \
  "" "${red[@]}"
has "a retitled issue is still found by its marker" "$calls" "issue comment 7"
lacks "...and no second issue opens" "$calls" "issue create"

# A body saved with Windows line endings still carries its marker.
run_reporter with-gh \
  "$(open_issues "$(issue 7 "$title" "$(printf 'Red.\r\n%s\r\nMore.' "$marker")")")" \
  "" "${red[@]}"
has "a marker on a line that ends in a carriage return still matches" "$calls" \
  "issue comment 7"

# An issue filed before the marker has only its title.
run_reporter with-gh "$titled" "" "${red[@]}"
has "an issue under the first title is still found" "$calls" "issue comment 7"
lacks "...and no second issue opens" "$calls" "issue create"

# ── A green run closes the open issue ───────────────────────────────────────

run_reporter with-gh "$marked" "" "${green[@]}"
exits "a green run exits 0" 0
has "a green run closes the issue its marker names" "$calls" \
  "issue close 7 --reason completed"
has "...with a comment naming the green run" "$calls" \
  "https://example.invalid/runs/8"
has "...that ticks the Definition of done box" "$calls" \
  "- [x] The cause of the failing scheduled run is fixed"

run_reporter with-gh "$titled" "" "${green[@]}"
has "a green run closes an issue under the first title" "$calls" \
  "issue close 7 --reason completed"

# A green run with nothing open reads the tracker once, and writes nothing.
# The jq filter spans several lines of the call log, so count the lines that
# start a call.
run_reporter with-gh "$none" "" "${green[@]}"
if [ "$(printf '%s\n' "$calls" | grep -c -e '^issue ' -e '^label ')" -eq 1 ]; then
  has "a green run with nothing open only reads the tracker" "$calls" \
    "issue list --label SCHEDULED-RED"
else
  bad "a green run with nothing open made more than one call: $calls"
fi

# ── A run that is neither red nor green changes nothing ─────────────────────

for conclusion in cancelled skipped startup_failure neutral; do
  run_reporter with-gh "$marked" "" \
    --workflow nightly-bench --conclusion "$conclusion"
  if [ "$rc" -eq 0 ] && [ -z "$calls" ]; then
    ok "a $conclusion run exits 0 and never calls gh"
  else
    bad "a $conclusion run exit $rc, calls [$calls]: $out"
  fi
done

# ── The flags can come from the environment ─────────────────────────────────

: >"$calls_log"
out="$(PATH="$sandbox/with-gh" GH_STUB_ISSUES="$none" \
  SCHEDULED_RED_WORKFLOW=nightly-bench SCHEDULED_RED_CONCLUSION=failure \
  SCHEDULED_RED_RUN_URL="https://example.invalid/runs/9" "$REPORTER" 2>&1)"
calls="$(cat "$calls_log")"
has "the environment names the workflow and the run" "$calls" \
  "https://example.invalid/runs/9"

echo
echo "scheduled-red — it fails open, and says so"

# No `gh` at all. The run must not read as a clean report.
run_reporter no-gh "$none" "" "${red[@]}"
exits "a run with no gh exits 0" 0
has "...and prints a warning the runner shows" "$out" "::warning::"
has "...naming gh as the missing tool" "$out" "gh is not on PATH"

# The tracker errors.
run_reporter with-gh "$none" "issue list" "${red[@]}"
exits "a tracker error exits 0" 0
has "...and warns that the tracker could not be read" "$out" \
  "::warning::scheduled-red: the tracker could not be read"
lacks "...and writes nothing" "$calls" "issue create"

# Two issues for one workflow, one found by its marker and one by the first
# title. Which to keep is a person's call.
two="$(open_issues "$(issue 7 "$title" "$(marked_body nightly-bench)")" \
  "$(issue 8 "$old_title" "No marker here.")")"
run_reporter with-gh "$two" "" "${red[@]}"
exits "two matching issues exit 0" 0
has "...and the warning names both" "$out" "#7 #8"
lacks "...and no issue gets a comment" "$calls" "issue comment"
lacks "...and no issue is created" "$calls" "issue create"
run_reporter with-gh "$two" "" "${green[@]}"
lacks "...and a green run closes neither" "$calls" "issue close"

# Two issues under the first title are the same question.
two_titled="$(open_issues "$(issue 7 "$old_title")" "$(issue 8 "$old_title")")"
run_reporter with-gh "$two_titled" "" "${red[@]}"
has "two issues under the first title are named too" "$out" "#7 #8"
lacks "...and neither gets a comment" "$calls" "issue comment"

# A write that fails still exits 0, and says it failed.
run_reporter with-gh "$none" "issue create" "${red[@]}"
exits "a failed write exits 0" 0
has "...and warns about the write" "$out" "::warning::scheduled-red: \"gh issue create\" failed"
lacks "...and does not claim it opened an issue" "$out" "Opened an issue"

echo
echo "scheduled-red — a caller mistake is exit 2"

run_reporter with-gh "" "" --conclusion failure
exits "a missing --workflow exits 2" 2
run_reporter with-gh "" "" --workflow nightly-bench
exits "a missing --conclusion exits 2" 2
run_reporter with-gh "" "" --workflow
exits "a flag with no value exits 2" 2
run_reporter with-gh "" "" --nonsense
exits "an unknown flag exits 2" 2

echo
echo "scheduled-red — the list of scheduled workflows"

# coverage <name> <want exit> <want text> <fixture dir>
coverage() {
  local name="$1" want="$2" want_text="$3" dir="$4"
  local cov_out cov_rc
  cov_out="$("$COVERAGE" "$dir" 2>&1)"
  cov_rc=$?
  if [ "$cov_rc" -ne "$want" ]; then
    bad "$name — exit $cov_rc, wanted $want: $cov_out"
    return
  fi
  has "$name" "$cov_out" "$want_text"
}

# A fixture tree. Each workflow is one small file.
fixture() {
  local dir="$sandbox/$1"
  rm -rf "$dir"
  mkdir -p "$dir"
  cat >"$dir/nightly.yml" <<'YML'
name: "Nightly thing"
on:
  schedule:
    - cron: "5 4 * * *"
  workflow_dispatch:
YML
  cat >"$dir/push.yml" <<'YML'
name: push-only
on:
  push:
    branches: [main]
YML
  # A schedule in a comment is not a schedule.
  cat >"$dir/commented.yml" <<'YML'
name: commented
on:
  #   schedule:
  #     - cron: "*/15 * * * *"
  workflow_dispatch:
YML
  # Nor is a deeper key that happens to be called schedule.
  cat >"$dir/deep.yml" <<'YML'
name: deep
on:
  workflow_dispatch:
    inputs:
      schedule:
        description: "not a trigger"
YML
  cat >"$dir/scheduled-red.yml" <<'YML'
name: scheduled-red
on:
  workflow_run:
    workflows:
      - Nightly thing # the quotes on its name: do not count
    types: [completed]
YML
  printf '%s' "$dir"
}

dir="$(fixture matched)"
coverage "a list naming each scheduled workflow passes" 0 "OK" "$dir"

dir="$(fixture missing)"
cat >"$dir/second.yml" <<'YML'
name: second
on:
  schedule:
    - cron: "1 1 * * 1"
YML
coverage "a scheduled workflow missing from the list fails" 1 \
  "'second' is missing" "$dir"

dir="$(fixture stale)"
cat >"$dir/scheduled-red.yml" <<'YML'
name: scheduled-red
on:
  workflow_run:
    workflows:
      - Nightly thing
      - gone
    types: [completed]
YML
coverage "a listed name with no scheduled workflow fails" 1 \
  "listens to 'gone'" "$dir"

dir="$(fixture unnamed)"
cat >"$dir/unnamed.yml" <<'YML'
on:
  schedule:
    - cron: "1 1 * * 1"
YML
coverage "a scheduled workflow with no name fails" 1 \
  "has no top-level name" "$dir"

dir="$(fixture flow)"
cat >"$dir/scheduled-red.yml" <<'YML'
name: scheduled-red
on:
  workflow_run:
    workflows: [Nightly thing]
YML
coverage "a flow list is refused" 1 "flow list" "$dir"

dir="$(fixture noreporter)"
rm "$dir/scheduled-red.yml"
coverage "a tree with no reporter fails" 1 "does not exist" "$dir"

dir="$(fixture nothing)"
rm "$dir/nightly.yml"
coverage "a tree with no scheduled workflow fails" 1 "watches nothing" "$dir"

# This tree. A new scheduled workflow must join the list.
coverage "this tree's scheduled-red.yml names every scheduled workflow" 0 \
  "OK" "$repo_root/.github/workflows"

# Only a scheduled run is reported. A push or a manual run has a person
# watching it already.
workflow="$(cat "$repo_root/.github/workflows/scheduled-red.yml")"
has "the reporter job runs for scheduled runs only" "$workflow" \
  "if: github.event.workflow_run.event == 'schedule'"
has "...and runs the reporter" "$workflow" "run: ./scripts/scheduled-red.sh"

echo
if [ "$fail" -eq 0 ]; then
  printf '\033[32mscheduled-red-test: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mscheduled-red-test: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
