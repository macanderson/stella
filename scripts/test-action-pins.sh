#!/usr/bin/env bash
#
# Hermetic tests for scripts/check-action-pins.sh.
#
#   ./scripts/test-action-pins.sh     (or: make action-pins-test)
#
# Every case builds a throwaway `.github/` tree and points the guard at it with
# `--fixture-root`, so nothing here depends on what this repository's own
# workflows happen to contain today.
#
# The case that carries the change is the composite action: `.github/actions/`
# does not exist here yet, so an unpinned `uses:` inside one would have been
# invisible to the guard and to every future reader of a green gate (#4288).
# The negative controls beside it — a pinned composite action, and a tree with
# workflows and no composite actions at all — are what stop the widening from
# being satisfied by a guard that simply fails more often.
#
# Deliberately not a `make gate` step, matching scripts/test-file-size.sh: the
# gate runs the guard, and this runs the guard's own directions.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-action-pins.sh"

pass=0
fail=0
work="$(mktemp -d "${TMPDIR:-/tmp}/stella-action-pins.XXXXXX")"
trap 'rm -rf "$work"' EXIT INT TERM

ok()  { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }

sha="3d3c42e5aac5ba805825da76410c181273ba90b1"

# want <name> <expect-pass|expect-fail> <want-substring> — run the guard over
# $work. An expected failure must also name its reason: "exit 1" is satisfied
# by a typo in the guard just as well as by the defect the case is about.
want() {
  local name="$1" expect="$2" want_text="$3"
  local out rc
  out="$("$SCRIPT" --fixture-root "$work" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ] && [ "$rc" -ne 0 ]; then
    bad "$name — expected pass, got exit $rc: $out"
    return
  fi
  if [ "$expect" = "expect-fail" ] && [ "$rc" -eq 0 ]; then
    bad "$name — expected the guard to FAIL, but it passed: $out"
    return
  fi
  case "$out" in
  *"$want_text"*) ok "$name" ;;
  *) bad "$name — wrong reason (wanted '$want_text'): $out" ;;
  esac
}

reset() {
  rm -rf "$work/.github"
  mkdir -p "$work/.github/workflows"
}

workflow_pinned() {
  cat >"$work/.github/workflows/ci.yml" <<EOF
jobs:
  build:
    steps:
      - uses: actions/checkout@$sha # v4
EOF
}

printf '\033[1maction-pins — a mutable tag anywhere under .github/ is a finding\033[0m\n'

# The baseline the guard already held: a workflow, pinned and unpinned.
reset
workflow_pinned
want "a pinned workflow passes" expect-pass "OK"

reset
cat >"$work/.github/workflows/ci.yml" <<'EOF'
jobs:
  build:
    steps:
      - uses: actions/checkout@v4
EOF
want "an unpinned workflow fails" expect-fail "not pinned to a commit SHA"

# The change: a composite action carries `uses:` too, and it runs with the
# calling job's secrets.
reset
mkdir -p "$work/.github/actions/probe"
workflow_pinned
cat >"$work/.github/actions/probe/action.yml" <<'EOF'
runs:
  using: composite
  steps:
    - uses: actions/checkout@v4
EOF
want "an unpinned composite action fails" expect-fail "not pinned to a commit SHA"
want "the failure names the composite action's file" expect-fail "actions/probe/action.yml"

cat >"$work/.github/actions/probe/action.yml" <<EOF
runs:
  using: composite
  steps:
    - uses: actions/checkout@$sha # v4
EOF
want "a pinned composite action passes" expect-pass "OK"

# A workflow calling that action names a path, not a ref: there is nothing to
# pin, and flagging it would make the widened guard unusable for its own case.
cat >"$work/.github/workflows/ci.yml" <<EOF
jobs:
  build:
    steps:
      - uses: ./.github/actions/probe
      - uses: actions/checkout@$sha # v4
EOF
want "a local ./.github/actions reference needs no SHA" expect-pass "OK"

# Per-root skipping: the repository as it stands today has workflows and no
# composite actions, and must still pass.
reset
workflow_pinned
want "no .github/actions at all still passes" expect-pass "OK"

# Neither root present: nothing to check, and not a failure.
rm -rf "$work/.github"
mkdir -p "$work/.github"
want "neither root present skips" expect-pass "skipping"

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
