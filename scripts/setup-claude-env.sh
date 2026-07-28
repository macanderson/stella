#!/usr/bin/env bash
#
# Prepare this checkout (or worktree) for agent-driven work on stella.
#
#   ./scripts/setup-claude-env.sh              # set up + report
#   ./scripts/setup-claude-env.sh --check      # report only, no writes
#   ./scripts/setup-claude-env.sh --install    # also install what is missing
#   ./scripts/setup-claude-env.sh --prune      # reclaim caches for dead worktrees
#
# Idempotent. Safe to re-run, and safe to run in every worktree — it is in fact
# meant to be run in every worktree, because the isolation it sets up is
# per-worktree.
#
# ── Why this exists ──────────────────────────────────────────────────────────
#
# Running several agents against several worktrees of this repo at once breaks
# in three ways that all look like flaky tests, and none of which are:
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
#   <worktree>/.claude/settings.json   env + the per-edit rustfmt hook
#   <worktree>/.claude/.stella-env-stamp
#   ~/.cache/stella-env/<slug>/{home,target}
#   git config core.hooksPath=.githooks   (shared across worktrees, like `make hooks`)
#
# .claude/ is gitignored in this repo, so nothing here can reach the remote —
# and nothing here is tracked, so it cannot trip check-no-scratch.sh.
#
# bash 3.2 compatible (macOS ships 3.2), POSIX tools only. No associative
# arrays, no mapfile.

set -euo pipefail

SETUP_VERSION=1
CACHE_ROOT="${STELLA_ENV_CACHE:-$HOME/.cache/stella-env}"

MODE=setup       # setup | check | prune
DO_INSTALL=0
DO_HOOKS=1

usage() {
  cat <<'EOF'
setup-claude-env.sh — prepare this worktree for agent-driven work on stella.

Gives the worktree its own STELLA_HOME and CARGO_TARGET_DIR (so parallel
worktrees stop contending on ~/.stella and on cargo's build lock), verifies the
tools the gate needs, wires the pre-push hook, and writes a Claude Code
settings.json that carries all of it. Idempotent; run it in every worktree.

Usage: ./scripts/setup-claude-env.sh [options]

Options:
  --check        Report only. Writes nothing. Exits 1 if a required tool is missing.
  --install      Install missing tools via brew / cargo install / rustup.
  --prune        Delete per-worktree caches whose worktree no longer exists, then exit.
  --no-hooks     Skip the per-edit rustfmt hook in the generated settings.json.
  -h, --help     This text.

Environment:
  STELLA_ENV_CACHE   Where per-worktree caches live (default ~/.cache/stella-env).
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --check) MODE=check ;;
    --prune) MODE=prune ;;
    --install) DO_INSTALL=1 ;;
    --no-hooks) DO_HOOKS=0 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "setup-claude-env: unknown option '$1' (try --help)" >&2; exit 2 ;;
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
  echo "setup-claude-env: not inside a git checkout of stella." >&2
  exit 1
fi
repo_root="$(cd "$repo_root" && pwd -P)"

if [ ! -f "$repo_root/rust-toolchain.toml" ] || [ ! -d "$repo_root/stella-core" ]; then
  echo "setup-claude-env: $repo_root does not look like the stella workspace." >&2
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

hdr "stella agent environment"
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
#   agent — assumed by the repo's agent workflow / this machine's conventions
#   opt   — one subsystem only; absence is fine until you touch that subsystem
#
# Format: name|tier|why|installer|package
tool_table() {
  cat <<'EOF'
cargo-deny|ci|CI job "cargo deny + cargo audit" is a required check; make deny|cargo|cargo-deny
cargo-audit|ci|same required check; make vuln-scan|cargo|cargo-audit
gh|agent|PR + release flow (scripts/release.sh hard-requires it)|brew|gh
rg|agent|repo convention: rg over grep, and it is gitignore-aware|brew|ripgrep
fd|agent|repo convention: fd over find|brew|fd
jq|agent|lets the Claude hook parse tool payloads exactly instead of by regex|brew|jq
cargo-watch|opt|make watch / watch-core / watch-lint (hard-errors without it)|cargo|cargo-watch
docker|opt|make serve-image + scripts/smoke-serve-image.sh|manual|
node|opt|make llms-txt and the website/ docs build|brew|node
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

# ── Claude Code settings ─────────────────────────────────────────────────────
hdr "Claude Code settings"
claude_dir="$repo_root/.claude"
settings="$claude_dir/settings.json"
stamp="$claude_dir/.stella-env-stamp"
mkdir -p "$claude_dir"

json_escape() { printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'; }

hook_block=""
if [ "$DO_HOOKS" -eq 1 ] && [ -x "$repo_root/scripts/claude-rustfmt-hook.sh" ]; then
  hook_block=",
  \"hooks\": {
    \"PostToolUse\": [
      {
        \"matcher\": \"Edit|Write|MultiEdit\",
        \"hooks\": [
          {
            \"type\": \"command\",
            \"command\": \"$(json_escape "$repo_root/scripts/claude-rustfmt-hook.sh")\",
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
    "RUST_BACKTRACE": "1",
    "CARGO_TERM_COLOR": "never"
  }$hook_block
}
EOF
)"

write_fresh() { printf '%s\n' "$generated" > "$settings"; }

if [ ! -f "$settings" ]; then
  write_fresh
  ok "wrote $settings"
elif [ -f "$stamp" ]; then
  write_fresh
  ok "refreshed $settings (previously generated by this script)"
else
  # Somebody hand-wrote settings for this worktree. Never silently clobber it.
  backup="$settings.bak.$$"
  cp "$settings" "$backup"
  if command -v jq >/dev/null 2>&1; then
    # `*` merges objects recursively; our keys win. Arrays are replaced, not
    # appended — so a pre-existing PostToolUse array is superseded, which is why
    # the backup above is unconditional.
    if printf '%s' "$generated" | jq --slurpfile cur "$settings" '$cur[0] * .' > "$settings.tmp" 2>/dev/null; then
      mv "$settings.tmp" "$settings"
      ok "merged into your existing $settings"
      note "backup: $backup"
    else
      rm -f "$settings.tmp"
      write_fresh
      warn "existing settings.json was not valid JSON — replaced it"
      note "backup: $backup"
    fi
  else
    write_fresh
    warn "replaced a hand-written settings.json (no jq available to merge)"
    note "backup: $backup"
  fi
fi

printf 'setup-claude-env v%s slug=%s\n' "$SETUP_VERSION" "$slug" > "$stamp"

if [ "$DO_HOOKS" -eq 1 ]; then
  if [ -x "$repo_root/scripts/claude-rustfmt-hook.sh" ]; then
    ok "per-edit hook: rustfmt + file-size ratchet on every .rs edit"
  else
    warn "scripts/claude-rustfmt-hook.sh missing or not executable — hook not wired"
  fi
fi

# ── The part that is easy to get wrong ───────────────────────────────────────
hdr "make gate vs CI"
cat <<'EOF'
  make gate  runs: no-scratch, action-pins, doc-citations, invariants, file-size,
             doc-warnings (RUSTDOCFLAGS=-D warnings), fmt --check, clippy, test.
             It stops at the FIRST failure; CI reports all of them in one run.

  make check is the fast subset — it skips tests, doc-citations AND rustdoc.
             Passing `make check` does not mean CI will pass.

  Green locally, red in CI, three ways:
    - release smoke   CI also runs `cargo build --workspace --release`
                      (thin LTO). Not in any make target.
    - normative-home  scripts/check-normative-home.sh runs in CI only.
    - supply chain    `cargo deny` + `cargo audit` are a SEPARATE required
                      check. `make gate` does not run them; `make supply-chain` does.
EOF

hdr "Next"
cat <<EOF
  make gate                          full local gate (what pre-push runs)
  make supply-chain                  the other required CI check
  ./scripts/setup-claude-env.sh --prune   reclaim caches for deleted worktrees
EOF

if [ -d "$CACHE_ROOT" ]; then
  note "cache total: $(du -sh "$CACHE_ROOT" 2>/dev/null | awk '{print $1}') in $CACHE_ROOT"
fi

if [ "$missing_required" -gt 0 ]; then
  printf '\n  %s%d required tool(s) still missing — see above%s\n' \
    "$C_RED" "$missing_required" "$C_RESET"
  exit 1
fi
