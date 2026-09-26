#!/usr/bin/env bash
#
# Tests for `scripts/version-writeback-defer.sh`.
#
#   `./scripts/test-version-writeback-defer.sh`
#   `make version-writeback-defer-test`
#
# No network and no real `gh`. Each case puts a fake `gh` first on `PATH`.
# It is the same shape `test-wait-for-armed-merge.sh` uses. It logs every
# call it gets, and answers from small fixture files the case writes first.
#
# `auto-tag.yml` has four warning paths that can leave the version
# write-back unmerged. These cases prove each one calls `open` with its
# own case number. They also prove `close` fires only when a caller says
# the merge landed, never just because one case's own check cleared.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/version-writeback-defer.sh"

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

# One fake `gh` per case, in its own folder. `$dir/open-issue`, when present,
# is what `gh issue list` answers — empty or absent means no issue is open
# yet. `$dir/calls.log` is the call trace every assertion below reads.
new_case() { # new_case <name> [open-issue-number]
  local dir="$work/$1"
  mkdir -p "$dir"
  [ -n "${2:-}" ] && printf '%s\n' "$2" >"$dir/open-issue"
  cat >"$dir/gh" <<'SHIM'
#!/usr/bin/env bash
set -u
dir="$GH_SHIM_DIR"
printf '%s\n' "$*" >>"$dir/calls.log"
case "${1:-}" in
issue)
  case "${2:-}" in
  list)
    [ -f "$dir/list-fails" ] && exit 1
    cat "$dir/open-issue" 2>/dev/null || true
    exit 0
    ;;
  comment)
    [ -f "$dir/comment-fails" ] && exit 1
    exit 0
    ;;
  close)
    [ -f "$dir/close-fails" ] && exit 1
    exit 0
    ;;
  create)
    [ -f "$dir/create-fails" ] && exit 1
    echo "https://github.com/macanderson/stella/issues/99"
    exit 0
    ;;
  esac
  ;;
label)
  if [ "${2:-}" = "create" ]; then
    [ -f "$dir/label-fails" ] && exit 1
    exit 0
  fi
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
  PATH="$dir:$PATH" GH_SHIM_DIR="$dir" "$SCRIPT" "$@" 2>&1
}

calls() { # calls <dir>
  cat "$1/calls.log" 2>/dev/null || true
}

contains() { # contains <name> <haystack> <needle>
  case "$2" in
  *"$3"*) ok "$1" ;;
  *) bad "$1 — wanted '$3', got: $2" ;;
  esac
}

lacks() { # lacks <name> <haystack> <needle>
  case "$2" in
  *"$3"*) bad "$1 — did not want '$3', got: $2" ;;
  *) ok "$1" ;;
  esac
}

printf '\033[1mversion-writeback-defer — one issue, named by whichever case fired\033[0m\n'

# ── open: no issue exists yet, one case per branch ─────────────────────────
for case_num in 2 3 4 5; do
  dir="$(new_case "open-fresh-$case_num")"
  out="$(run_case "$dir" open --case "$case_num" --branch bot/version-sync \
    --version 0.9.500 --sha abc1234)"
  rc=$?
  if [ "$rc" -eq 0 ]; then
    ok "case $case_num: open exits 0"
  else
    bad "case $case_num: open exited $rc: $out"
  fi
  contains "case $case_num: it ensures the label first" "$(calls "$dir")" \
    "label create version-writeback-deferred"
  contains "case $case_num: it opens a new issue" "$(calls "$dir")" "issue create"
  contains "case $case_num: the issue names the branch" "$(calls "$dir")" "bot/version-sync"
  contains "case $case_num: the issue names the version" "$(calls "$dir")" "0.9.500"
  contains "case $case_num: the issue names the commit" "$(calls "$dir")" "abc1234"
  contains "case $case_num: the issue names which case fired" "$(calls "$dir")" \
    "Case ${case_num}:"
done

# Each case's own wording reaches the issue body, not a generic placeholder
# shared by all four, so a reader can tell them apart.
dir="$(new_case case2-wording)"
out="$(run_case "$dir" open --case 2 --branch bot/version-sync --version 0.9.500 --sha abc1234)"
contains "case 2 names a red or unreported check" "$(calls "$dir")" \
  "went red or never reported"

dir="$(new_case case3-wording)"
run_case "$dir" open --case 3 --branch bot/version-sync --version 0.9.500 --sha abc1234 >/dev/null
contains "case 3 names the 45-minute timeout" "$(calls "$dir")" "did not finish within 45 minutes"

dir="$(new_case case4-wording)"
run_case "$dir" open --case 4 --branch bot/version-sync --version 0.9.500 --sha abc1234 >/dev/null
contains "case 4 names the unresolvable lockfile" "$(calls "$dir")" "Cargo.lock\` unresolvable"

dir="$(new_case case5-wording)"
run_case "$dir" open --case 5 --branch bot/version-sync --version 0.9.500 --sha abc1234 >/dev/null
contains "case 5 names the armed, unmerged auto-merge" "$(calls "$dir")" "auto-merge is armed instead"

# ── open: an issue is already open — comment, don't file a second one ──────
dir="$(new_case open-existing 42)"
out="$(run_case "$dir" open --case 5 --branch bot/version-sync --version 0.9.500 --sha def5678)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "an existing issue: open still exits 0"
else
  bad "an existing issue: open exited $rc: $out"
fi
contains "it comments on the existing issue" "$(calls "$dir")" "issue comment 42"
contains "the comment names which case fired again" "$(calls "$dir")" "case 5:"
lacks "and does not open a second issue" "$(calls "$dir")" "issue create"
lacks "and does not re-create the label" "$(calls "$dir")" "label create"

# `--fixture-open-issue` skips the `gh issue list` lookup entirely, the same
# seam main-canary.sh's own flag of the same name provides.
dir="$(new_case fixture-skips-list)"
run_case "$dir" open --case 2 --branch bot/version-sync --version 0.9.500 --sha abc1234 \
  --fixture-open-issue 7 >/dev/null
lacks "a fixture issue number skips the list lookup" "$(calls "$dir")" "issue list"
contains "and still comments on that fixture issue" "$(calls "$dir")" "issue comment 7"

# ── close: the confirmed-merge path, and only that path ────────────────────
dir="$(new_case close-existing 42)"
out="$(run_case "$dir" close --branch bot/version-sync --sha def5678)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "close on an open issue exits 0"
else
  bad "close on an open issue exited $rc: $out"
fi
contains "it comments that the write-back merged" "$(calls "$dir")" "merged at \`def5678\`"
contains "it closes the issue" "$(calls "$dir")" "issue close 42"

# Nothing closes an issue merely because a case's own blocking condition
# cleared. `close` takes no `--case` and no `--version` at all, so a caller
# cannot pass "the lockfile check passed" where "the merge landed" is meant
# — the two are different calls into this script.
dir="$(new_case close-needs-no-case)"
out="$(run_case "$dir" close --branch bot/version-sync --sha def5678 --case 4 2>&1)"
rc=$?
if [ "$rc" -eq 2 ]; then
  ok "close rejects a --case argument as unknown"
else
  bad "close --case did not exit 2: rc=$rc out=$out"
fi

# ── close: no issue is open — nothing to do, and it says so ────────────────
dir="$(new_case close-nothing-open)"
out="$(run_case "$dir" close --branch bot/version-sync --sha def5678)"
rc=$?
if [ "$rc" -eq 0 ]; then
  ok "close with nothing open still exits 0"
else
  bad "close with nothing open exited $rc: $out"
fi
contains "and it says there is nothing to do" "$out" "nothing to do"
lacks "and it never comments on anything" "$(calls "$dir")" "issue comment"
lacks "and it never closes anything" "$(calls "$dir")" "issue close"

# ── argument validation ──────────────────────────────────────────────────
expect_exit2() { # expect_exit2 <name> <arg...>
  local name="$1" out rc
  shift
  out="$("$SCRIPT" "$@" 2>&1)"
  rc=$?
  if [ "$rc" -eq 2 ]; then
    ok "$name"
  else
    bad "$name — wanted exit 2, got $rc: $out"
  fi
}

expect_exit2 "no mode at all exits 2"
expect_exit2 "an unknown mode exits 2" bogus
expect_exit2 "open with no --case exits 2" \
  open --branch bot/version-sync --version 0.9.500 --sha abc1234
expect_exit2 "open with an out-of-range --case exits 2" \
  open --case 9 --branch bot/version-sync --version 0.9.500 --sha abc1234
expect_exit2 "open with no --branch exits 2" \
  open --case 2 --version 0.9.500 --sha abc1234
expect_exit2 "open with no --version exits 2" \
  open --case 2 --branch bot/version-sync --sha abc1234
expect_exit2 "close with no --branch exits 2" close --sha abc1234
expect_exit2 "close with no --sha exits 2" close --branch bot/version-sync
expect_exit2 "an unknown flag exits 2" \
  open --case 2 --branch bot/version-sync --version 0.9.500 --sha abc1234 --nonsense

out="$("$SCRIPT" --help 2>&1)"
rc=$?
if [ "$rc" -eq 0 ]; then
  contains "--help prints the usage" "$out" "Usage:"
else
  bad "--help exited $rc: $out"
fi

printf '\n'
if [ "$fail" -eq 0 ]; then
  printf '\033[32mversion-writeback-defer-test: OK\033[0m — %d checks passed\n' "$pass"
  exit 0
fi
printf '\033[31mversion-writeback-defer-test: FAILED\033[0m — %d passed, %d failed\n' "$pass" "$fail"
exit 1
