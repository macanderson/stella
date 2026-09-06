#!/usr/bin/env bash
#
# Tests that scripts/check-schema-tier-parity.sh can still FAIL.
#
#   ./scripts/test-schema-tier-parity.sh
#
# A `-schema` step can drift out of step with its base step. One gets
# added to CHECK_STEPS, and the other is left out. Each case below is one
# way that drift could slip past the guard.
#
# Hermetic. Every case builds a throwaway tree with its own Makefile, one
# that exposes `print-gate-steps` and `print-check-steps`. It copies the
# real guard in, and runs it there. Nothing reads or writes this repo.
# That is why this is not a `make gate` step, the same posture as
# scripts/test-file-size.sh.
#
# Works on bash 3.2.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
guard="$repo_root/scripts/check-schema-tier-parity.sh"

pass=0
fail=0

tmp="$(mktemp -d "${TMPDIR:-/tmp}/stella-schema-tier-parity.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

# fixture <name> <gate-steps> <check-steps>.
# Builds a tree that exposes the two variables the guard reads.
fixture() {
  local dir="$tmp/$1" gate_steps="$2" check_steps="$3"
  rm -rf "$dir"
  mkdir -p "$dir/scripts"
  cp "$guard" "$dir/scripts/check-schema-tier-parity.sh"

  cat >"$dir/Makefile" <<EOF
GATE_STEPS := $gate_steps
CHECK_STEPS := $check_steps

print-gate-steps:
	@echo \$(GATE_STEPS)

print-check-steps:
	@echo \$(CHECK_STEPS)
EOF

  printf '%s' "$dir"
}

# expect <name> <wanted-exit> <dir> [needle].
# Runs the guard and checks its exit code, and its output when asked.
expect() {
  local name="$1" want="$2" dir="$3" needle="${4:-}"
  local out rc
  out="$(cd "$dir" && ./scripts/check-schema-tier-parity.sh 2>&1)"
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

# S1. Both members present at check. This must pass.
d="$(fixture both_present 'guard-a lint lint-schema doc-warnings doc-warnings-schema' \
                          'guard-a lint lint-schema')"
expect "S1  lint and lint-schema both at check passes" 0 "$d"

# S2. Both members absent from check. This must also pass.
d="$(fixture both_absent 'guard-a doc-warnings doc-warnings-schema' 'guard-a')"
expect "S2  doc-warnings and doc-warnings-schema both absent from check passes" 0 "$d"

# S3. The real bug. Base present, schema sibling forgotten.
d="$(fixture schema_forgotten 'guard-a lint lint-schema' 'guard-a lint')"
expect "S3  lint at check without lint-schema fails" 1 "$d" \
  "'lint' runs at the 'check' rung but its schema sibling"

# S4. The mirror case. Schema sibling present, base forgotten.
d="$(fixture base_forgotten 'guard-a lint lint-schema' 'guard-a lint-schema')"
expect "S4  lint-schema at check without lint fails" 1 "$d" \
  "'lint-schema' runs at the 'check' rung but its base step"

# S5. A schema step with no base sibling. The guard has no opinion here.
d="$(fixture orphan_schema 'guard-a orphan-schema' 'guard-a')"
expect "S5  a -schema step with no base in GATE_STEPS is ignored" 0 "$d"

# S6. Several pairs, only one broken. Every mismatch must be named.
d="$(fixture several_pairs 'lint lint-schema doc-warnings doc-warnings-schema' \
                           'lint doc-warnings-schema')"
out="$(cd "$d" && ./scripts/check-schema-tier-parity.sh 2>&1)"
rc=$?
if [ "$rc" -eq 1 ] \
  && printf '%s' "$out" | grep -qF "'lint' runs at the 'check' rung but its schema sibling" \
  && printf '%s' "$out" | grep -qF "'doc-warnings-schema' runs at the 'check' rung but its base step"; then
  echo "ok    S6  both mismatches in a mixed fixture are reported"
  pass=$((pass + 1))
else
  echo "FAIL  S6  expected both mismatches reported, got exit $rc:"
  printf '%s\n' "$out" | sed 's/^/      /'
  fail=$((fail + 1))
fi

# S7. No print-gate-steps target. The guard must fail, and say why.
d="$(fixture no_gate_target '' '')"
cat >"$d/Makefile" <<'EOF'
print-check-steps:
	@echo
EOF
expect "S7  no print-gate-steps target fails" 1 "$d" "could not read GATE_STEPS"

# S8. No print-check-steps target. Same rule, other side.
d="$(fixture no_check_target '' '')"
cat >"$d/Makefile" <<'EOF'
print-gate-steps:
	@echo lint lint-schema
EOF
expect "S8  no print-check-steps target fails" 1 "$d" "could not read CHECK_STEPS"

# ── The second half: the shared rustdoc cache ────────────────────────────────
#
# `cargo clean --doc` takes no package filter, so an unscoped one empties the
# whole `target/doc` tree — including the workspace rustdoc `doc-warnings`
# built minutes earlier, which every later gate run then rebuilds (`#5991`). The
# scope cargo does offer is the target directory, and dropping it leaves a
# command that still works, so nothing about the recipe shows the rule.

# recipe_fixture <name> <recipe body line> — a tree whose Makefile carries
# the two print targets the guard reads plus one recipe line to judge.
recipe_fixture() {
  local dir="$tmp/$1" body="$2"
  rm -rf "$dir"
  mkdir -p "$dir/scripts"
  cp "$guard" "$dir/scripts/check-schema-tier-parity.sh"
  # `$(GATE_STEPS)` is make's expansion, written into the fixture verbatim —
  # single quotes are what keep the shell out of it.
  # shellcheck disable=SC2016
  {
    printf 'GATE_STEPS := lint lint-schema\n'
    printf 'CHECK_STEPS := lint lint-schema\n\n'
    printf 'print-gate-steps:\n\t@echo $(GATE_STEPS)\n\n'
    printf 'print-check-steps:\n\t@echo $(CHECK_STEPS)\n\n'
    printf 'doc-warnings-schema:\n\t%s\n' "$body"
  } >"$dir/Makefile"
  printf '%s' "$dir"
}

# S9. The defect. This is the recipe as it stood before `#5991`.
d="$(recipe_fixture unscoped_clean 'cargo clean --doc')"
expect "S9  an unscoped 'cargo clean --doc' recipe line fails" 1 "$d" \
  "runs an unscoped rustdoc clean"

# S10. The fix. The same command, scoped to a directory this step owns.
d="$(recipe_fixture scoped_clean 'CARGO_TARGET_DIR=target/doc-schema cargo clean --doc')"
expect "S10 the same clean under its own CARGO_TARGET_DIR passes" 0 "$d"

# S11. The guard reads recipe lines, not the comments that explain them. A
# header naming the command in order to ban it must not fire the ban.
d="$(recipe_fixture commented_clean 'CARGO_TARGET_DIR=target/doc-schema cargo clean --doc')"
# shellcheck disable=SC2016  # a Makefile comment, written verbatim.
printf '# never run a bare `cargo clean --doc` here\n' >>"$d/Makefile"
expect "S11 a comment naming the command is not a recipe line" 0 "$d"

echo
echo "test-schema-tier-parity: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
