#!/usr/bin/env bash
#
# Tests for check-website-inputs.sh — the inventory that ties ci.yml's prose
# carve-out to what the Rust sources actually read (#4632).
#
#   ./scripts/test-website-inputs.sh
#
# Not part of `make gate`: it builds throwaway git repositories, the same
# posture as scripts/test-file-size.sh.
#
# ── Why this suite exists ────────────────────────────────────────────────────
#
# The guard's whole value is its failure directions, and each of the three is
# invisible when it stops working: an inventory that has quietly become
# incapable of failing reports the same green line as a healthy tree, and the
# next website-only rename lands on top of it.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-website-inputs.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway repository with the guard installed at the path it expects, one
# Rust source naming a website path, and that path present. $1 = case name.
new_repo() {
  local dir="$TMP/$1"
  mkdir -p "$dir/scripts" "$dir/crates/stella-cli/src" "$dir/website/content/docs/commands"
  cp "$SCRIPT" "$dir/scripts/check-website-inputs.sh"
  cat >"$dir/crates/stella-cli/src/tests.rs" <<'EOF'
const COMMANDS_DOCS_DIR: &str = "website/content/docs/commands";
EOF
  printf 'run\n' >"$dir/website/content/docs/commands/run.mdx"
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  echo "$dir"
}

# Overwrite the inventory. $1 = repo, then one "<kind> <path>" per entry.
set_inventory() {
  local dir="$1"
  shift
  {
    printf '# test inventory\n'
    local entry
    for entry in "$@"; do printf '%s\n' "$entry"; done
  } >"$dir/scripts/website-rust-inputs.txt"
}

# want <name> <expect-pass|expect-fail> <repo> [substring]
want() {
  local name="$1" expect="$2" dir="$3" sub="${4:-}" out rc
  git -C "$dir" add -A >/dev/null 2>&1
  out="$("$dir/scripts/check-website-inputs.sh" 2>&1)"
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
r="$(new_repo clean)"
set_inventory "$r" "read website/content/docs/commands/"
want "W1 a declared, present, still-named path passes" \
  expect-pass "$r" "check-website-inputs: OK"

# ── 1. A declared path that vanished (#4632's own shape) ─────────────────────
#
# The website-only rename this guard exists to catch, and the direction ci.yml
# cannot report: its Rust job is skipped for exactly this diff.
r="$(new_repo moved)"
set_inventory "$r" "read website/content/docs/commands/"
rm -rf "$r/website/content/docs/commands"
want "W2 a declared path that no longer exists is flagged" \
  expect-fail "$r" "which does not exist"

# ── 2. A Rust source naming a path nobody declared ───────────────────────────
#
# The forward direction: a new test that reads a website file must land in the
# inventory, because that is what puts it in ci.yml's carve-out.
r="$(new_repo undeclared)"
mkdir -p "$r/website/src/components"
printf 'x\n' >"$r/website/src/components/provider-catalog.ts"
cat >>"$r/crates/stella-cli/src/tests.rs" <<'EOF'
const CATALOG: &str = "website/src/components/provider-catalog.ts";
EOF
set_inventory "$r" "read website/content/docs/commands/"
want "W3 an undeclared website path a Rust source names is flagged" \
  expect-fail "$r" "provider-catalog.ts"

# ── 3. A stale entry ─────────────────────────────────────────────────────────
r="$(new_repo stale)"
mkdir -p "$r/website/content/docs/configuration"
printf 'x\n' >"$r/website/content/docs/configuration/stella-toml.mdx"
set_inventory "$r" \
  "read website/content/docs/commands/" \
  "read website/content/docs/configuration/stella-toml.mdx"
want "W4 an entry no Rust source names any more is flagged" \
  expect-fail "$r" "which no Rust source names"

# ── A mention covers itself and nothing under it ─────────────────────────────
#
# One line must never silence a directory: declaring the docs root as prose
# cannot be what lets an unread page pass for a page a test opens.
r="$(new_repo mention_is_narrow)"
mkdir -p "$r/website/content/docs/configuration"
printf 'x\n' >"$r/website/content/docs/configuration/stella-toml.mdx"
cat >>"$r/crates/stella-cli/src/tests.rs" <<'EOF'
/// See `website/content/docs/`.
const PAGE: &str = "website/content/docs/configuration/stella-toml.mdx";
EOF
set_inventory "$r" \
  "read website/content/docs/commands/" \
  "mention website/content/docs/"
want "W5 a mentioned directory does not cover the files under it" \
  expect-fail "$r" "stella-toml.mdx"

# ...and the same tree passes once the page is declared for what it is.
set_inventory "$r" \
  "read website/content/docs/commands/" \
  "mention website/content/docs/" \
  "read website/content/docs/configuration/stella-toml.mdx"
want "W6 declaring it clears W5" expect-pass "$r" "check-website-inputs: OK"

# ── The inventory itself ─────────────────────────────────────────────────────
r="$(new_repo unknown_kind)"
set_inventory "$r" "reads website/content/docs/commands/"
want "W7 an unknown kind is flagged rather than ignored" \
  expect-fail "$r" "unknown kind"

r="$(new_repo no_inventory)"
rm -f "$r/scripts/website-rust-inputs.txt"
want "W8 a missing inventory is a failure, not a skip" \
  expect-fail "$r" "does not exist"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
