#!/usr/bin/env bash
#
# Tests that scripts/check-gate-parity.sh can still FAIL (#3820, #4427).
#
#   ./scripts/test-gate-parity.sh
#
# The guard grew a third subject: not "do the two documents name every gate
# step?" but "does anything server-side RUN it?" — plus the same question for
# the hermetic self-test suites. That check is a grep over the workflows, and a
# grep is exactly the kind of thing that goes quietly permissive: widen the
# haystack by one line and a step named in a `paths:` filter, a YAML comment or
# a shell comment reads as a step that runs. The cases below are each a way the
# check could stop discriminating.
#
# Hermetic: every case builds a throwaway tree with its own Makefile, its own
# two documents and its own workflows, copies the real guard into it, and runs
# it there. Nothing reads or writes this repository. That is why this is not a
# `make gate` step, the same posture as scripts/test-file-size.sh.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
guard="$repo_root/scripts/check-gate-parity.sh"

pass=0
fail=0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/stella-gate-parity.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

# Ten steps, because the guard refuses a count its number_word() table cannot
# spell — the documents write the total in prose, so a fixture below that floor
# would fail for a reason no case here is about.
STEPS="alpha bravo charlie delta echo foxtrot golf hotel india juliett"

# fixture <name> — a tree the guard passes on. Each case then breaks one thing.
fixture() {
  local dir="$tmp/$1"
  rm -rf "$dir"
  mkdir -p "$dir/scripts" "$dir/.github/workflows"
  cp "$guard" "$dir/scripts/check-gate-parity.sh"

  cat >"$dir/Makefile" <<EOF
GATE_STEPS := $STEPS
UNHOSTED_SELF_TESTS :=

print-gate-steps:
	@echo \$(GATE_STEPS)

print-unhosted-self-tests:
	@echo \$(UNHOSTED_SELF_TESTS)
EOF

  {
    echo "### The gate"
    echo
    echo "Runs: $STEPS"
  } >"$dir/AGENTS.md"

  {
    echo "### The gate"
    echo
    echo '```bash'
    for s in $STEPS; do echo "./scripts/check-$s.sh"; done
    echo '```'
  } >"$dir/CONTRIBUTING.md"

  {
    echo "name: fixture"
    echo "on:"
    echo "  pull_request:"
    echo "jobs:"
    echo "  guards:"
    echo "    runs-on: ubuntu-latest"
    echo "    steps:"
    for s in $STEPS; do
      echo "      - name: $s"
      echo "        run: ./scripts/check-$s.sh"
    done
  } >"$dir/.github/workflows/fixture.yml"

  # One self-test suite, hosted, so the baseline is green on that half too.
  : >"$dir/scripts/test-hosted.sh"
  {
    echo "      - name: hosted self-test"
    echo "        run: ./scripts/test-hosted.sh"
  } >>"$dir/.github/workflows/fixture.yml"

  printf '%s' "$dir"
}

# expect <name> <wanted-exit> <dir> [needle]
expect() {
  local name="$1" want="$2" dir="$3" needle="${4:-}"
  local out rc
  out="$(cd "$dir" && ./scripts/check-gate-parity.sh 2>&1)"
  rc=$?
  if [ "$rc" -ne "$want" ]; then
    echo "FAIL  $name — exit $rc, wanted $want"
    printf '%s\n' "$out" | sed 's/^/      /'
    fail=$((fail + 1))
    return
  fi
  if [ -n "$needle" ] && ! printf '%s' "$out" | grep -qF -- "$needle"; then
    echo "FAIL  $name — report never says '$needle'"
    printf '%s\n' "$out" | sed 's/^/      /'
    fail=$((fail + 1))
    return
  fi
  echo "ok    $name"
  pass=$((pass + 1))
}

# ── G1  the fixture itself is green ──────────────────────────────────────────
d="$(fixture baseline)"
expect "G1  a tree where every step runs passes" 0 "$d"

# ── G2  a step no workflow runs ──────────────────────────────────────────────
d="$(fixture unrun)"
grep -v 'check-juliett.sh' "$d/.github/workflows/fixture.yml" >"$d/wf.tmp"
mv "$d/wf.tmp" "$d/.github/workflows/fixture.yml"
expect "G2  a step run by no workflow fails" 1 "$d" "no workflow runs the gate step 'juliett'"

# ── G3  named only by a paths: filter ────────────────────────────────────────
#
# The discrimination that matters most. wire-schema.yml lists its own guard
# under `paths:` — it watches the file, it does not run it — and a grep over
# whole lines would call that hosted.
d="$(fixture paths_only)"
sed 's|        run: ./scripts/check-juliett.sh|        # watched, not run|' \
  "$d/.github/workflows/fixture.yml" >"$d/wf.tmp"
mv "$d/wf.tmp" "$d/.github/workflows/fixture.yml"
{
  echo "on:"
  echo "  pull_request:"
  echo "    paths:"
  echo '      - "scripts/check-juliett.sh"'
} >>"$d/.github/workflows/fixture.yml"
expect "G3  a step named only in a paths: filter fails" 1 "$d" "no workflow runs the gate step 'juliett'"

# ── G4  named only in a YAML comment ─────────────────────────────────────────
d="$(fixture yaml_comment)"
sed 's|        run: ./scripts/check-juliett.sh|        # ./scripts/check-juliett.sh used to run here|' \
  "$d/.github/workflows/fixture.yml" >"$d/wf.tmp"
mv "$d/wf.tmp" "$d/.github/workflows/fixture.yml"
expect "G4  a step named only in a YAML comment fails" 1 "$d" "no workflow runs the gate step 'juliett'"

# ── G5  named only in a shell comment inside a run: block ────────────────────
d="$(fixture shell_comment)"
sed 's|        run: ./scripts/check-juliett.sh|        run: \|\n          # ./scripts/check-juliett.sh — retired\n          true|' \
  "$d/.github/workflows/fixture.yml" >"$d/wf.tmp"
mv "$d/wf.tmp" "$d/.github/workflows/fixture.yml"
expect "G5  a step named only in a shell comment fails" 1 "$d" "no workflow runs the gate step 'juliett'"

# ── G6  a run: block scalar counts ───────────────────────────────────────────
d="$(fixture block_scalar)"
sed 's|        run: ./scripts/check-juliett.sh|        run: \|\n          ./scripts/check-juliett.sh|' \
  "$d/.github/workflows/fixture.yml" >"$d/wf.tmp"
mv "$d/wf.tmp" "$d/.github/workflows/fixture.yml"
expect "G6  a step run inside a block scalar passes" 0 "$d"

# ── G7/G8  a self-test suite in neither place, then excluded out loud ────────
d="$(fixture orphan_suite)"
: >"$d/scripts/test-orphan.sh"
expect "G7  a self-test in no workflow fails" 1 "$d" "no workflow runs the guard self-test 'scripts/test-orphan.sh'"

sed 's|^UNHOSTED_SELF_TESTS :=$|UNHOSTED_SELF_TESTS := test-orphan.sh|' "$d/Makefile" >"$d/mk.tmp"
mv "$d/mk.tmp" "$d/Makefile"
expect "G8  naming it in UNHOSTED_SELF_TESTS passes" 0 "$d"

# ── G9  an exclusion for a suite that does not exist ─────────────────────────
d="$(fixture ghost_exclusion)"
sed 's|^UNHOSTED_SELF_TESTS :=$|UNHOSTED_SELF_TESTS := test-vanished.sh|' "$d/Makefile" >"$d/mk.tmp"
mv "$d/mk.tmp" "$d/Makefile"
expect "G9  an exclusion for a missing suite fails" 1 "$d" "which does not exist"

# ── G10  an exclusion for a suite a workflow does run ────────────────────────
d="$(fixture stale_exclusion)"
sed 's|^UNHOSTED_SELF_TESTS :=$|UNHOSTED_SELF_TESTS := test-hosted.sh|' "$d/Makefile" >"$d/mk.tmp"
mv "$d/mk.tmp" "$d/Makefile"
expect "G10 a stale exclusion fails" 1 "$d" "the exclusion is stale"

# ── G11  the Makefile stops answering ────────────────────────────────────────
d="$(fixture no_target)"
grep -v 'print-unhosted-self-tests' "$d/Makefile" >"$d/mk.tmp"
mv "$d/mk.tmp" "$d/Makefile"
expect "G11 a missing print target fails" 1 "$d" "could not read UNHOSTED_SELF_TESTS"

# ── G12  the prose checks still work ─────────────────────────────────────────
d="$(fixture prose)"
grep -v '^Runs:' "$d/AGENTS.md" >"$d/ag.tmp"
mv "$d/ag.tmp" "$d/AGENTS.md"
expect "G12 a document that omits every step still fails" 1 "$d" "never mentions the gate step"

# ── G13  a reader that closes the pipe cannot change the verdict ─────────────
#
# The guard buffers its report and ignores SIGPIPE for exactly this (#1815);
# a red verdict truncated by `| head -1` must stay red.
d="$(fixture sigpipe)"
grep -v 'check-juliett.sh' "$d/.github/workflows/fixture.yml" >"$d/wf.tmp"
mv "$d/wf.tmp" "$d/.github/workflows/fixture.yml"
sig_fail=0
for _ in 1 2 3 4 5; do
  (cd "$d" && ./scripts/check-gate-parity.sh 2>&1 | head -1 >/dev/null)
  [ "${PIPESTATUS[0]}" -eq 1 ] || sig_fail=1
done
if [ "$sig_fail" -eq 0 ]; then
  echo "ok    G13 a truncated reader still gets exit 1"
  pass=$((pass + 1))
else
  echo "FAIL  G13 a truncated reader changed the verdict"
  fail=$((fail + 1))
fi

# ── G14  a green verdict is stable across repeats ────────────────────────────
#
# The workflow scan asks the same haystack a question per step. Held in a
# variable and piped into `grep -q`, that read is a race: grep exits on its
# first match, the writer takes a SIGPIPE, and `pipefail` reports 141 for a
# pipeline that matched — intermittently, and only once the haystack is big
# enough for the writer to still be writing. This fixture's workflow is padded
# past that threshold, and every run must agree.
d="$(fixture repeats)"
i=0
while [ "$i" -lt 2000 ]; do
  echo "      - run: ./scripts/filler-$i.sh"
  i=$((i + 1))
done >>"$d/.github/workflows/fixture.yml"
flap=0
for _ in 1 2 3 4 5 6 7 8 9 10; do
  (cd "$d" && ./scripts/check-gate-parity.sh >/dev/null 2>&1) || flap=1
done
if [ "$flap" -eq 0 ]; then
  echo "ok    G14 a green verdict survives a large workflow set, ten times"
  pass=$((pass + 1))
else
  echo "FAIL  G14 a green verdict flapped across repeats"
  fail=$((fail + 1))
fi

echo
echo "test-gate-parity: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
