#!/usr/bin/env bash
#
# Stamp NEW_VERSION everywhere the sources carry a version. See #786.
#
# Called from auto-tag.yml in two places that must stay byte-identical in
# effect — which is why this is a script and not two inlined copies:
#
#   1. Building the release commit the tag points at, so a from-tag source
#      build (`cargo install --git … --tag vX.Y.Z`, Homebrew, a distro
#      packager) reports its own version without any build-time stamping.
#   2. Building the `bot/version-sync` PR that writes the same version back
#      to main afterwards.
#
# Edits, in order:
#   - `[workspace.package].version` in Cargo.toml (every crate inherits it)
#   - the workspace-member entries in Cargo.lock (`cargo update --workspace`;
#     registry and git pins are untouched)
#   - packaging/homebrew/stella.rb (build-from-source formula tag + version)
#   - CHANGELOG.md: the `[Unreleased]` section rolls under the new version.
#     Deliberately tolerant — a missing file or heading is a warning, not a
#     failure, because a changelog bookkeeping slip must never stop a release.
#
# perl, not `sed -i "0,/re/"`: that address form is GNU-only and SILENTLY
# no-ops on BSD sed (macOS).
set -euo pipefail

version="${NEW_VERSION:?set NEW_VERSION to the bare semver, e.g. 0.5.68}"
export NEW_VERSION

# The one `^version = ` line is [workspace.package].version.
perl -pi -e 's/^version = "[^"]*"/version = "$ENV{NEW_VERSION}"/' Cargo.toml
grep -m1 '^version = ' Cargo.toml | grep -q "\"${version}\"" \
  || { echo "::error::workspace version bump did not take" >&2; exit 1; }

cargo update --workspace

perl -pi -e 's/tag: "v[^"]*"/tag: "v$ENV{NEW_VERSION}"/; s/^  version "[^"]*"/  version "$ENV{NEW_VERSION}"/' \
  packaging/homebrew/stella.rb

if [ -f CHANGELOG.md ] && grep -q '^## \[Unreleased\]' CHANGELOG.md; then
  # If the release arrived with an empty [Unreleased] and auto-tag.yml drafted
  # entries from the release diff (scripts/changelog-ai.sh), inject them under
  # the heading before rolling — so the roll below carries them down instead
  # of minting yet another blank version heading. Hand-written entries always
  # win: a non-empty section is left exactly as the contributor wrote it.
  # Injection lives here, not in the workflow, because this script runs at two
  # call sites (the tagged release commit and the bot/version-sync PR) and the
  # tag and main must roll identical text — one script, two call sites, no
  # drift (#786).
  if [ -n "${CHANGELOG_ENTRIES_FILE:-}" ] && [ -s "${CHANGELOG_ENTRIES_FILE}" ]; then
    if awk '/^## \[Unreleased\]/{inside=1; next}
            inside && /^## \[/{exit}
            inside && NF{found=1; exit}
            END{exit found ? 1 : 0}' CHANGELOG.md; then
      ENTRIES_FILE="${CHANGELOG_ENTRIES_FILE}" perl -pi -e '
        if (/^## \[Unreleased\]$/) {
          open my $fh, "<", $ENV{ENTRIES_FILE} or next;
          my $e = do { local $/; <$fh> };
          $e =~ s/\s+\z//;
          $_ .= "\n" . $e . "\n";
        }' CHANGELOG.md
      echo "sync-versions: injected drafted changelog entries for ${version}."
    else
      echo "sync-versions: [Unreleased] already has entries; drafted entries not injected."
    fi
  fi
  # Inserting *after* the heading (rather than renaming it) is what carries
  # the existing bullets down into the new version's section.
  RELEASE_DATE="${RELEASE_DATE:-$(date -u +%F)}" \
    perl -pi -e 's/^## \[Unreleased\]$/## [Unreleased]\n\n## [$ENV{NEW_VERSION}] — $ENV{RELEASE_DATE}/' CHANGELOG.md
  grep -q "^## \[${version}\] " CHANGELOG.md \
    || echo "::warning::CHANGELOG.md roll did not take for ${version}"
else
  echo "::warning::CHANGELOG.md missing or has no [Unreleased] heading; skipping the roll."
fi
