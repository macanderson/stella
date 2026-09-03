#!/usr/bin/env bash
#
# Reconcile tags against published releases. See #1464.
#
#   ./scripts/check-releases-published.sh [--grace-secs N]
#   ./scripts/check-releases-published.sh --select --now <unix> --grace-secs <n>   # pure: JSON on stdin
# --help text ends here.
#
# ── The silence ──────────────────────────────────────────────────────────────
#
# `release.yml` failed on 31 consecutive tags (v0.6.75 → v0.6.108, about two
# days) and nothing anywhere said so. The repository kept cutting version
# numbers the whole time while shipping no binaries.
#
# It was invisible because every surface a maintainer glances at reported
# success: `ci` on main green, `auto-tag` succeeded (it tags, dispatches, and
# exits without looking at the outcome), the version-sync PR opened and
# auto-merged, CHANGELOG.md rolled, and the manifests were stamped. Only the
# last two jobs of `release.yml` skipped. `gh release list` was the single
# place the gap showed, and nobody reads it on a cadence.
#
# ── Why a reconciler rather than watching the dispatched run ─────────────────
#
# The obvious fix is to have `auto-tag` watch the `release.yml` run it
# dispatches. But `auto-tag`'s concurrency group is serialized with
# `cancel-in-progress: false`, and its comments are explicit that every failure
# path warns and exits 0 *specifically* so a bad release cannot jam the queue
# under a burst of merges. Holding that job open for an hour of full-LTO builds
# is the thing it was written to avoid.
#
# Comparing state also catches strictly more: a dispatch that never started, a
# run cancelled by a runner outage, and a release deleted after the fact are all
# invisible to a watcher of one run and all visible here.
#
# ── The grace window ─────────────────────────────────────────────────────────
#
# A red release run is indistinguishable at a glance from one still building —
# full-LTO plus the independent rebuild arm takes the better part of an hour, so
# "not published yet" is the NORMAL state for a long window after every merge.
# A tag younger than the grace window is therefore not evidence of anything and
# is skipped. Too short and this cries wolf on every release; too long and the
# silence it exists to break lasts that much longer.
#
# ── Why the release list has no fixed cap ────────────────────────────────────
#
# A fixed `--limit` is a trap that reopens itself. A cap of 200 once reported
# 137 missing tags instead of 26 real ones, because every release past the
# cut looked unpublished. Raising the number just buys a few weeks: this
# repo cuts a release on almost every merge.
#
# `repos/{owner}/{repo}/releases` is a plain paginated list, not a search
# endpoint, so it has no result ceiling. `gh api --paginate` walks every
# page and gets the whole set no matter how big it grows. No cap is needed.
# `max_pages` below is not a cap either. It is a sanity check that paging
# itself still works. See "the fetch" further down.
set -euo pipefail

# shellcheck source=scripts/lib/help-header.sh
. "$(dirname "$0")/lib/help-header.sh"

grace_secs=$((90 * 60))
select_only=0
update=0
now=""

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
# Tags already known to have shipped nothing, so the alarm is about NEW silence.
# Enforcing bare would have fired on 26 historical tags the first time it ran,
# and an alarm that reports a backlog every run is one everybody mutes — which
# is the same outcome as the silence it replaced. Same reasoning, and the same
# shape, as scripts/file-size-baseline.txt.
baseline="$repo_root/scripts/unpublished-tags-baseline.txt"

while [ $# -gt 0 ]; do
  case "$1" in
    --grace-secs) grace_secs="${2:?--grace-secs needs a number}"; shift 2 ;;
    --now) now="${2:?--now needs a unix timestamp}"; shift 2 ;;
    --select) select_only=1; shift ;;
    --update) update=1; shift ;;
    -h|--help) print_help_header "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# ── The decision, as a pure function ─────────────────────────────────────────
#
# Reads one JSON document on stdin:
#
#   { "tags":     [ { "name": "v0.6.75", "created_unix": 1234567890 }, … ],
#     "releases": [ { "name": "v0.6.74", "draft": false }, … ] }
#
# and prints one `<tag>\t<age-in-seconds>` line per tag that should have been
# published and was not, oldest first. Kept free of I/O so
# scripts/test-releases-published.sh can drive every rule from a fixture rather
# than from the live repository — a suite that asked GitHub would pass or fail
# for reasons that have nothing to do with the rule under test.
#
# A draft release does not count as published: `brew install` cannot serve
# one and it can sit open indefinitely, so a tag with only a draft release
# must still be reported. `draft` rides on each release object rather than
# being filtered out before this function sees the data, so a fixture can
# cover the rule directly instead of trusting the wrapper below got it right.
#
# Note what is NOT consulted: the outcome of any workflow run. A `release`/
# `homebrew` job is gated on `startsWith(github.ref, 'refs/tags/')`, so
# "skipped" is the correct state for a `workflow_dispatch` on a branch and
# reading run conclusions would report those as failures. Tag-versus-release is
# the question that has one right answer.
#
# `.known` is the grandfathered set (see `baseline` above). It is an input to
# the pure function rather than read from disk here, so the suite can drive the
# grandfathering rule too — an exemption that nothing tests is an exemption
# nobody can be sure still applies to what it was written for.
select_unpublished() {
  jq -r --argjson now "$now" --argjson grace "$grace_secs" '
    ( [ .releases[] | select(.draft != true) | { (.name): true } ] | add // {} ) as $published
    | ( [ (.known // [])[] | { (.): true } ] | add // {} ) as $known
    | [ .tags[]
        | select($published[.name] | not)
        | select($known[.name] | not)
        | select(($now - .created_unix) > $grace)
      ]
    | sort_by(.created_unix)
    | .[]
    | "\(.name)\t\($now - .created_unix)"
  '
}

# The baseline as a JSON array, ignoring comments and blank lines.
known_json() {
  if [ -f "$baseline" ]; then
    grep -v '^[[:space:]]*#' "$baseline" | jq -R -s 'split("\n") | map(select(length > 0))'
  else
    echo '[]'
  fi
}

if [ "$select_only" -eq 1 ]; then
  [ -n "$now" ] || { echo "--select needs --now" >&2; exit 2; }
  select_unpublished
  exit 0
fi

command -v gh >/dev/null 2>&1 || { echo "::error::gh is not installed" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "::error::jq is not installed" >&2; exit 1; }
[ -n "$now" ] || now="$(date -u +%s)"

# Tag creation dates come from git rather than the API: `gh api .../tags` pages
# by name and carries no date at all, and the workflow checks out with
# fetch-tags anyway. `creatordate` is the tag's own date for an annotated tag
# and the commit's for a lightweight one, which is the date a reader means.
tags_json="$(
  git for-each-ref --format='%(refname:short) %(creatordate:unix)' 'refs/tags/v*' \
    | jq -R -s 'split("\n") | map(select(length > 0) | split(" ") | { name: .[0], created_unix: (.[1] | tonumber) })'
)"

# `gh` colorizes even plain output when it believes it has a terminal, which
# makes the result invalid JSON. It costs nothing on a runner (never a TTY) and
# is the difference between working and not when a maintainer runs this by hand.
#
# ── The fetch ─────────────────────────────────────────────────────────────────
#
# `--paginate` walks every page until none remain, so the set below is every
# release, not just the newest ones. `--slurp` wraps each page's array into
# one outer array. It cannot be combined with `--jq` (gh rejects that), so
# the flatten from pages to releases happens in the plain `jq` call after.
#
# `max_pages` is a sanity check, not a cap. A normal run needs single-digit
# pages. Needing close to a thousand does not mean the repo grew that much —
# it means paging broke, and a human should look before trusting this run.
max_pages=1000 # 100000 releases at per_page=100 — decades past this repo's rate
pages_json="$(CLICOLOR_FORCE=0 NO_COLOR=1 gh api --paginate --slurp 'repos/{owner}/{repo}/releases?per_page=100')"
page_count="$(printf '%s' "$pages_json" | jq 'length')"
if [ "$page_count" -gt "$max_pages" ]; then
  echo "::error::the release list needed ${page_count} pages to walk fully, past the ${max_pages}-page sanity ceiling. That is not plausible growth for this repository — gh's pagination itself likely misbehaved. Investigate before trusting this run's answer." >&2
  exit 1
fi
releases_json="$(printf '%s' "$pages_json" | jq '[ .[][] | { name: .tag_name, draft: .draft } ]')"

doc="$(jq -n --argjson tags "$tags_json" --argjson releases "$releases_json" \
  --argjson known "$(known_json)" '{ tags: $tags, releases: $releases, known: $known }')"

if [ "$update" -eq 1 ]; then
  {
    echo "# Tags that shipped nothing and are NOT going to be republished."
    echo "# See #1463 (what to do with them) and #1464 (why this file exists)."
    echo "#"
    echo "# Regenerate with: make releases-baseline-update"
    echo "#"
    echo "# Grandfathering these is what lets check-releases-published.sh be an"
    echo "# alarm about NEW silence. A tag added here is a release that will never"
    echo "# exist, so every addition should be a decision someone made, visible in"
    echo "# review — not a way to quiet the check."
    printf '%s' "$doc" | jq -r --argjson now "$now" --argjson grace "$grace_secs" '
      ( [ .releases[] | select(.draft != true) | { (.name): true } ] | add // {} ) as $published
      | [ .tags[] | select($published[.name] | not) | select(($now - .created_unix) > $grace) ]
      | sort_by(.created_unix) | .[] | .name'
  } >"$baseline.tmp"
  mv "$baseline.tmp" "$baseline"
  echo "check-releases-published: baseline updated — $(grep -cv '^#' "$baseline") grandfathered tag(s)."
  exit 0
fi

report="$(printf '%s' "$doc" | select_unpublished)"

total_tags="$(printf '%s' "$tags_json" | jq 'length')"
known_count="$(known_json | jq 'length')"
if [ -z "$report" ]; then
  echo "check-releases-published: OK — every v* tag older than $((grace_secs / 60))m has a published release (${total_tags} tag(s) checked, ${known_count} grandfathered)."
  exit 0
fi

count="$(printf '%s\n' "$report" | wc -l | tr -d ' ')"
echo "::error::${count} tag(s) were cut but never published. The repository is stamping version numbers while shipping no binaries — this is the #1464 silence, live."
echo ""
echo "Tag                  Age"
printf '%s\n' "$report" | while IFS="$(printf '\t')" read -r tag age; do
  printf '  %-18s %sh\n' "$tag" "$((age / 3600))"
done
echo ""
echo "Check what happened:  gh run list --workflow=release.yml --limit 40 --json conclusion,headBranch"
echo "Re-run one by hand:   see RELEASING.md § When a release fails"
exit 1
