#!/usr/bin/env bash
#
# Hermetic tests for `scripts/scheduled-red.sh` and
# `scripts/check-scheduled-red-coverage.sh`.
#
#   ./scripts/test-scheduled-red.sh    (or: make scheduled-red-test)
#
# No network and no real `gh`. The reporter runs with a `PATH` that holds
# `bash` and a stub `gh`, and nothing else. The stub prints the open issues a
# case asks for, and logs each call it gets. So a case checks the write the
# reporter sent, and not only what it printed.
#
# That `PATH` has no `jq` in it. Every reporter case below shows the script
# needs none.
#
# The negative controls matter most. A reporter that fails open looks the
# same as one that never ran, unless a case shows the warning it prints.
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

sandbox="$(mktemp -d)"
trap 'rm -rf "$sandbox"' EXIT

bash_path="$(command -v bash)"

# `bash` and the stub, and nothing else.
mkdir -p "$sandbox/with-gh" "$sandbox/no-gh"
ln -s "$bash_path" "$sandbox/with-gh/bash"
ln -s "$bash_path" "$sandbox/no-gh/bash"

# The stub. `GH_STUB_LISTING` is what `gh issue list` prints. A call that
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
"issue list") printf '%s' "${GH_STUB_LISTING:-}" ;;
esac
exit 0
GH_STUB
chmod +x "$sandbox/with-gh/gh"

calls_log="$sandbox/calls.log"
export GH_STUB_CALLS_LOG="$calls_log"

# One `<number><TAB><title>` line per issue, as `gh --jq` prints them.
issue_line() {
  printf '%s\t%s\n' "$1" "$2"
}

title="Scheduled workflow failing: nightly-bench"

# run_reporter <path dir> <listing> <fail match> <args...>
# Sets `out`, `rc` and `calls`.
run_reporter() {
  local path_dir="$1" listing="$2" fail_match="$3"
  shift 3
  : >"$calls_log"
  out="$(env -u SCHEDULED_RED_WORKFLOW -u SCHEDULED_RED_CONCLUSION \
    -u SCHEDULED_RED_RUN_URL \
    PATH="$sandbox/$path_dir" GH_STUB_LISTING="$listing" \
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

echo "scheduled-red — the reporter"

# ── A red run with nothing open opens one issue ─────────────────────────────

run_reporter with-gh "" "" "${red[@]}"
exits "a red run exits 0" 0
has "a red run with nothing open creates an issue" "$calls" \
  "issue create --title $title --label scheduled-red"
has "...after it makes sure the label exists" "$calls" \
  "label create scheduled-red"
has "...and the issue carries the run URL" "$calls" \
  "https://example.invalid/runs/7"
has "...and a Definition of done box" "$calls" \
  "- [ ] The cause of the failing scheduled run is fixed"
lacks "...and it comments on nothing" "$calls" "issue comment"

# An issue for another workflow shares the label. It is not this one.
run_reporter with-gh "$(issue_line 7 "Scheduled workflow failing: msrv")" "" \
  "${red[@]}"
has "another workflow's issue does not count as open" "$calls" "issue create"
lacks "...and gets no comment" "$calls" "issue comment 7"

# The title must match in full, never as a prefix.
run_reporter with-gh "$(issue_line 7 "$title-extra")" "" "${red[@]}"
has "a title that only starts with the name does not match" "$calls" \
  "issue create"

# A run that timed out did not finish its work.
run_reporter with-gh "" "" --workflow nightly-bench --conclusion timed_out
has "a timed-out run counts as red" "$calls" "issue create"

# ── A second red run comments on the open issue ─────────────────────────────

run_reporter with-gh "$(issue_line 7 "$title")" "" "${red[@]}"
exits "a repeat red run exits 0" 0
has "a repeat red run comments on the open issue" "$calls" "issue comment 7"
lacks "...and opens no second issue" "$calls" "issue create"

# ── A green run closes the open issue ───────────────────────────────────────

run_reporter with-gh "$(issue_line 7 "$title")" "" "${green[@]}"
exits "a green run exits 0" 0
has "a green run closes the open issue" "$calls" \
  "issue close 7 --reason completed"
has "...with a comment naming the green run" "$calls" \
  "https://example.invalid/runs/8"
has "...that ticks the Definition of done box" "$calls" \
  "- [x] The cause of the failing scheduled run is fixed"

# A green run with nothing open reads the tracker once, and writes nothing.
run_reporter with-gh "" "" "${green[@]}"
if [ "$(printf '%s\n' "$calls" | grep -c .)" -eq 1 ]; then
  has "a green run with nothing open only reads the tracker" "$calls" \
    "issue list --label scheduled-red"
else
  bad "a green run with nothing open made more than one call: $calls"
fi

# ── A run that is neither red nor green changes nothing ─────────────────────

for conclusion in cancelled skipped startup_failure neutral; do
  run_reporter with-gh "$(issue_line 7 "$title")" "" \
    --workflow nightly-bench --conclusion "$conclusion"
  if [ "$rc" -eq 0 ] && [ -z "$calls" ]; then
    ok "a $conclusion run exits 0 and never calls gh"
  else
    bad "a $conclusion run exit $rc, calls [$calls]: $out"
  fi
done

# ── The flags can come from the environment ─────────────────────────────────

: >"$calls_log"
out="$(PATH="$sandbox/with-gh" GH_STUB_LISTING="" \
  SCHEDULED_RED_WORKFLOW=nightly-bench SCHEDULED_RED_CONCLUSION=failure \
  SCHEDULED_RED_RUN_URL="https://example.invalid/runs/9" "$REPORTER" 2>&1)"
calls="$(cat "$calls_log")"
has "the environment names the workflow and the run" "$calls" \
  "https://example.invalid/runs/9"

echo
echo "scheduled-red — it fails open, and says so"

# No `gh` at all. The run must not read as a clean report.
run_reporter no-gh "" "" "${red[@]}"
exits "a run with no gh exits 0" 0
has "...and prints a warning the runner shows" "$out" "::warning::"
has "...naming gh as the missing tool" "$out" "gh is not on PATH"

# The tracker errors.
run_reporter with-gh "" "issue list" "${red[@]}"
exits "a tracker error exits 0" 0
has "...and warns that the tracker could not be read" "$out" \
  "::warning::scheduled-red: the tracker could not be read"
lacks "...and writes nothing" "$calls" "issue create"

# Two issues for one workflow. Which to keep is a person's call.
two="$(issue_line 7 "$title")
$(issue_line 8 "$title")"
run_reporter with-gh "$two" "" "${red[@]}"
exits "two matching issues exit 0" 0
has "...and the warning names both" "$out" "#7 #8"
lacks "...and no issue gets a comment" "$calls" "issue comment"
lacks "...and no issue is created" "$calls" "issue create"
run_reporter with-gh "$two" "" "${green[@]}"
lacks "...and a green run closes neither" "$calls" "issue close"

# A write that fails still exits 0, and says it failed.
run_reporter with-gh "" "issue create" "${red[@]}"
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
