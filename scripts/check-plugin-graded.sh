#!/usr/bin/env bash
# Every first-party plugin is spawned by an in-tree test.
#
# ADR 0030 says this repository ships no SDK for plugin authors. The manifest
# plus the socket is the whole contract. That choice leans on one thing. The
# plugins under `plugins/` speak the same wire a stranger's plugin speaks. A
# test here spawns each of them through the host's own transport. So a wire
# change reddens this repository's CI, not an install somewhere else.
#
# `plugins/README.md` said that and nothing checked it. Add a plugin with no
# test and the claim stays true of the rest. That is the quiet way to break
# it: the sentence still reads right.
#
# The rule. Every folder under `plugins/` with a `plugin.toml` is named on a
# line of some `crates/*/tests/**/*.rs` that is not a comment. A `//` mention
# is a pointer, not a run. `driver_socket.rs` names `stella-selfdriving` in a
# doc comment and spawns it nowhere.
#
# This cannot see whether a test spawns the plugin or just reads its manifest.
# It does not try. A path written into test code is a claim a reviewer can
# check. `check-consumer-sites.sh` holds a `site:` string to the same bar.
#
# Two overrides exist for `scripts/test-plugin-graded.sh` alone. It needs
# fixture trees to show this guard can still fail. The real tree is green by
# design.
set -euo pipefail

cd "$(dirname "$0")/.."

plugin_root="${PLUGIN_ROOT:-plugins}"
crates_root="${CRATES_ROOT:-crates}"

fail=0
report=""
note() { report="${report}$1"$'\n'; }

if [ ! -d "$plugin_root" ]; then
  echo "check-plugin-graded: FAILED — no $plugin_root/ directory to check." >&2
  exit 1
fi

# Every test source that could name a plugin, gathered once. `find` rather
# than a glob so the depth under tests/ is unbounded (tests/common/mod.rs is
# one level down and names a plugin).
sources="$(mktemp "${TMPDIR:-/tmp}/stella-plugin-graded.XXXXXX")"
trap 'rm -f "$sources"' EXIT INT TERM
if [ -d "$crates_root" ]; then
  find "$crates_root" -mindepth 3 -path '*/tests/*' -name '*.rs' -type f >"$sources"
fi

checked=0
for manifest in "$plugin_root"/*/plugin.toml; do
  [ -f "$manifest" ] || continue
  dir="$(dirname "$manifest")"
  name="$(basename "$dir")"
  checked=$((checked + 1))

  # A line naming the plugin's directory, with `//` comment lines dropped
  # first so a doc comment cannot pass for a spawn.
  #
  # The strip and the search do not share a pipe. Under `pipefail` a `grep -q`
  # that matches early exits while `sed` is still writing, `sed` takes SIGPIPE
  # and reports 141, and the pipeline reads as no-match. Which files lost that
  # race varied run to run: five runs of the piped form over this repository
  # reported stella-witness four times, stella-candidates twice, and once
  # reported nothing at all.
  graders=""
  while IFS= read -r src; do
    [ -n "$src" ] || continue
    stripped="$(sed 's|^[[:space:]]*//.*$||' "$src")"
    case "$stripped" in
      *"plugins/$name"*) graders="${graders}${src}"$'\n' ;;
    esac
  done <"$sources"

  if [ -z "$graders" ]; then
    fail=1
    note "  $dir is graded by no test."
  fi
done

if [ "$checked" -eq 0 ]; then
  echo "check-plugin-graded: FAILED — no $plugin_root/*/plugin.toml found." >&2
  echo >&2
  echo "Nothing to check is a defect, not a pass: this guard exists because" >&2
  echo "the plugins here are the wire contract's canary." >&2
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo "check-plugin-graded: FAILED" >&2
  echo >&2
  printf '%s' "$report" >&2
  echo >&2
  echo "Each of these is a program written against the wrapper socket, and" >&2
  echo "nothing in $crates_root/*/tests/ spawns it. A change to the wire would" >&2
  echo "break it silently, which is what ADR 0030 says cannot happen to a" >&2
  echo "first-party plugin." >&2
  echo >&2
  echo "Add a test that runs it — crates/stella-runtime/tests/ holds the" >&2
  echo "wrapper harnesses, crates/stella-tui/tests/ the panel ones — and name" >&2
  echo "its directory in the test's code, not only in a comment." >&2
  exit 1
fi

echo "check-plugin-graded: OK — $checked first-party plugin(s), each named by a test."
