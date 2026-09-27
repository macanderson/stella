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
# fallback cannot eat real content. C6 to C8 pin what happens when the version
# already has a section. The roll adds only draft bullets whose PR refs the
# section does not cite, and it never adds a second heading.
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

# ── C7: section exists + drafter has new entries → append the new ones ──────
printf '\nC7  section exists, drafter has newer entries with new PR refs\n'
f="$(changelog_with c7)"
# Numbers, not literal citations, so this file spells none itself.
pr_existing=2850
pr_new_a=2867
pr_new_b=2859
# Plant an existing section with one PR cited.
planted="$(printf -- '- something from (#%s)\n' "$pr_existing")"
PLANTED="$planted" perl -0777 -pi -e '
  my $planted = $ENV{PLANTED};
  s/^## \[Unreleased\]\n/"## [Unreleased]\n\n## [0.10.0] — 2026-08-07\n\n### Added\n\n" . $planted . "\n"/mse;
' "$f"
# Draft entries that cite PRs not in the existing section.
printf '### Added\n\n- **Another thing.** From the later batch (#%s).\n- **Third thing.** Also later (#%s).\n' \
  "$pr_new_a" "$pr_new_b" >"$TMP/c7-entries.md"
out="$(roll "$f" 0.10.0 "$TMP/c7-entries.md")"
check "exactly one 0.10.0 heading exists" "$(grep -c '^## \[0.10.0\]' "$f")" "1"
check "the original entry survives" \
  "$(grep -c "something from (#${pr_existing})" "$f")" "1"
check "the new draft entry was appended" \
  "$(grep -c "Another thing.*#${pr_new_a}" "$f")" "1"
check "the second new draft entry was appended" \
  "$(grep -c "Third thing.*#${pr_new_b}" "$f")" "1"
case "$out" in
  *"appended"*|*"newer entries"*|*"added"*) ok "appending is logged" ;;
  *) no "appending is logged" "$out" "a notice about appending new entries" ;;
esac

# ── C8: a draft that overlaps the section adds only what the section lacks ───
printf '\nC8  section exists, draft overlaps it\n'
f="$(changelog_with c8)"
# Numbers, not literal citations, as in C7.
r_hand_a=2850
r_hand_b=2851
r_hand_fix=2852
r_hand_prose=2900
r_late=2870
r_mixed=2871
r_changed=2872
r_draft_prose=2999
planted="$(printf '%s\n' \
  "Written by hand. It names issue #${r_hand_prose} in passing." \
  "" \
  "### Added" \
  "" \
  "- **Hand thing.** Written by a maintainer (#${r_hand_a})." \
  "- **Other hand thing.** Also by hand (#${r_hand_b})." \
  "" \
  "### Fixed" \
  "" \
  "- **A fix.** By hand (#${r_hand_fix}).")"
PLANTED="$planted" perl -0777 -pi -e '
  s/^## \[Unreleased\]\n/"## [Unreleased]\n\n## [0.11.0] — 2026-08-07\n\n" . $ENV{PLANTED} . "\n"/mse;
' "$f"
# The draft is the whole series, so it covers the hand-written bullets too.
{
  printf 'Everything since 0.10.0. It cites #%s in passing.\n\n' "$r_draft_prose"
  printf '### Added\n\n'
  printf -- '- **Hand thing, drafted.** Same change (#%s).\n' "$r_hand_a"
  printf -- '- **Other hand thing, drafted.** It wraps onto\n  a second line (#%s).\n' "$r_hand_b"
  printf -- '- **Late thing.** It landed after the release PR (#%s).\n' "$r_late"
  printf '\n### Fixed\n\n'
  printf -- '- **A fix, drafted.** Same fix (#%s).\n' "$r_hand_fix"
  printf -- '- **Mixed.** One old change and one new (#%s, #%s).\n' "$r_hand_fix" "$r_mixed"
  printf '\n### Changed\n\n'
  printf -- '- **New kind.** Only the draft has this heading (#%s).\n' "$r_changed"
} >"$TMP/c8-entries.md"
out="$(roll "$f" 0.11.0 "$TMP/c8-entries.md")"

# How many times ref $2 appears in file $1, as a whole number.
ref_count() {
  grep -oE "#$2([^0-9]|\$)" "$1" | wc -l | tr -d '[:space:]'
}
# The body of the 0.11.0 section, line by line.
c8_section="$(awk '/^## \[0.11.0\]/{f=1;next} /^## \[/{f=0} f' "$f")"
# The bullets under one `###` heading of that section.
c8_sub() {
  printf '%s\n' "$c8_section" | awk -v want="### $1" '$0 == want {f=1;next} /^### /{f=0} f'
}

check "exactly one 0.11.0 heading exists" "$(grep -c '^## \[0.11.0\]' "$f")" "1"
check "the first hand-written ref appears once" "$(ref_count "$f" "$r_hand_a")" "1"
check "the wrapped hand-written ref appears once" "$(ref_count "$f" "$r_hand_b")" "1"
check "the hand-written fix ref appears once" "$(ref_count "$f" "$r_hand_fix")" "1"
check "the hand-written prose survives once" "$(ref_count "$f" "$r_hand_prose")" "1"
check "no drafted copy of a hand-written bullet lands" "$(grep -c 'drafted' "$f")" "0"
check "the draft's prose never lands" "$(ref_count "$f" "$r_draft_prose")" "0"
check "a bullet that cites an old and a new PR is left out" "$(ref_count "$f" "$r_mixed")" "0"
check "the section keeps one Added heading" "$(printf '%s\n' "$c8_section" | grep -c '^### Added$')" "1"
check "the section keeps one Fixed heading" "$(printf '%s\n' "$c8_section" | grep -c '^### Fixed$')" "1"
check "the late bullet lands once, under Added" "$(c8_sub Added | grep -c "Late thing.*#${r_late}")" "1"
check "the late bullet appears nowhere else" "$(ref_count "$f" "$r_late")" "1"
check "a heading only the draft has is added once" "$(printf '%s\n' "$c8_section" | grep -c '^### Changed$')" "1"
check "its bullet lands under it" "$(c8_sub Changed | grep -c "New kind.*#${r_changed}")" "1"
check "a blank line comes before the next version heading" \
  "$(grep -B1 '^## \[0.6.0\]' "$f" | head -n 1)" ""
case "$out" in
  *"appended 2 "*) ok "the two added bullets are logged" ;;
  *) no "the two added bullets are logged" "$out" "an 'appended 2 draft bullet(s)' notice" ;;
esac

# The roll runs at two call sites per release. The second must change nothing.
before="$(cat "$f")"
out="$(roll "$f" 0.11.0 "$TMP/c8-entries.md")"
check "a second roll with the same draft changes nothing" "$(cat "$f")" "$before"
case "$out" in
  *"already has a"*) ok "the second roll logs that it left the section alone" ;;
  *) no "the second roll logs that it left the section alone" "$out" "an 'already has a [0.11.0] section' notice" ;;
esac

printf '\n%s passed, %s failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
