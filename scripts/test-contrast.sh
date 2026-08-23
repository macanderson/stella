#!/usr/bin/env bash
#
# Tests for check-contrast.py's RATCHET DIRECTION (#4423).
#
#   ./scripts/test-contrast.sh
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway token trees, the same posture as `make typed-errors-test`.
#
# ── What the suite is for ────────────────────────────────────────────────────
#
# The guard became a gate step carrying four pre-existing failures, so its
# ratchet is the only thing standing between "this palette has known debt" and
# "this palette has a permission slip". Two directions have to hold at once and
# they fail independently:
#
#   * a colour that gets DARKER than its recorded ratio fails, and `--update`
#     refuses to write the lower number;
#   * a colour that gets LIGHTER retightens — past its threshold it leaves the
#     file entirely, and the check says so rather than passing quietly, because
#     a stale entry is a licence nobody is using.
#
# The refusals are checked twice each: `--update` must exit non-zero AND leave
# the file alone. A writer that refuses loudly and writes anyway passes a suite
# that only reads the verdict, which is the exact shape #3750 found in the
# typed-errors ratchet.
#
# Every writing case runs against its own fixture root under $TMP. Pointing
# `--update` at the real repository would rewrite scripts/contrast-baseline.txt
# as a side effect of running the tests; R1 is the one real-tree case and is
# read-only.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-contrast.py"
TOKENS_REL="design/tokens/stella-tokens.json"
BASELINE_REL="scripts/contrast-baseline.txt"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway root holding this repository's real token file, so every pairing
# in PAIRINGS resolves, plus the baseline the guard reads and writes.
# $1 = case name. Echoes the root path.
new_root() {
  local dir="$TMP/$1"
  mkdir -p "$dir/design/tokens" "$dir/scripts"
  cp "$repo_root/$TOKENS_REL" "$dir/$TOKENS_REL"
  cp "$repo_root/$BASELINE_REL" "$dir/$BASELINE_REL"
  echo "$dir"
}

# Repaint one token. $1 = root, $2 = token name, $3 = new hex.
repaint() {
  python3 - "$1/$TOKENS_REL" "$2" "$3" <<'PY'
import json, sys
path, name, hex_value = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as handle:
    doc = json.load(handle)
for token in doc["tokens"]:
    if token["name"] == name:
        token["hex"] = hex_value
        break
else:
    raise SystemExit(f"no token named {name}")
with open(path, "w") as handle:
    json.dump(doc, handle, indent=2)
PY
}

# want <name> <expect-pass|expect-fail> <root> [substring] [flag]
#
# The substring is checked on both verdicts: on expect-fail it pins which
# pairing was named and in what words, and on expect-pass it pins what a
# passing run reported, so a guard that passed by losing the pairing entirely
# cannot satisfy the case.
want() {
  local name="$1" expect="$2" root="$3" sub="${4:-}" flag="${5:-}" out rc
  out="$(python3 "$SCRIPT" ${flag:+"$flag"} "$root" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -ne 0 ]; then
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
      return
    fi
  elif [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — the guard passed what it should have flagged:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — did not report '$sub':"; echo "$out" ;;
  esac
}

# entry_is <name> <root> <fg> <bg> <ratio|absent>
#
# `--update` writes and then reports, so "did it refuse" and "did it write
# anyway" are two questions and only this one answers the second.
entry_is() {
  local name="$1" file="$2/$BASELINE_REL" fg="$3" bg="$4" expect="$5" got
  got="$(awk -v f="$fg" -v b="$bg" '$1 == f && $2 == b { print $3; exit }' "$file")"
  [ -n "$got" ] || got=absent
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — baseline says '$got', wanted '$expect':"; cat "$file"
  fi
}

# ── U: a pairing that got worse ──────────────────────────────────────────────
# `silver` is comfortably over AA today (8.58:1 on the canvas) and carries no
# baseline entry, so darkening it is a first-time violation rather than a
# regression against a recorded number.
r="$(new_root new_pairing)"
repaint "$r" silver "#5A5A64"
want "U1 a first-time sub-threshold pairing is flagged" \
  expect-fail "$r" "no baseline entry"
want "U2 --update refuses to grandfather it" \
  expect-fail "$r" "new sub-threshold pairing: silver on bg" "--update"
entry_is "U3 the refused --update wrote no entry" "$r" silver bg absent

# The other half: a pairing that IS baselined and got darker still.
r="$(new_root darkened)"
repaint "$r" muted "#6E6E79"
want "U4 a baselined pairing that got darker is flagged with both numbers" \
  expect-fail "$r" "darker than the 4.47:1 the ratchet holds it to"
want "U5 --update refuses to lower a floor" \
  expect-fail "$r" "darkened: muted on bg: 4.47:1 -> 3.93:1" "--update"
entry_is "U6 the refused --update left the floor where it was" "$r" muted bg 4.47

# ── D: a pairing that got better ─────────────────────────────────────────────
# Lighter but still under its 3.0 floor, so it stays on the ledger at a higher
# number rather than dropping off it.
r="$(new_root lightened)"
repaint "$r" comment "#5E5E68"
want "D1 a pairing that improved but still fails passes the check" expect-pass "$r" "held by the ratchet"
want "D2 --update raises its floor" expect-pass "$r" "retightened to 4 pairing(s)" "--update"
entry_is "D3 the floor is what was really measured" "$r" comment panel 2.99

# Over the threshold: the entry must go, and the check must say so rather than
# pass — a baselined pairing nobody needs is a standing permission slip.
r="$(new_root cleared)"
repaint "$r" muted "#7A7A85"
want "D4 a pairing that cleared its threshold is reported, not passed" \
  expect-fail "$r" "clears its threshold now"
want "D5 --update drops it" expect-pass "$r" "retightened to 2 pairing(s)" "--update"
entry_is "D6 the cleared pairing is gone" "$r" muted bg absent

# ── B: bootstrap runs once ───────────────────────────────────────────────────
r="$(new_root bootstrap_guard)"
want "B1 --bootstrap refuses when the ratchet already exists" \
  expect-fail "$r" "refusing to bootstrap" "--bootstrap"
entry_is "B2 the refused --bootstrap left the ratchet intact" "$r" muted bg 4.47

# ── N: the negative direction ────────────────────────────────────────────────
# Without this the suite is satisfiable by a guard that fails on everything.
r="$(new_root unchanged)"
want "N1 an untouched tree passes" expect-pass "$r" "none darker than it allows"

# An exempt pairing is reported and never failed on: gold on paper measures
# 1.61:1 and is a logotype, which is the carve-out rule 6 rests on.
want "N2 the report names the exemption rather than hiding it" \
  expect-pass "$r" "logotype exemption" "--report"

# ── R: the real workspace ────────────────────────────────────────────────────
# Check mode only. Never --update here.
want "R1 this repository matches its own ratchet" expect-pass "$repo_root" "none darker than it allows"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
