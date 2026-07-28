#!/usr/bin/env bash
#
# Format one Rust file and hold it to the file-size ratchet.
#
#   ./scripts/fmt-file.sh path/to/file.rs     # by hand, or from an editor
#   echo '{"tool_input":{"file_path":"..."}}' | ./scripts/fmt-file.sh
#
# Two entry points on purpose: a plain path, for humans and for editors with a
# format-on-save command, and a JSON payload on stdin, for agent harnesses that
# pipe an edit event to a hook command. scripts/setup-dev-env.sh
# --agent-settings wires the second form.
#
# It does two things to the file it is given:
#
#   1. rustfmt it, in place. `cargo fmt --check` is a hard gate, so the tree's
#      formatting is not negotiable — the only question is whether you find out
#      now or after a full `make gate`. Formatting one file costs milliseconds;
#      discovering it at the end costs a whole cycle.
#
#      Formatting as you go also closes a trap that is otherwise invisible:
#      rustfmt's wrapping ADDS lines, so a file that sat under its
#      file-size-baseline ceiling while unformatted can breach it the moment
#      `cargo fmt` runs. Fixing the size gate first and formatting second means
#      re-breaking the gate you just fixed. Formatting first makes the line
#      count you see the same one check-file-size.sh will see.
#
#   2. Re-run the file-size ratchet for that one file. `make gate` runs
#      check-file-size.sh over the whole tree; this is the same rule scoped to
#      the file in hand, so growing a grandfathered file past its ceiling is
#      reported at the edit that did it rather than at the end of a long session
#      with no memory of which change was responsible.
#
# Exit codes:
#   0 → nothing to say. (Also the answer for every input this script does not
#       handle: a non-Rust file, a path outside the repo, an unreadable payload,
#       a missing rustfmt, a file that does not parse mid-edit. A tool that nags
#       about its own breakage trains the reader to ignore it.)
#   2 → the file breaches the ratchet, with the reason on stderr. Chosen so an
#       agent harness surfaces the message as feedback, and so a shell caller
#       sees a distinct nonzero code.
#
# Deliberately NOT run: `cargo fmt`, `cargo check`, `cargo clippy`. Those are
# workspace-scoped and take seconds to minutes; on a per-edit hook they would
# make every edit feel broken. `make gate` remains the thing that must pass.
#
# bash 3.2 compatible.

# No `set -e`: as a hook this must never abort the caller by accident. Failures
# below are handled explicitly.
set -uo pipefail

LIMIT=1500

# ── Locate the repo ──────────────────────────────────────────────────────────
repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$repo_root" ] || exit 0

# ── Find the file ────────────────────────────────────────────────────────────
# An argument wins. Otherwise read a JSON object from stdin and pull out
# .tool_input.file_path — jq when available, a narrow sed otherwise. The sed
# path is sufficient because file_path is a plain filesystem path, so it holds
# no escaped quotes for the regex to get wrong.
if [ $# -gt 0 ]; then
  file="$1"
else
  # No argument and stdin is a terminal means someone ran this bare, expecting
  # usage — not that they intend to type a JSON payload. Without this, `cat`
  # below blocks forever on the tty and the script looks hung.
  if [ -t 0 ]; then
    echo "usage: ${0##*/} <file.rs>   (or pipe a JSON edit payload on stdin)" >&2
    exit 0
  fi
  payload="$(cat 2>/dev/null || true)"
  [ -n "$payload" ] || exit 0
  if command -v jq >/dev/null 2>&1; then
    file="$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty' 2>/dev/null || true)"
  else
    file="$(printf '%s' "$payload" \
      | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
      | head -1)"
  fi
fi

[ -n "$file" ] || exit 0
case "$file" in
  *.rs) ;;
  *) exit 0 ;;
esac
[ -f "$file" ] || exit 0

# Only touch files inside this repo. A .rs file elsewhere on the machine is none
# of this script's business.
abs_file="$(cd "$(dirname "$file")" 2>/dev/null && pwd -P)/$(basename "$file")" || exit 0
canon_root="$(cd "$repo_root" 2>/dev/null && pwd -P)" || exit 0
case "$abs_file" in
  "$canon_root"/*) ;;
  *) exit 0 ;;
esac
rel="${abs_file#"$canon_root"/}"

# check-file-size.sh judges `git ls-files '*.rs'` — tracked files only. A
# gitignored scratch file will never be tracked (check-no-scratch.sh enforces
# that), so warning about one would be pure noise. An untracked-but-committable
# file IS checked here, deliberately: it is going to be added, and hearing about
# the limit at creation beats hearing about it at push.
if git check-ignore -q "$abs_file" 2>/dev/null; then
  exit 0
fi

# ── 1. Format ────────────────────────────────────────────────────────────────
# The edition matters: without --edition, rustfmt assumes 2015 and reformats
# 2024 code incorrectly. Read it from the workspace rather than hardcoding, so
# this keeps working across an edition bump.
if command -v rustfmt >/dev/null 2>&1; then
  edition="$(sed -n 's/^edition[[:space:]]*=[[:space:]]*"\([0-9]*\)".*/\1/p' \
    "$canon_root/Cargo.toml" 2>/dev/null | head -1)"
  [ -n "$edition" ] || edition=2024
  # A file caught mid-refactor may not parse. That is not worth reporting —
  # clippy and the compiler will say so far more precisely.
  rustfmt --edition "$edition" "$abs_file" >/dev/null 2>&1 || true
fi

# ── 2. File-size ratchet, scoped to this file ────────────────────────────────
# Same rule as scripts/check-file-size.sh: a file listed in the baseline may not
# exceed its recorded ceiling; any other file may not exceed LIMIT.
baseline="$canon_root/scripts/file-size-baseline.txt"
# `wc -l`, not `awk END{print NR}` — check-file-size.sh uses wc, which counts
# newlines. The two disagree by one on a file with no trailing newline, and a
# preview that disagrees with the gate it previews is worse than no preview.
lines="$(wc -l <"$abs_file" 2>/dev/null | tr -d ' ' || echo 0)"
[ -n "$lines" ] && [ "$lines" -gt 0 ] 2>/dev/null || exit 0

ceiling="$LIMIT"
grandfathered=0
if [ -f "$baseline" ]; then
  found="$(awk -v p="$rel" '$1 !~ /^#/ && $2 == p { print $1; exit }' "$baseline" 2>/dev/null || true)"
  if [ -n "$found" ]; then
    ceiling="$found"
    grandfathered=1
  fi
fi

if [ "$lines" -le "$ceiling" ]; then
  exit 0
fi

# Over the line. Say which of the two rules broke and what the fix is — being
# told "too long" without being told which rule applies invites a guess, and
# guessing wrong here means editing the baseline when it should mean splitting
# the file.
{
  if [ "$grandfathered" -eq 1 ]; then
    echo "check-file-size: $rel is now $lines lines, over its baseline ceiling of $ceiling."
    echo
    echo "This file is grandfathered in scripts/file-size-baseline.txt, so it may"
    echo "shrink freely but may not grow past $ceiling. \`make gate\` will fail as-is."
    echo "Either move the addition into a new module, or — if the growth is"
    echo "irreducible — run \`make file-size-update\` and commit the baseline diff so"
    echo "the raise is visible in review."
  else
    echo "check-file-size: $rel is $lines lines, over the ${LIMIT}-line limit."
    echo
    echo "This file is NOT in scripts/file-size-baseline.txt, and new files over the"
    echo "limit are a hard block — no baseline entry may be added for one."
    echo "\`make gate\` will fail as-is. Split it into modules."
  fi
} >&2

exit 2
