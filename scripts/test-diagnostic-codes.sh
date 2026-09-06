#!/usr/bin/env bash
#
# Tests for check-diagnostic-codes.sh — the registry that holds
# docs/reference/diagnostics.md and website/content/docs/diagnostics.mdx to
# the codes the tree actually emits (#2507, #3045, #4948).
#
#   ./scripts/test-diagnostic-codes.sh
#
# Not part of `make gate`: it builds throwaway fixture trees, the same posture
# as scripts/test-website-inputs.sh.
#
# ── Why this suite exists ────────────────────────────────────────────────────
#
# The guard has six failure directions and had a self-test for none of them.
# Every one is invisible when it stops working: a registry that has quietly
# become incapable of failing prints the same green line as a healthy tree, and
# the next undocumented code lands on top of it. #4947 widened what the guard
# covers on four ablations run by hand in a pull-request body, which is a
# session's evidence rather than a standing property.
#
# The green case is here for the opposite error: without it the suite would
# prove only that the script can fail, which a `exit 1` would satisfy.
#
# ── The fixture ──────────────────────────────────────────────────────────────
#
# A tree with `crates/<name>/src/*.rs`, `docs/reference/diagnostics.md` and
# `website/content/docs/diagnostics.mdx`, which is everything the guard reads.
# The reference is GENERATED into the fixture by the script's own `write` mode
# rather than pasted here: its header and facts-block format are the script's,
# so a hand-written copy would make this suite fail on an unrelated edit to
# either.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
GUARD="$repo_root/scripts/check-diagnostic-codes.sh"
GENERATOR="$repo_root/scripts/diagnostic-codes.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# The prose each generated section gets, replacing the placeholder. One line,
# so the substitution below is a line rewrite rather than a block edit.
PROSE="Fixture prose, so the section is not the placeholder."

# A fixture tree whose reference and website table both agree with its sources.
# $1 = case name; echoes the tree root.
#
# Two codes, because several directions need one code left alone while another
# is broken. `warn` and `error` differ, so a level swap is a real change.
new_tree() {
  local dir="$TMP/$1"
  mkdir -p "$dir/crates/fixture-one/src" "$dir/website/content/docs"
  cat >"$dir/crates/fixture-one/src/lib.rs" <<'EOF'
pub fn slow(dx: &Dx) {
    diag!(dx, warn, "fixture.thing.slow", elapsed_ms = 1);
}

pub fn broken(dx: &Dx) {
    diag!(dx, error, "fixture.thing.broken");
}
EOF
  write_website "$dir" '## Reading the codes' 'fixture.thing.slow' 'warn'
  regenerate "$dir"
  echo "$dir"
}

# The website page: an unrelated leading table (the real page has one, and the
# anchor is what tells them apart), then the curated table under $2, naming
# code $3 at level $4.
write_website() {
  local dir="$1" anchor="$2" code="$3" level="$4"
  cat >"$dir/website/content/docs/diagnostics.mdx" <<EOF
# Diagnostics

| Plane | Where |
| --- | --- |
| records | the terminal |

$anchor

| Code | Level | What it means |
| --- | --- | --- |
| \`$code\` | $level | Something a reader arrived here about. |
EOF
}

# Generate the reference from the fixture's sources, then write real prose into
# every section — a section left as the placeholder is one of the directions
# under test, so the healthy tree must not carry one.
regenerate() {
  local dir="$1"
  python3 "$GENERATOR" write "$dir" >/dev/null
  local ref="$dir/docs/reference/diagnostics.md"
  sed "s/^_TODO: .*_\$/$PROSE/" "$ref" >"$ref.next" && mv "$ref.next" "$ref"
}

# want <name> <expect-pass|expect-fail> <tree> [substring]
want() {
  local name="$1" expect="$2" dir="$3" sub="${4:-}" out rc
  out="$("$GUARD" "$dir" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ] && [ "$rc" -ne 0 ]; then
    fail=$((fail + 1))
    echo "FAIL $name — expected OK, got:"
    echo "$out"
    return
  fi
  if [ "$expect" = "expect-fail" ] && [ "$rc" -eq 0 ]; then
    fail=$((fail + 1))
    echo "FAIL $name — the guard passed something it should have flagged:"
    echo "$out"
    return
  fi
  case "$out" in
  *"$sub"*)
    pass=$((pass + 1))
    echo "ok   $name"
    ;;
  *)
    fail=$((fail + 1))
    echo "FAIL $name — verdict was right, report was not (wanted '$sub'):"
    echo "$out"
    ;;
  esac
}

# ── The healthy tree ─────────────────────────────────────────────────────────
#
# Without this the suite would prove the script can fail and nothing else.
t="$(new_tree clean)"
want "D0 a tree whose reference and table both agree passes" \
  expect-pass "$t" "check-diagnostic-codes: OK"

# ── 1. A code emitted with no section in the reference ───────────────────────
#
# The direction the registry exists for: public surface shipping undocumented.
t="$(new_tree undocumented)"
cat >>"$t/crates/fixture-one/src/lib.rs" <<'EOF'

pub fn fresh(dx: &Dx) {
    diag!(dx, info, "fixture.thing.fresh");
}
EOF
want "D1 a code with no section is flagged" \
  expect-fail "$t" "\`fixture.thing.fresh\` is emitted"

# ── 2. A section for a code nothing emits ────────────────────────────────────
#
# The other direction: a deleted emit site leaves its section behind, reading
# as authority.
t="$(new_tree orphan_section)"
cat >>"$t/docs/reference/diagnostics.md" <<EOF

### \`fixture.thing.retired\`

$PROSE
EOF
want "D2 a section for a code nothing emits is flagged" \
  expect-fail "$t" "which nothing emits any more"

# ── 3. Two crates claiming one code ──────────────────────────────────────────
#
# A facet vocabulary has one owner (doc:diagnostics §5.1).
t="$(new_tree two_owners)"
mkdir -p "$t/crates/fixture-two/src"
cat >"$t/crates/fixture-two/src/lib.rs" <<'EOF'
pub fn also_slow(dx: &Dx) {
    diag!(dx, warn, "fixture.thing.slow");
}
EOF
want "D3 one code emitted by two crates is flagged" \
  expect-fail "$t" "is emitted by more than one crate"

# ── 4. A section whose prose is still the placeholder ────────────────────────
#
# A generated section with nothing written under it documents nothing; the
# registry would otherwise count it as covered.
t="$(new_tree placeholder)"
sed "s/^$PROSE\$/_TODO: say what this code means and what to do about it._/" \
  "$t/docs/reference/diagnostics.md" >"$t/ref.next" &&
  mv "$t/ref.next" "$t/docs/reference/diagnostics.md"
want "D4 a section still reading as the placeholder is flagged" \
  expect-fail "$t" "is still the placeholder"

# ── 5. Stale generated facts ─────────────────────────────────────────────────
#
# The facts block is generated from the emit site, so a hand-edit to it is a
# claim about the tree that the tree does not make. Edited in the REFERENCE
# rather than in the source, so this direction fires alone: moving the source's
# level would also move the website table's answer and flag direction 6a too.
t="$(new_tree stale_facts)"
sed 's/^- \*\*Level:\*\* .*$/- **Level:** whatever a hand says/' \
  "$t/docs/reference/diagnostics.md" >"$t/ref.next" &&
  mv "$t/ref.next" "$t/docs/reference/diagnostics.md"
want "D5 a hand-edited facts block is flagged as stale" \
  expect-fail "$t" "generated facts are stale"

# ── 6a. The website's level cell no longer matches the emit site ─────────────
#
# #3045's shape: the cell was transcribed by hand and read by no gate, so a
# level change left the page confidently wrong with everything green.
t="$(new_tree website_level)"
write_website "$t" '## Reading the codes' 'fixture.thing.slow' 'info'
want "D6a a website level cell the tree contradicts is flagged" \
  expect-fail "$t" "the tree emits it at warn"

# ── 6b. The website surfaces a code the tree no longer emits ─────────────────
t="$(new_tree website_gone)"
write_website "$t" '## Reading the codes' 'fixture.thing.gone' 'warn'
want "D6b a website code nothing emits is flagged" \
  expect-fail "$t" "documents \`fixture.thing.gone\`"

# ── 6c. The anchor the table is located by is gone ───────────────────────────
#
# The arm that stops the guard going quiet when the page is restructured: a
# guard that reports "nothing to check" is indistinguishable from one that
# checked and found nothing.
t="$(new_tree website_anchor)"
write_website "$t" '## How to read a code' 'fixture.thing.slow' 'warn'
want "D6c a renamed anchor is a failure, not a skip" \
  expect-fail "$t" "cannot find the table"

# ── 6d. The table under the anchor names no codes ────────────────────────────
t="$(new_tree website_empty)"
cat >"$t/website/content/docs/diagnostics.mdx" <<'EOF'
# Diagnostics

## Reading the codes

A record's code is stable, versioned, public surface.
EOF
want "D6d an emptied table is a failure, not a skip" \
  expect-fail "$t" "emptied or restructured"

# ── 7. A partly-opaque emit expression is hedged, not silently short ────────
#
# The generator's `field_names` set its "fields built at runtime" flag only
# when NO name was found at all. A call that mixes a helper this scanner
# cannot see inside with visible `.with(...)`s -- `helper(dx.at_seq(), class,
# confidence).with("decays", decays)` -- found `decays` (and `seq`, from the
# `at_seq()` substring check) and stopped there: `class` and `confidence`
# vanished from the generated reference with no hedge warning a reader that
# the list is short. This exercises `write` mode directly rather than
# `check-diagnostic-codes.sh`, because the defect is in what gets generated,
# not in whether a committed file matches it.
mixed_tree() {
  local dir="$TMP/mixed"
  mkdir -p "$dir/crates/fixture-one/src"
  cat >"$dir/crates/fixture-one/src/lib.rs" <<'EOF'
pub fn logged(dx: &Dx, class: &str, confidence: u32, decays: bool) {
    dx.emit(
        Level::Debug,
        "fixture.thing.mixed",
        helper(dx.at_seq(), class, confidence).with("decays", decays),
    );
}
EOF
  echo "$dir"
}

t="$(mixed_tree)"
python3 "$GENERATOR" write "$t" >/dev/null
fields_line="$(grep '\*\*Fields:\*\*' "$t/docs/reference/diagnostics.md")"
case "$fields_line" in
*"seq"*"decays"*"plus fields built at runtime"*)
  pass=$((pass + 1))
  echo "ok   D9 a partly-opaque emit expression is hedged, not silently short"
  ;;
*)
  fail=$((fail + 1))
  echo "FAIL D9 a partly-opaque emit expression is hedged, not silently short — got:"
  echo "$fields_line"
  ;;
esac

# ── The two files themselves ─────────────────────────────────────────────────
#
# Both fail closed. A registry whose reference has been deleted has documented
# every code vacuously, which is the worst possible green.
t="$(new_tree no_reference)"
rm -f "$t/docs/reference/diagnostics.md"
want "D7 a missing reference is a failure, not a skip" \
  expect-fail "$t" "does not exist"

t="$(new_tree no_website)"
rm -f "$t/website/content/docs/diagnostics.mdx"
want "D8 a missing website page is a failure, not a skip" \
  expect-fail "$t" "if the page moved, move this check with it"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
