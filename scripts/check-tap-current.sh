#!/usr/bin/env bash
#
# Reconcile the Homebrew tap formula against the newest published release.
#
#   ./scripts/check-tap-current.sh [--grace-secs N] [--tap-repo OWNER/REPO]
#   ./scripts/check-tap-current.sh --select --now <unix> --grace-secs <n>   # pure: JSON on stdin
# --help text ends here.
#
# ── The silence this closes ──────────────────────────────────────────────────
#
# `release.yml`'s `homebrew` job renders the formula and pushes it to the tap,
# and it exits 0 with a warning when neither tap credential is set, so a release
# is never blocked on the tap being wired. That is the right call for the
# release and it means a tap that stops being updated says nothing anywhere: the
# tag is cut, the GitHub Release is published, `check-releases-published.sh`
# reports a clean bill of health, and `brew install macanderson/tap/stella`
# quietly keeps serving whichever build the tap last received. An expired deploy
# key, a revoked token, or a renamed tap repo all land here, and all of them look
# exactly like success from every surface a maintainer glances at.
#
# That is the same shape check-releases-published.sh beside it exists for, one
# hop further down the pipeline, and it gets the same remedy: compare the state
# the tap is in against the state it should be in, on a cadence, and say so.
#
# ── The grace window ─────────────────────────────────────────────────────────
#
# The tap is pushed by a job that runs after the GitHub Release exists, so a
# release published seconds ago legitimately has no tap commit yet. This repo
# cuts a release on nearly every merge to main, so the newest release is
# routinely that young. A release inside the grace window is therefore not
# evidence of anything and is not what the formula is measured against.
#
# ── What "stale" means here ──────────────────────────────────────────────────
#
# The formula is compared against the newest release that has settled, and it
# is a finding only when the formula names an OLDER version. A formula that is
# newer than the newest settled release is the normal state a few minutes after
# every release — the tap job has run and the release it shipped has not aged
# past the window yet.
set -euo pipefail

# shellcheck source=scripts/lib/help-header.sh
. "$(dirname "$0")/lib/help-header.sh"

grace_secs=$((90 * 60))
select_only=0
now=""
tap_repo="macanderson/homebrew-tap"
formula_path="Formula/stella.rb"

while [ $# -gt 0 ]; do
  case "$1" in
    --grace-secs) grace_secs="${2:?--grace-secs needs a number}"; shift 2 ;;
    --now) now="${2:?--now needs a unix timestamp}"; shift 2 ;;
    --tap-repo) tap_repo="${2:?--tap-repo needs OWNER/REPO}"; shift 2 ;;
    --formula-path) formula_path="${2:?--formula-path needs a path}"; shift 2 ;;
    --select) select_only=1; shift ;;
    -h|--help) print_help_header "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# ── The decision, as a pure function ─────────────────────────────────────────
#
# Reads one JSON document on stdin:
#
#   { "releases":        [ { "tag": "v0.9.292", "published_unix": 1234567890 }, … ],
#     "formula_version": "0.9.235" }
#
# and prints one tab-separated finding line when the tap is behind:
#
#   <formula-version>\t<expected-version>\t<releases-behind>\t<age-of-expected-secs>
#
# `formula_version` is null when the tap carries no formula at all, which is
# reported as `(none)` and counts as behind every settled release. A version
# string the rule cannot parse lands in the same place, for the same reason the
# I/O half below errors on an unreachable tap: a check that cannot establish the
# tap is current must not report that it is. `parts` therefore returns null on
# anything it cannot read rather than emitting nothing — a jq definition that
# emits nothing on one input silently drops the whole document through the
# binding that consumes it, and the script then prints a clean OK for a formula
# it never compared.
#
# A release with a null `published_unix` is a draft. `gh` reports one with a
# publish date of `0001-01-01T00:00:00Z`, which is not a date at all, and a
# draft is not something `brew install` can serve — so it is skipped rather than
# taken for the newest release and made the standard the tap is held to.
#
# Kept free of I/O so scripts/test-tap-current.sh can drive every rule from a
# fixture. A suite that asked GitHub would pass or fail for reasons that have
# nothing to do with the rule under test, and the rules that matter here are the
# boundary ones: an alarm that fires on the minutes between a release and its
# tap push is one a maintainer mutes, and a muted alarm is the silence again.
#
# Versions compare as number arrays rather than as strings, so 0.9.235 sorts
# below 0.9.9 the way a reader means and not the way ASCII does. A tag that is
# not a plain dotted-numeric version is skipped rather than guessed at.
select_stale() {
  jq -r --argjson now "$now" --argjson grace "$grace_secs" '
    def parts:
      ltrimstr("v")
      | if test("^[0-9]+(\\.[0-9]+)*$") then split(".") | map(tonumber) else null end;

    ( .formula_version // null ) as $formula
    | ( if $formula == null then null else ($formula | parts) end ) as $formula_parts
    | [ .releases[]
        | select(.published_unix != null)
        | select(($now - .published_unix) > $grace)
        | . + { parts: (.tag | parts) }
        | select(.parts != null)
      ] as $settled
    | if ($settled | length) == 0 then empty
      else
        ( $settled | sort_by(.parts) | last ) as $newest
        | ( if $formula_parts == null then $settled
            else [ $settled[] | select(.parts > $formula_parts) ]
            end ) as $behind
        | if ($behind | length) == 0 then empty
          else
            "\($formula // "(none)")\t\($newest.tag | ltrimstr("v"))\t\($behind | length)\t\($now - $newest.published_unix)"
          end
      end
  '
}

if [ "$select_only" -eq 1 ]; then
  [ -n "$now" ] || { echo "--select needs --now" >&2; exit 2; }
  select_stale
  exit 0
fi

command -v gh >/dev/null 2>&1 || { echo "::error::gh is not installed" >&2; exit 1; }
command -v jq >/dev/null 2>&1 || { echo "::error::jq is not installed" >&2; exit 1; }
[ -n "$now" ] || now="$(date -u +%s)"

# `gh` colorizes even `--json` output when it believes it has a terminal, which
# makes the result invalid JSON. It costs nothing on a runner and is the
# difference between working and not when a maintainer runs this by hand.
#
# The limit is above the real count and then CHECKED, for the reason
# check-releases-published.sh checks its own: a truncated release list silently
# changes the answer, and here it would hide the newest release and report a
# current tap as current for the wrong reason.
releases_limit=1000
#
# A draft carries no publish date — `gh` renders it as `0001-01-01T00:00:00Z`,
# which `fromdateiso8601` refuses outright — so it is carried through as a null
# the rule skips rather than dropped here. Dropping it would put the rule's
# reason for ignoring drafts in the half of the script no fixture can reach.
releases_json="$(
  CLICOLOR_FORCE=0 NO_COLOR=1 gh release list --limit "$releases_limit" --json tagName,publishedAt,isDraft \
    --jq '[ .[] | { tag: .tagName,
                    published_unix: (if .isDraft then null else (.publishedAt | try fromdateiso8601 catch null) end) } ]'
)"
if [ "$(printf '%s' "$releases_json" | jq 'length')" -ge "$releases_limit" ]; then
  echo "::error::the release list hit the ${releases_limit} page limit, so it may be truncated and the newest release may be missing from it. Raise releases_limit in $0." >&2
  exit 1
fi

# The formula, straight from the tap. A missing FILE is the finding this script
# exists for (the tap was never written, or the formula was renamed out from
# under `brew install`); an unreachable REPO is an unknown, and an alarm that
# cannot ask its question must say so rather than report a clean bill of health
# it did not earn.
formula_version="null"
if formula_text="$(gh api "repos/${tap_repo}/contents/${formula_path}" \
  -H "Accept: application/vnd.github.raw" 2>/dev/null)"; then
  parsed="$(printf '%s\n' "$formula_text" | sed -n 's/^[[:space:]]*version[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
  if [ -z "$parsed" ]; then
    echo "::error::${tap_repo}/${formula_path} carries no \`version \"…\"\` line, so the tap's version cannot be read. The formula is rendered from .github/homebrew/stella.rb.tmpl — check that template still pins an explicit version." >&2
    exit 1
  fi
  formula_version="$(jq -n --arg v "$parsed" '$v')"
elif ! gh api "repos/${tap_repo}" >/dev/null 2>&1; then
  echo "::error::cannot reach the tap repo ${tap_repo}. This check could not ask its question — it is not reporting that the tap is fine. Confirm the repo still exists and is readable by this token." >&2
  exit 1
fi

doc="$(jq -n --argjson releases "$releases_json" --argjson formula "$formula_version" \
  '{ releases: $releases, formula_version: $formula }')"

report="$(printf '%s' "$doc" | select_stale)"

release_count="$(printf '%s' "$releases_json" | jq 'length')"
shown="$(printf '%s' "$formula_version" | jq -r '. // "(none)"')"
if [ -z "$report" ]; then
  echo "check-tap-current: OK — ${tap_repo}/${formula_path} is at ${shown}, which is not behind any release older than $((grace_secs / 60))m (${release_count} release(s) checked)."
  exit 0
fi

IFS="$(printf '\t')" read -r formula expected behind age <<EOF
$report
EOF

echo "::error::the Homebrew tap is ${behind} release(s) behind. ${tap_repo}/${formula_path} serves ${formula} while the newest published release is ${expected} — the tap stopped being updated and every other surface still reports success."
echo ""
echo "  tap              ${tap_repo}/${formula_path}"
echo "  tap formula      ${formula}"
echo "  newest release   ${expected}  (published $((age / 3600))h ago)"
echo "  releases behind  ${behind}"
echo ""
echo "Check what happened:  gh run list --workflow=release.yml --limit 20 --json conclusion,displayTitle"
echo "The job that pushes:  .github/workflows/release.yml, the \`homebrew\` job"
echo "Most likely cause:    neither HOMEBREW_TAP_DEPLOY_KEY nor HOMEBREW_TAP_TOKEN is set or still valid — that path warns and exits 0. See RELEASING.md § One-time setup."
exit 1
