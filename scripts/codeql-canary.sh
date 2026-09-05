#!/usr/bin/env bash
#
# Is CodeQL still running, and is it green?
#
#   scripts/codeql-canary.sh --conclusion success|failure
#
# CodeQL's last clean scan here ran on 2026-07-13. Every run after that
# failed in under ten seconds. That is too fast for a real Rust build to
# fail, so a bad setting was the likely cause, not broken code.
#
# Nobody saw it happen. CodeQL ran as a GitHub-managed setup, not a file in
# this repository, so no local check read its result. It stayed red until
# GitHub turned the scan off on its own after too many failed runs. After
# that there were no runs left to fail. Weeks passed with a dead scanner,
# and nothing said so.
#
# `.github/workflows/codeql.yml` fixes the run itself: a committed workflow
# this repository owns, in place of a setting nobody was watching. This
# script is the other half. It makes a sustained red visible, the way
# `main-canary.sh` does for a broken `main`. It does not add CodeQL as a
# required check. That call stays advisory, and the tracking issue records
# why: a required check nobody can pass is worse than one that runs quiet.
#
# It is much smaller than `main-canary.sh`. That script owns several checks
# and re-runs each one. This one owns a single fact: did the CodeQL job pass
# or fail. The caller already knows the answer and passes it in with
# `--conclusion`. Both scripts share one shape: one label, one issue kept
# open across every red run, closed on the next green one, with a
# "Definition of done" box so the close guard leaves it alone.
set -euo pipefail

# Same trap as main-canary.sh, and the same reason: the exit code is the
# verdict. A closed pipe must not turn a lost printf into a lost verdict.
trap '' PIPE

announce=0
dry_run=0
conclusion=""
sha="unknown"
run_url="<no run URL given>"
label="codeql-red"
fixture_open_issue=""

while [ $# -gt 0 ]; do
  case "$1" in
  --announce)
    announce=1
    shift
    ;;
  --dry-run)
    dry_run=1
    shift
    ;;
  --conclusion)
    if [ $# -lt 2 ]; then
      echo "codeql-canary: --conclusion needs a value" >&2
      exit 2
    fi
    conclusion="$2"
    shift 2
    ;;
  --sha)
    if [ $# -lt 2 ]; then
      echo "codeql-canary: --sha needs a value" >&2
      exit 2
    fi
    sha="$2"
    shift 2
    ;;
  --run-url)
    if [ $# -lt 2 ]; then
      echo "codeql-canary: --run-url needs a value" >&2
      exit 2
    fi
    run_url="$2"
    shift 2
    ;;
  --label)
    if [ $# -lt 2 ]; then
      echo "codeql-canary: --label needs a value" >&2
      exit 2
    fi
    label="$2"
    shift 2
    ;;
  --fixture-open-issue)
    if [ $# -lt 2 ]; then
      echo "codeql-canary: --fixture-open-issue needs a value" >&2
      exit 2
    fi
    fixture_open_issue="$2"
    shift 2
    ;;
  *)
    echo "codeql-canary: unknown argument: $1" >&2
    exit 2
    ;;
  esac
done

case "$conclusion" in
success | failure) ;;
*)
  echo "codeql-canary: --conclusion must be 'success' or 'failure' (got '${conclusion:-<empty>}')" >&2
  exit 2
  ;;
esac

# Same rule as main-canary.sh: --dry-run alone would be a silent no-op that
# still looks like it did something.
if [ "$dry_run" -eq 1 ] && [ "$announce" -eq 0 ]; then
  echo "codeql-canary: --dry-run only means something with --announce" >&2
  exit 2
fi

gh_run() {
  if [ "$dry_run" -eq 1 ]; then
    printf 'codeql-canary: [dry-run] gh %s\n' "$*" || true
    return 0
  fi
  # The verdict is already decided by --conclusion. A gh outage here must
  # not turn a green run's exit status red.
  if ! gh "$@"; then
    printf 'codeql-canary: WARN — "gh %s" failed; the verdict is unaffected\n' "$*" >&2 || true
  fi
}

# The open canary issue, or empty. A gh outage here must not change the exit
# status below either.
open_issue=""
if [ -n "$fixture_open_issue" ]; then
  open_issue="$fixture_open_issue"
elif [ "$announce" -eq 1 ] && [ "$dry_run" -eq 0 ]; then
  open_issue="$(gh issue list --label "$label" --state open --limit 1 \
    --json number --jq '.[0].number // empty' 2>/dev/null || true)"
fi

if [ "$announce" -eq 1 ]; then
  if [ "$conclusion" = "success" ]; then
    if [ -n "$open_issue" ]; then
      body="CodeQL analysis succeeded again at \`${sha}\` (${run_url}). Closing
automatically — reopen if you disagree.

**Definition of done**

- [x] CodeQL analysis completes successfully on \`main\`.

<!-- codeql-canary -->"
      gh_run issue comment "$open_issue" --body "$body"
      gh_run issue close "$open_issue" \
        --reason completed \
        --comment "Closed by the CodeQL canary at ${sha}."
      printf 'codeql-canary: CodeQL recovered — closed #%s\n' "$open_issue" || true
    else
      printf 'codeql-canary: green, nothing open to close\n' || true
    fi
  else
    body="CodeQL's analyze job failed on \`main\` at \`${sha}\`.

Run: ${run_url}

**Why this matters**

CodeQL went silently red for five and a half weeks before anyone found it —
the run failed, then GitHub auto-disabled default setup after enough
consecutive failures, and after that there were not even red runs to notice.
This issue is the fix for the silence: it stays open for as long as CodeQL
does, one issue with a growing comment thread rather than a new issue per
failing run, and the canary closes it itself on the next green run.

**How to investigate**

\`\`\`sh
gh run view --log-failed \\
  \"\$(gh run list --repo macanderson/stella --workflow CodeQL --limit 1 \\
      --json databaseId --jq '.[0].databaseId')\"
\`\`\`

**Definition of done**

- [ ] CodeQL analysis completes successfully on \`main\`.

The canary closes this issue itself on its next green run, so a box nobody
ticks costs the issue nothing.

<!-- codeql-canary -->"

    if [ -n "$open_issue" ]; then
      gh_run issue comment "$open_issue" --body "Still red at \`${sha}\` (${run_url})."
      printf 'codeql-canary: still red — commented on #%s\n' "$open_issue" || true
    else
      # Make the label first. `gh issue create --label` fails if the label
      # does not exist yet, and `--force` makes this safe to repeat.
      gh_run label create "$label" \
        --color B60205 \
        --description "CodeQL is failing on main — filed by the CodeQL canary" \
        --force
      gh_run issue create \
        --title "CodeQL is red on main" \
        --label "$label" \
        --body "$body"
      printf 'codeql-canary: opened an issue for the failing scan\n' || true
    fi
  fi
fi

if [ "$conclusion" = "success" ]; then
  printf 'codeql-canary: OK — CodeQL is green at %s.\n' "$sha" || true
  exit 0
fi

printf 'codeql-canary: FAIL — CodeQL is red at %s.\n' "$sha" >&2 || true
exit 1
