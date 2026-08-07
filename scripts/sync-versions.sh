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
#   - CHANGELOG.md, via scripts/changelog-roll.sh — on a MINOR or MAJOR
#     release only. A patch release does not touch that file; see the rationale
#     in changelog-roll.sh's header and RELEASING.md § "What records a change".
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

# CHANGELOG.md is rolled by a sibling script, not inlined here: the roll has
# rules of its own (minor/major only, never a heading with an empty body) that
# deserve a direct test, and testing them through this script would mean
# standing up a throwaway Cargo workspace for `cargo update` above.
# scripts/test-changelog-roll.sh covers it; `make changelog-roll-test` runs it.
"$(dirname "$0")/changelog-roll.sh" CHANGELOG.md
