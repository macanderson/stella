#!/usr/bin/env bash
#
# Tests for ci-rust-scope.sh — the one place that decides whether a diff runs
# the Rust gate (#4606, #4632).
#
#   ./scripts/test-ci-rust-scope.sh
#
# Not part of `make gate`: it builds throwaway git repositories, the same
# posture as scripts/test-file-size.sh.
#
# ── Why this suite exists ────────────────────────────────────────────────────
#
# The rule this script implements decides whether the required Rust context
# runs at all. When it answers wrongly in the cheap direction nothing reports:
# the job is skipped, the PR is green, and the failure surfaces on somebody
# else's branch hours later. #4588 is that shape, and it cost four consecutive
# red commits on `main`.
#
# The decision used to live in a YAML `run:` block, where nothing could
# exercise it, beside a `paths-ignore` list a comment asked a human to keep in
# agreement with it. It is one script now, and this suite is what exercises it.
#
# bash 3.2 compatible.

set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
SCRIPT="$repo_root/scripts/ci-rust-scope.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

pass=0
fail=0

# A throwaway repository with the script installed at the path it expects (it
# derives the repo root from its own location) and an inventory declaring one
# read path and one mention. $1 = case name. Echoes the repo path.
new_repo() {
  local dir="$TMP/$1"
  mkdir -p "$dir/scripts"
  cp "$SCRIPT" "$dir/scripts/ci-rust-scope.sh"
  cat >"$dir/scripts/website-rust-inputs.txt" <<'EOF'
# test inventory
read website/content/docs/commands/
read website/src/components/provider-catalog.ts
mention website/content/docs/scripting.mdx
EOF
  git -C "$dir" init -q
  git -C "$dir" config user.email t@t.invalid
  git -C "$dir" config user.name t
  printf 'seed\n' >"$dir/seed.txt"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m base
  echo "$dir"
}

# $1 = repo, then paths to create and commit as one change.
change() {
  local dir="$1"
  shift
  local p
  for p in "$@"; do
    mkdir -p "$(dirname "$dir/$p")"
    printf 'x\n' >>"$dir/$p"
  done
  git -C "$dir" add -A
  git -C "$dir" commit -q -m change
}

# want <name> <true|false> <repo> [base]
#
# `${4-...}` and not `${4:-...}`: O3 passes an EMPTY base on purpose, and the
# colon form would substitute the default for it and test HEAD^ instead.
want() {
  local name="$1" expect="$2" dir="$3" base="${4-HEAD^}" got
  got="$("$dir/scripts/ci-rust-scope.sh" "$base" HEAD 2>/dev/null)"
  if [ "$got" = "$expect" ]; then
    pass=$((pass + 1))
    echo "ok   $name"
  else
    fail=$((fail + 1))
    echo "FAIL $name — wanted '$expect', got '$got'"
  fi
}

# ── The prose paths skip ─────────────────────────────────────────────────────
r="$(new_repo prose_docs)"
change "$r" "docs/spec/thing.md"
want "P1 a docs-only diff is prose" false "$r"

r="$(new_repo prose_root_md)"
change "$r" "README.md"
want "P2 a root-level *.md is prose" false "$r"

r="$(new_repo prose_website)"
change "$r" "website/src/app/page.tsx"
want "P3 an ordinary website file is prose" false "$r"

r="$(new_repo prose_issue_template)"
change "$r" ".github/ISSUE_TEMPLATE/bug.yml"
want "P4 an issue template is prose" false "$r"

# ── ...and what is not prose ─────────────────────────────────────────────────
r="$(new_repo rust)"
change "$r" "crates/stella-core/src/lib.rs"
want "R1 a crate diff runs the gate" true "$r"

# A crate's own README is not a root-level *.md, which is the distinction the
# `[^/]+\.md$` anchor exists to make.
r="$(new_repo crate_readme)"
change "$r" "crates/stella-core/README.md"
want "R2 a crate README runs the gate" true "$r"

r="$(new_repo mixed)"
change "$r" "docs/spec/thing.md" "crates/stella-core/src/lib.rs"
want "R3 prose mixed with code runs the gate" true "$r"

# ── The carve-out: website paths a Rust test reads (#4632) ───────────────────
#
# C1 is the witness for #4588's shape: a diff confined to website/, touching
# nothing but the file `docs_sync` reads. It skipped the gate, and `main` was
# red on a test nobody had run.
r="$(new_repo carve_file)"
change "$r" "website/src/components/provider-catalog.ts"
want "C1 a website file a Rust test reads runs the gate" true "$r"

r="$(new_repo carve_dir)"
change "$r" "website/content/docs/commands/run.mdx"
want "C2 a file under a read directory runs the gate" true "$r"

# The carve-out must stay exactly as wide as the inventory. A `mention` is
# prose: nothing reads it, so it must not drag the gate in.
r="$(new_repo mention_is_prose)"
change "$r" "website/content/docs/scripting.mdx"
want "C3 a mentioned-but-unread page is still prose" false "$r"

# The dot in a filename is a regex metacharacter, and an unescaped one makes
# the carve-out match paths it was never given.
r="$(new_repo dot_is_literal)"
change "$r" "website/src/components/provider-catalogXts"
want "C4 the carve-out's dots are literal" false "$r"

# ── Failing open ─────────────────────────────────────────────────────────────
#
# Every one of these answers the expensive way. A base this clone cannot
# resolve must never read as "nothing changed": the cost of a needless gate run
# is minutes, and the cost of a skipped one is a red main nobody is watching.
r="$(new_repo open_null)"
change "$r" "docs/spec/thing.md"
want "O1 the null commit (a branch's first push) runs the gate" true "$r" \
  0000000000000000000000000000000000000000

r="$(new_repo open_missing)"
change "$r" "docs/spec/thing.md"
want "O2 a base commit this clone does not have runs the gate" true "$r" \
  deadbeefdeadbeefdeadbeefdeadbeefdeadbeef

r="$(new_repo open_empty)"
change "$r" "docs/spec/thing.md"
want "O3 an empty base runs the gate" true "$r" ""

# Two histories with no common ancestor. `git diff A...B` exits 128 with "no
# merge base" rather than printing an empty diff, and an empty diff is exactly
# what `false` means — so without this branch the guard would read a
# force-pushed rewrite as "nothing changed".
r="$(new_repo open_no_merge_base)"
change "$r" "docs/spec/thing.md"
orphan_base="$(git -C "$r" rev-parse HEAD)"
git -C "$r" checkout -q --orphan unrelated
git -C "$r" rm -rq --cached .
rm -f "$r/seed.txt" "$r/docs/spec/thing.md"
change "$r" "docs/spec/other.md"
want "O4 two histories with no common ancestor run the gate" true "$r" "$orphan_base"

# An inventory that has gone missing cannot build a carve-out, so the filter
# would silently narrow back to what it was before #4605 — the exact hole.
r="$(new_repo open_no_inventory)"
change "$r" "website/src/components/provider-catalog.ts"
rm -f "$r/scripts/website-rust-inputs.txt"
want "O5 a missing inventory runs the gate rather than narrowing the filter" true "$r"

# ── The negative direction ───────────────────────────────────────────────────
# Without this the suite is satisfiable by a script that always answers true.
r="$(new_repo empty_diff)"
want "N1 an empty diff runs nothing" false "$r" HEAD

echo
echo "passed ${pass}, failed ${fail}"
[ "$fail" -eq 0 ]
