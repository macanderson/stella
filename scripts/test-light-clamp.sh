#!/usr/bin/env bash
#
# Tests for check-light-clamp.py (#4941).
#
#   ./scripts/test-light-clamp.sh
#
# Run it after touching that script. Not part of `make gate`: it builds
# throwaway surface trees, the same posture as `make contrast-test`.
#
# ── What the suite is for ────────────────────────────────────────────────────
#
# The guard exists because the `warm-paper` clamp was enforced over the token
# table alone, so no shipped surface was ever judged against it. It joins the
# gate carrying 34 pre-existing violations, which makes its ratchet the only
# thing between "this light scheme has known debt" and "this light scheme has a
# permission slip". Four directions have to hold and they fail independently:
#
#   * a cool light neutral in a shipped surface FAILS and is named;
#   * the same surface with a warm one PASSES -- without this the suite is
#     satisfiable by a guard that fails on everything;
#   * a role that is neither a kit token nor a declared family is
#     UNCLASSIFIABLE and fails, because a new hand-picked neutral arriving
#     under a new name is the event the guard exists to catch;
#   * `--update` refuses to grandfather a new violation AND refuses to move an
#     entry to a different off-clamp value -- checked twice each, verdict and
#     file, because a writer that refuses loudly and writes anyway passes a
#     suite that only reads the verdict (#3750's shape).
#
# Every writing case runs against its own fixture root under $TMP. Pointing
# `--update` at the real repository would rewrite the ratchet as a side effect
# of running the tests; R1 is the one real-tree case and is read-only.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-light-clamp.py"
BASELINE_REL="scripts/light-clamp-baseline.txt"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# Every file the guard reads, so a fixture root is a whole tree it can judge.
SURFACE_FILES="crates/stella-observatory/src/assets/index.html
crates/stella-cli/src/export.rs
crates/stella-transcript/src/html/transcript.css
docs/benchmarks/index.html
docs/benchmarks/terminal-bench-2-1-glm-5-2.html
crates/stella-tui/src/palette.rs"

# A throwaway root holding this repository's real surfaces, token file, the
# generator whose predicates the guard imports, and the ratchet.
# $1 = case name. Echoes the root path.
new_root() {
  local dir="$TMP/$1" rel
  mkdir -p "$dir/design/tokens" "$dir/scripts"
  cp "$repo_root/design/tokens/stella-tokens.json" "$dir/design/tokens/"
  cp "$repo_root/scripts/gen-tokens.py" "$dir/scripts/"
  cp "$repo_root/$BASELINE_REL" "$dir/$BASELINE_REL"
  echo "$SURFACE_FILES" | while IFS= read -r rel; do
    mkdir -p "$dir/$(dirname "$rel")"
    cp "$repo_root/$rel" "$dir/$rel"
  done
  echo "$dir"
}

# Repaint one hex inside one surface. $1 = root, $2 = file, $3 = old, $4 = new.
repaint() {
  python3 - "$1/$2" "$3" "$4" <<'PY'
import sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as handle:
    text = handle.read()
for spelling in (old, old.lower()):
    if spelling in text:
        with open(path, "w") as handle:
            handle.write(text.replace(spelling, new if spelling == old else new.lower()))
        break
else:
    raise SystemExit(f"{path} does not contain {old}")
PY
}

# want <name> <expect-pass|expect-fail> <root> [substring] [flag]
#
# The substring is checked on both verdicts: on expect-fail it pins which role
# was named and in what words, and on expect-pass it pins what a passing run
# reported, so a guard that passed by losing the surface entirely cannot
# satisfy the case.
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

# entry_is <name> <root> <file> <role> <hex|absent>
#
# `--update` writes and then reports, so "did it refuse" and "did it write
# anyway" are two questions and only this one answers the second.
entry_is() {
  local name="$1" file="$2/$BASELINE_REL" surface="$3" role="$4" expect="$5" got
  got="$(awk -v s="$surface" -v r="$role" '$1 == s && $2 == r { print $3; exit }' "$file")"
  [ -n "$got" ] || got=absent
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1)); echo "ok   $name"
  else
    fail=$((fail + 1)); echo "FAIL $name — baseline says '$got', wanted '$expect'"
  fi
}

BENCH="docs/benchmarks/index.html"

# ── C: a cool light neutral is caught ────────────────────────────────────────
# `--panel` on the benchmark index is #FFFFFF today — achromatic, so it clears
# warm-paper and carries no ratchet entry. Cooling it is a first-time violation
# rather than a regression against a recorded value.
r="$(new_root cool_neutral)"
repaint "$r" "$BENCH" "#FFFFFF" "#F4F6FB"
want "C1 a cool light neutral in a shipped surface is flagged" \
  expect-fail "$r" "no baseline entry"
want "C2 the failure names the role and the conjunct it broke" \
  expect-fail "$r" "needs r >= g >= b"
want "C3 --update refuses to grandfather it" \
  expect-fail "$r" "refusing to grandfather" "--update"
entry_is "C4 the refused --update wrote no entry" "$r" "$BENCH" --panel absent

# ── W: the same surface with a warm value passes ─────────────────────────────
# Without this the suite is satisfiable by a guard that fails on everything.
# `--rule` is #E9E9EE and baselined; the kit's own `paper-seam` is warm and
# passes, so the entry must LEAVE the ratchet rather than stay as a licence.
r="$(new_root warm_neutral)"
repaint "$r" "$BENCH" "#E9E9EE" "#E0DDD7"
want "W1 a warm replacement is reported as clearing, not passed silently" \
  expect-fail "$r" "satisfies its clamp now"
want "W2 --update drops the cleared entry" expect-pass "$r" "retightened to 33" "--update"
entry_is "W3 the cleared entry is gone" "$r" "$BENCH" --rule absent

# ── U: a role that names no family at all ────────────────────────────────────
# The one outcome that must never be silent. A guard that skipped what it could
# not classify would be blind to exactly the event it exists to catch.
r="$(new_root unclassified)"
# Anchored to the light GATE, not to the declaration text: this page declares
# its light scheme twice -- once in the bare `:root` and once under the
# attribute -- so an unanchored insert lands in the block the guard does not
# read and the case passes over nothing. It did, before this comment existed.
python3 - "$r/$BENCH" <<'PY'
import sys
path = sys.argv[1]
with open(path) as handle:
    text = handle.read()
gate = ':root[data-theme="light"]{'
assert text.count(gate) == 1, "fixture gate moved"
with open(path, "w") as handle:
    handle.write(text.replace(gate, gate + "\n  --well:#EEF2F7;", 1))
PY
want "U1 an off-kit value under an undeclared role is unclassifiable" \
  expect-fail "$r" "not a kit token and names no family"
want "U2 --update refuses to write the ratchet around it" \
  expect-fail "$r" "refusing to write the ratchet while" "--update"
# `--bootstrap` needs its own case: it is reachable only on a tree with no
# ratchet yet, which is exactly the tree with nobody to notice what it wrote.
# Removing the ratchet is what gets past the "already exists" refusal, so this
# case reaches the check it is about rather than stopping one door earlier.
rm -f "$r/$BASELINE_REL"
want "U3 --bootstrap refuses to write the ratchet around it either" \
  expect-fail "$r" "refusing to write the ratchet while" "--bootstrap"
if [ -f "$r/$BASELINE_REL" ]; then
  fail=$((fail + 1)); echo "FAIL U4 the refused --bootstrap wrote a ratchet anyway"
else
  pass=$((pass + 1)); echo "ok   U4 the refused --bootstrap wrote no ratchet"
fi

# ── M: a baselined role repainted to a DIFFERENT off-clamp value ─────────────
# The ratchet records a measured value, not a name. Sliding a licensed role to
# another cool hex is a new decision and gets asked about.
r="$(new_root moved)"
repaint "$r" "$BENCH" "#D0D0D8" "#CED4DE"
want "M1 a repaint to another off-clamp value is flagged with both hexes" \
  expect-fail "$r" "was #D0D0D8 and is now #CED4DE"
want "M2 --update refuses to move the entry" \
  expect-fail "$r" "refusing to move an entry" "--update"
entry_is "M3 the refused --update left the old value" "$r" "$BENCH" --rule-2 "#D0D0D8"

# ── B: bootstrap runs once ───────────────────────────────────────────────────
r="$(new_root bootstrap_guard)"
want "B1 --bootstrap refuses when the ratchet already exists" \
  expect-fail "$r" "refusing to bootstrap" "--bootstrap"
entry_is "B2 the refused --bootstrap left the ratchet intact" "$r" "$BENCH" --rule "#E9E9EE"

# ── N: the negative direction ────────────────────────────────────────────────
r="$(new_root unchanged)"
want "N1 an untouched fixture tree passes" expect-pass "$r" "none off its declared clamp"
want "N2 the report names the exemptions rather than hiding them" \
  expect-pass "$r" "declared exempt" "--report"

# ── R: the real workspace ────────────────────────────────────────────────────
# Check mode only. Never --update here.
want "R1 this repository matches its own ratchet" \
  expect-pass "$repo_root" "none off its declared clamp"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
