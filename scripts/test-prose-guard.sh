#!/usr/bin/env bash
#
# Tests for check-prose.py's ratchet direction and its citation exemption.
#
#   ./scripts/test-prose-guard.sh
#
# Run it after touching that script. Not part of `make gate`: it builds a
# handful of throwaway git trees, the same posture as
# `make typed-errors-test`.
#
# The guard's promise is one sentence: a (file, pattern) pair may shrink its
# count, never grow it, and a pair absent from the baseline must be at zero. P2
# is the witness
# for that promise -- it is the case that fails before the guard exists and
# passes after. P5 covers the half a ratchet usually gets wrong, which is the
# writer rather than the checker: a `--update` that grandfathers a first-time
# offender turns the gate green without deleting a word, and that is the
# expedient the rule forbids, performed by the guard on the author's behalf.
#
# P7 and P8 pull the other way. A guard that flagged its own rule's examples
# would make CLAUDE.md unwritable, so a backticked span and a fenced block are
# citations and never count.
#
# Fixtures are their own git roots because the guard enumerates with
# `git ls-files`, and because `--update` WRITES -- pointing it at this
# repository would rewrite scripts/prose-baseline.txt as a side effect of
# running the tests.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-prose.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() {
  pass=$((pass + 1))
  printf 'ok   %s\n' "$1"
}

no() {
  fail=$((fail + 1))
  printf 'FAIL %s -- %s\n' "$1" "$2"
}

# A throwaway repository root with a scripts/ directory for the baseline.
# $1 = case name. Echoes the root path.
new_root() {
  dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  (cd "$dir" && git init -q .) >/dev/null 2>&1
  echo "$dir"
}

# $1 = root, $2 = relative path, and the body on stdin.
doc() {
  mkdir -p "$(dirname "$1/$2")"
  cat >"$1/$2"
  (cd "$1" && git add -A) >/dev/null 2>&1
}

# $1 = root, then `path pattern count` triples.
baseline() {
  root="$1"
  shift
  printf '# test baseline\n' >"$root/scripts/prose-baseline.txt"
  while [ $# -gt 2 ]; do
    printf '%s %s %s\n' "$1" "$2" "$3" >>"$root/scripts/prose-baseline.txt"
    shift 3
  done
}

# $1 = case name, $2 = root. The guard must exit 0.
expect_pass() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then
    ok "$1"
  else
    no "$1" "the guard exited non-zero"
  fi
}

# $1 = case name, $2 = root. The guard must exit non-zero.
expect_fail() {
  if python3 "$SCRIPT" "$2" >/dev/null 2>&1; then
    no "$1" "the guard exited 0"
  else
    ok "$1"
  fi
}

# $1 = case name, $2 = root. `--update` must refuse.
expect_update_refused() {
  if python3 "$SCRIPT" --update "$2" >/dev/null 2>&1; then
    no "$1" "--update exited 0"
  else
    ok "$1"
  fi
}

# $1 = case name, $2 = root, $3 = path that must NOT be in the baseline.
expect_absent() {
  if grep -q "^$3 " "$2/scripts/prose-baseline.txt" 2>/dev/null; then
    no "$1" "$3 is listed in the baseline"
  else
    ok "$1"
  fi
}

# $1 = case name, $2 = root, $3 = a line that must be in the baseline.
expect_line() {
  if grep -qx "$3" "$2/scripts/prose-baseline.txt" 2>/dev/null; then
    ok "$1"
  else
    no "$1" "the baseline has no line \"$3\""
  fi
}

# ── P1: clean prose passes ───────────────────────────────────────────────────
r="$(new_root p1)"
doc "$r" docs/a.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
baseline "$r"
expect_pass "P1 clean tree passes" "$r"

# ── P2: a first-time construction fails (the witness) ────────────────────────
r="$(new_root p2)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that, and the second is the hard one.
EOF
baseline "$r"
expect_fail "P2 first-time construction fails" "$r"

# ── P3: a grandfathered count passes ─────────────────────────────────────────
r="$(new_root p3)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
EOF
baseline "$r" docs/a.md enumerative-announcement 1
expect_pass "P3 grandfathered count passes" "$r"

# ── P4: growing past the baseline fails ──────────────────────────────────────
r="$(new_root p4)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
Both halves matter here.
EOF
baseline "$r" docs/a.md enumerative-announcement 1
expect_fail "P4 growth past baseline fails" "$r"

# ── P5: --update refuses to grandfather a first-time offender ────────────────
# Two assertions, because the defect this guards against failed both ways: a
# writer can refuse loudly and still write the entry.
r="$(new_root p5)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
EOF
baseline "$r"
expect_update_refused "P5 --update refuses to raise" "$r"
expect_absent "P5 --update wrote no entry" "$r" "docs/a.md"

# ── P6: --update retightens when prose is deleted ────────────────────────────
r="$(new_root p6)"
doc "$r" docs/a.md <<'EOF'
The order is required, not stylistic.
EOF
baseline "$r" docs/a.md empty-epigram 3
if python3 "$SCRIPT" --update "$r" >/dev/null 2>&1; then
  ok "P6 --update accepts a shrink"
else
  no "P6 --update accepts a shrink" "--update exited non-zero"
fi
expect_absent "P6 --update drops a file that reached zero" "$r" "docs/a.md"

# ── P7: a backticked citation is not a use ───────────────────────────────────
r="$(new_root p7)"
doc "$r" docs/a.md <<'EOF'
Never open a section with `Two things follow` or `Both halves matter`.
A span may also wrap, like `Two things
follow`, and still be a citation.
EOF
baseline "$r"
expect_pass "P7 backticked citation passes" "$r"

# ── P8: a fenced block is not prose ──────────────────────────────────────────
r="$(new_root p8)"
doc "$r" docs/a.md <<'EOF'
Bad openers, for reference:

```text
Two things follow from that.
Both halves matter.
```
EOF
baseline "$r"
expect_pass "P8 fenced block passes" "$r"

# ── P9: banned vocabulary fails ──────────────────────────────────────────────
# The witness for the vocabulary patterns: honesty declarations, interface
# jargon, and Rust terms in prose. Fails before those patterns exist, passes
# after.
r="$(new_root p9)"
doc "$r" docs/a.md <<'EOF'
The honest number is the one the TUI shows for each enum variant.
EOF
baseline "$r"
expect_fail "P9 banned vocabulary fails" "$r"

# ── P10: one file cannot trade one pattern's fix for another's growth ────────
# The witness for the per-pattern baseline. Under a single per-file total this
# tree passes: one construction was deleted and one was added, so the count is
# unchanged. Per pattern it fails, because `empty-epigram` grew from zero.
r="$(new_root p10)"
doc "$r" docs/a.md <<'EOF'
The order is required, not decoration.
EOF
baseline "$r" docs/a.md enumerative-announcement 1
expect_fail "P10 a swap between patterns fails" "$r"

# ── P11: --adopt records one pattern's debt and nothing else ─────────────────
# Adding a pattern to PATTERNS is the one case a count in the baseline
# legitimately goes up: the prose predates the check. --adopt is that door, and
# it must not touch any other pattern's number on the way through.
r="$(new_root p11)"
doc "$r" docs/a.md <<'EOF'
Two things follow from that.
The order is deliberately required.
EOF
baseline "$r" docs/a.md enumerative-announcement 5
if python3 "$SCRIPT" --adopt=filler-adverb "$r" >/dev/null 2>&1; then
  ok "P11 --adopt accepts a new pattern"
else
  no "P11 --adopt accepts a new pattern" "--adopt exited non-zero"
fi
expect_line "P11 --adopt records the new pattern" "$r" "docs/a.md filler-adverb 1"
expect_line "P11 --adopt leaves other patterns alone" "$r" \
  "docs/a.md enumerative-announcement 5"

# ── P12: --adopt refuses a pattern already in the baseline ──────────────────
# Once per pattern. A second adoption would be a raise wearing the name of the
# one legitimate exception.
r="$(new_root p12)"
doc "$r" docs/a.md <<'EOF'
The order is deliberately required, and deliberately documented.
EOF
baseline "$r" docs/a.md filler-adverb 1
if python3 "$SCRIPT" --adopt=filler-adverb "$r" >/dev/null 2>&1; then
  no "P12 --adopt refuses a second adoption" "--adopt exited 0"
else
  ok "P12 --adopt refuses a second adoption"
fi
expect_line "P12 --adopt wrote nothing" "$r" "docs/a.md filler-adverb 1"

# ── P13: --adopt rejects a name that is not a pattern ────────────────────────
r="$(new_root p13)"
doc "$r" docs/a.md <<'EOF'
The store keys every child table off `executions.id`.
EOF
baseline "$r"
if python3 "$SCRIPT" --adopt=no-such-pattern "$r" >/dev/null 2>&1; then
  no "P13 --adopt rejects an unknown pattern" "--adopt exited 0"
else
  ok "P13 --adopt rejects an unknown pattern"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
