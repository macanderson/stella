#!/usr/bin/env bash
#
# Guard: every path under website/ that a Rust source names is declared in
# scripts/website-rust-inputs.txt, and every declared path still exists. See
# #4632.
#
# ── Why ──────────────────────────────────────────────────────────────────────
#
# `website/` is on ci.yml's prose list: a diff confined to it skips the Rust
# gate. That is safe only while nothing under website/ can break a Rust test,
# and three files there are test inputs today. #4588 moved PROVIDER_CATALOG out
# of the file `config::tests::docs_sync` read, in a website-only diff. Nothing
# ran that test before the merge, and nothing ran it on the push either, so
# `main` was red until the next commit that touched a crate went red with it
# and took three more down.
#
# The instance was repaired by carving the two known paths out of the prose
# filter. The class is that the carve-out is a hand-kept list in a YAML file,
# with no relation to what the tests actually read — and it was already
# incomplete when this guard was written: `settings/completeness.rs` reads
# `website/content/docs/configuration/stella-toml.mdx`, which the carve-out did
# not name.
#
# So the inventory moves into one file that both sides read. ci.yml builds its
# carve-out from the `read` entries (scripts/ci-rust-scope.sh), and this guard
# holds the entries to the tree. Neither is a copy of the other.
#
# ── What it checks ───────────────────────────────────────────────────────────
#
#   1. Every declared path exists. A website-only PR that renames, moves or
#      deletes a file a Rust test reads goes red here, before the merge — which
#      is the pre-merge report #4632 asks for, and the one ci.yml cannot give
#      because its Rust job is skipped for exactly that diff. This guard runs in
#      docs-guards.yml, which triggers on website/**.
#
#   2. Every `website/...` path a Rust source under crates/ names is covered by
#      an entry. Adding a test that reads a new website file, or a doc comment
#      pointing at one, is a red gate until the path is declared — and a `read`
#      declaration lands in ci.yml's carve-out by construction, so the gate will
#      run for a diff that touches it. Had this existed, #4588's diff would have
#      run the Rust gate and gone red on its own PR.
#
#   3. No stale entry: every declared path is still named by some Rust source.
#      An entry nobody reads widens the carve-out for nothing, and reads as a
#      cross-boundary coupling that no longer exists.
#
# ── What it does NOT check ───────────────────────────────────────────────────
#
# Whether a `read` was declared as a `mention`. Nothing mechanical can tell a
# doc comment that names a path from a test that opens it, and pretending a
# grep could would be worse than saying so: a reviewer decides, and the two
# kinds are spelled differently in the inventory so there is something to
# decide about.
#
# It also sees a path only where it appears as one literal. A test that builds
# `root.join("website").join("src/…")` names no `website/…` string, so nothing
# here can find it — declare it, and say the path once in the doc comment above
# the helper, which is what `docs_sync.rs` does.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

inventory="${WEBSITE_INPUTS_FILE:-scripts/website-rust-inputs.txt}"

fail=0
report=""
note() { report="${report}check-website-inputs: $1"$'\n'; }

# The verdict is decided before anything is written, and the write is
# best-effort: a reader that closed the pipe (`| head -1`) must be able to
# change neither the report nor the exit code (#1815).
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

if [ ! -f "$inventory" ]; then
  note "FAIL — $inventory does not exist."
  note "     It is the single list ci.yml's prose carve-out is built from;"
  note "     without it the Rust gate cannot know which website paths are"
  note "     test inputs."
  emit
  exit 1
fi

# `<kind> <path>`, comments and blanks dropped. A trailing slash is stripped
# here so the whole script compares normalized paths and the inventory can
# spell a directory either way.
entries="$(sed -e 's/#.*//' "$inventory" | awk 'NF >= 2 { sub(/\/$/, "", $2); print $1, $2 }')"

if [ -z "$entries" ]; then
  note "FAIL — $inventory declares nothing."
  emit
  exit 1
fi

while read -r kind path; do
  [ -n "${kind:-}" ] || continue
  case "$kind" in
  read | mention) ;;
  *)
    note "FAIL — unknown kind '$kind' for '$path' in $inventory."
    note "     Use 'read' (a Rust test opens it) or 'mention' (prose names it)."
    fail=1
    continue
    ;;
  esac
  if [ ! -e "$path" ]; then
    note "FAIL — $inventory declares '$path', which does not exist."
    note "     A Rust source names it, so moving or deleting it breaks that"
    note "     source. Repoint the Rust side and this entry together, in one"
    note "     change — that is the whole point of declaring it here."
    fail=1
  fi
done <<EOF
$entries
EOF

# Every website path any Rust source under crates/ names, as one literal.
#
# `|| true` on the grep: it exits 1 when nothing matches, and `pipefail` would
# turn that into the whole script dying here — before any verdict, with no
# message — for a tree where no Rust source names a website path at all. That
# tree is the goal state, not an error, and it is exactly when the stale-entry
# check below has something to say. Same trap #1800 documents in
# scripts/check-file-size.sh.
literals="$(
  { git grep -ohE 'website/[A-Za-z0-9._/-]*' -- 'crates/**/*.rs' || true; } |
    sed -e 's/\/$//' | LC_ALL=C sort -u
)"

# A `read` entry covers its subtree, because the tests walk one. A `mention`
# covers itself and nothing below it: one line must never silence a directory.
covered() {
  local needle="$1" kind path
  while read -r kind path; do
    [ -n "${kind:-}" ] || continue
    [ "$needle" = "$path" ] && return 0
    if [ "$kind" = read ]; then
      case "$needle" in
      "$path"/*) return 0 ;;
      esac
    fi
  done <<EOF
$entries
EOF
  return 1
}

undeclared=""
for literal in $literals; do
  covered "$literal" || undeclared="${undeclared}  ${literal}"$'\n'
done

if [ -n "$undeclared" ]; then
  note "FAIL — a Rust source names these website paths, and $inventory does not:"
  report="${report}${undeclared}"
  note ""
  note "     Declare each one. 'read <path>' if a test opens it — that also"
  note "     puts it in ci.yml's carve-out, so a diff touching it runs the"
  note "     Rust gate instead of skipping it as prose. 'mention <path>' if"
  note "     it is only prose pointing a reader somewhere."
  fail=1
fi

# The other direction: an entry nothing names any more.
while read -r kind path; do
  [ -n "${kind:-}" ] || continue
  found=0
  for literal in $literals; do
    if [ "$literal" = "$path" ]; then
      found=1
      break
    fi
    if [ "$kind" = read ]; then
      case "$literal" in
      "$path"/*)
        found=1
        break
        ;;
      esac
    fi
  done
  if [ "$found" -eq 0 ]; then
    note "FAIL — $inventory declares '$path', which no Rust source names."
    note "     Drop the entry. A stale 'read' widens the Rust gate's trigger"
    note "     for a coupling that no longer exists, and reads as one that does."
    fail=1
  fi
done <<EOF
$entries
EOF

if [ "$fail" -ne 0 ]; then
  emit
  exit 1
fi

emit
declared="$(printf '%s\n' "$entries" | awk 'NF' | wc -l | tr -d ' ')"
reads="$(printf '%s\n' "$entries" | awk '$1 == "read"' | wc -l | tr -d ' ')"
trap '' PIPE
printf 'check-website-inputs: OK — %s declared website path(s), %s of them Rust test inputs, all present.\n' \
  "$declared" "$reads" || true
