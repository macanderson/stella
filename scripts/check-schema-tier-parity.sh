#!/usr/bin/env bash
#
# Guard: a `-schema` gate step must run at the same rung as its base step, and
# must not destroy the build cache that base step shares.
#
# GATE_STEPS has two such pairs today: `lint`/`lint-schema` and
# `doc-warnings`/`doc-warnings-schema`. `check` is a smaller rung than
# `gate`. It is what `GATE=fast git push` runs. Without this guard, a
# pair can split: `lint` runs at `check`, but `lint-schema` does not. So
# `cargo clippy` at that rung never sees the three schema-gated crates. A
# bad import there passes on this laptop and fails only in CI.
#
# This guard does not know what "clippy" or "rustdoc" means. It only knows
# that `X-schema` is the sibling of `X`. It checks that `check` runs both,
# or runs neither. Today `doc-warnings` and `doc-warnings-schema` are
# correctly both left out — `check` skips rustdoc entirely. And `lint` and
# `lint-schema` are correctly both included. A new pair that splits the
# same way fails here, before it ever reaches CI.
#
# It reads GATE_STEPS and CHECK_STEPS from the Makefile, through two
# `print-*` targets — the same way scripts/check-gate-parity.sh reads
# GATE_STEPS. That keeps this list derived, not a second hand-copied one.
#
# ## The second half: a shared cache is not a schema step's to wipe
#
# `doc-warnings-schema` has to clean rustdoc before it runs, because cargo's
# freshness check can call a stale `doc` unit up to date and print nothing
# (#5139). `cargo clean --doc` takes no package filter — cargo refuses
# `--doc` alongside `-p` — so an unscoped one empties the whole `target/doc`
# tree, including the workspace rustdoc `doc-warnings` had just built, and
# every `make gate` run pays a full rebuild for it (#5991).
#
# The scope cargo does offer is the target directory. So a recipe line here
# that runs `cargo clean --doc` must set its own `CARGO_TARGET_DIR` on the
# same line. This guard reads the Makefile's recipe lines for that, because
# the rule is invisible in the recipe it protects: dropping the environment
# variable leaves a command that still works and quietly costs everyone else
# their cache.
#
#   ./scripts/check-schema-tier-parity.sh
#
# Needs only `make` on PATH. No other toolchain, so it runs on a bare CI
# runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

fail=0

# The verdict is fixed before anything prints. A reader that closes the
# pipe early — `| head -1` — must not be able to turn a real failure into
# a clean exit. scripts/check-gate-parity.sh uses the same shape.
report=""
note() { report="${report}check-schema-tier-parity: $1"$'\n'; }
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if ! gate_steps="$(make -s print-gate-steps 2>/dev/null)"; then
  note "FAIL — could not read GATE_STEPS (\`make -s print-gate-steps\`)."
  note "     That target is what keeps this guard derived, not a"
  note "     second copy of the list. Restore it in the Makefile."
  emit
  exit 1
fi

if ! check_steps="$(make -s print-check-steps 2>/dev/null)"; then
  note "FAIL — could not read CHECK_STEPS (\`make -s print-check-steps\`)."
  note "     That target is what keeps this guard derived, not a"
  note "     second copy of the list. Restore it in the Makefile."
  emit
  exit 1
fi

# in_list <word> <space-separated list> — a whole-word membership test.
in_list() {
  case " $2 " in
  *" $1 "*) return 0 ;;
  *) return 1 ;;
  esac
}

for step in $gate_steps; do
  case "$step" in
  *-schema)
    base="${step%-schema}"
    # A `-schema` step with no base in GATE_STEPS has no pair to check.
    in_list "$base" "$gate_steps" || continue

    schema_at_check=0
    in_list "$step" "$check_steps" && schema_at_check=1
    base_at_check=0
    in_list "$base" "$check_steps" && base_at_check=1

    if [ "$schema_at_check" -ne "$base_at_check" ]; then
      if [ "$base_at_check" -eq 1 ]; then
        note "FAIL — '$base' runs at the 'check' rung but its schema sibling"
        note "     '$step' does not, so GATE=fast git push (and a hand-run"
        note "     'make check') can pass with the schema-gated crates never"
        note "     checked. Add '$step' to CHECK_STEPS in the Makefile, or"
        note "     say in AGENTS.md's rung table why this pair is allowed to"
        note "     split."
      else
        note "FAIL — '$step' runs at the 'check' rung but its base step"
        note "     '$base' does not. Drop '$step' from CHECK_STEPS, or add"
        note "     '$base' beside it."
      fi
      fail=1
    fi
    ;;
  esac
done

if [ "$fail" -ne 0 ]; then
  note ""
  note "A -schema step and its base step are one question, asked under two"
  note "feature configurations. 'check' must answer it the same way for"
  note "both. See AGENTS.md's four-rung table."
  emit
  exit 1
fi

# Recipe lines only — a line starting with a tab. The header comments above
# the recipe name the command in order to explain it, and a guard that read
# those would fire on its own documentation.
clean_unscoped=0
if [ -f Makefile ]; then
  while IFS= read -r line; do
    case "$line" in
    $'\t'*) ;;
    *) continue ;;
    esac
    case "$line" in
    *"cargo clean --doc"*) ;;
    *) continue ;;
    esac
    case "$line" in
    *CARGO_TARGET_DIR=*) continue ;;
    esac
    note "FAIL — this recipe line runs an unscoped rustdoc clean:"
    note ""
    note "      ${line#	}"
    note ""
    note "     \`cargo clean --doc\` takes no package filter, so without its own"
    note "     CARGO_TARGET_DIR it empties the \`target/doc\` tree that"
    note "     \`doc-warnings\` caches the workspace rustdoc in, and every gate"
    note "     run pays a full rebuild for it (#5991). Set CARGO_TARGET_DIR on"
    note "     the same line, as \`doc-warnings-schema\` does."
    clean_unscoped=1
  done <Makefile
fi

if [ "$clean_unscoped" -ne 0 ]; then
  emit
  exit 1
fi

emit
printf 'check-schema-tier-parity: OK — every schema-tier gate step agrees with its base step about the check rung, and cleans only the cache it owns.\n'
