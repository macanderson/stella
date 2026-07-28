#!/usr/bin/env bash
#
# Claude Code PostToolUse hook: keep agent-authored Rust inside the gate.
#
# Wired up by scripts/setup-claude-env.sh, which writes it into the worktree's
# (gitignored) .claude/settings.json. It runs after every Edit/Write/MultiEdit
# and does two things to a `.rs` file the agent just touched:
#
#   1. rustfmt it, in place. `cargo fmt --check` is a hard gate, so the tree's
#      formatting is not negotiable — the only question is whether the agent
#      finds out now or after a full `make gate` at push time. Formatting one
#      file costs milliseconds; discovering it at the end costs a whole cycle.
#
#      Formatting as you go also closes a trap that is otherwise invisible:
#      rustfmt's wrapping ADDS lines, so a file that sat under its
#      file-size-baseline ceiling while unformatted can breach it the moment
#      `cargo fmt` runs. Fixing the size gate first and formatting second means
#      re-breaking the gate you just fixed. Formatting first makes the line
#      count the agent sees the same one check-file-size.sh will see.
#
#   2. Re-run the file-size ratchet for that one file. `make gate` runs
#      check-file-size.sh over the whole tree; this is the same rule scoped to
#      the file in hand, so an agent that grows a grandfathered file past its
#      ceiling is told immediately, at the edit that did it, rather than at the
#      end of a long session with no memory of which change was responsible.
#
# Exit codes are the hook contract, not a normal script's:
#   0 → silent success. The tool call proceeds.
#   2 → stderr is fed back to Claude as actionable feedback. Used ONLY for a
#       real ratchet breach, i.e. something `make gate` would fail on.
# The tool call has already run by the time a PostToolUse hook fires, so a
# nonzero exit cannot undo the edit — it can only inform. Every other failure
# mode here (no stdin, unparseable payload, missing rustfmt, a file that does
# not parse mid-edit) exits 0 on purpose: a hook that nags about its own
# breakage trains the reader to ignore it.
#
# Deliberately NOT run: `cargo fmt`, `cargo check`, `cargo clippy`. Those are
# workspace-scoped and take seconds to minutes; on a per-edit hook they would
# make every edit feel broken. `make gate` remains the thing that must pass.
#
# bash 3.2 compatible (macOS ships 3.2, and this runs on the dev's machine).

# No `set -e`: a hook must never abort a tool call by accident. Failures below
# are handled explicitly.
set -uo pipefail

LIMIT=1500

# ── Locate the repo ──────────────────────────────────────────────────────────
# The hook's cwd is the Claude Code project dir, but resolve it properly rather
# than assuming: worktrees make $PWD-relative guesses wrong.
repo_root="$(git rev-parse --show-toplevel 2>/dev/null || true)"
[ -n "$repo_root" ] || exit 0

# ── Read the tool payload ────────────────────────────────────────────────────
# Claude Code delivers a JSON object on stdin. We need exactly one field:
# .tool_input.file_path. jq if it is available, a narrow sed otherwise — the
# sed path is sufficient because file_path is a plain filesystem path, so it
# contains no escaped quotes for a regex to get wrong.
payload="$(cat 2>/dev/null || true)"
[ -n "$payload" ] || exit 0

if command -v jq >/dev/null 2>&1; then
  file="$(printf '%s' "$payload" | jq -r '.tool_input.file_path // empty' 2>/dev/null || true)"
else
  file="$(printf '%s' "$payload" \
    | sed -n 's/.*"file_path"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    | head -1)"
fi

[ -n "$file" ] || exit 0
case "$file" in
  *.rs) ;;
  *) exit 0 ;;
esac
[ -f "$file" ] || exit 0

# Only touch files inside this repo. An agent editing a .rs file elsewhere on
# the machine is none of this hook's business.
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
  # A file caught mid-refactor may not parse. That is not an error worth
  # reporting — clippy and the compiler will say so far more precisely.
  rustfmt --edition "$edition" "$abs_file" >/dev/null 2>&1 || true
fi

# ── 2. File-size ratchet, scoped to this file ────────────────────────────────
# Same rule as scripts/check-file-size.sh: a file listed in the baseline may not
# exceed its recorded ceiling; any other file may not exceed LIMIT.
baseline="$canon_root/scripts/file-size-baseline.txt"
# `wc -l`, not `awk END{print NR}` — check-file-size.sh uses wc, which counts
# newlines. The two disagree by one on a file with no trailing newline, and a
# hook that disagrees with the gate it is previewing is worse than no hook.
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

# Over the line. Say which rule broke and what the fix is — an agent that is
# told "too long" without being told which of the two rules applies will guess,
# and guessing wrong here means editing the baseline when it should be
# splitting the file.
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
