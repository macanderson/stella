#!/usr/bin/env bash
#
# Tests for `scripts/dispatch-main-verification.sh`.
#
#   `./scripts/test-dispatch-main-verification.sh`
#   `make dispatch-main-verification-test`
#
# No network and no real `gh`. Each case puts a fake one first on `PATH`.
# Each case then reads the log of what the script asked it to do.
#
# The log is the test. A script that prints the right words and starts no run
# would pass a test that read only its output.
#
# The first case is the one to keep. A script that starts a run every time it
# is called would pass all the rest. It would also start a run per release
# for a commit that had one already.
#
# Not a `make gate` step, like the other guard suites here. The script under
# test talks to the Actions API, and the gate stays offline.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/dispatch-main-verification.sh"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

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

SHA=aaaaaaaabbbbbbbbccccccccddddddddeeeeeeee

# One fake `gh` per case, in its own folder. A case reads only the answers it
# set up.
#
# The `counts` file holds one line per run query, in the shape
# `<count> <url>`. Its last line repeats for later queries. The `sha` file
# holds what a commit lookup returns. A `dispatch-fails` or `api-fails` file
# turns that call into a failure.
new_case() { # new_case <name> <counts>
  local dir="$work/$1"
  mkdir -p "$dir"
  printf '%s\n' "$2" >"$dir/counts"
  printf '%s\n' "$SHA" >"$dir/sha"
  cat >"$dir/gh" <<'SHIM'
#!/usr/bin/env bash
set -u
dir="$GH_SHIM_DIR"
printf '%s\n' "$*" >>"$dir/calls.log"
case "${1:-}" in
api)
  case "${2:-}" in
  *runs\?head_sha=*)
    [ -f "$dir/api-fails" ] && exit 1
    # A `counts-<workflow>` file answers for that workflow alone, with its own
    # cursor; without one every workflow reads the shared `counts` list, which
    # is what the cases that do not care about the split want.
    wf="${2#*/actions/workflows/}"
    wf="${wf%%/runs*}"
    counts="$dir/counts-$wf"
    cursor="$dir/cursor-$wf"
    if [ ! -f "$counts" ]; then
      counts="$dir/counts"
      cursor="$dir/cursor"
    fi
    n=0
    [ -f "$cursor" ] && n="$(cat "$cursor")"
    n=$((n + 1))
    printf '%s' "$n" >"$cursor"
    line="$(sed -n "${n}p" "$counts")"
    [ -z "$line" ] && line="$(tail -n 1 "$counts")"
    printf '%s\n' "$line"
    exit 0
    ;;
  */commits/*)
    cat "$dir/sha"
    exit 0
    ;;
  esac
  echo "fake gh: unhandled api call: $*" >&2
  exit 3
  ;;
workflow)
  [ -f "$dir/dispatch-fails" ] && exit 1
  exit 0
  ;;
repo)
  echo "macanderson/stella"
  exit 0
  ;;
esac
echo "fake gh: unhandled call: $*" >&2
exit 3
SHIM
  chmod +x "$dir/gh"
  echo "$dir"
}

# The script under test, with that case's fake `gh` and nothing else changed.
run_case() { # run_case <dir> [args...]
  local dir="$1"
  shift
  PATH="$dir:$PATH" GH_SHIM_DIR="$dir" GITHUB_REPOSITORY="" GITHUB_ACTIONS="" \
    "$SCRIPT" --poll-seconds 0 "$@" 2>&1
}

said() { # said <name> <text> <output>
  case "$3" in
  *"$2"*) ok "$1" ;;
  *) bad "$1 — wanted '$2', got: $3" ;;
  esac
}

started_count() { # started_count <dir> <workflow file>
  grep -c "^workflow run $2" "$1/calls.log" 2>/dev/null || true
}

printf '\033[1mdispatch-main-verification — the release commit gets its run\033[0m\n'

# ── A commit that has a run is left alone. ───────────────────────────────────
dir="$(new_case quiet '1 https://example.test/runs/1')"
out="$(run_case "$dir")"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a commit with a run exits 0"
else
  bad "a commit with a run exited $rc: $out"
fi
said "and says the run is already there" "already has 1 ci run" "$out"
if [ "$(started_count "$dir" ci.yml)" -eq 0 ] &&
  [ "$(started_count "$dir" main-canary.yml)" -eq 0 ]; then
  ok "and starts nothing"
else
  bad "it started a run for a commit that already had one:
$(cat "$dir/calls.log")"
fi

# ── A commit with no run gets both runs. ─────────────────────────────────────
dir="$(new_case dispatch '0 -
1 https://example.test/runs/7')"
out="$(run_case "$dir")"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a commit with no run exits 0"
else
  bad "a commit with no run exited $rc: $out"
fi
if [ "$(started_count "$dir" ci.yml)" -eq 1 ] &&
  [ "$(started_count "$dir" main-canary.yml)" -eq 1 ]; then
  ok "and starts ci and the canary, once each"
else
  bad "it did not start both runs:
$(cat "$dir/calls.log")"
fi
first="$(grep '^workflow run' "$dir/calls.log" | head -n 1)"
case "$first" in
"workflow run ci.yml"*) ok "and starts ci first, so the canary can see it" ;;
*) bad "the canary went first: $first" ;;
esac
said "and prints the link to the run it started" "https://example.test/runs/7" "$out"
case "$(grep '^workflow run ci.yml' "$dir/calls.log")" in
*"--ref main"*) ok "and it asks for the run on main" ;;
*) bad "the start did not name the ref: $(grep '^workflow run' "$dir/calls.log")" ;;
esac

# ── The run that never shows up. ─────────────────────────────────────────────
# A merge landing in between takes the new run. This commit then keeps none.
# The script has to say so, not claim a win.
dir="$(new_case slow '0 -')"
out="$(run_case "$dir")"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a run that never appears still exits 0"
else
  bad "a missing run exited $rc: $out"
fi
said "and says the commit may have kept no run" "no ci.yml run has landed" "$out"
if [ "$(started_count "$dir" main-canary.yml)" -eq 1 ]; then
  ok "and the canary is started anyway, so something asks the question"
else
  bad "the canary was not started:
$(cat "$dir/calls.log")"
fi

# ── The canary run that went to a later commit. ──────────────────────────────
# The observed shape on 2026-09-05: the `ci` run landed on the commit, `main`
# moved, and the canary run carried the newer `head_sha`. The closing line
# claimed the commit was being checked and named neither half.
dir="$(new_case canary_missed '0 -')"
printf '0 -\n1 https://example.test/runs/7\n' >"$dir/counts-ci.yml"
printf '0 -\n' >"$dir/counts-main-canary.yml"
out="$(run_case "$dir")"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a canary run that never lands still exits 0"
else
  bad "a missing canary run exited $rc: $out"
fi
said "and the ci run is still named" "https://example.test/runs/7" "$out"
said "and the canary is named as missing" "no main-canary.yml run has landed" "$out"
case "$out" in
*"is being checked"*) bad "it still claims the commit is checked: $out" ;;
*) ok "and it does not claim the commit is checked outright" ;;
esac

# ── Both runs land, and both are named. ──────────────────────────────────────
dir="$(new_case both_landed '0 -')"
printf '0 -\n1 https://example.test/runs/7\n' >"$dir/counts-ci.yml"
printf '1 https://example.test/runs/8\n' >"$dir/counts-main-canary.yml"
out="$(run_case "$dir")"
said "both runs landed: the ci run is named" "https://example.test/runs/7" "$out"
said "and so is the canary run" "main-canary: https://example.test/runs/8" "$out"

# ── Every unknown exits 0. ───────────────────────────────────────────────────
# This step runs at the end of a release. A step that can red the release is
# worse than the gap it watches.
dir="$(new_case refused '0 -')"
touch "$dir/dispatch-fails"
out="$(run_case "$dir")"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "a refused start exits 0"
else
  bad "a refused start exited $rc: $out"
fi
said "and says nothing was started" "UNKNOWN" "$out"

dir="$(new_case apidown '0 -')"
touch "$dir/api-fails"
out="$(run_case "$dir")"
rc=$?
if [ "$rc" -eq 0 ] && [ -z "${out##*UNKNOWN*}" ]; then
  ok "an unreachable API is UNKNOWN, not a failure"
else
  bad "an unreachable API exited $rc: $out"
fi

# An empty folder, not an empty `PATH`. Some C libraries read an empty one as
# the default search path. `bash` runs by full path here, because the script's
# own first line would look for it on the emptied `PATH`.
nogh="$work/nogh"
mkdir -p "$nogh"
bash_bin="$(command -v bash)"
out="$(PATH="$nogh" GITHUB_REPOSITORY="" "$bash_bin" "$SCRIPT" --poll-seconds 0 2>&1)"
rc=$?
if [ "$rc" -eq 0 ] && [ -z "${out##*UNKNOWN*}" ]; then
  ok "no gh at all is UNKNOWN, not a failure"
else
  bad "a missing gh exited $rc: $out"
fi

# ── A caller mistake is loud. ────────────────────────────────────────────────
# Exit 2, never a quiet 0 that reads like a run was started.
out="$("$SCRIPT" --nonsense 2>&1)"
if [ $? -eq 2 ]; then
  ok "an unknown flag exits 2"
else
  bad "an unknown flag did not exit 2: $out"
fi

out="$("$SCRIPT" --poll-seconds banana 2>&1)"
if [ $? -eq 2 ]; then
  ok "a flag given a word instead of a number exits 2"
else
  bad "a bad number did not exit 2: $out"
fi

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf '\033[32mdispatch-main-verification-test: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mdispatch-main-verification-test: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
