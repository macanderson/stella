#!/usr/bin/env bash
#
# Tests for check scripts/changelog-roll.sh — WHICH RELEASES GET A SECTION.
#
#   ./scripts/test-changelog-roll.sh
#
# Run it after touching that script. Not part of `make gate`: it is hermetic
# and cheap, the same posture as `make file-size-test`.
#
# ── Why this suite exists ────────────────────────────────────────────────────
#
# CHANGELOG.md reached 180 sections of which 77 were completely empty — a bare
# `## [0.6.x] — <date>` with nothing under it. Two facts composed into that:
#
#   1. auto-tag.yml cuts a PATCH release on every merge to main (127 tags in
#      the 0.6 line alone), and the roll fired on every one of them.
#   2. scripts/changelog-ai.sh degrades open BY CONTRACT — no API key, no
#      non-bot commits, or a response that does not look like changelog
#      markdown all print nothing and exit 0.
#
# When (2) fired, (1) still stamped the heading. Neither script was individually
# wrong; the composition was. Both halves of the fix are asserted here, because
# both are invisible from the script's own output — a run that stamps an empty
# heading and a run that correctly skips print nothing alarming either way.
#
# C1/C2 pin rule one (patches record nothing). C3/C4 pin rule two (a minor
# release never emits a heading with an empty body, even when the drafter
# degraded). C5 pins that a hand-written [Unreleased] is still honored, so the
# fallback cannot eat real content.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/changelog-roll.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() {
  pass=$((pass + 1))
  printf '  ok    %s\n' "$1"
}
no() {
  fail=$((fail + 1))
  printf '  FAIL  %s\n' "$1"
  printf '        want: %s\n        got:  %s\n' "$3" "$2"
}
check() {
  if [ "$2" = "$3" ]; then ok "$1"; else no "$1" "$2" "$3"; fi
}

# A CHANGELOG.md with $1 as the [Unreleased] body. Echoes its path.
changelog_with() {
  local dir="$TMP/$1"
  mkdir -p "$dir"
  {
    printf '# Changelog\n\n## [Unreleased]\n'
    printf '%s' "${2:-}"
    printf '\n## [0.6.0] — 2026-07-16\n\n### Added\n\n- something older\n'
  } >"$dir/CHANGELOG.md"
  printf '%s/CHANGELOG.md' "$dir"
}

# Body of the section headed $2 in file $1, whitespace stripped.
section_body() {
  awk -v want="$2" '
    $0 ~ "^## \\[" want "\\]" { f = 1; next }
    /^## \[/ { f = 0 }
    f
  ' "$1" | tr -d '[:space:]'
}

roll() {
  local file="$1" version="$2" entries="${3:-}"
  NEW_VERSION="$version" RELEASE_DATE=2026-08-07 CHANGELOG_ENTRIES_FILE="$entries" \
    "$SCRIPT" "$file" 2>&1
}

# ── C1: a patch release leaves the file byte-identical ───────────────────────
printf '\nC1  patch release records nothing\n'
f="$(changelog_with c1)"
before="$(cat "$f")"
out="$(roll "$f" 0.6.132)"
check "CHANGELOG.md is byte-identical" "$(cat "$f")" "$before"
case "$out" in
  *"records minor and major lines only"*) ok "the skip is logged" ;;
  *) no "the skip is logged" "$out" "a 'minor and major lines only' notice" ;;
esac

# ── C2: a patch release does not stamp a heading even with entries drafted ───
printf '\nC2  patch release ignores drafted entries\n'
f="$(changelog_with c2)"
printf '### Added\n\n- **A thing.** It happened.\n' >"$TMP/c2-entries.md"
roll "$f" 0.6.133 "$TMP/c2-entries.md" >/dev/null
check "no 0.6.133 heading exists" "$(grep -c '^## \[0.6.133\]' "$f")" "0"
check "[Unreleased] is still empty" "$(section_body "$f" Unreleased)" ""

# ── C3: a minor release rolls the drafted entries under a new heading ────────
printf '\nC3  minor release rolls the draft\n'
f="$(changelog_with c3)"
printf '### Added\n\n- **A new thing.** It does things.\n' >"$TMP/c3-entries.md"
roll "$f" 0.7.0 "$TMP/c3-entries.md" >/dev/null
check "the 0.7.0 heading is created once" "$(grep -c '^## \[0.7.0\] — 2026-08-07$' "$f")" "1"
check "the drafted bullet landed under it" \
  "$(grep -c '^- \*\*A new thing\.\*\* It does things\.$' "$f")" "1"
check "[Unreleased] is left behind, empty" "$(section_body "$f" Unreleased)" ""
check "the older section survives" "$(grep -c '^## \[0.6.0\] — 2026-07-16$' "$f")" "1"

# ── C4: THE REGRESSION. Minor + degraded drafter must not leave a bare heading ─
printf '\nC4  minor release with no draft writes a pointer, not an empty section\n'
f="$(changelog_with c4)"
roll "$f" 0.8.0 "" >/dev/null
check "the 0.8.0 heading is still created" "$(grep -c '^## \[0.8.0\] — 2026-08-07$' "$f")" "1"
if [ -n "$(section_body "$f" 0.8.0)" ]; then
  ok "the section has a body (not the 77-empty-headings shape)"
else
  no "the section has a body" "an empty section" "a releases-page pointer"
fi
check "the pointer names the releases page" \
  "$(grep -c 'github.com/macanderson/stella/releases' "$f")" "1"

# ── C5: a hand-written [Unreleased] survives a degraded draft ────────────────
printf '\nC5  hand-written [Unreleased] is not eaten by the fallback\n'
f="$(changelog_with c5 '
### Fixed

- a hand-written note
')"
roll "$f" 0.9.0 "" >/dev/null
check "the hand-written note rolled under 0.9.0" \
  "$(awk '/^## \[0.9.0\]/{f=1;next} /^## \[/{f=0} f' "$f" | grep -c 'a hand-written note')" "1"
check "the fallback pointer was NOT written" \
  "$(grep -c 'could not be generated' "$f")" "0"

# ── C6: idempotent — an existing section for this version is never duplicated ─
printf '\nC6  a version that already has a section is left alone\n'
f="$(changelog_with c6)"
# Plant the section the roll is about to write, as a release PR or a first
# invocation at the other call site would have.
perl -0777 -pi -e 's/^## \[Unreleased\]\n/## [Unreleased]\n\n## [1.0.0] — 2026-08-07\n\n### Added\n\n- hand-written\n/ms' "$f"
printf '### Added\n\n- **CI draft.** Would have replaced it.\n' >"$TMP/c6-entries.md"
out="$(roll "$f" 1.0.0 "$TMP/c6-entries.md")"
check "exactly one 1.0.0 heading exists" "$(grep -c '^## \[1.0.0\]' "$f")" "1"
check "the existing body survives" "$(grep -c '^- hand-written$' "$f")" "1"
check "the CI draft was not written" "$(grep -c 'CI draft' "$f")" "0"
case "$out" in
  *"already has a"*) ok "the skip is logged" ;;
  *) no "the skip is logged" "$out" "an 'already has a [1.0.0] section' notice" ;;
esac

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
