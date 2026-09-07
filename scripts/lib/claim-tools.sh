#!/usr/bin/env bash
#
# The tools a claim check needs, and what it says when one of them is gone.
#
# `scripts/main-red-claim.sh` and `scripts/issue-claim.sh` both ask the
# tracker. `gh` places the call. `jq` reads the claims out of the reply.
#
# ## A check that did not run is not a check that found nothing
#
# Both scripts fail open. Every unknown means go. So a run with no tools must
# not read like a run that asked and heard nothing.
#
# The gate holds the same line. A step with no `shellcheck` on `PATH` prints
# `UNAVAILABLE` and stops. It never prints a pass.
#
# So the banner below names the tool, says that nothing was asked, and says
# how to ask by hand.
#
# ## Why both tools
#
# `gh --jq` carries its own copy of jq, so a tracker call needs `gh` alone.
# The claim comments then go through a real `jq` filter. With `gh` there and
# `jq` gone, the scripts blamed the comments for what was a missing tool.
#
# Sourced, never run. It defines two functions and does nothing else.

# The claim tools that are not on `PATH`, one space between them. An empty
# answer means both are there.
missing_claim_tools() {
  missing=""
  for tool in gh jq; do
    command -v "$tool" >/dev/null 2>&1 || missing="$missing $tool"
  done
  printf '%s' "${missing# }"
}

# The banner, on stderr. The first argument is the tool list. The second says
# what went unasked. The third says what to do by hand.
report_claim_tools_unavailable() {
  echo "$1: UNAVAILABLE — THIS CHECK DID NOT RUN" >&2
  echo "" >&2
  echo "     $2" >&2
  echo "     That is no answer at all, and it is not an answer of" >&2
  echo "     'nobody is on it'." >&2
  echo "" >&2
  echo "     Going ahead anyway: a claim check that can block work is worse" >&2
  echo "     than the duplication it stops." >&2
  echo "" >&2
  printf '%s\n' "$3" >&2
}
