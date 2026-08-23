#!/usr/bin/env bash
#
# Tests for the deck code-map path guard (#3573).
#
#   ./scripts/test-deck-paths.sh
#
# Hermetic: fixture decks in $TMPDIR, cited against this repository's real
# tree. Not part of `make gate` -- the guard itself is the gate step, and this
# is the harness that pins its directions, the same posture as
# `make prose-test` beside it.
#
# The fixture cannot live under website/public/presentations/: a deck there is
# measured by the real deck-fit job and read by the real guard, so a
# deliberately-dead path committed as a fixture would red-line both.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
subject="$repo_root/scripts/check-deck-paths.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

ok() { pass=$((pass + 1)); echo "ok   $1"; }
no() {
  fail=$((fail + 1))
  echo "FAIL $1"
  shift
  [ $# -gt 0 ] && printf '%s\n' "$@"
  return 0
}

out=""
rc=0
run() {
  out="$(python3 "$subject" "$1" 2>&1)"
  rc=$?
}

# ── A: everything the code map really contains, and all of it resolving ──────
#
# Both spellings, because the table uses both -- some `<td class="mono">` cells
# carry the `crates/` prefix and some do not. And both suffix forms, because
# some rows append a symbol or a note to the path.

A="$TMP/green/deck"
mkdir -p "$A"
cat >"$A/index.html" <<'EOF'
<table>
  <tr><td class="mono">stella-core/src/driver.rs</td></tr>
  <tr><td class="mono">crates/stella-core/src/loop_detect.rs</td></tr>
  <tr><td class="mono">crates/stella-cli/src/agent/prompt.rs::assemble_system_prompt</td></tr>
  <tr><td class="mono">stella-core/src/retry.rs &mdash; the backoff table</td></tr>
  <tr><td class="mono">crates/stella-runtime/src/wrapper/</td></tr>
</table>
<p>The dispatcher lives in stella-core/src/driver/dispatch.rs.</p>
EOF

run "$TMP/green"
if [ "$rc" -eq 0 ]; then
  ok "A1 every resolving citation passes, in both spellings"
else
  no "A1 every resolving citation passes, in both spellings" "$out"
fi

case "$out" in
*"6 cited path(s) resolve"*) ok "A2 all six rows were checked, not skipped" ;;
*) no "A2 all six rows were checked, not skipped" "$out" ;;
esac

# ── B: the bare spelling is normalised, not ignored ──────────────────────────
#
# The direction that would make this guard useless while staying green: treat
# an unprefixed path as unrecognised and check only the `crates/` rows. Two
# thirds of the code map is unprefixed.

B="$TMP/bare/deck"
mkdir -p "$B"
echo '<td class="mono">stella-core/src/no_such_module.rs</td>' >"$B/index.html"
run "$TMP/bare"
if [ "$rc" -ne 0 ]; then
  ok "B1 a dead path in the bare spelling fails"
else
  no "B1 a dead path in the bare spelling fails" "$out"
fi
case "$out" in
*"crates/stella-core/src/no_such_module.rs"*) ok "B2 the failure names the normalised path" ;;
*) no "B2 the failure names the normalised path" "$out" ;;
esac
case "$out" in
*"index.html:1"*) ok "B3 the failure names the deck and the line" ;;
*) no "B3 the failure names the deck and the line" "$out" ;;
esac

# ── C: the crates/ spelling ──────────────────────────────────────────────────

C="$TMP/prefixed/deck"
mkdir -p "$C"
echo '<td class="mono">crates/stella-pipeline/src/verify.rs</td>' >"$C/index.html"
run "$TMP/prefixed"
if [ "$rc" -ne 0 ]; then
  ok "C1 a dead path in the crates/ spelling fails"
else
  no "C1 a dead path in the crates/ spelling fails" "$out"
fi

# ── D: the suffixes the deck appends ─────────────────────────────────────────
#
# A `::symbol` or an em-dash note must not be read as part of the filename --
# that would make every annotated row a false failure, and a guard that cries
# wolf on its own subject gets deleted rather than fixed.

D="$TMP/suffixed/deck"
mkdir -p "$D"
cat >"$D/index.html" <<'EOF'
<td class="mono">crates/stella-core/src/driver.rs::run_turn</td>
<td class="mono">stella-core/src/ports.rs &mdash; the ToolExecutor seam</td>
EOF
run "$TMP/suffixed"
if [ "$rc" -eq 0 ]; then
  ok "D1 a ::symbol and an em-dash note do not corrupt the path"
else
  no "D1 a ::symbol and an em-dash note do not corrupt the path" "$out"
fi

# The same suffix stripping must not hide a dead path behind a live symbol.
D2="$TMP/suffix-dead/deck"
mkdir -p "$D2"
echo '<td class="mono">crates/stella-core/src/gone.rs::run_turn</td>' >"$D2/index.html"
run "$TMP/suffix-dead"
if [ "$rc" -ne 0 ]; then
  ok "D2 a dead path carrying a ::symbol still fails"
else
  no "D2 a dead path carrying a ::symbol still fails" "$out"
fi

# ── E: the walk ──────────────────────────────────────────────────────────────
#
# A deck given its own subdirectory is the natural choice once it carries
# assets, and it is exactly what #3376 taught this directory to expect.

E="$TMP/nested"
mkdir -p "$E/turn-loop/assets"
echo '<p>fine</p>' >"$E/index.html"
echo '<td class="mono">stella-core/src/vanished.rs</td>' >"$E/turn-loop/assets/deep.html"
run "$E"
if [ "$rc" -ne 0 ]; then
  ok "E1 the walk reaches a deck in a subdirectory"
else
  no "E1 the walk reaches a deck in a subdirectory" "$out"
fi

# ── F: the real decks ────────────────────────────────────────────────────────
#
# The guard landed green, which is the claim worth pinning: it was written
# against a tree where every cited path already resolved, so a failure here is
# a real regression rather than the guard's own arrival.

if out="$(python3 "$subject" 2>&1)"; then
  ok "F1 every path the shipped decks cite resolves"
else
  no "F1 every path the shipped decks cite resolves" "$out"
fi

echo
echo "passed $pass, failed $fail"
[ "$fail" -eq 0 ]
