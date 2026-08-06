#!/usr/bin/env bash
#
# Tests for check-releases-published.sh's reconciliation rule (#1464).
#
#   ./scripts/test-releases-published.sh
#
# Run it after touching that script. Not part of `make gate`: a dozen jq
# invocations over fixtures, the same posture as `make impacted-test`.
#
# ── What is under test ───────────────────────────────────────────────────────
#
# This is an alarm, and both ways an alarm fails are bad in different ways:
#
#   silent   the thing it exists to catch goes unreported. That already
#            happened for two days and 31 tags, which is why the script exists.
#   crying   it fires while a release is still building. Full-LTO plus the
#            independent rebuild arm takes the better part of an hour, so a
#            just-cut tag with no release is the NORMAL state — an alarm that
#            reports it is one everybody learns to ignore, which is the same
#            outcome as silence but with more noise on the way.
#
# Every case below straddles one of those. The `--select` mode exists so they
# can: it is the rule with the I/O taken out, so a fixture is a complete input
# and no live repository or API is involved.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-releases-published.sh"

# A fixed clock. Real time would make these cases drift into and out of the
# grace window depending on when the suite ran.
NOW=1000000
GRACE=5400 # 90 minutes, the script's default

pass=0
fail=0

# want <name> <expected-tags-space-separated-or-empty> <json>
want() {
  local name="$1" expect="$2" json="$3" got
  got="$(printf '%s' "$json" | "$SCRIPT" --select --now "$NOW" --grace-secs "$GRACE" 2>&1 | cut -f1 | tr '\n' ' ')"
  got="${got% }"
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — wanted '${expect}', got '${got}'"
  fi
}

# tag <name> <seconds-ago>
tag() { printf '{"name":"%s","created_unix":%s}' "$1" "$((NOW - $2))"; }

day=86400

# ── Silent: the failure this exists to catch ─────────────────────────────────
# The #1464 shape — a run of consecutive tags, none published.
want "S1 a run of unpublished old tags is reported, oldest first" \
  "v0.6.75 v0.6.76 v0.6.77" \
  "{\"tags\":[$(tag v0.6.77 $((2 * day))),$(tag v0.6.75 $((4 * day))),$(tag v0.6.76 $((3 * day)))],\"releases\":[\"v0.6.74\"]}"

want "S2 one unpublished tag among published ones is still caught" \
  "v0.6.76" \
  "{\"tags\":[$(tag v0.6.75 $((3 * day))),$(tag v0.6.76 $((2 * day))),$(tag v0.6.77 $day)],\"releases\":[\"v0.6.75\",\"v0.6.77\"]}"

# A release that was published and later deleted looks exactly like one that
# never published, and is equally worth knowing about. A watcher of the
# dispatched run cannot see this at all.
want "S3 a tag whose release was deleted after the fact is caught" \
  "v0.6.80" \
  "{\"tags\":[$(tag v0.6.80 $((5 * day)))],\"releases\":[]}"

# ── Crying wolf: what must NOT fire ──────────────────────────────────────────
want "C1 a tag inside the grace window is not reported — the release is still building" \
  "" \
  "{\"tags\":[$(tag v0.6.90 $((30 * 60)))],\"releases\":[]}"

# The boundary, from both sides. An off-by-one here is the difference between
# an alarm that fires every release and one that fires none.
want "C2 a tag exactly at the grace boundary is not reported" \
  "" \
  "{\"tags\":[$(tag v0.6.90 $GRACE)],\"releases\":[]}"

want "C3 a tag one second past the boundary is reported" \
  "v0.6.90" \
  "{\"tags\":[$(tag v0.6.90 $((GRACE + 1)))],\"releases\":[]}"

want "C4 a fully published history reports nothing" \
  "" \
  "{\"tags\":[$(tag v0.6.75 $((3 * day))),$(tag v0.6.76 $((2 * day)))],\"releases\":[\"v0.6.75\",\"v0.6.76\"]}"

want "C5 no tags at all reports nothing" "" '{"tags":[],"releases":[]}'

# A release for a tag that no longer exists is somebody else's problem, not an
# unpublished tag. Reporting it would send a reader looking for a build that
# was never owed.
want "C6 a release with no matching tag is not a finding" \
  "" \
  "{\"tags\":[$(tag v0.6.75 $((2 * day)))],\"releases\":[\"v0.6.75\",\"v0.6.99\"]}"

# ── Grandfathering ───────────────────────────────────────────────────────────
# The baseline is what lets this be an alarm about NEW silence instead of a
# daily recital of a backlog nobody is going to republish (#1463). An exemption
# nothing tests is one nobody can be sure still covers what it was written for.
want "K1 a grandfathered tag is not reported" \
  "" \
  "{\"tags\":[$(tag v0.4.29 $((30 * day)))],\"releases\":[],\"known\":[\"v0.4.29\"]}"

want "K2 grandfathering one tag does not silence its neighbours" \
  "v0.4.30" \
  "{\"tags\":[$(tag v0.4.29 $((30 * day))),$(tag v0.4.30 $((29 * day)))],\"releases\":[],\"known\":[\"v0.4.29\"]}"

want "K3 an absent baseline grandfathers nothing" \
  "v0.4.29" \
  "{\"tags\":[$(tag v0.4.29 $((30 * day)))],\"releases\":[]}"

# ── Mixed: the realistic case ────────────────────────────────────────────────
# Old-and-published, old-and-missing, and fresh-and-missing together. A rule
# applied in the wrong order reports the fresh one or misses the old one.
want "M1 a mixed history reports only the old unpublished tag" \
  "v0.6.76" \
  "{\"tags\":[$(tag v0.6.75 $((3 * day))),$(tag v0.6.76 $((2 * day))),$(tag v0.6.99 $((10 * 60)))],\"releases\":[\"v0.6.75\"]}"

# The age column is what tells a reader "two days" from "twenty minutes", so it
# has to be right, not merely present.
age_of() {
  printf '%s' "{\"tags\":[$(tag v0.6.75 $((2 * day)))],\"releases\":[]}" \
    | "$SCRIPT" --select --now "$NOW" --grace-secs "$GRACE" | cut -f2
}
if [ "$(age_of)" = "$((2 * day))" ]; then
  pass=$((pass + 1)); echo "ok   A1 the reported age is the tag's real age"
else
  fail=$((fail + 1)); echo "FAIL A1 the reported age is the tag's real age — got '$(age_of)'"
fi

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
