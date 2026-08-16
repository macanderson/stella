#!/usr/bin/env bash
#
# The post-merge canary: is `main` still green after the merge? (#3332)
#
# ── The failure this exists for ──────────────────────────────────────────────
#
# Some of this repository's guards are enforced against a *shared cell* — one
# file every PR of a given shape must write. `Cargo.lock` is one. So is
# `scripts/file-size-baseline.txt`, which AGENTS.md already names as the single
# biggest cause of a red `main`. Two branches can each write that cell
# correctly, pass every check, and compose into a broken tree the moment both
# land — because the defect is in the *composition*, and no pre-merge check
# has both halves in front of it.
#
# On 2026-08-16 this fired twice in one day. #3311 added a crate; the 0.9.50
# release sync (#3323) regenerated the lock while it was in flight; the merge
# left 23 crates at 0.9.50 and stella-plugin alone at 0.9.49. Every `--locked`
# build on `main` failed — the MSRV job, the container build, and install.sh,
# which is the one that reaches people installing from source. Both PRs were
# green. Neither author did anything wrong.
#
# `scripts/check-lockfile-sync.sh` is the pre-merge half and cannot see this;
# its own header says so. This is the other half: it looks at `main` *after*
# the merge, which is the first moment the question can be asked at all.
#
# ── Why it files an issue instead of just going red ──────────────────────────
#
# A scheduled workflow that fails silently into the Actions tab is the exact
# shape of #1464, where release.yml failed on 31 consecutive tags over two days
# and nothing anywhere said so. Every surface a maintainer glances at was
# green. So the canary's output is an *issue*: it survives, it notifies, and it
# is the thing people actually read.
#
# It is equally important that it closes that issue when `main` recovers. A
# canary that only ever opens issues becomes a stale-issue generator, gets
# muted, and is then worth less than nothing — it is a monitor everyone has
# learned to ignore. Recovery is detected and announced on the same run.
#
# Exactly one issue is kept open at a time, found by label. A red `main` that
# stays red across ten merges is one issue with ten comments, not ten issues.
#
# ── What it deliberately does NOT do ─────────────────────────────────────────
#
# It does not open a fix PR. It could — the remediation for a lock skew is one
# regenerated file, and two humans hand-wrote that same commit on 2026-08-16.
# But a bot with push access, opening branches against a protected `main` under
# an auto-merge policy, is a much larger authority decision than "tell someone
# accurately, fast". The issue body carries the exact commands instead. If that
# tradeoff should change, change it deliberately, not by extending this script.
#
# Usage:
#   scripts/main-canary.sh                      # check only; exit 1 if main is red
#   scripts/main-canary.sh --announce           # ...and open/refresh/close the issue
#   scripts/main-canary.sh --dry-run            # ...printing gh actions instead of running them
#   scripts/main-canary.sh --manifest-dir DIR   # check a fixture tree (tests)
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

announce=0
dry_run=0
manifest_dir=""
fixture_open_issue=""
label="main-red"

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
  --label)
    [ $# -ge 2 ] || {
      echo "main-canary: --label needs a value" >&2
      exit 2
    }
    label="$2"
    shift 2
    ;;
  --manifest-dir)
    [ $# -ge 2 ] || {
      echo "main-canary: --manifest-dir needs a directory" >&2
      exit 2
    }
    manifest_dir="$2"
    shift 2
    ;;
  # Test-only: stand in for the `gh issue list` lookup below. The recovery
  # branch — close the issue when main goes green — only runs when an issue is
  # already open, so without this seam the one branch that keeps this canary
  # from becoming a stale-issue generator could never be exercised offline.
  --fixture-open-issue)
    [ $# -ge 2 ] || {
      echo "main-canary: --fixture-open-issue needs a number" >&2
      exit 2
    }
    fixture_open_issue="$2"
    shift 2
    ;;
  -h | --help)
    sed -n '2,58p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "main-canary: unknown argument '$1'" >&2
    exit 2
    ;;
  esac
done

# `--dry-run` without `--announce` would be a no-op that still reads as a
# configured monitor. Reject it rather than run something that cannot report.
if [ "$dry_run" -eq 1 ] && [ "$announce" -eq 0 ]; then
  echo "main-canary: --dry-run only means something with --announce" >&2
  exit 2
fi

sha="$(git -C "$repo_root" rev-parse HEAD 2>/dev/null || echo unknown)"
short_sha="${sha:0:9}"

# ── The checks ───────────────────────────────────────────────────────────────
#
# Only composition-sensitive checks belong here — the ones where two green
# branches can still compose red. A check that a single PR fully determines is
# already caught pre-merge, and running it again post-merge buys noise, not
# safety. Add a row when a new shared cell appears, not to be thorough.
#
# Each row is "<name>|<command>". The command runs from the repo root.
checks=(
  "lockfile-sync|./scripts/check-lockfile-sync.sh${manifest_dir:+ --manifest-dir $manifest_dir}"
)

failures=""
failed_names=""

cd "$repo_root"

for row in "${checks[@]}"; do
  name="${row%%|*}"
  cmd="${row#*|}"
  if out="$(eval "$cmd" 2>&1)"; then
    printf 'main-canary: ok   %s\n' "$name"
  else
    printf 'main-canary: FAIL %s\n' "$name" >&2
    failed_names="${failed_names}${failed_names:+, }${name}"
    failures="${failures}### \`${name}\`

\`\`\`
${out}
\`\`\`

"
  fi
done

green=1
[ -n "$failed_names" ] && green=0

# ── Announcing ───────────────────────────────────────────────────────────────

gh_run() {
  if [ "$dry_run" -eq 1 ]; then
    printf 'main-canary: [dry-run] gh %s\n' "$*"
    return 0
  fi
  gh "$@"
}

# The open canary issue, or empty. Best-effort by design: a gh outage must not
# turn a green main red, nor a red main green — the verdict is already decided
# above and the exit status below does not consult this.
open_issue=""
if [ -n "$fixture_open_issue" ]; then
  open_issue="$fixture_open_issue"
elif [ "$announce" -eq 1 ] && [ "$dry_run" -eq 0 ]; then
  open_issue="$(gh issue list --label "$label" --state open --limit 1 \
    --json number --jq '.[0].number // empty' 2>/dev/null || true)"
fi

if [ "$announce" -eq 1 ]; then
  if [ "$green" -eq 1 ]; then
    if [ -n "$open_issue" ]; then
      body="\`main\` is green again as of ${short_sha}: every composition check
below passes. Closing automatically — reopen if you disagree.

Checked: $(printf '%s ' "${checks[@]%%|*}")"
      gh_run issue comment "$open_issue" --body "$body"
      gh_run issue close "$open_issue" \
        --reason completed \
        --comment "Closed by the post-merge canary at ${short_sha}."
      printf 'main-canary: main recovered — closed #%s\n' "$open_issue"
    fi
  else
    body="The post-merge canary found \`main\` broken at \`${short_sha}\`.

Failing: **${failed_names}**

${failures}
### Why this was not caught before the merge

These checks are enforced against a shared cell — a file every PR of a given
shape must write. Two branches can each write it correctly, pass every check,
and still compose into a broken tree once both land. No pre-merge check has
both halves in front of it, which is why this canary exists (#3332).

### How to fix

\`\`\`sh
git checkout main && git pull
cargo metadata --format-version 1 >/dev/null   # or: cargo check --workspace
git add Cargo.lock
\`\`\`

Land it on its own from a fresh \`main\` — do not fold it into an unrelated PR,
for the reason AGENTS.md gives about the file-size baseline.

### Blast radius while this is open

Everything that passes \`--locked\`: the MSRV check, the \`stella-serve\`
container build, and \`install.sh\` — which builds \`--locked\`, so this reaches
people installing from source, not only CI. A red \`main\` also reds every open
PR, so the next contributor inherits a failure they did not cause.

<!-- main-canary -->"

    if [ -n "$open_issue" ]; then
      gh_run issue comment "$open_issue" --body "Still red at \`${short_sha}\` — failing: ${failed_names}."
      printf 'main-canary: still red — commented on #%s\n' "$open_issue"
    else
      # Ensure the label first: `gh issue create --label` fails outright on a
      # label that does not exist, which would break the canary at the exact
      # moment it has something to say. `--force` makes this idempotent, so it
      # is a no-op on every run after the first.
      gh_run label create "$label" \
        --color B60205 \
        --description "main is broken on the merged tree — filed by the post-merge canary" \
        --force
      gh_run issue create \
        --title "main is red: ${failed_names} fails on the merged tree" \
        --label "$label" \
        --body "$body"
      printf 'main-canary: opened an issue for %s\n' "$failed_names"
    fi
  fi
fi

if [ "$green" -eq 1 ]; then
  printf 'main-canary: OK — main composes green at %s.\n' "$short_sha"
  exit 0
fi

printf 'main-canary: FAIL — main is red at %s (%s).\n' "$short_sha" "$failed_names" >&2
exit 1
