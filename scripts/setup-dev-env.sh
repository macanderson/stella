#!/usr/bin/env bash
#
# Prepare this checkout (or worktree) for development.
#
#   ./scripts/setup-dev-env.sh              # set up + report
#   ./scripts/setup-dev-env.sh --check      # report only, no writes
#   ./scripts/setup-dev-env.sh --install    # also install what is missing
#   ./scripts/setup-dev-env.sh --prune      # reclaim caches for dead worktrees
#
# Idempotent. Safe to re-run, and meant to be run in every worktree, because the
# isolation it sets up is per-worktree.
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# Working in several worktrees of this repo at once breaks in three ways that
# all look like flaky tests, and none of which are:
#
#   1. `~/.stella` is machine-global. stella_home() (stella-store/src/home.rs)
#      resolves STELLA_HOME, else $HOME/.stella — and usage.db, catalog.db,
#      media-operations.db, sessions/ and notifications/ all live under it. Two
#      worktrees running `cargo test --workspace` contend on one set of SQLite
#      files, keyed by nothing but $HOME. The media-operation journal
#      (stella-cli/src/agent/tools.rs) is the one that bites most often.
#
#      Setting STELLA_HOME per worktree fixes it, and — checked, not assumed —
#      does NOT cost you provider auth: credentials.toml resolves through
#      $HOME/.stella directly (stella-model/src/credential.rs), not through
#      stella_home(), so keys keep working while the mutable state separates.
#
#   2. One CARGO_TARGET_DIR shared by N worktrees means cargo's build lock
#      serializes them: worktree B's test run blocks until A's finishes, which
#      reads exactly like a hang. Per-worktree target dirs cost disk instead of
#      wall-clock — hence --prune, below, which is the other half of that trade.
#
#   3. Neither of the above is discoverable from a failure. You get a flaky
#      test, not a message about shared state.
#
# The rest is the boring half: verify the tools the gate actually needs, wire
# the pre-push hook, and print the one thing that is genuinely easy to get
# wrong — where `make gate` and CI disagree.
#
# ── What it writes ───────────────────────────────────────────────────────────
#
#   <worktree>/.dev-env                   sourceable env file (the main output)
#   ~/.cache/stella-env/<slug>/{home,target}
#   git config core.hooksPath=.githooks   (shared across worktrees, like `make hooks`)
#
# `.dev-env` is a plain shell file, so it works everywhere an environment is
# needed — `. ./.dev-env` in a terminal, `source_env` from a direnv .envrc, an
# exported env in CI, or any editor/agent harness that reads a JSON settings
# file (see --agent-settings). Nothing here is tracked, so none of it can reach
# the remote or trip check-no-scratch.sh.
#
# bash 3.2 compatible (macOS ships 3.2), POSIX tools only. No associative
# arrays, no mapfile.

set -euo pipefail

SETUP_VERSION=2
CACHE_ROOT="${STELLA_ENV_CACHE:-$HOME/.cache/stella-env}"

MODE=setup       # setup | check | prune
DO_INSTALL=0
DO_HOOKS=1
AGENT_SETTINGS="${DEV_ENV_AGENT_SETTINGS:-}"

usage() {
  cat <<'EOF'
setup-dev-env.sh — prepare this worktree for development on stella.

Gives the worktree its own STELLA_HOME and CARGO_TARGET_DIR (so parallel
worktrees stop contending on ~/.stella and on cargo's build lock), verifies the
tools the gate needs, wires the pre-push hook, and writes a sourceable .dev-env
carrying all of it. Idempotent; run it in every worktree.

Usage: ./scripts/setup-dev-env.sh [options]

Options:
  --check              Report only. Writes nothing. Exits 1 if a required tool is missing.
  --install            Install missing tools via brew / cargo install / rustup.
  --prune              Delete per-worktree caches whose worktree no longer exists, then exit.
  --agent-settings F   Also write the same environment into JSON settings file F,
                       plus a format-on-edit hook. For editors and agent harnesses
                       that read a JSON settings file with an "env" map and
                       "hooks.PostToolUse" command entries. Merged into F if it
                       already exists; F is backed up first either way.
  --no-hooks           With --agent-settings, write only the env, not the edit hook.
  -h, --help           This text.

Environment:
  STELLA_ENV_CACHE          Where per-worktree caches live (default ~/.cache/stella-env).
  DEV_ENV_AGENT_SETTINGS    Default for --agent-settings, so you can set it once in
                            your shell profile instead of passing it every time.
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --check) MODE=check ;;
    --prune) MODE=prune ;;
    --install) DO_INSTALL=1 ;;
    --no-hooks) DO_HOOKS=0 ;;
    --agent-settings)
      shift
      [ $# -gt 0 ] || { echo "setup-dev-env: --agent-settings needs a file path" >&2; exit 2; }
      AGENT_SETTINGS="$1"
      ;;
    -h|--help) usage; exit 0 ;;
    *) echo "setup-dev-env: unknown option '$1' (try --help)" >&2; exit 2 ;;
  esac
  shift
done

# ── Output helpers ───────────────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  C_RESET=$'\033[0m'; C_DIM=$'\033[2m'; C_BOLD=$'\033[1m'
  C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'; C_RED=$'\033[31m'
else
  C_RESET=; C_DIM=; C_BOLD=; C_GREEN=; C_YELLOW=; C_RED=
fi

hdr()  { printf '\n%s%s%s\n' "$C_BOLD" "$1" "$C_RESET"; }
ok()   { printf '  %sok%s    %s\n' "$C_GREEN" "$C_RESET" "$1"; }
warn() { printf '  %swarn%s  %s\n' "$C_YELLOW" "$C_RESET" "$1"; }
bad()  { printf '  %smiss%s  %s\n' "$C_RED" "$C_RESET" "$1"; }
note() { printf '        %s%s%s\n' "$C_DIM" "$1" "$C_RESET"; }

# ── Locate the checkout ──────────────────────────────────────────────────────
if ! repo_root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
  echo "setup-dev-env: not inside a git checkout of stella." >&2
  exit 1
fi
repo_root="$(cd "$repo_root" && pwd -P)"

if [ ! -f "$repo_root/rust-toolchain.toml" ] || [ ! -d "$repo_root/stella-core" ]; then
  echo "setup-dev-env: $repo_root does not look like the stella workspace." >&2
  exit 1
fi

# A linked worktree has --git-dir != --git-common-dir. Worth reporting, because
# "which worktree am I in" is the question behind most cross-worktree confusion.
git_dir="$(git rev-parse --absolute-git-dir)"
git_common="$(cd "$(git rev-parse --git-common-dir)" && pwd -P)"
if [ "$git_dir" = "$git_common" ]; then
  worktree_kind="main checkout"
else
  worktree_kind="linked worktree"
fi

# Slug: stable per absolute path, readable at a glance. cksum is POSIX, so this
# needs neither shasum nor md5 — both of which differ across platforms.
path_hash="$(printf '%s' "$repo_root" | cksum | awk '{print $1}')"
slug="$(basename "$repo_root")-$path_hash"

# ── --prune ──────────────────────────────────────────────────────────────────
# Only ever removes directories carrying our own .owner marker whose recorded
# worktree is gone. A cache dir this script did not create is never touched —
# that matters on a machine with a hand-rolled target-dir scheme already in
# place, where a broad `rm` would eat someone's warm build cache.
if [ "$MODE" = prune ]; then
  hdr "Pruning $CACHE_ROOT"
  if [ ! -d "$CACHE_ROOT" ]; then
    note "nothing to prune (no cache dir yet)"
    exit 0
  fi
  reclaimed=0
  kept=0
  for dir in "$CACHE_ROOT"/*; do
    [ -d "$dir" ] || continue
    marker="$dir/.owner"
    if [ ! -f "$marker" ]; then
      warn "$(basename "$dir") — no .owner marker, leaving alone"
      continue
    fi
    owner="$(cat "$marker" 2>/dev/null || true)"
    if [ -n "$owner" ] && [ -d "$owner" ]; then
      kept=$((kept + 1))
      continue
    fi
    size="$(du -sh "$dir" 2>/dev/null | awk '{print $1}')"
    rm -rf "$dir"
    ok "removed $(basename "$dir") ($size) — worktree ${owner:-?} is gone"
    reclaimed=$((reclaimed + 1))
  done
  printf '\n'
  note "$reclaimed removed, $kept still live"
  [ -d "$CACHE_ROOT" ] && note "cache now: $(du -sh "$CACHE_ROOT" 2>/dev/null | awk '{print $1}')"
  exit 0
fi

hdr "stella development environment"
note "$worktree_kind: $repo_root"
note "branch:        $(git rev-parse --abbrev-ref HEAD)"
note "cache slug:    $slug"

# ── Toolchain ────────────────────────────────────────────────────────────────
hdr "Rust toolchain"
missing_required=0

channel="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p' \
  "$repo_root/rust-toolchain.toml" | head -1)"

if ! command -v rustup >/dev/null 2>&1; then
  bad "rustup — rust-toolchain.toml pins $channel; without rustup you get whatever cargo is on PATH"
  note "install: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
  missing_required=$((missing_required + 1))
elif rustup toolchain list 2>/dev/null | grep -q "^$channel"; then
  ok "toolchain $channel installed"
  for comp in clippy rustfmt; do
    if rustup component list --toolchain "$channel" 2>/dev/null \
       | grep -q "^$comp.* (installed)"; then
      ok "component $comp"
    else
      bad "component $comp — \`make lint\` / \`make format-check\` need it"
      if [ "$DO_INSTALL" -eq 1 ]; then
        note "installing..."
        rustup component add "$comp" --toolchain "$channel"
      else
        note "install: rustup component add $comp --toolchain $channel"
      fi
    fi
  done
else
  bad "toolchain $channel not installed"
  if [ "$DO_INSTALL" -eq 1 ]; then
    note "installing..."
    rustup toolchain install "$channel" --component clippy --component rustfmt
  else
    note "install: rustup toolchain install $channel --component clippy --component rustfmt"
    missing_required=$((missing_required + 1))
  fi
fi

# ── Tools ────────────────────────────────────────────────────────────────────
# git and cargo are deliberately absent from this table: we could not have
# reached this line without git (repo detection above uses it), and cargo is
# covered by the toolchain section. A row for either would be unreachable.
# The hard-failure count therefore comes from the toolchain section alone —
# nothing below is fatal, because `make gate` runs without all of it.
#
# Tiers, in the order they will actually cost you:
#   ci    — a REQUIRED GitHub check you cannot reproduce locally without it
#   repo  — assumed by this repo's documented workflow
#   opt   — one subsystem only; absence is fine until you touch that subsystem
#
# Format: name|tier|why|installer|package
tool_table() {
  cat <<'EOF'
cargo-deny|ci|CI job "cargo deny + cargo audit" (name kept for branch protection; only cargo-deny actually runs, see #919) is a required check; make deny|cargo|cargo-deny
gh|repo|PR + release flow (scripts/release.sh hard-requires it)|brew|gh
rg|repo|repo convention: rg over grep, and it is gitignore-aware|brew|ripgrep
fd|repo|repo convention: fd over find|brew|fd
jq|repo|lets --agent-settings merge into an existing file instead of replacing it|brew|jq
cargo-watch|opt|make watch / watch-core / watch-lint (hard-errors without it)|cargo|cargo-watch
docker|opt|make serve-image + scripts/smoke-serve-image.sh|manual|
node|opt|the website/ docs build|brew|node
pnpm|opt|website/ uses pnpm exclusively (never npm)|brew|pnpm
uv|opt|make bench-test (bench/harbor_adapter, bench/terminal_bench_analysis)|brew|uv
gsed|opt|scripts/release.sh + release.yml use GNU-only sed ranges; BSD sed no-ops them|brew|gnu-sed
zig|opt|scripts/release.sh cross-builds via cargo-zigbuild|brew|zig
EOF
}

hdr "Tools"
brew_wanted=""
cargo_wanted=""

while IFS='|' read -r name tier why installer pkg; do
  [ -n "$name" ] || continue
  if command -v "$name" >/dev/null 2>&1; then
    ok "$name"
    continue
  fi
  case "$tier" in
    ci) bad "$name — $why" ;;
    *)  warn "$name — $why" ;;
  esac
  case "$installer" in
    brew)  note "install: brew install $pkg";        brew_wanted="$brew_wanted $pkg" ;;
    cargo) note "install: cargo install --locked $pkg"; cargo_wanted="$cargo_wanted $pkg" ;;
    *)     note "install: (manual)" ;;
  esac
done <<EOF
$(tool_table)
EOF

if [ "$DO_INSTALL" -eq 1 ]; then
  if [ -n "$brew_wanted" ]; then
    if command -v brew >/dev/null 2>&1; then
      hdr "Installing via brew"
      # Unquoted on purpose: $brew_wanted is a space-separated package list.
      # shellcheck disable=SC2086
      brew install $brew_wanted
    else
      warn "brew not found; skipping:$brew_wanted"
    fi
  fi
  if [ -n "$cargo_wanted" ]; then
    hdr "Installing via cargo"
    for pkg in $cargo_wanted; do
      cargo install --locked "$pkg"
    done
  fi
fi

if [ "$MODE" = check ]; then
  hdr "Check only — nothing written"
  if [ "$missing_required" -gt 0 ]; then
    printf '  %s%d required tool(s) missing%s\n' "$C_RED" "$missing_required" "$C_RESET"
    exit 1
  fi
  ok "all required tools present"
  exit 0
fi

# ── Per-worktree isolation ───────────────────────────────────────────────────
hdr "Per-worktree isolation"
wt_cache="$CACHE_ROOT/$slug"
stella_home="$wt_cache/home"
target_dir="$wt_cache/target"

mkdir -p "$stella_home" "$target_dir"
printf '%s\n' "$repo_root" > "$wt_cache/.owner"

ok "STELLA_HOME       $stella_home"
note "isolates usage.db / catalog.db / media-operations.db / sessions/ from other worktrees"
note "provider keys still come from \$HOME/.stella/credentials.toml — unaffected"
ok "CARGO_TARGET_DIR  $target_dir"
note "no build-lock contention with sibling worktrees; reclaim with --prune"

# ── Git hooks ────────────────────────────────────────────────────────────────
hdr "Git hooks"
current_hooks="$(git config --get core.hooksPath 2>/dev/null || true)"
if [ "$current_hooks" = ".githooks" ]; then
  ok "core.hooksPath = .githooks"
else
  git config core.hooksPath .githooks
  chmod +x "$repo_root"/.githooks/* 2>/dev/null || true
  ok "core.hooksPath set to .githooks (pre-push runs \`make gate\`)"
fi
note "bypass for a WIP push: SKIP_GATE=1 git push"

# ── The environment file ─────────────────────────────────────────────────────
hdr "Environment"
env_file="$repo_root/.dev-env"
cat > "$env_file" <<EOF
# Generated by scripts/setup-dev-env.sh (v$SETUP_VERSION) — re-run it rather
# than editing this file by hand.
#
# Per-worktree isolation for $repo_root:
#   STELLA_HOME       keeps this worktree's usage.db / catalog.db /
#                     media-operations.db / sessions/ out of the machine-global
#                     ~/.stella that every other worktree also writes to.
#   CARGO_TARGET_DIR  keeps cargo's build lock from serializing sibling worktrees.
#
# Use it:
#   . ./.dev-env                       in a shell
#   source_env .dev-env                from a direnv .envrc
#
export STELLA_HOME="$stella_home"
export CARGO_TARGET_DIR="$target_dir"
export RUST_BACKTRACE="\${RUST_BACKTRACE:-1}"
EOF
ok "wrote $env_file"
note "source it:  . ./.dev-env"

# ── Optional: the same environment as an editor/agent settings file ──────────
# No default path, on purpose. Which settings file applies — if any — is a
# property of the tool you happen to use, not of this repo, so it has to be
# supplied via --agent-settings or DEV_ENV_AGENT_SETTINGS.
if [ -n "$AGENT_SETTINGS" ]; then
  hdr "Agent settings"
  case "$AGENT_SETTINGS" in
    /*) settings="$AGENT_SETTINGS" ;;
    *)  settings="$repo_root/$AGENT_SETTINGS" ;;
  esac
  mkdir -p "$(dirname "$settings")"

  json_escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

  hook_block=""
  if [ "$DO_HOOKS" -eq 1 ] && [ -x "$repo_root/scripts/fmt-file.sh" ]; then
    hook_block=",
  \"hooks\": {
    \"PostToolUse\": [
      {
        \"matcher\": \"Edit|Write|MultiEdit\",
        \"hooks\": [
          {
            \"type\": \"command\",
            \"command\": \"$(json_escape "$repo_root/scripts/fmt-file.sh")\",
            \"timeout\": 30
          }
        ]
      }
    ]
  }"
  fi

  generated="$(cat <<EOF
{
  "env": {
    "STELLA_HOME": "$(json_escape "$stella_home")",
    "CARGO_TARGET_DIR": "$(json_escape "$target_dir")",
    "RUST_BACKTRACE": "1"
  }$hook_block
}
EOF
)"

  if [ ! -f "$settings" ]; then
    printf '%s\n' "$generated" > "$settings"
    ok "wrote $settings"
  else
    # Never silently clobber a settings file somebody hand-wrote.
    backup="$settings.bak.$$"
    cp "$settings" "$backup"
    if command -v jq >/dev/null 2>&1 \
       && printf '%s' "$generated" | jq --slurpfile cur "$settings" '$cur[0] * .' \
            > "$settings.tmp" 2>/dev/null; then
      # `*` merges objects recursively; our keys win. Arrays are replaced, not
      # appended — so a pre-existing PostToolUse array is superseded, which is
      # why the backup above is unconditional.
      mv "$settings.tmp" "$settings"
      ok "merged into existing $settings"
    else
      rm -f "$settings.tmp"
      printf '%s\n' "$generated" > "$settings"
      warn "replaced $settings (not valid JSON, or jq unavailable to merge)"
    fi
    note "backup: $backup"
  fi

  if [ "$DO_HOOKS" -eq 1 ]; then
    if [ -x "$repo_root/scripts/fmt-file.sh" ]; then
      ok "edit hook: rustfmt + file-size ratchet on every .rs edit"
    else
      warn "scripts/fmt-file.sh missing or not executable — hook not wired"
    fi
  else
    # --no-hooks has to REMOVE our hook, not merely decline to add it. Merging
    # leaves whatever the file already had, so on a re-run with --no-hooks the
    # hook a previous run wrote would silently survive the flag that asked for
    # its absence. Only entries pointing at our own script are dropped, so an
    # unrelated PostToolUse hook in the same file is left intact.
    if command -v jq >/dev/null 2>&1; then
      if jq --arg cmd "$repo_root/scripts/fmt-file.sh" '
            if (.hooks? | objects | has("PostToolUse")) then
              .hooks.PostToolUse |= map(
                select(((.hooks // []) | map(.command) | index($cmd)) | not))
              | if (.hooks.PostToolUse | length) == 0 then del(.hooks.PostToolUse) else . end
              | if (.hooks | length) == 0 then del(.hooks) else . end
            else . end
          ' "$settings" > "$settings.tmp" 2>/dev/null; then
        mv "$settings.tmp" "$settings"
        ok "edit hook removed (--no-hooks)"
      else
        rm -f "$settings.tmp"
        warn "could not strip the edit hook from $settings"
      fi
    else
      warn "--no-hooks needs jq to remove an already-written hook; left as-is"
    fi
  fi
else
  note "no --agent-settings given; skipping editor/agent wiring"
fi

# ── The part that is easy to get wrong ───────────────────────────────────────
hdr "make gate vs CI"
cat <<'EOF'
  make gate  runs: no-scratch, action-pins, invariants, doc-links, file-size,
             doc-warnings (RUSTDOCFLAGS=-D warnings), fmt --check, clippy, test.
             It stops at the FIRST failure; CI reports all of them in one run.

  make check is the fast subset — it skips tests AND rustdoc.
             Passing `make check` does not mean CI will pass.

  doc-links  cite documents by frontmatter id (`doc:context-reuse §4`), not by
             path, so a move cannot break a citation. If one does break,
             `make doc-links-fix` repoints it. `make doc-report` lists what
             has gone stale.

  Green locally, red in CI, two ways:
    - release smoke   CI also runs `cargo build --workspace --release`
                      (thin LTO). Not in any make target.
    - supply chain    `cargo deny` is a SEPARATE required check (the CI job
                      keeps the name "cargo deny + cargo audit" for branch
                      protection; cargo-audit itself was dropped in #919).
                      `make gate` does not run it; `make supply-chain` does.
EOF

hdr "Next"
cat <<EOF
  . ./.dev-env                       load the isolated environment
  make gate                          full local gate (what pre-push runs)
  make supply-chain                  the other required CI check
  ./scripts/setup-dev-env.sh --prune reclaim caches for deleted worktrees
EOF

if [ -d "$CACHE_ROOT" ]; then
  note "cache total: $(du -sh "$CACHE_ROOT" 2>/dev/null | awk '{print $1}') in $CACHE_ROOT"
fi

if [ "$missing_required" -gt 0 ]; then
  printf '\n  %s%d required tool(s) still missing — see above%s\n' \
    "$C_RED" "$missing_required" "$C_RESET"
  exit 1
fi
