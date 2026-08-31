#!/usr/bin/env bash
#
# Check the Homebrew tap formula against the newest published release.
#
#   ./scripts/check-tap-current.sh [--grace-secs N] [--tap-repo OWNER/REPO]
#   ./scripts/check-tap-current.sh --select --now <unix> --grace-secs <n>   # pure: JSON on stdin
# --help text ends here.
#
# ── The silence this closes ──────────────────────────────────────────────────
#
# The `homebrew` job in `release.yml` builds the formula and pushes it to the
# tap. It exits 0 with a warning when neither tap credential is set. So a
# release is never blocked on the tap. That is right for the release. It also
# means a tap that stops updating says nothing. The tag is cut. The release is
# published. `check-releases-published.sh` reports a clean bill of health. And
# `brew install macanderson/tap/stella` keeps serving the old build.
#
# An expired deploy key does this. So does a revoked token, or a renamed tap
# repo. Each one looks like success from every screen a maintainer checks.
#
# `check-releases-published.sh` beside it covers the same shape one step
# earlier. This gets the same cure. Compare the tap against what it should be,
# on a schedule, and say so.
#
# ── The grace window ─────────────────────────────────────────────────────────
#
# The tap is pushed by a job that runs after the release exists. So a release
# from seconds ago has no tap commit yet, and that is fine. This repo cuts a
# release on nearly every merge, so the newest one is often that young. A
# release inside the window proves nothing. The formula is not measured
# against it.
#
# ── What "stale" means here ──────────────────────────────────────────────────
#
# The formula is compared against the newest release that has settled. It is a
# finding only when the formula names an OLDER version. A formula newer than
# the newest settled release is normal. It happens for a few minutes after
# every release, while the new one ages past the window.
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
# and prints one tab-separated line when the tap is behind:
#
#   <formula-version>\t<expected-version>\t<releases-behind>\t<age-of-expected-secs>
#
# `formula_version` is null when the tap has no formula. That prints as
# `(none)` and counts as behind every settled release. A version string the
# rule cannot read lands there too. The reason is the one the I/O half below
# errors for: a check that cannot prove the tap is current must not say that it
# is. So `parts` returns null on input it cannot read. It must not emit
# nothing. A jq definition that emits nothing drops the whole document through
# the binding that reads it, and the script then prints a clean OK for a
# formula it never compared.
#
# A release with a null `published_unix` is a draft. `gh` gives it a publish
# date of `0001-01-01T00:00:00Z`, which is not a date. A draft is not
# something `brew install` can serve. So it is skipped. It must not become the
# newest release and the standard the tap is held to.
#
# No I/O here, so scripts/test-tap-current.sh can drive every rule from a
# fixture. A suite that asked GitHub would pass or fail for reasons outside
# the rule under test. The rules that matter here are the boundary ones. An
# alarm that fires in the minutes between a release and its tap push is one a
# maintainer mutes, and a muted alarm is the silence again.
#
# Versions compare as number arrays, not as strings. So 0.9.235 sorts below
# 0.9.9, the way a reader means it. ASCII would get it wrong. A tag that is
# not a plain dotted number is skipped, never guessed at.
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

# `gh` adds colour to `--json` output when it thinks it has a terminal. That
# makes the result invalid JSON. Turning it off costs nothing on a runner. By
# hand it is the difference between working and not.
#
# The limit is set above the real count and then CHECKED. This is the reason
# check-releases-published.sh checks its own: a cut-off release list changes
# the answer and says nothing. Here it would hide the newest release and call
# a stale tap current.
releases_limit=1000
#
# A draft carries no publish date. `gh` renders it as `0001-01-01T00:00:00Z`,
# which `fromdateiso8601` refuses. So it is carried through as a null that the
# rule skips. Dropping it here would move the reason for ignoring drafts into
# the half of the script no fixture can reach.
releases_json="$(
  CLICOLOR_FORCE=0 NO_COLOR=1 gh release list --limit "$releases_limit" --json tagName,publishedAt,isDraft \
    --jq '[ .[] | { tag: .tagName,
                    published_unix: (if .isDraft then null else (.publishedAt | try fromdateiso8601 catch null) end) } ]'
)"
if [ "$(printf '%s' "$releases_json" | jq 'length')" -ge "$releases_limit" ]; then
  echo "::error::the release list hit the ${releases_limit} page limit, so it may be truncated and the newest release may be missing from it. Raise releases_limit in $0." >&2
  exit 1
fi

# The formula, straight from the tap. A missing FILE is the finding this
# script exists for: the tap was never written, or the formula was renamed out
# from under `brew install`. An unreachable REPO is an unknown. An alarm that
# cannot ask its question must say so. It must not report a clean bill of
# health it did not earn.
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
