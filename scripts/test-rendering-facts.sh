#!/usr/bin/env bash
#
# Hermetic self-test for scripts/check-rendering-facts.sh.
#
# The guard's whole value is that it fails on a rendering that draws a retired
# fact, and a guard nobody has watched fail is an assertion, not evidence — the
# thing it is guarding against is precisely a check that quietly matches
# nothing. So this plants the exact shape #5291 found, in a throwaway tree, and
# asserts the guard rejects it.
#
# Runs in guard-self-tests.yml. Not a `make gate` step: it builds a scratch
# directory and runs the guard against it, which is work the gate has no use
# for on every push.

set -euo pipefail

cd "$(dirname "$0")/.."
GUARD="$PWD/scripts/check-rendering-facts.sh"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/design/tui-v2/renderings/svg" "$work/website/public/tui"
cd "$work"
# The guard resolves its directory from its own location, so it has to be
# invoked from a copy sitting in the scratch tree.
mkdir -p scripts
cp "$GUARD" scripts/

svg=design/tui-v2/renderings/svg/00-fixture.svg
web=website/public/tui/00-fixture.svg
frame='<svg viewBox="0 0 680 520" xmlns="http://www.w3.org/2000/svg">'

pass=0
fail=0
check() {
  local want=$1 name=$2 out rc
  set +e
  out=$(./scripts/check-rendering-facts.sh 2>&1)
  rc=$?
  set -e
  if [[ $rc -eq $want ]]; then
    pass=$((pass + 1))
    echo "  ok   $name"
  else
    fail=$((fail + 1))
    echo "  FAIL $name — wanted exit $want, got $rc"
    echo "${out//$'\n'/$'\n'       }"
  fi
}

echo "test-rendering-facts:"

clean() {
  printf '%s\n<text x="16" y="16">kimi-k3 · execute · ctx</text>\n</svg>\n' "$frame" >"$1"
}
plant() {
  printf '%s\n<text x="16" y="16">%s</text>\n</svg>\n' "$frame" "$2" >"$1"
}

# A rendering with no retired fact passes.
clean "$svg"
clean "$web"
check 0 "a clean rendering passes"

# The exact shape #5291 found on the status bar.
plant "$svg" "det 86%"
check 1 "a rendering drawing det 86% fails"

# Any percentage, not the one literal that happened to be in the tree — the
# same cell carried det 87% and det 88% in three other renderings.
plant "$svg" "det 88%"
check 1 "a rendering drawing det 88% fails"

# The QUALIFIED spelling, which the first version of this guard missed
# outright: the start-work estimate line draws `det est 84%`, and a pattern
# anchored on a digit right after `det ` walks past it. SPEC §1 strikes the
# metric from that exact cell, so a guard blind to it bans the symptom and not
# the fact (#5276).
clean "$svg"
plant "$svg" "det est 84%"
check 1 "a rendering drawing det est 84% fails"

# And the website's public copies are held to the same prose. They are a second
# tree rather than a mirror — theirs carry different content from design/, down
# to a different frame for the command palette — so a guard scoped to design/
# alone reports OK while the copies the site serves still draw the retired cell.
clean "$svg"
plant "$web" "det 86%"
check 1 "the website copy is checked too, not just design/"
clean "$web"

# The near miss that must keep passing: the task-zoom metrics panel splits
# checks by provenance (`DET / MODEL  88 / 12`), which SPEC §3 keeps. The
# retired cell is the percentage on the status bar, and the guard has to tell
# them apart or the fix for it would be deleting a live surface.
plant "$svg" "DET / MODEL  88 / 12"
check 0 "the task-zoom DET / MODEL split still passes"

# A missing renderings directory is a broken invocation, not a clean tree: a
# guard that reports OK over nothing is the failure mode it exists to prevent.
# Checked on each tree, so dropping one from DIRS cannot pass silently.
rm -rf website
check 1 "a missing website tree fails rather than passing over nothing"
mkdir -p website/public/tui
clean "$web"
rm -rf design
check 1 "a missing design tree fails rather than passing over nothing"

echo "test-rendering-facts: $pass passed, $fail failed"
[[ $fail -eq 0 ]]
