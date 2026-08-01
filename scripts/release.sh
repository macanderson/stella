#!/usr/bin/env bash
#
# release.sh — cut a complete stella release from your local machine.
#
# Builds all four target binaries locally, publishes a GitHub Release with the
# tarballs + SHA256SUMS, and pushes the matching Homebrew formula to the tap —
# no CI required (handy while org Actions is unavailable). Everything the org
# release workflow does, done from your Mac.
#
#   Usage:  scripts/release.sh [patch|minor|major]     (default: patch)
#
#     patch   0.1.15 -> 0.1.16   (default)
#     minor   0.1.15 -> 0.2.0
#     major   0.1.15 -> 1.0.0
#
# Requirements (checked up front; zig/cargo-zigbuild/targets are auto-installed):
#   - macOS + Homebrew + Rust (rustup) + gh (run `gh auth login` first)
#   - write access to the release repo and the Homebrew tap
#
# Safety: refuses to run unless your checkout is clean and exactly matches
# origin/main, and it never leaves your working tree modified (the version
# stamp is reverted on exit; the tagged release commit is cut in a throwaway
# worktree).
#
# ⚠ THIS IS THE DEGRADED RELEASE PATH — see the ALLOW_UNATTESTED gate below.
# A release cut here is NOT byte-equivalent to one cut by release.yml and
# carries NO build-provenance attestation. Use it only when Actions cannot run.
#
set -euo pipefail

# ── Config ──────────────────────────────────────────────────────────────────
REPO="macanderson/stella"
TAP_REPO="macanderson/homebrew-tap"   # repo the formula is pushed to (git)
TAP="macanderson/tap"                 # brew tap name → maps to repo homebrew-tap
BIN="stella"
CRATE="stella-cli"
MAC_TARGETS=(aarch64-apple-darwin x86_64-apple-darwin)
LINUX_TARGETS=(aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu)
GLIBC="2.17"   # build Linux against an old glibc so the binaries run broadly
TMPL=".github/homebrew/stella.rb.tmpl"

BUMP="${1:-patch}"

# ── Output helpers ──────────────────────────────────────────────────────────
bold() { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '\033[36m▸ %s\033[0m\n' "$*"; }
ok()   { printf '\033[32m✔ %s\033[0m\n' "$*"; }
die()  { printf '\033[31mERROR: %s\033[0m\n' "$*" >&2; exit 1; }

case "$BUMP" in patch|minor|major) ;; *) die "bump must be patch|minor|major (got: $BUMP)";; esac

# ── Refuse to silently ship an unattested release ───────────────────────────
# release.yml attests every tarball AND the SHA256SUMS file with
# actions/attest-build-provenance: a Sigstore bundle bound to that workflow at
# that commit, which nobody holding the release can reissue. This script cannot
# produce one — attestation needs the OIDC token only a GitHub Actions job can
# mint — so everything it publishes is checksum-only.
#
# That degradation is invisible at the other end. install.sh deliberately
# distinguishes "no verifier available" from "verifier said no", and a release
# with no attestation at all lands in the first bucket: `gh attestation verify`
# reports "no attestation", install.sh logs one info line and installs anyway.
# So cutting a release from here silently downgrades the supply-chain guarantee
# for every user of that version, and the only one who can notice is the person
# running this script.
#
# The artifacts also differ from CI's: the Linux targets below are zig
# cross-compiles against the old glibc pinned in GLIBC above, where release.yml
# builds them natively. Same tag, different bytes, depending on who cut it.
#
# None of that makes this path wrong — it exists because the org's Actions has
# been billing-locked before and a release still had to ship. It makes it a
# decision, so require it to be made out loud.
if [ "${ALLOW_UNATTESTED:-}" != "1" ]; then
  die "this local release path publishes NO build-provenance attestation.

     Every user who installs the resulting version gets checksum-only
     verification, and install.sh will not warn them loudly — it treats a
     missing attestation as 'no verifier available', not as a failure.

     Prefer the attested path: push the tag and let .github/workflows/release.yml
     build, attest and publish it (see RELEASING.md).

     If Actions genuinely cannot run and you accept shipping unattested
     artifacts, re-run with:

         ALLOW_UNATTESTED=1 scripts/release.sh ${BUMP}

     and say so in the release notes so users know why STELLA_REQUIRE_PROVENANCE=1
     will reject this version."
fi
info "ALLOW_UNATTESTED=1 — publishing checksum-only artifacts with no provenance."

# ── Locate repo root (script lives in scripts/) ─────────────────────────────
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
[ -f "$TMPL" ] || die "run from the stella repo — $TMPL not found"

# ── Preflight: tooling ──────────────────────────────────────────────────────
info "checking tooling"
command -v cargo >/dev/null || die "cargo/rustup not found — install Rust: https://rustup.rs"
command -v gh    >/dev/null || die "gh not found — brew install gh"
command -v brew  >/dev/null || die "Homebrew not found — https://brew.sh"
gh auth status  >/dev/null 2>&1 || die "gh not authenticated — run: gh auth login"
command -v zig            >/dev/null || { info "installing zig";           brew install zig >/dev/null; }
command -v cargo-zigbuild >/dev/null || { info "installing cargo-zigbuild"; cargo install cargo-zigbuild --locked >/dev/null; }
for t in "${MAC_TARGETS[@]}" "${LINUX_TARGETS[@]}"; do
  rustup target list --installed 2>/dev/null | grep -qx "$t" || { info "adding target $t"; rustup target add "$t" >/dev/null; }
done
ok "tooling ready"

# ── Preflight: release exactly what's on origin/main, from a clean tree ──────
# Normally the checkout must be clean and identical to origin/main. Escape
# hatches for releasing from a prepared branch (e.g. release infra that isn't
# on main yet, provably not touching crate code):
#   ALLOW_NONMAIN=1     skip the "HEAD == origin/main" check (clean tree still required)
#   RELEASE_TARGET=ref  commit-ish the stamped release commit goes on top of
#                       (default: HEAD) — the tag points at the stamp, whose
#                       parent is this ref
info "checking git state"
[ -z "$(git status --porcelain)" ] || die "working tree is dirty — commit or stash first"
git fetch origin main --tags --quiet
head_sha="$(git rev-parse HEAD)"
main_sha="$(git rev-parse origin/main)"
if [ "$head_sha" != "$main_sha" ]; then
  [ "${ALLOW_NONMAIN:-}" = "1" ] || die "HEAD is not origin/main ($(git rev-parse --short HEAD) vs $(git rev-parse --short origin/main)) — run: git checkout main && git pull  (or set ALLOW_NONMAIN=1)"
  info "ALLOW_NONMAIN=1 — releasing from $(git rev-parse --short HEAD) (not origin/main)"
fi
target_sha="$(git rev-parse "${RELEASE_TARGET:-HEAD}")"
ok "checkout clean; releasing on top of $(git rev-parse --short "$target_sha")"

# ── Compute next version from the newest v-tag ──────────────────────────────
last="$(git tag -l 'v*' --sort=-v:refname | head -n1 || true)"
base="${last#v}"; base="${base:-0.0.0}"
IFS=. read -r MAJ MIN PAT <<< "$base"; MAJ=${MAJ:-0}; MIN=${MIN:-0}; PAT=${PAT:-0}
case "$BUMP" in
  major) MAJ=$((MAJ+1)); MIN=0; PAT=0 ;;
  minor) MIN=$((MIN+1)); PAT=0 ;;
  patch) PAT=$((PAT+1)) ;;
esac
VERSION="${MAJ}.${MIN}.${PAT}"; TAG="v${VERSION}"
git rev-parse -q --verify "refs/tags/${TAG}" >/dev/null && die "tag ${TAG} already exists"
gh release view "$TAG" --repo "$REPO" >/dev/null 2>&1 && die "release ${TAG} already exists on ${REPO}"
bold ""
bold "Releasing ${TAG}  (${BUMP} bump from ${last:-<none>})"
bold ""

# ── Create the release commit the tag will point at (#786 / #822) ───────────
# Mirrors auto-tag.yml: the tag must point at a tree that carries its own
# version, or every from-tag SOURCE build — install.sh's cargo fallback
# (`cargo install --git … --tag`), packaging/homebrew/stella.rb, a distro
# packager — produces a binary whose --version reports the PREVIOUS release
# (only the prebuilt tarballs below get the stamp). The stamp is one commit on
# top of target_sha, cut in a throwaway worktree so this checkout is never
# touched, and reachable only through the tag — exactly like auto-tag's, it is
# never pushed to a branch. Created before the long builds so a stamp failure
# costs seconds, not an hour of LTO; tagged + pushed only after they succeed.
info "creating the stamped release commit (scripts/sync-versions.sh)"
relwt="$(mktemp -d)/release-commit"
cleanup_relwt() { git worktree remove --force "$relwt" >/dev/null 2>&1 || true; }
trap 'cleanup_relwt' EXIT
git worktree add --quiet --detach "$relwt" "$target_sha"
(
  cd "$relwt"
  NEW_VERSION="$VERSION" ./scripts/sync-versions.sh
  git add Cargo.toml Cargo.lock packaging/homebrew/stella.rb
  # An `if`, not `[ -f … ] && git add …`: under `set -e` a false test in a
  # bare `&&` chain aborts the whole release (same hazard auto-tag.yml notes).
  if [ -f CHANGELOG.md ]; then git add CHANGELOG.md; fi
  git commit --quiet --no-verify -m "${BIN} ${VERSION}"
)
# The commit lives in the shared object store, so it survives the worktree.
release_sha="$(git -C "$relwt" rev-parse HEAD)"
cleanup_relwt
trap - EXIT
ok "release commit $(git rev-parse --short "$release_sha") stamps ${VERSION} on top of $(git rev-parse --short "$target_sha")"

# ── Stamp the workspace version, guaranteed reverted on exit ────────────────
# The one `^version = ` line is [workspace.package].version. (The CI workflow's
# `sed "0,/re/"` is GNU-only and silently no-ops on macOS — perl is portable.)
cp Cargo.toml .Cargo.toml.relbak
cp Cargo.lock .Cargo.lock.relbak   # cargo rewrites workspace-member versions in the lock during the stamped build
restore_manifest() {
  if [ -f .Cargo.toml.relbak ]; then mv .Cargo.toml.relbak Cargo.toml; fi
  if [ -f .Cargo.lock.relbak ]; then mv .Cargo.lock.relbak Cargo.lock; fi
}
trap 'restore_manifest' EXIT
perl -pi -e "s/^version = \"[^\"]*\"/version = \"${VERSION}\"/" Cargo.toml
grep -m1 '^version = ' Cargo.toml | grep -q "\"${VERSION}\"" || die "version stamp failed"
ok "stamped workspace version ${VERSION}"

# ── Build + package every target ────────────────────────────────────────────
# CARGO_TARGET_DIR-aware: a sourced .dev-env (scripts/setup-dev-env.sh) moves
# cargo's output out of ./target, and packaging must copy from wherever the
# build above actually wrote — a hardcoded ./target would miss it, or worse,
# pick up a stale binary from an earlier non-isolated build.
TARGET_ROOT="${CARGO_TARGET_DIR:-target}"
DIST="$(mktemp -d)/dist"; mkdir -p "$DIST"
package() {  # <target-triple>
  local tgt="$1" stem="${BIN}-${VERSION}-$1"
  mkdir -p "$DIST/$stem"
  cp "${TARGET_ROOT}/${tgt}/release/${BIN}" "$DIST/$stem/${BIN}"
  # AGPL §4/§5 require the license text to travel with every distributed copy.
  cp LICENSE NOTICE LICENSING.md README.md "$DIST/$stem/"
  tar -C "$DIST" -czf "$DIST/${stem}.tar.gz" "$stem"
  rm -rf "$DIST/${stem:?}"
}
for t in "${MAC_TARGETS[@]}"; do
  info "building $t (native)"
  cargo build --release --target "$t" --package "$CRATE" --bin "$BIN"
  package "$t"
done
for t in "${LINUX_TARGETS[@]}"; do
  info "building $t (zig cross-compile, glibc ${GLIBC})"
  cargo zigbuild --release --target "${t}.${GLIBC}" --package "$CRATE" --bin "$BIN"
  package "$t"
done
restore_manifest; trap - EXIT   # manifest back to pristine now that builds are done
ok "built + packaged 4 targets"

# ── Checksums + version sanity check on the native binary ───────────────────
( cd "$DIST" && shasum -a 256 "${BIN}"-"${VERSION}"-*.tar.gz > SHA256SUMS )
native="${TARGET_ROOT}/$(rustc -vV | sed -n 's/host: //p')/release/${BIN}"
if [ -x "$native" ]; then
  "$native" --version 2>/dev/null | grep -q "${VERSION}" || die "built binary reports the wrong version (expected ${VERSION}) — aborting before publish"
  ok "binary reports ${BIN} ${VERSION}"
fi
sha_of() { awk -v f="${BIN}-${VERSION}-$1.tar.gz" '$2==f{print $1}' "$DIST/SHA256SUMS"; }

# ── Release notes: commits since the previous tag ───────────────────────────
notes="$(mktemp)"
{
  printf 'Release %s.\n\n## Changes since %s\n\n' "$VERSION" "${last:-the beginning}"
  git log --no-merges --pretty='- %s' ${last:+${last}..HEAD} | head -n 100
  [ -n "$last" ] && printf '\n**Full changelog**: https://github.com/%s/compare/%s...%s\n' "$REPO" "$last" "$TAG"
} > "$notes"

# ── Tag the stamped release commit, then publish the GitHub Release ─────────
# The tag is pushed BEFORE `gh release create`, which then attaches to it via
# `--verify-tag`. The old `--target "$target_sha"` form minted the tag at a
# tree still carrying the previous version stamp (#822) — the stamped commit
# created above is what the tag must point at.
#
# `--no-verify` on the push: the pre-push hook runs the full local gate, but
# everything reachable from this tag is either already on origin (target_sha
# arrived through that same gate) or the mechanical stamp commit — the same
# trust model as auto-tag.yml, which tags CI-validated commits without
# re-gating. Note a tag pushed with user credentials (unlike auto-tag's
# GITHUB_TOKEN push) does trigger release.yml's `on: push: tags` — harmless
# here, since this path exists for when Actions cannot run.
info "pushing tag ${TAG} (stamped release commit)"
git tag -a "$TAG" "$release_sha" -m "${BIN} ${VERSION}"
git push --no-verify origin "refs/tags/${TAG}" \
  || { git tag -d "$TAG" >/dev/null 2>&1; die "tag push failed — nothing published (local tag removed)"; }

# All assets in ONE call → immutable-safe.
info "creating GitHub Release ${TAG}"
gh release create "$TAG" --repo "$REPO" --verify-tag \
  --title "$TAG" --notes-file "$notes" \
  "$DIST"/"${BIN}"-"${VERSION}"-*.tar.gz "$DIST/SHA256SUMS"
ok "release: https://github.com/${REPO}/releases/tag/${TAG}"

# ── Render + push the Homebrew formula ──────────────────────────────────────
info "rendering + pushing Homebrew formula"
rendered="$(mktemp)"
sed \
  -e "s/@VERSION@/${VERSION}/g" \
  -e "s/@SHA_AARCH64_DARWIN@/$(sha_of aarch64-apple-darwin)/g" \
  -e "s/@SHA_X86_64_DARWIN@/$(sha_of x86_64-apple-darwin)/g" \
  -e "s/@SHA_AARCH64_LINUX@/$(sha_of aarch64-unknown-linux-gnu)/g" \
  -e "s/@SHA_X86_64_LINUX@/$(sha_of x86_64-unknown-linux-gnu)/g" \
  "$TMPL" > "$rendered"
# Match only real placeholder tokens: the template's own comment contains the
# literal "@SHA_*@", which a bare '@SHA' grep false-positives on.
grep -qE '@(VERSION|SHA_[A-Z0-9_]+)@' "$rendered" && die "formula still has unrendered placeholders"

tap="$(mktemp -d)/tap"
gh repo clone "$TAP_REPO" "$tap" -- --depth 1 --quiet
mkdir -p "$tap/Formula"; cp "$rendered" "$tap/Formula/${BIN}.rb"
git -C "$tap" add "Formula/${BIN}.rb"
if git -C "$tap" diff --cached --quiet; then
  ok "tap already current for ${VERSION}"
else
  git -C "$tap" commit --quiet -m "${BIN} ${VERSION}"
  git -C "$tap" push --quiet origin HEAD
  ok "formula pushed to ${TAP_REPO}"
fi

# ── Verify via Homebrew (fetch = download + checksum, no install) ───────────
info "verifying via Homebrew"
brew tap "$TAP" >/dev/null 2>&1 || true
brew update-reset "$(brew --repo "$TAP")" >/dev/null 2>&1 || true
if brew fetch "${TAP}/${BIN}" >/dev/null 2>&1; then
  ok "brew fetch + checksum verified for ${VERSION}"
else
  info "brew couldn't verify yet (release assets may still be propagating) — retry: brew fetch ${TAP}/${BIN}"
fi

bold ""
ok "Released ${TAG}"
printf '   install:  brew install %s/%s\n' "$TAP" "$BIN"
printf '   upgrade:  brew upgrade %s\n' "$BIN"
printf '   note:     main still carries the previous version stamp. auto-tag.yml\n'
printf '             normally writes it back via the bot/version-sync PR; while\n'
printf '             Actions is down, sync manually when convenient:\n'
printf '               git checkout -b version-sync && NEW_VERSION=%s scripts/sync-versions.sh\n' "$VERSION"
printf '               then commit with "[skip release]" in the title and open a PR.\n'
