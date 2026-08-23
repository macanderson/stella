#!/usr/bin/env bash
# No tracked path may use a Windows reserved device name.
#
# Windows reserves AUX, CON, PRN, NUL, COM1-COM9 and LPT1-LPT9 as device
# names, with or without an extension, in any directory. Git on Windows
# refuses to *write* such a path: `git checkout` fails with
#
#   error: invalid path 'crates/stella-cli/src/config/aux.rs'
#   The process 'git.exe' failed with exit code 128
#
# and exits before a single file reaches disk. Nothing downstream runs — not
# a build, not a test, not a contributor's editor. The repository is simply
# not clonable there.
#
# This tree carried two of them (`crates/stella-cli/src/config/aux.rs` and
# `crates/stella-model/src/credential/aux.rs`) for as long as those modules
# have existed. Nothing found them, because nothing in CI touched Windows,
# and no Rust toolchain ever will: the name is legal Rust and legal POSIX.
#
# `windows-check.yml` does find them — but one per run, because a failed
# checkout reports the first bad path and stops. That makes it a detector
# with an O(n) feedback loop measured in CI minutes. This is the same
# question asked locally, over the whole tree at once, in milliseconds, with
# no toolchain: `make guards-fast` runs it.
#
# The check is on the *tracked* set, not the working tree, because that is
# what a clone reproduces.
set -euo pipefail

cd "$(dirname "$0")/.."

# The reserved set, lowercase. `CLOCK$` is reserved too but cannot appear in
# a git path on any platform, so it is omitted rather than listed and never
# matched.
reserved='con|prn|aux|nul|com[1-9]|lpt[1-9]'

# Every path segment of every tracked file, stem only (the part before the
# first dot) — `aux.rs`, `aux.tar.gz` and a bare `aux` directory are all
# refused, which is what Windows does.
offenders=$(
  git ls-files -z \
    | tr '\0' '\n' \
    | awk -v re="^($reserved)$" '
        {
          n = split($0, seg, "/")
          for (i = 1; i <= n; i++) {
            stem = seg[i]
            sub(/\..*$/, "", stem)
            if (tolower(stem) ~ re) { print $0; next }
          }
        }
      '
)

if [ -n "$offenders" ]; then
  echo "check-reserved-paths: FAILED" >&2
  echo >&2
  echo "These tracked paths use a Windows reserved device name. Git on Windows" >&2
  echo "refuses to check them out at all, so the whole repository is unclonable" >&2
  echo "there — the failure is not in the build, it is before it." >&2
  echo >&2
  while IFS= read -r path; do
    echo "  $path" >&2
  done <<<"$offenders"
  echo >&2
  echo "Rename the segment. AUX, CON, PRN, NUL, COM1-COM9 and LPT1-LPT9 are" >&2
  echo "reserved with or without an extension, in every directory." >&2
  exit 1
fi

echo "check-reserved-paths: OK — $(git ls-files | wc -l | tr -d ' ') tracked paths, none using a Windows device name."
