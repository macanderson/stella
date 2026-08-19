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
# Then it PROVES the stamp it just wrote resolves, with the same oracle ci.yml
# and `make gate` use, and refuses to hand back a tree that would red `main`
# (#3336). Both call sites need that guarantee for a different reason: the tag
# is what `install.sh --locked` and Homebrew build from, and the write-back PR
# is merged by auto-tag.yml itself with an admin bypass, so no required check
# stands between it and `main`.
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

# Prove the lock this script just wrote resolves against the manifests it just
# rewrote, before either call site commits it.
#
# `cargo update --workspace` above does pick up a workspace member that did not
# exist in the previous lock — scripts/test-release-lockfile.sh drives this
# script over a fixture that adds one and asserts the result passes `cargo
# metadata --locked`. So this is not a second opinion about a suspected bug in
# that line. It is the release path declining to author a version stamp it has
# not checked: the failure it guards against is invisible in the produced diff,
# only ever surfaced by a `--locked` build, and by then it is on a tag or on
# `main`, where it fails everyone (#3336).
#
# The sibling guard rather than a bare `cargo metadata --locked`: one oracle,
# with the same wording ci.yml prints, instead of a second copy that can drift
# from the check the sync is trying to satisfy. `--manifest-dir "$PWD"` because
# the tree being stamped is the caller's working directory, which is not
# necessarily the checkout this script was invoked from.
if ! "$(dirname "$0")/check-lockfile-sync.sh" --manifest-dir "$PWD"; then
  echo "::error::Cargo.lock does not resolve after stamping ${version} — refusing to author a version-sync that would fail every --locked build." >&2
  exit 1
fi
