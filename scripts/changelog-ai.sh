#!/usr/bin/env bash
#
# Draft CHANGELOG.md entries for a release from its actual diff.
#
# Usage: changelog-ai.sh [<git-range>]        e.g. changelog-ai.sh v0.6.42..HEAD
#
# Prints a Keep-a-Changelog section body (### Added / ### Changed / ### Fixed
# bullets, or a one-line "_Internal: …_" note for a release with nothing
# user-facing) to stdout. Prints NOTHING and exits 0 when it cannot draft —
# no API key, the call failed, or the response didn't look like changelog
# markdown — so callers can treat empty output as "leave the changelog alone".
#
# Called from auto-tag.yml, which generates the entries once per release and
# hands the file to BOTH scripts/sync-versions.sh call sites (the tagged
# release commit and the bot/version-sync PR) via CHANGELOG_ENTRIES_FILE, so
# the tag and main roll identical text. Also runnable locally against any
# range when writing an entry by hand and wanting a first draft.
#
# Uses the same AI Gateway + key as release.yml's GitHub-release notes step:
# the two prose surfaces should not need two credentials. Same degrade-open
# rule too — a release is never blocked, and never altered, by a failed draft.
#
#   AI_GATEWAY_API_KEY   required to produce output; unset -> warn, print nothing
#   CHANGELOG_AI_MODEL   optional, default anthropic/claude-sonnet-5
set -euo pipefail

range="${1:-}"

if ! command -v jq >/dev/null 2>&1; then
  echo "changelog-ai: jq not found; printing nothing." >&2
  exit 0
fi
if [ -z "${AI_GATEWAY_API_KEY:-}" ]; then
  echo "changelog-ai: AI_GATEWAY_API_KEY not set; printing nothing." >&2
  exit 0
fi

model="${CHANGELOG_AI_MODEL:-anthropic/claude-sonnet-5}"

# A SERIES range — the whole minor line, which is what a section is drafted
# from now — spans hundreds of commits, and its raw diff is far past anything a
# 100,000-character window represents fairly: truncating it hands the model
# whatever git happened to emit first and silently drops the rest, so the
# section would describe the start of the range rather than the important part
# of it. Above the threshold the diff is dropped entirely and the draft leans on
# commit subjects and bodies — which in this repository are conventional-commit
# titles carrying the PR number and a real sentence, a better signal per token
# than a truncated hunk — with the subject and body caps widened to match.
# Below it a single release's diff still fits, and the diff still wins: commit
# messages describe intent, the diff is what shipped.
commit_count="$(git rev-list --no-merges --count ${range:+"$range"} 2>/dev/null || echo 0)"
if [ "${commit_count}" -gt 60 ]; then
  scope="series"
  subject_cap=600
  body_cap=1200
  echo "changelog-ai: ${commit_count} commits in '${range:-<all>}'; drafting a series section from subjects and diffstat." >&2
else
  scope="release"
  subject_cap=100
  body_cap=400
fi

# The release's real content: everything in the range except the release
# machinery's own commits (the "stella X.Y.Z" stamp and the version-sync PR).
bot_re='^(stella [0-9]|chore\(release\): sync versions)'
commits="$(git log --no-merges --format='%s' ${range:+"$range"} | grep -Ev "$bot_re" | head -n "$subject_cap" || true)"
if [ -z "$commits" ]; then
  echo "changelog-ai: no non-bot commits in range '${range:-<all>}'; printing nothing." >&2
  exit 0
fi

bodies="$(git log --no-merges --format='--- %s%n%b' ${range:+"$range"} | head -n "$body_cap" || true)"
stat="$(git diff --stat ${range:+"$range"} -- ':(exclude)Cargo.lock' | tail -n 100 || true)"

if [ "$scope" = series ]; then
  diff=""
else
  diff="$(git diff ${range:+"$range"} -- ':(exclude)Cargo.lock' ':(exclude)website/pnpm-lock.yaml' 2>/dev/null | head -c 100000 || true)"
fi

# The shared half of the instruction — voice, format, and what counts as
# user-facing — is identical either way; only the scope sentence differs.
common='Keep a Changelog format: "### Added" / "### Changed" / "### Fixed" / "### Removed" / "### Security" headings (only the ones that apply, in that order), each bullet starting with a bold lead claim, then one or two plain sentences saying what a user of the CLI would notice — concrete, everyday words, wrapped at 80 columns, ending with the PR reference like (#1234) when the commit subject carries one. Tests, CI workflows, gates and baselines, bench-harness internals, and refactors are not user-facing. Output ONLY the section body markdown - no version heading, no preamble, no code fences.'

if [ "$scope" = series ]; then
  intro="Write the CHANGELOG.md section body for one MINOR RELEASE LINE of \"stella\", a Rust coding-agent CLI — the whole series of releases listed below, summarized as one section a reader upgrading across it would want. ${common} 8-16 bullets total: group the work into themes, lead with the changes that alter what the tool does, and drop everything routine. Base every claim on the commit subjects and bodies below."
else
  intro="Write the CHANGELOG.md section body for one release of \"stella\", a Rust coding-agent CLI. ${common} 1-3 bullets total; pick the biggest user-visible changes and drop the rest. Base every claim on the diff below — commit messages describe intent, the diff is what shipped. If the release contains nothing user-facing, output exactly one line instead: _Internal: <six-to-twelve-word summary>; no user-facing changes._"
fi

# shellcheck disable=SC2016  # the format string's backticks are literal markdown
prompt="$(printf '%s\n\n## Commit subjects\n%s\n\n## Commit bodies (squash-merge PR descriptions)\n%s\n\n## Files changed\n%s\n\n## Diff (truncated)\n```diff\n%s\n```\n' \
  "$intro" "$commits" "$bodies" "$stat" "$diff")"

payload="$(jq -n --arg m "$model" --arg c "$prompt" \
  '{model:$m, messages:[{role:"user",content:$c}], temperature:0.2}')"

resp="$(curl -sS --max-time 150 \
  -H "Authorization: Bearer ${AI_GATEWAY_API_KEY}" \
  -H "Content-Type: application/json" \
  -d "$payload" \
  https://ai-gateway.vercel.sh/v1/chat/completions || true)"

entries="$(printf '%s' "$resp" | jq -r '.choices[0].message.content // empty' 2>/dev/null || true)"

# Defensive: strip a stray ``` fence wrapper, then require the result to look
# like a section body before letting it anywhere near CHANGELOG.md.
entries="$(printf '%s\n' "$entries" | sed -e '/^```/d')"
entries="$(printf '%s' "$entries" | sed -e 's/[[:space:]]*$//')"
case "$entries" in
  '### '* | '- '* | '_Internal:'*) ;;
  *)
    echo "changelog-ai: response did not look like changelog markdown; printing nothing." >&2
    exit 0
    ;;
esac

printf '%s\n' "$entries"
