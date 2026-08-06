#!/usr/bin/env bash
#
# Guard: every `stella` subcommand has a reference page, and every reference
# page names a subcommand that still exists. See #993 (residual of #928).
#
# #928 found seven undocumented CLI surfaces at once. Five gained pages in PR
# #989 and two more later, but the half of #928 that explained *how seven
# accumulated* — "and nothing in the gate would notice" — stayed true: no check
# anywhere related the `Command` enum to website/content/docs/commands/. A new
# subcommand could land with no page and every gate stayed green, which is
# exactly how the original seven built up unnoticed.
#
# `make doc-links` is a different property: it asserts that docs paths *cited
# from Rust* resolve. A command nobody ever cited is invisible to it.
#
# Three failures this catches:
#
#   1. **A new subcommand with no page.** The recurrence #928 documented.
#   2. **A page that no sidebar lists.** meta.json is hand-maintained, so a
#      page can exist on disk and be unreachable by navigation — undocumented
#      in every sense that matters to a reader.
#   3. **An orphan page.** A command renamed or removed leaves a page behind
#      describing a surface that no longer ships. Stale reference docs are
#      worse than absent ones: they read as current.
#
# Intentionally-undocumented surfaces go in scripts/command-docs-exempt.txt,
# committed and visible in review like scripts/file-size-baseline.txt — so an
# omission is a decision somebody signed off on rather than an oversight.
#
# Uses portable POSIX awk/sed/grep so it runs on a bare CI runner with no
# toolchain, alongside the other early guards in ci.yml.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

# Where the top-level subcommand enum lives. It has moved twice already
# (stella-cli/src/main.rs -> stella-cli/src/cli.rs, PR #999; then under
# crates/ with the rest of the workspace, PR #1385), which is why this fails
# loudly rather than silently finding nothing.
enum_file="crates/stella-cli/src/cli.rs"
enum_decl="pub(crate) enum Command {"
docs_dir="website/content/docs/commands"
meta_file="$docs_dir/meta.json"
exempt_file="scripts/command-docs-exempt.txt"

status=0

# The verdict is decided before anything is written (#1815). Failure lines are
# buffered while the checks run and emitted in one final write: a guard that
# prints as it scans dies mid-report when its reader exits early, and under
# `set -euo pipefail` whatever partial state it had reached becomes the exit
# status. scripts/check-file-size.sh is the shape being copied.
report=""
note() { report="${report}$1"$'\n'; }

# Emission is best-effort: the verdict is already decided, so a reader that
# closed the pipe (`| head -1`, `| true`) must be able to change neither the
# report nor the exit code. SIGPIPE is ignored so a failed write surfaces as a
# discarded error instead of killing the script (#1815).
emit() {
  trap '' PIPE
  printf '%s' "$report" >&2 || true
}

# Membership over a newline-delimited list, in pure shell.
#
# Deliberately NOT `printf ... | grep -qx`: under `set -o pipefail` that is a
# race, not a test. `grep -q` exits the moment it matches, `printf` then dies
# of SIGPIPE (141), and pipefail reports the pipeline as failed — so a slug
# that IS present intermittently reads as absent, depending only on whether
# printf had drained its buffer. It surfaced here as a different random subset
# of commands being reported missing on every run.
contains() {
  case "
$1
" in
  *"
$2
"*) return 0 ;;
  *) return 1 ;;
  esac
}

if [ ! -f "$enum_file" ]; then
  note "FAIL $enum_file not found."
  note "     This guard reads the top-level subcommand list from that file. If"
  note "     it moved, update \$enum_file in scripts/check-command-docs.sh in"
  note "     the same PR."
  emit
  exit 1
fi

if [ ! -d "$docs_dir" ]; then
  note "FAIL $docs_dir not found."
  note "     This guard expects the command reference pages to live there."
  emit
  exit 1
fi

# --- Parse the enum ---------------------------------------------------------
# Variants sit at exactly four spaces of indent; their fields sit at eight,
# doc comments open with `///`, and attributes with `#[`. Anchoring on the
# four-space indent is what separates a variant from everything nested inside
# one without needing to parse Rust. `$1` drops the indent, and the gsub
# trims whatever follows the name (`{`, `(` or `,`).
variants="$(awk -v decl="$enum_decl" '
  index($0, decl) == 1 { inside = 1; next }
  inside && /^\}/       { exit }
  inside && /^    [A-Z][A-Za-z0-9]*[ ,{(]/ {
    name = $1
    gsub(/[,{(].*/, "", name)
    if (name != "") { print name }
  }
' "$enum_file")"

if [ -z "$variants" ]; then
  note "FAIL parsed zero variants from $enum_file."
  note "     Expected a block opening with: $enum_decl"
  note "     If the declaration changed, update \$enum_decl in"
  note "     scripts/check-command-docs.sh in the same PR."
  emit
  exit 1
fi

# clap's derive renames variants to kebab-case by default, so `FooBar` is
# invoked as `stella foo-bar` and its page is foo-bar.mdx.
slugs="$(printf '%s\n' "$variants" \
  | sed 's/\([a-z0-9]\)\([A-Z]\)/\1-\2/g' \
  | tr '[:upper:]' '[:lower:]' \
  | sort)"

# --- Exemptions -------------------------------------------------------------
# One slug per line; `#` opens a comment. An exempt command needs no page and
# is not reported as missing, but an exempt slug that no longer names a real
# command is still reported below so the list cannot rot.
exempt=""
if [ -f "$exempt_file" ]; then
  exempt="$(sed 's/#.*//' "$exempt_file" | tr -d '[:blank:]' | grep -v '^$' | sort || true)"
fi

is_exempt() {
  [ -n "$exempt" ] || return 1
  contains "$exempt" "$1"
}

# --- The pages on disk ------------------------------------------------------
# index.mdx is the section landing page, not a command reference.
pages="$(find "$docs_dir" -maxdepth 1 -name '*.mdx' -exec basename {} .mdx \; \
  | grep -vx 'index' | sort)"

# --- The sidebar ------------------------------------------------------------
# meta.json's "pages" array carries both page slugs and `---Group---` section
# separators. Keys are on lines containing a colon; separators start with a
# dash inside the quotes. What survives is the navigable slugs.
sidebar=""
if [ -f "$meta_file" ]; then
  sidebar="$(grep -v ':' "$meta_file" \
    | sed -n 's/.*"\([a-z][a-z0-9-]*\)".*/\1/p' | sort || true)"
fi

# --- 1. Every command has a page -------------------------------------------
missing=0
while IFS= read -r slug; do
  [ -n "$slug" ] || continue
  is_exempt "$slug" && continue
  if [ ! -f "$docs_dir/$slug.mdx" ]; then
    note "FAIL \`stella $slug\` has no reference page: $docs_dir/$slug.mdx"
    missing=$((missing + 1))
    status=1
  fi
done <<EOF
$slugs
EOF

if [ "$missing" -ne 0 ]; then
  note ""
  note "     Write the page, or add the slug to $exempt_file with a reason."
  note "     An exemption is a decision on the record; a missing page is not."
fi

# --- 2. Every page is reachable from the sidebar ----------------------------
if [ -n "$sidebar" ]; then
  unlisted=0
  while IFS= read -r slug; do
    [ -n "$slug" ] || continue
    is_exempt "$slug" && continue
    [ -f "$docs_dir/$slug.mdx" ] || continue
    if ! contains "$sidebar" "$slug"; then
      note "FAIL $docs_dir/$slug.mdx is not listed in $meta_file."
      unlisted=$((unlisted + 1))
      status=1
    fi
  done <<EOF
$slugs
EOF
  if [ "$unlisted" -ne 0 ]; then
    note ""
    note "     A page no sidebar lists is undocumented to anyone browsing."
    note "     Add the slug to the \"pages\" array under the right group."
  fi
fi

# --- 3. No orphan pages -----------------------------------------------------
orphans=0
while IFS= read -r page; do
  [ -n "$page" ] || continue
  if ! contains "$slugs" "$page"; then
    note "FAIL $docs_dir/$page.mdx documents no \`Command\` variant."
    orphans=$((orphans + 1))
    status=1
  fi
done <<EOF
$pages
EOF

if [ "$orphans" -ne 0 ]; then
  note ""
  note "     The command was renamed or removed and its page outlived it."
  note "     Delete the page, or repoint it at the surface that replaced it."
fi

# --- 4. No stale exemptions -------------------------------------------------
if [ -n "$exempt" ]; then
  while IFS= read -r slug; do
    [ -n "$slug" ] || continue
    if ! contains "$slugs" "$slug"; then
      note "FAIL $exempt_file exempts \"$slug\", which is not a subcommand."
      note "     Remove the line: a stale exemption silently covers a future"
      note "     command that happens to reuse the name."
      status=1
    fi
  done <<EOF
$exempt
EOF
fi

emit
if [ "$status" -eq 0 ]; then
  count="$(printf '%s\n' "$slugs" | wc -l | tr -d '[:blank:]')"
  echo "check-command-docs: OK — $count subcommands, each with a listed reference page." || true
fi
exit "$status"
