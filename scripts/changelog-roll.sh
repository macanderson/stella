#!/usr/bin/env bash
#
# Roll CHANGELOG.md's `[Unreleased]` section under a new version heading.
#
# Usage: NEW_VERSION=0.7.0 [RELEASE_DATE=2026-08-07] \
#          [CHANGELOG_ENTRIES_FILE=<path>] scripts/changelog-roll.sh [<file>]
#
# Split out of scripts/sync-versions.sh so it can be tested without standing up
# a Cargo workspace (scripts/test-changelog-roll.sh, `make changelog-roll-test`).
# sync-versions.sh calls it at both of its call sites — the tagged release
# commit and the bot/version-sync PR — so the tag and main roll identical text.
#
# ## The rule: minor and major lines only
#
# Every merge to main cuts a patch release, so this roll used to fire ~130
# times per minor line. scripts/changelog-ai.sh degrades open by contract (no
# API key, no non-bot commits, or a response that does not look like changelog
# markdown all print nothing and exit 0), but the roll ran regardless —
# depositing a bare `## [0.6.x] — <date>` heading with nothing under it. That is
# how CHANGELOG.md reached 180 sections of which 77 were empty: the file was
# structurally guaranteed to fill with noise no matter how good the drafter got.
#
# So a patch release does not touch this file at all. Its detail is not lost —
# release.yml publishes GitHub Release notes for every tag, which is the surface
# built for "what changed in this exact build". CHANGELOG.md is the curated
# durable record: one section per minor line, drafted once from the whole series
# range. RELEASING.md § "What records a change" documents the split.
#
# ## The second rule: never emit a heading with nothing under it
#
# A minor release whose draft failed would otherwise reproduce the original
# defect once per line. When the drafter degrades open AND `[Unreleased]` is
# empty, this writes a pointer to the releases page instead — truthful, because
# the per-tag detail genuinely does exist there. A bare heading is never an
# acceptable output of this script.
#
# Deliberately tolerant everywhere else: a missing file or a missing
# `[Unreleased]` heading is a warning, not a failure. A changelog bookkeeping
# slip must never be the reason a release fails to ship.
#
# perl, not `sed -i "0,/re/"`: that address form is GNU-only and SILENTLY
# no-ops on BSD sed (macOS).
set -euo pipefail

version="${NEW_VERSION:?set NEW_VERSION to the bare semver, e.g. 0.7.0}"
export NEW_VERSION
changelog="${1:-CHANGELOG.md}"

# The releases page carries every tag's notes; the fallback body points there.
releases_url="https://github.com/macanderson/stella/releases"

# Checking the patch component here — rather than trusting a caller to pass the
# bump kind — keeps the rule true at both call sites with no third place to
# drift.
case "${version}" in
  *.*.0) ;;
  *)
    echo "changelog-roll: ${version} is a patch release; CHANGELOG.md records minor and major lines only."
    exit 0
    ;;
esac

if [ ! -f "${changelog}" ] || ! grep -q '^## \[Unreleased\]' "${changelog}"; then
  echo "::warning::${changelog} missing or has no [Unreleased] heading; skipping the roll."
  exit 0
fi

# Replace whatever sits under [Unreleased] with $1's contents.
inject_entries() {
  ENTRIES_FILE="$1" perl -0777 -pi -e '
    open my $fh, "<", $ENV{ENTRIES_FILE} or die "cannot open $ENV{ENTRIES_FILE}: $!";
    my $entries = do { local $/; <$fh> };
    close $fh;
    $entries =~ s/\s+\z//;
    s/^## \[Unreleased\]\n.*?(?=^## \[|\z)/"## [Unreleased]\n\n" . $entries . "\n\n"/mse;
  ' "${changelog}"
}

unreleased_body="$(awk '/^## \[Unreleased\]$/{f=1;next} /^## \[/{f=0} f' "${changelog}" | tr -d '[:space:]')"

if [ -n "${CHANGELOG_ENTRIES_FILE:-}" ] && [ -s "${CHANGELOG_ENTRIES_FILE}" ]; then
  # The CI draft is authoritative: it REPLACES whatever already sits under
  # [Unreleased], hand-written or not. Changelog authorship lives in the release
  # job, not in feature PRs, so entries stay in one voice instead of whatever a
  # contributor or coding agent happened to type.
  inject_entries "${CHANGELOG_ENTRIES_FILE}"
  echo "changelog-roll: CI-drafted entries written for ${version}."
elif [ -z "${unreleased_body}" ]; then
  fallback="$(mktemp)"
  printf '_The draft for this section could not be generated. Per-release notes for\nevery tag in this line are published on the [releases page](%s)._\n' \
    "${releases_url}" > "${fallback}"
  inject_entries "${fallback}"
  rm -f "${fallback}"
  echo "::warning::no entries drafted for ${version}; wrote a releases-page pointer rather than an empty section."
else
  echo "::warning::no entries drafted for ${version}; rolling the existing [Unreleased] body as-is."
fi

# Inserting *after* the heading (rather than renaming it) is what carries the
# section's content down into the new version's section.
RELEASE_DATE="${RELEASE_DATE:-$(date -u +%F)}" \
  perl -pi -e 's/^## \[Unreleased\]$/## [Unreleased]\n\n## [$ENV{NEW_VERSION}] — $ENV{RELEASE_DATE}/' "${changelog}"

grep -q "^## \[${version}\] " "${changelog}" \
  || echo "::warning::${changelog} roll did not take for ${version}"
