#!/usr/bin/env bash
#
# Guard: the four agent-config role names have one spelling, across four
# languages. See #1449.
#
# ── The failure this exists to catch ─────────────────────────────────────────
#
# #1394 renamed the non-worker verification role, `judge` -> `verifier`. The
# Rust compiler caught every Rust call site. It caught none of the producers of
# that same name that are not Rust, and the fallout took three PRs:
#
#   crates/stella-tui/src/statline.rs   matched PipelineRole::Judge
#                                       -> compiler; `main` shipped red (#1417)
#   arenabench/arenabench/config.py     translated verifier -> judge
#                                       -> NOTHING
#   bench/terminal_bench_analysis/      posture digests went stale
#                                       -> one of four pinned by luck
#   crates/stella-observatory/…/index.html  filters agents by a literal list
#                                       -> NOTHING
#
# The arenabench one is the shape that matters, and it is why this guard is
# worth its weight: no error, no failing test, no stopped match. The seat runs
# with the verifier inheriting the worker baseline while the scoreboard reports
# it as configured — so the contest publishes a number for a pairing it never
# ran. A wrong measurement, not a crash, is the worst thing this repository can
# ship (CLAUDE.md, "Measure honestly, especially when it costs us").
#
# ── The normative home ───────────────────────────────────────────────────────
#
# `role_key()` in crates/stella-cli/src/config_wiring.rs, which maps
# `EngineAgentKind` to the exact string that appears in `agents.<role>` and
# `pipeline_<role>_model`. The enum is the type; `role_key` is the *wire
# spelling*, and the wire spelling is what the other three languages copy.
#
# Note this is NOT `stella_protocol::ModelCallRole`, which has fourteen variants
# describing individual model calls (PlanRepair, WitnessAuthor, Summarization…).
# Those never cross a language boundary. The four agent-config roles do, and
# they are the whole subject here.
#
# ── Aliases ──────────────────────────────────────────────────────────────────
#
# `judge` is a deliberately retired spelling. It survives in exactly two places,
# both of which read it and neither of which writes it: a serde `alias` in Rust,
# and `_ROLE_ALIASES` in arenabench's config loader, which upgrades an old match
# file on the way in. That is compatibility, not drift, so the alias table below
# names it explicitly rather than the guard pretending it does not exist.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

fail=0
note() { printf 'check-role-names: %s\n' "$1" >&2; }

# Retired spellings, and the modern role each maps to. A producer may mention
# one of these only as an alias — never as a member of its own role set.
retired_spellings="judge"

# ── The truth ────────────────────────────────────────────────────────────────

rust_home="crates/stella-cli/src/config_wiring.rs"

if [ ! -f "$rust_home" ]; then
  note "FAIL — $rust_home does not exist; role_key() is the normative home."
  exit 1
fi

# The match arms of role_key(): `EngineAgentKind::Verifier => "verifier",`
roles="$(
  awk '
    /pub fn role_key/ { inside = 1; next }
    inside && /^\}/   { exit }
    inside && /EngineAgentKind::[A-Za-z]+ *=> *"[a-z_]+"/ {
      match($0, /"[a-z_]+"/)
      print substr($0, RSTART + 1, RLENGTH - 2)
    }
  ' "$rust_home" | LC_ALL=C sort
)"

if [ -z "$roles" ]; then
  note "FAIL — could not read any role from role_key() in $rust_home."
  note "     If that function moved or changed shape, repoint this guard;"
  note "     do not delete it. It is the only thing holding four languages"
  note "     to one spelling."
  exit 1
fi

roles_flat="$(printf '%s\n' "$roles" | tr '\n' ' ')"

is_role() {
  case " $roles_flat " in
  *" $1 "*) return 0 ;;
  *) return 1 ;;
  esac
}

# Extract every double- or single-quoted lowercase token from stdin, one per
# line, sorted and deduplicated. Used to read a literal list out of Python or
# JavaScript without teaching this script either language.
quoted_tokens() {
  tr ',' '\n' | sed -n 's/.*["'"'"']\([a-z_][a-z_]*\)["'"'"'].*/\1/p' | LC_ALL=C sort -u
}

# Compare a producer's role set against the Rust one.
#   $1 producer path (for the message), $2 what it is, $3 the extracted set
expect_exact_set() {
  path="$1"
  what="$2"
  found="$3"

  if [ -z "$found" ]; then
    note "FAIL — $path: could not extract $what."
    note "     The producer moved or changed shape. Repoint the extraction in"
    note "     this guard rather than dropping the producer from it."
    fail=1
    return
  fi

  missing="$(comm -23 <(printf '%s\n' "$roles") <(printf '%s\n' "$found") | tr '\n' ' ')"
  extra="$(comm -13 <(printf '%s\n' "$roles") <(printf '%s\n' "$found") | tr '\n' ' ')"

  if [ -n "${missing// /}" ]; then
    note "FAIL — $path ($what) is missing role(s): $missing"
    fail=1
  fi
  if [ -n "${extra// /}" ]; then
    note "FAIL — $path ($what) has role(s) the engine does not know: $extra"
    fail=1
  fi
}

# Every role named in a `pipeline_<role>_model` key anywhere in a file must be a
# real role. A subset check, not an exact one: `default` has no flat key by
# design (`default_model` is its key), so the set is legitimately smaller.
expect_flat_keys_known() {
  path="$1"
  [ -f "$path" ] || return 0
  sed -n 's/.*pipeline_\([a-z_][a-z_]*\)_model.*/\1/p' "$path" | LC_ALL=C sort -u |
    while IFS= read -r r; do
      [ -n "$r" ] || continue
      # Not a role name — these are the pipeline's own numeric settings.
      case "$r" in
      max_revisions | candidates) continue ;;
      esac
      if ! is_role "$r"; then
        note "FAIL — $path writes pipeline_${r}_model, and '$r' is not a role."
        printf 'x' >>"$flagfile"
      fi
    done
}

# `while read` in a pipeline runs in a subshell, so `fail=1` set inside one
# would not survive. A marker file is the portable way to carry it back.
flagfile="$(mktemp "${TMPDIR:-/tmp}/stella-role-names.XXXXXX")"
trap 'rm -f "$flagfile"' EXIT

# ── The producers ────────────────────────────────────────────────────────────
#
# Each entry states where the set lives and how it is spelled. Finding these
# again when one moves is half the work this guard does; a producer that
# vanishes should be deleted from here in the same PR, never silently skipped.

# 1. arenabench's canonical tuple.
p="arenabench/arenabench/model.py"
if [ -f "$p" ]; then
  expect_exact_set "$p" "ROLES tuple" \
    "$(grep -E '^ROLES: *tuple' "$p" | quoted_tokens)"
else
  note "FAIL — $p is gone; update this guard's producer list."
  fail=1
fi

# 2. arenabench's per-role loop, which drives what a seat actually writes.
p="arenabench/arenabench/harbor_agent.py"
if [ -f "$p" ]; then
  expect_exact_set "$p" "per-role loop" \
    "$(grep -E 'for role in \(' "$p" | quoted_tokens)"
  expect_flat_keys_known "$p"
else
  note "FAIL — $p is gone; update this guard's producer list."
  fail=1
fi

# 3. The Observatory's agent filter — a JS literal, invisible to every Rust and
#    Python test in the tree. This is one of the two that silently vanished a
#    row in #1394.
p="crates/stella-observatory/src/assets/index.html"
if [ -f "$p" ]; then
  expect_exact_set "$p" "agent-name filter" \
    "$(grep -E '\]\.filter\(n *=> *agents\[n\]\)' "$p" | quoted_tokens)"
else
  note "FAIL — $p is gone; update this guard's producer list."
  fail=1
fi

# 4. The harbor adapter's posture writer. Every role it names goes into a hashed
#    posture, so a stale spelling here re-hashes every registered arm
#    (bench/READINESS.md §8.4.4).
expect_flat_keys_known "bench/harbor_adapter/stella_harbor/posture.py"

# 5. The TUI's second enum. Compiler-checked as a type, but its variants are a
#    parallel vocabulary that must stay inside the engine's — a subset, since
#    the deck has no `default` slot.
p="crates/stella-tui/src/deck.rs"
if [ -f "$p" ]; then
  # Redirected rather than piped: a `while read` on the right of a pipe runs in
  # a subshell, where `fail=1` would be set and then thrown away.
  while IFS= read -r variant; do
    [ -n "$variant" ] || continue
    if ! is_role "$variant"; then
      note "FAIL — $p: PipelineRole::${variant} is not an engine role."
      fail=1
    fi
  done < <(awk '
    /pub enum PipelineRole/ { inside = 1; next }
    inside && /^\}/         { exit }
    inside && /^ *[A-Z][A-Za-z]*,/ { gsub(/[ ,]/, ""); print tolower($0) }
  ' "$p")
fi

# ── Retired spellings ────────────────────────────────────────────────────────
#
# A retired name may appear only where it is being *translated*. Anywhere else
# it is the drift this guard exists to catch.

for old in $retired_spellings; do
  if is_role "$old"; then
    note "FAIL — '$old' is listed as retired but role_key() still emits it."
    fail=1
  fi
done

if [ -s "$flagfile" ]; then
  fail=1
fi

if [ "$fail" -ne 0 ]; then
  note ""
  note "The role names live in role_key() in $rust_home."
  note "Renaming one is a cross-language change: the Rust compiler will not"
  note "find the Python dict or the JavaScript literal, and neither will your"
  note "tests. Update every producer named above in the same PR."
  exit 1
fi

printf 'check-role-names: OK — %d role(s) [%s] consistent across every producer.\n' \
  "$(printf '%s\n' "$roles" | wc -l | tr -d ' ')" "$(printf '%s' "$roles_flat" | sed 's/ $//')"
