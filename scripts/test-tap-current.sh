#!/usr/bin/env bash
#
# Tests for check-tap-current.sh's staleness rule.
#
#   ./scripts/test-tap-current.sh
#
# Run it after touching that script. Not part of `make gate`: a dozen jq
# invocations over fixtures, the same posture as `make releases-published-test`.
#
# ── What is under test ───────────────────────────────────────────────────────
#
# This is an alarm, and both ways an alarm fails are bad in different ways:
#
#   silent   the tap stops being updated and nothing says so. `brew install`
#            keeps serving an old build while the tag, the release, and
#            check-releases-published.sh all report success. Every surface a
#            maintainer glances at says the release shipped.
#   crying   it fires in the minutes between a release being published and the
#            `homebrew` job pushing the formula for it. This repo releases on
#            nearly every merge, so that window is entered constantly, and an
#            alarm that fires there is one a maintainer mutes.
#
# Every case below straddles one of those. The `--select` mode is the rule with
# the I/O taken out, so a fixture is a complete input and no live repository,
# tap, or API is involved.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-tap-current.sh"

# A fixed clock. Real time would make these cases drift into and out of the
# grace window depending on when the suite ran.
NOW=1000000
GRACE=5400 # 90 minutes, the script's default

pass=0
fail=0

# The whole finding line, so the expected version, the behind count and the age
# are covered rather than only the fact that something fired. A count that is
# always 1 would satisfy a bare "did it report" assertion and tell a reader
# nothing about how far behind the tap actually is.
run() { printf '%s' "$1" | "$SCRIPT" --select --now "$NOW" --grace-secs "$GRACE" 2>&1 | tr '\t' ' ' | tr '\n' ' ' | sed 's/ *$//'; }

# want <name> <expected-line-or-empty> <json>
want() {
  local name="$1" expect="$2" json="$3" got
  got="$(run "$json")"
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — wanted '${expect}', got '${got}'"
  fi
}

# rel <tag> <seconds-ago>
rel() { printf '{"tag":"%s","published_unix":%s}' "$1" "$((NOW - $2))"; }

day=86400
hour=3600

# ── Silent: the failure this exists to catch ─────────────────────────────────
# Releases keep flowing and the tap stops receiving them.
want "S1 a tap many releases behind is reported with the gap" \
  "0.9.235 0.9.238 3 172800" \
  "{\"formula_version\":\"0.9.235\",\"releases\":[$(rel v0.9.235 $((5 * day))),$(rel v0.9.236 $((4 * day))),$(rel v0.9.237 $((3 * day))),$(rel v0.9.238 $((2 * day)))]}"

want "S2 a tap one release behind is reported" \
  "0.9.291 0.9.292 1 7200" \
  "{\"formula_version\":\"0.9.291\",\"releases\":[$(rel v0.9.291 $((4 * hour))),$(rel v0.9.292 $((2 * hour)))]}"

# A tap that never received a formula at all reads as behind every settled
# release, not as "nothing to compare". The credential being unset from the
# first release is the same defect as it expiring on the hundredth.
want "S3 a tap carrying no formula is behind every settled release" \
  "(none) 0.9.292 2 7200" \
  "{\"formula_version\":null,\"releases\":[$(rel v0.9.291 $((4 * hour))),$(rel v0.9.292 $((2 * hour)))]}"

# ── Crying wolf: what must NOT fire ──────────────────────────────────────────
want "C1 a tap at the newest settled release reports nothing" \
  "" \
  "{\"formula_version\":\"0.9.292\",\"releases\":[$(rel v0.9.291 $((4 * hour))),$(rel v0.9.292 $((2 * hour)))]}"

# The normal state for the minutes after every release: the tap job has already
# pushed the formula for a release that has not aged past the window yet.
want "C2 a tap AHEAD of the newest settled release reports nothing" \
  "" \
  "{\"formula_version\":\"0.9.293\",\"releases\":[$(rel v0.9.292 $((2 * hour))),$(rel v0.9.293 $((10 * 60)))]}"

want "C3 a release still inside the grace window is not what the tap is measured against" \
  "" \
  "{\"formula_version\":\"0.9.292\",\"releases\":[$(rel v0.9.292 $((2 * hour))),$(rel v0.9.293 $((30 * 60)))]}"

want "C4 nothing settled yet reports nothing" \
  "" \
  "{\"formula_version\":\"0.9.235\",\"releases\":[$(rel v0.9.292 $((20 * 60)))]}"

want "C5 no releases at all reports nothing" "" '{"formula_version":"0.9.235","releases":[]}'

# The boundary, from both sides. An off-by-one here is the difference between an
# alarm that fires on every release and one that fires on none.
want "C6 a release exactly at the grace boundary has not settled" \
  "" \
  "{\"formula_version\":\"0.9.291\",\"releases\":[$(rel v0.9.292 $GRACE)]}"

want "C7 a release one second past the boundary has settled" \
  "0.9.291 0.9.292 1 5401" \
  "{\"formula_version\":\"0.9.291\",\"releases\":[$(rel v0.9.292 $((GRACE + 1)))]}"

# ── Version ordering ─────────────────────────────────────────────────────────
# The reason versions are compared as number arrays. As strings "0.9.235" sorts
# below "0.9.9", which would report a current tap as 1 release behind on every
# run for the rest of the 0.9 series.
want "V1 0.9.235 is newer than 0.9.9, not older" \
  "" \
  "{\"formula_version\":\"0.9.235\",\"releases\":[$(rel v0.9.9 $((3 * day))),$(rel v0.9.235 $((2 * day)))]}"

want "V2 the newest release is picked by version, not by publish order" \
  "0.9.100 0.9.101 1 172800" \
  "{\"formula_version\":\"0.9.100\",\"releases\":[$(rel v0.9.101 $((2 * day))),$(rel v0.9.99 $((1 * day)))]}"

# A tag that is not a plain dotted-numeric version cannot be compared, and
# guessing at it would either invent a finding or hide one.
want "V3 a non-numeric tag is skipped rather than guessed at" \
  "" \
  "{\"formula_version\":\"0.9.292\",\"releases\":[$(rel v0.9.292 $((2 * day))),$(rel v0.9.293-rc1 $((1 * day)))]}"

want "V4 skipping a non-numeric tag does not hide a real gap behind it" \
  "0.9.291 0.9.292 1 172800" \
  "{\"formula_version\":\"0.9.291\",\"releases\":[$(rel v0.9.292 $((2 * day))),$(rel nightly $((1 * day)))]}"

# ── Drafts ───────────────────────────────────────────────────────────────────
# A draft is not something `brew install` can serve, and `gh` gives it a publish
# date of `0001-01-01T00:00:00Z`. Measuring the tap against one would report
# every tap in the world as behind, permanently, for a release that does not
# exist yet. Found by running the live check, which hit exactly this on the
# repo's own open draft.
want "D1 a draft newer than the tap is not a finding" \
  "" \
  "{\"formula_version\":\"0.9.292\",\"releases\":[$(rel v0.9.292 $((2 * day))),{\"tag\":\"v1.0.0\",\"published_unix\":null}]}"

want "D2 a draft does not hide a real gap among the published releases" \
  "0.9.291 0.9.292 1 172800" \
  "{\"formula_version\":\"0.9.291\",\"releases\":[$(rel v0.9.292 $((2 * day))),{\"tag\":\"v1.0.0\",\"published_unix\":null}]}"

# A version with a different number of components still orders correctly.
want "V5 0.10.0 is newer than 0.9.292" \
  "0.9.292 0.10.0 1 86400" \
  "{\"formula_version\":\"0.9.292\",\"releases\":[$(rel v0.9.292 $((2 * day))),$(rel v0.10.0 $day)]}"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
