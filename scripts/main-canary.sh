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
# A shared cell is not always a file. On 2026-08-18 it was a *seam*: two PRs
# each redesigned how `agent::init_workspace` talks to its surface, both green,
# and git reported no conflict because neither side contained the other's
# lines. The composed tree did not compile (#3659). This script ran on that
# commit and reported `ok` for everything it knew how to ask — it compiled
# nothing (#3660). The `compile` row below is that gap closed, and it is why
# this canary is minutes rather than seconds.
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

# A decided verdict must survive a reader that closes the pipe early (#1815).
# Unlike the buffered guards, this script prints per check, so a `| head -1`
# reader can close the pipe while writes are still coming. `trap '' PIPE`
# turns the resulting SIGPIPE into a failed write instead of a kill, and every
# progress write below carries `|| true` so `set -e` cannot promote that
# failed write into the exit status. The exit status is the verdict, never
# the fate of a printf.
trap '' PIPE

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
    # The whole comment block after the shebang, bounded by its first
    # non-comment line — hardcoded line numbers truncate silently the first
    # time the header grows.
    awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"
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
  # The other shared cell this file's header already names, and the one
  # AGENTS.md calls the single biggest cause of a red `main` (#3447). It earns
  # its row on the rule above rather than on thoroughness: the pre-merge guard
  # is deliberately base-relative (#2004, #2397) and fails a PR only for growth
  # past what it inherited, so drift already sitting on `main` is invisible to
  # every branch in flight — by design, which is precisely what makes it
  # unfixable before a merge. On 2026-08-17 `command_deck.rs` sat 4 lines over
  # its ceiling on `main` while every open PR stayed correctly green; it was
  # found by hand during an unrelated merge and fixed by #3444.
  #
  # `--absolute` is load-bearing and not a tidiness flag. Without it the guard
  # judges a push to `main` against `HEAD^1`, so it would only ever notice
  # drift the newest commit introduced — and the 2026-08-17 drift, already two
  # commits old by the time anyone looked, passes that test cleanly. Verified
  # by simulation before this row was added: with the baseline set back to
  # 4010, the default invocation still reported ok.
  "file-size|./scripts/check-file-size.sh --absolute"
  # Does the merged tree still BUILD? This row costs more than the two above
  # put together, and it earns that on the admission rule rather than despite
  # it: a shared cell does not have to be a file. It can be a *seam*.
  #
  # On 2026-08-18 `main` did not compile at 9fa4163fa. #3642 and #3647 each
  # redesigned how `agent::init_workspace` talks to its surface — one passing
  # the transcript sink and question channel as one `InitIo`, the other
  # splitting a line into `Step` vs `Progress`. Both branches were internally
  # consistent, both were green, and git reported no conflict because neither
  # side contained the other's lines. The merge kept one's signature with the
  # other's body and produced an `/init` arm that is not parseable Rust
  # (#3659). Every `stella-cli` test and the release binary were down.
  #
  # This canary ran on that exact commit and said `ok lockfile-sync`. It
  # noticed only that the splice had pushed `command_deck.rs` four lines over
  # its ceiling — the build break itself was invisible, because nothing here
  # compiled anything (#3660).
  #
  # `--all-targets` is load-bearing: the same splice shape lands in test files
  # (AGENTS.md's deleted-test guard documents #1976 and #1860), and a break
  # confined to a test target is exactly the one no other post-merge check
  # sees. `--locked` matches what `install.sh` builds under, and stops cargo
  # from silently rewriting the lock to paper over a skew the row above is
  # trying to report. `--offline` applies only to the hermetic fixture, whose
  # workspace has no dependencies to fetch.
  "compile|cargo check --workspace --all-targets --locked${manifest_dir:+ --offline --manifest-path $manifest_dir/Cargo.toml}"
)

# The remediation for ONE failing check. Per-check on purpose: this block used
# to be a single hardcoded lockfile recipe printed whatever had failed, so
# #3655 — a file-size failure — told its reader to regenerate `Cargo.lock`,
# which fixes nothing and costs them the time it takes to find that out. An
# issue that misdirects is worse than an issue that only names the check.
remedy_for() {
  case "$1" in
  lockfile-sync)
    cat <<'SH'
cargo metadata --format-version 1 >/dev/null   # regenerates Cargo.lock
git add Cargo.lock
SH
    ;;
  file-size)
    cat <<'SH'
# Split the oversized file so it fits its ceiling — a sibling submodule, the
# way crates/stella-core/src/driver/settlement.rs came out of driver.rs.
# ONLY if the growth is genuinely irreducible, and say so in review:
#   ./scripts/check-file-size.sh --update
SH
    ;;
  compile)
    cat <<'SH'
cargo check --workspace --all-targets --locked
SH
    ;;
  *)
    printf '# no recorded remedy for this check — see its output above\n'
    ;;
  esac
}

failures=""
failed_names=""
remedies=""

cd "$repo_root"

for row in "${checks[@]}"; do
  name="${row%%|*}"
  cmd="${row#*|}"
  if out="$(eval "$cmd" 2>&1)"; then
    printf 'main-canary: ok   %s\n' "$name" || true
  else
    printf 'main-canary: FAIL %s\n' "$name" >&2 || true
    failed_names="${failed_names}${failed_names:+, }${name}"
    failures="${failures}### \`${name}\`

\`\`\`
${out}
\`\`\`

"
    remedies="${remedies}#### \`${name}\`

\`\`\`sh
$(remedy_for "$name")
\`\`\`

"
  fi
done

green=1
[ -n "$failed_names" ] && green=0

# ── Announcing ───────────────────────────────────────────────────────────────

gh_run() {
  if [ "$dry_run" -eq 1 ]; then
    printf 'main-canary: [dry-run] gh %s\n' "$*" || true
    return 0
  fi
  # Best-effort, like the lookup below: the verdict is already decided, and a
  # gh outage while announcing must not turn a green main red under `set -e`.
  # The failure still lands in the workflow log, where a broken announcer is
  # an operational problem — not a statement about the tree.
  if ! gh "$@"; then
    printf 'main-canary: WARN — "gh %s" failed; the verdict is unaffected\n' "$*" >&2 || true
  fi
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
      printf 'main-canary: main recovered — closed #%s\n' "$open_issue" || true
    fi
  else
    body="The post-merge canary found \`main\` broken at \`${short_sha}\`.

Failing: **${failed_names}**

${failures}
### Why this was not caught before the merge

These checks are enforced against a shared cell — something every PR of a
given shape must write. Usually that is a file (\`Cargo.lock\`,
\`scripts/file-size-baseline.txt\`); it can equally be a *seam* two branches
each redesign in good faith (#3659). Either way, two branches can each be
correct, pass every check, and still compose into a broken tree once both
land. No pre-merge check has both halves in front of it, which is why this
canary exists (#3332).

### How to fix

\`\`\`sh
git checkout main && git pull
\`\`\`

...then, per failing check:

${remedies}Land it on its own from a fresh \`main\` — do not fold it into an unrelated PR,
for the reason AGENTS.md gives about the file-size baseline.

### Blast radius while this is open

A red \`main\` reds every open PR, so the next contributor inherits a failure
they did not cause and may spend their session on it.

If **compile** is among the failures, nothing builds: the whole test suite and
the release binary are down, not one crate. If **lockfile-sync** is, everything
that passes \`--locked\` breaks — the MSRV check, the \`stella-serve\` container
build, and \`install.sh\`, which reaches people installing from source rather
than only CI.

<!-- main-canary -->"

    if [ -n "$open_issue" ]; then
      gh_run issue comment "$open_issue" --body "Still red at \`${short_sha}\` — failing: ${failed_names}."
      printf 'main-canary: still red — commented on #%s\n' "$open_issue" || true
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
      printf 'main-canary: opened an issue for %s\n' "$failed_names" || true
    fi
  fi
fi

if [ "$green" -eq 1 ]; then
  printf 'main-canary: OK — main composes green at %s.\n' "$short_sha" || true
  exit 0
fi

printf 'main-canary: FAIL — main is red at %s (%s).\n' "$short_sha" "$failed_names" >&2 || true
exit 1
