#!/usr/bin/env bash
#
# Guard: every `uses:` under .github/ must name a 40-hex commit SHA.
#
# Before this guard, 34 of 34 action references were floating tags or branches,
# and .github/dependabot.yml claimed the opposite. A tag is mutable: the owner
# of any of those 15 actions could repoint it at new code, and it would run
# with whatever secrets the job holds. `dtolnay/rust-toolchain@stable` was not
# even a tag but a *branch*, which moves with no release event at all.
#
# Two of the un-pinned refs sat on the paths where that matters most:
# softprops/action-gh-release publishes the release artifacts, and
# contributor-assistant/github-action is the gate deciding who may contribute.
#
# The tag is kept in a trailing comment (`@<sha> # v7`) because the SHA alone
# is unreadable and un-reviewable. Dependabot bumps the SHA and that comment
# together, so pinning does not freeze the actions in place.
#
# Matching is anchored to a `uses:` *key* rather than a bare substring, because
# `statuses: write` (cla.yml) ends in "uses:" and a naive grep flags it.
#
# `.github/actions/<name>/action.yml` is scanned alongside the workflows, and
# for the same reason: a composite action's own `uses:` steps run with whatever
# secrets and OIDC permissions the calling job holds, so a workflow with every
# reference pinned can still call `./.github/actions/deploy` into an action
# that pulls `some/action@main` (#4288). Each root is skipped on its own — a
# repository with workflows and no composite actions must still pass.
#
# Uses portable POSIX tools so it runs on a bare CI runner (no ripgrep).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# A test seam: the hermetic suite points the scan at a fixture tree rather than
# at this repository (scripts/test-action-pins.sh).
if [ "${1:-}" = "--fixture-root" ]; then
  repo_root="$2"
fi
cd "$repo_root"

roots=""
present=""
for root in .github/workflows .github/actions; do
  roots="$roots $root"
  if [ -d "$root" ]; then
    present="$present $root"
  fi
done

if [ -z "$present" ]; then
  echo "check-action-pins: no$roots directory; skipping."
  exit 0
fi

# A `uses:` key: optional leading list dash, then `uses:` and whitespace.
uses_key='^[[:space:]]*-?[[:space:]]*uses:[[:space:]]'
# A correctly pinned value: <action>@<40 lowercase hex>, then EOL or a comment.
pinned='uses:[[:space:]]*[^[:space:]]+@[0-9a-f]{40}([[:space:]]|$)'

# shellcheck disable=SC2086 # word-split on purpose: $present is a root list.
all_uses="$(grep -rnE "$uses_key" $present || true)"

if [ -z "$all_uses" ]; then
  echo "check-action-pins: no 'uses:' references found; skipping."
  exit 0
fi

# A local reference — `uses: ./.github/actions/<name>` — has nothing to pin: it
# resolves inside the checked-out commit, so it moves only when this repository
# moves. It is exempt from the SHA requirement, and its *contents* are covered
# because .github/actions is a scan root above.
local_ref='uses:[[:space:]]*\./'

unpinned="$(printf '%s\n' "$all_uses" | grep -vE "$pinned" | grep -vE "$local_ref" || true)"

if [ -n "$unpinned" ]; then
  echo "check-action-pins: these 'uses:' references are not pinned to a commit SHA:" >&2
  echo >&2
  printf '%s\n' "$unpinned" >&2
  echo >&2
  echo "Pin each one to a 40-hex commit SHA, keeping the tag in a comment:" >&2
  echo >&2
  echo "    uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7" >&2
  echo >&2
  echo "Resolve a tag to its commit with:" >&2
  echo >&2
  echo "    gh api repos/<owner>/<repo>/commits/<tag> --jq .sha" >&2
  echo >&2
  echo "Use the API rather than 'git ls-remote': for an annotated tag, ls-remote" >&2
  echo "prints the tag object's SHA, and a workflow pinned to a tag object fails" >&2
  echo "at startup." >&2
  exit 1
fi

external="$(printf '%s\n' "$all_uses" | grep -vE "$local_ref" || true)"
total="$(printf '%s\n' "$external" | grep -c . || true)"
total="$(printf '%s' "$total" | tr -d '[:space:]')"

# Not fatal: a missing tag comment is a readability problem, not a security one.
# A local reference has no tag to name, so it is not asked for one.
no_comment="$(printf '%s\n' "$external" | grep -vE '@[0-9a-f]{40}[[:space:]]*#' || true)"

# The verdict is already decided; the writes below are best-effort. SIGPIPE is
# ignored and each write's failure discarded, so a reader that closed the pipe
# (`| head -1`, `| true`) cannot turn a green verdict into a failure (#1815).
trap '' PIPE
if [ -n "$no_comment" ]; then
  echo "check-action-pins: pinned, but with no '# <tag>' comment to say what version this is:" || true
  printf '%s\n' "$no_comment" || true
fi

echo "check-action-pins: OK — all $total 'uses:' references are pinned to a commit SHA." || true
