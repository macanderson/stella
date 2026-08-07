#!/usr/bin/env bash
#
# Tests for check-module-reachability.py (#1750).
#
#   ./scripts/test-module-reachability.sh
#
# Run it after touching that script. Not part of `make gate`: it builds a
# dozen throwaway git repositories, the same posture as `make impacted-test`.
#
# ── Why a fixture instead of the real workspace ──────────────────────────────
#
# Asserting against this repository alone would only ever prove the tree is
# currently clean — which is the one thing the guard says out loud anyway. What
# needs proving is that it FAILS on an orphan, and there is no orphan in the
# tree to fail on. So each case builds the shape it is about.
#
# ── The two ways this guard can be wrong, and which is worse ─────────────────
#
#   misses     an orphaned file is reported reachable. The file's tests never
#              run and the suite says ok — the exact #1750 defect.
#   fabricates a reachable file is reported orphaned. Worse in practice: it
#              fails the gate on a healthy tree, and the reader's only recourse
#              is to distrust the guard.
#
# The `S` cases are the second kind, and they are not hypothetical — the first
# version of this guard fabricated three orphans in stella-cli because a string
# literal containing `protected/**` opened a block comment that never closed.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/check-module-reachability.py"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway one-crate workspace, tracked in git (the guard asks git which
# files are tracked, so an untracked fixture would look empty and pass).
new_crate() { # <case>
  local dir="$TMP/$1/mycrate"
  mkdir -p "$dir/src"
  printf '[package]\nname = "mycrate"\nversion = "0.0.0"\nedition = "2021"\n' >"$dir/Cargo.toml"
  git -C "$TMP/$1" init -q
  git -C "$TMP/$1" config user.email t@t.invalid
  git -C "$TMP/$1" config user.name t
  echo "$dir"
}

track() { git -C "$1" add -A; }

# want <name> <expect-pass|expect-fail> <workspace-root> [substring]
want() {
  local name="$1" expect="$2" root="$3" sub="${4:-}" out rc
  out="$(python3 "$SCRIPT" "$root" 2>&1)"
  rc=$?
  if [ "$expect" = "expect-pass" ]; then
    if [ "$rc" -eq 0 ]; then
      pass=$((pass + 1)); echo "ok   $name"
    else
      fail=$((fail + 1)); echo "FAIL $name — expected OK, got:"; echo "$out"
    fi
    return
  fi
  if [ "$rc" -eq 0 ]; then
    fail=$((fail + 1)); echo "FAIL $name — the guard passed an orphan:"; echo "$out"
    return
  fi
  case "$out" in
    *"$sub"*) pass=$((pass + 1)); echo "ok   $name" ;;
    *) fail=$((fail + 1)); echo "FAIL $name — failed for the wrong reason (wanted '$sub'):"; echo "$out" ;;
  esac
}

# ── O: the defect ────────────────────────────────────────────────────────────
c="$(new_crate orphan)"
printf 'pub fn a() {}\n' >"$c/src/lib.rs"
printf '#[test]\nfn never_runs() { assert!(false); }\n' >"$c/src/orphan.rs"
track "$TMP/orphan"
want "O1 a file no mod reaches is reported" expect-fail "$TMP/orphan" "src/orphan.rs"

# The #1739 shape exactly: a test module whose declaration was deleted.
c="$(new_crate dropped_tests)"
printf 'pub fn a() {}\n' >"$c/src/lib.rs"
mkdir -p "$c/src/store"
printf 'pub fn s() {}\n' >"$c/src/store.rs"
printf '#[test]\nfn t() {}\n' >"$c/src/store/tests.rs"
track "$TMP/dropped_tests"
want "O2 a store.rs missing its 'mod tests;' is reported" \
  expect-fail "$TMP/dropped_tests" "src/store/tests.rs"

# ── N: the shapes that must NOT be reported ──────────────────────────────────
c="$(new_crate flat)"
printf 'mod foo;\n' >"$c/src/lib.rs"
printf 'pub fn f() {}\n' >"$c/src/foo.rs"
track "$TMP/flat"
want "N1 mod foo; -> src/foo.rs is reachable" expect-pass "$TMP/flat"

c="$(new_crate nested)"
printf 'mod foo;\n' >"$c/src/lib.rs"
mkdir -p "$c/src/foo"
printf 'mod bar;\n' >"$c/src/foo.rs"
printf 'pub fn b() {}\n' >"$c/src/foo/bar.rs"
track "$TMP/nested"
want "N2 a nested module under its parent's directory is reachable" expect-pass "$TMP/nested"

c="$(new_crate modrs)"
printf 'mod foo;\n' >"$c/src/lib.rs"
mkdir -p "$c/src/foo"
printf 'pub fn f() {}\n' >"$c/src/foo/mod.rs"
track "$TMP/modrs"
want "N3 the mod.rs form is reachable" expect-pass "$TMP/modrs"

# A cfg-gated declaration still makes the file reachable. Treating it otherwise
# would report every test module in the tree.
c="$(new_crate cfgtest)"
printf 'pub fn a() {}\n\n#[cfg(test)]\nmod tests;\n' >"$c/src/lib.rs"
printf '#[test]\nfn t() {}\n' >"$c/src/tests.rs"
track "$TMP/cfgtest"
want "N4 #[cfg(test)] mod tests; is reachable" expect-pass "$TMP/cfgtest"

c="$(new_crate cfgtest_inline)"
printf 'pub fn a() {}\n#[cfg(test)] mod tests;\n' >"$c/src/lib.rs"
printf '#[test]\nfn t() {}\n' >"$c/src/tests.rs"
track "$TMP/cfgtest_inline"
want "N5 an attribute on the same line as the declaration is reachable" expect-pass "$TMP/cfgtest_inline"

# src/bin/*.rs is Cargo auto-discovery — each is its own root, not a module of
# anything. A walker that missed that reports every one as an orphan.
c="$(new_crate autobin)"
printf 'pub fn a() {}\n' >"$c/src/lib.rs"
mkdir -p "$c/src/bin"
printf 'fn main() {}\n' >"$c/src/bin/tool.rs"
track "$TMP/autobin"
want "N6 src/bin/*.rs is a root, not an orphan" expect-pass "$TMP/autobin"

c="$(new_crate pubmod)"
printf 'pub(crate) mod foo;\n' >"$c/src/lib.rs"
printf 'pub fn f() {}\n' >"$c/src/foo.rs"
track "$TMP/pubmod"
want "N7 pub(crate) mod is reachable" expect-pass "$TMP/pubmod"

# ── S: fabricated orphans — the regression that shipped in the first draft ───
# `protected/**` inside a string literal opened a block comment that never
# closed, blanking the rest of the file and hiding every declaration after it.
c="$(new_crate string_slashstar)"
{
  printf 'pub const GUARD: &str = "guard-deny-path: protected/**";\n'
  printf 'mod foo;\n'
} >"$c/src/lib.rs"
printf 'pub fn f() {}\n' >"$c/src/foo.rs"
track "$TMP/string_slashstar"
want "S1 a '/*' inside a string literal does not blank the rest of the file" \
  expect-pass "$TMP/string_slashstar"

c="$(new_crate raw_string)"
{
  printf 'pub const G: &str = r#"a /* b "quoted" c"#;\n'
  printf 'mod foo;\n'
} >"$c/src/lib.rs"
printf 'pub fn f() {}\n' >"$c/src/foo.rs"
track "$TMP/raw_string"
want "S2 the same through a raw string with an embedded quote" expect-pass "$TMP/raw_string"

# The other direction: a real comment must still be stripped, so a `mod` named
# only in prose does not count as a declaration.
c="$(new_crate comment_mod)"
{
  printf '// Historically this file carried mod ghost; — it does not now.\n'
  printf '/* mod ghost; */\n'
  printf 'pub fn a() {}\n'
} >"$c/src/lib.rs"
printf 'pub fn g() {}\n' >"$c/src/ghost.rs"
track "$TMP/comment_mod"
want "S3 a mod named only in a comment does not count as a declaration" \
  expect-fail "$TMP/comment_mod" "src/ghost.rs"

# ── P: the shape the guard refuses to guess about ────────────────────────────
c="$(new_crate pathattr)"
printf '#[path = "elsewhere/thing.rs"]\nmod thing;\n' >"$c/src/lib.rs"
mkdir -p "$c/src/elsewhere"
printf 'pub fn t() {}\n' >"$c/src/elsewhere/thing.rs"
track "$TMP/pathattr"
want "P1 #[path] fails loudly rather than answering wrongly" \
  expect-fail "$TMP/pathattr" "does not model"

# ── R: the real workspace ────────────────────────────────────────────────────
want "R1 this repository is clean" expect-pass "$repo_root"

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
