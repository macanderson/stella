#!/usr/bin/env bash
#
# Does the dispatch path give each worker its own tree?
#
#   ./scripts/test-session-isolation.sh    (or: make session-isolation-test)
#
# Two agent sessions in one checkout are one branch switch away from losing
# each other's work. A switch restores the tracked files and leaves the
# untracked ones, so the tree ends up holding a new module that nothing
# declares. Nothing prints an error anywhere.
#
# So this does not ask whether the two paths differ. It runs the loss on the
# trees the dispatch handed out. Leave an edit in one tree. Force a branch
# switch in the other. Then look for the edit. Give both workers one tree and
# the edit goes, and this suite goes red.
#
# Three cases:
#
#   - a run that names no isolation gets two trees. The edit lives. This case
#     fails before ADR 0027. The old default was the shared tree, so both
#     workers landed in the checkout and the edit went.
#   - a plan naming `isolated` gets the same answer. That makes the default a
#     default, not a rename.
#   - a plan naming `shared_tree` gets one tree. The edit dies. That is the
#     control. A suite that can only pass is not a suite. It also pins the gap
#     the README names. Path claims guard one file. A branch switch rewrites
#     them all.
#
# Hermetic. A throwaway git repository, a stub model on loopback, and a
# `$STELLA_HOME` under the temp tree. No network, no key, no real provider.
# It needs `stella`, so it runs in ci.yml beside test-self-driving.sh. The
# suites in guard-self-tests.yml want no toolchain at all. Not a `make gate`
# step, for the reason the other repository-building suites are not.
#
# bash 3.2 compatible.
set -uo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
ROOT="$(mktemp -d)"
readonly repo_root ROOT

pass=0
fail=0

ok() { printf '  \033[32m✓\033[0m %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31m✗\033[0m %s\n' "$*"; fail=$((fail + 1)); }
head_() { printf '\n\033[1m%s\033[0m\n' "$*"; }

provider_pid=""
cleanup() {
  [ -n "$provider_pid" ] && kill "$provider_pid" 2>/dev/null
  rm -rf "$ROOT"
}
trap cleanup EXIT

# ── The binary ───────────────────────────────────────────────────────────────
#
# Same rule as test-self-driving.sh: take a pinned one, else a built one, else
# build a debug one. Correctness is under test, not speed.
locate_stella() {
  if [ -n "${STELLA_BIN:-}" ]; then printf '%s' "$STELLA_BIN"; return 0; fi
  local candidate
  for candidate in "$repo_root/target/release/stella" "$repo_root/target/debug/stella"; do
    [ -x "$candidate" ] && { printf '%s' "$candidate"; return 0; }
  done
  echo "session-isolation-test: no stella binary found; building one…" >&2
  (cd "$repo_root" && cargo build -q -p stella-cli --bin stella) >&2 || return 1
  printf '%s' "$repo_root/target/debug/stella"
}

STELLA="$(locate_stella)" || { echo "session-isolation-test: could not build stella" >&2; exit 1; }
readonly STELLA

# ── The stub model ───────────────────────────────────────────────────────────
#
# One completion, no tool calls, so each worker turn ends in a single step.
# The shape is the one the shared chat-completions adapter parses: a content
# delta, a usage frame, then the end marker.
write_provider() {
  cat >"$ROOT/provider.py" <<'PY'
"""A stub chat-completions endpoint on loopback. Prints its port, then serves."""
import http.server

BODY = (
    'data: {"choices":[{"delta":{"content":"did the requested work"}}]}\n\n'
    'data: {"choices":[{"delta":{}}],'
    '"usage":{"prompt_tokens":8,"completion_tokens":3}}\n\n'
    'data: [DONE]\n\n'
).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(BODY)))
        self.end_headers()
        self.wfile.write(BODY)

    def log_message(self, *args):
        pass


server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
PY
}

start_provider() {
  write_provider
  python3 "$ROOT/provider.py" >"$ROOT/port" 2>"$ROOT/provider.err" &
  provider_pid=$!
  local waited=0
  while [ ! -s "$ROOT/port" ] && [ "$waited" -lt 100 ]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  PORT="$(cat "$ROOT/port" 2>/dev/null)"
  if [ -z "$PORT" ]; then
    echo "session-isolation-test: the stub model never came up:" >&2
    cat "$ROOT/provider.err" >&2
    exit 1
  fi
}

# ── The fixture repository ───────────────────────────────────────────────────
#
# `keep.txt` is the tracked file the experiment edits. It has to be committed
# and it has to differ from the edit, or a forced switch would have nothing to
# restore and the experiment could never fail.
make_repo() {
  local dir="$1"
  mkdir -p "$dir"
  git -C "$dir" init --quiet
  git -C "$dir" config user.email "fixture@example.invalid"
  git -C "$dir" config user.name "Fixture"
  git -C "$dir" config commit.gpgsign false
  printf 'seed\n' >"$dir/keep.txt"
  printf 'fixture\n' >"$dir/README.md"
  git -C "$dir" add keep.txt README.md
  git -C "$dir" commit --quiet -m "seed"
}

write_plan() { # write_plan <file> <isolation>
  cat >"$1" <<JSON
{
  "tasks": [
    { "id": "alpha", "title": "alpha", "prompt": "answer in one short sentence",
      "isolation": "$2" },
    { "id": "beta", "title": "beta", "prompt": "answer in one short sentence",
      "isolation": "$2" }
  ]
}
JSON
}

# ── The dispatch ─────────────────────────────────────────────────────────────
#
# Every key the binary could pick up is cleared here. One inherited variable
# is all it takes to reach a billed backend.
run_fleet() { # run_fleet <repo> <log> <fleet args...>
  local repo="$1" log="$2"
  shift 2
  ( cd "$repo" &&
    env \
      -u OPENROUTER_API_KEY -u ANTHROPIC_API_KEY -u ZAI_API_KEY -u OPENAI_API_KEY \
      -u XAI_API_KEY -u DEEPSEEK_API_KEY -u GEMINI_API_KEY -u GOOGLE_API_KEY \
      -u GOOGLE_APPLICATION_CREDENTIALS -u VERTEX_ACCESS_TOKEN \
      -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u AWS_SESSION_TOKEN \
      -u STELLA_EMBED_URL -u STELLA_EMBED_MODEL -u STELLA_EMBED_API_KEY \
      -u STELLA_EMBED_DIMS -u STELLA_EMBED_FLOOR -u VOYAGE_API_KEY \
      STELLA_HOME="$ROOT/home" \
      STELLA_DATA_DIR="$ROOT/home" \
      STELLA_NO_ENV_FILE=1 \
      STELLA_TRUST_PROJECT=1 \
    "$STELLA" \
      --model zai/glm-5.2 \
      --api-key sk-test-session-isolation \
      --base-url "http://127.0.0.1:$PORT" \
      --spend-limit 5.0 \
      fleet "$@" --max-concurrency 2 --task-timeout 120 \
      </dev/null ) >"$log" 2>&1
}

# The trees the dispatch handed out, one per line, main checkout excluded.
worker_trees() { # worker_trees <repo>
  git -C "$1" worktree list --porcelain |
    sed -n 's/^worktree //p' |
    grep -F "/.stella/worktrees/"
}

# ── The experiment ───────────────────────────────────────────────────────────
#
# Leave uncommitted work in A. Then have B switch branch the hardest way git
# offers. Answers `kept` or `lost` for the tracked edit, and again for the
# untracked file. The pair is what makes this failure so hard to see: after
# the loss the tree looks fuller, not emptier.
experiment() { # experiment <tree_a> <tree_b> <branch>
  local a="$1" b="$2" branch="$3" base tracked untracked
  base="$(git -C "$a" rev-parse HEAD)"
  printf 'work in progress\n' >"$a/keep.txt"
  printf 'a new module\n' >"$a/new_module.txt"
  git -C "$b" checkout --force -B "$branch" "$base" >/dev/null 2>&1
  if [ "$(cat "$a/keep.txt" 2>/dev/null)" = "work in progress" ]; then
    tracked=kept
  else
    tracked=lost
  fi
  if [ -f "$a/new_module.txt" ]; then untracked=kept; else untracked=lost; fi
  printf '%s %s\n' "$tracked" "$untracked"
}

# Run the whole test on one dispatch. Report both halves.
two_trees_survive() { # two_trees_survive <repo> <log> <switch branch>
  local repo="$1" log="$2" branch="$3" trees tree_count tree_a tree_b result
  trees="$(worker_trees "$repo")"
  tree_count="$(printf '%s\n' "$trees" | grep -c . )"
  if [ "$tree_count" -eq 2 ]; then
    ok "the dispatch cut one tree per worker"
  else
    bad "expected 2 worker trees, got $tree_count. The run said:"
    sed -e 's/^/      /' "$log" >&2
    return
  fi
  tree_a="$(printf '%s\n' "$trees" | sed -n 1p)"
  tree_b="$(printf '%s\n' "$trees" | sed -n 2p)"
  result="$(experiment "$tree_a" "$tree_b" "$branch")"
  case "$result" in
  "kept kept") ok "a branch switch in one tree left the other's work alone" ;;
  *) bad "the sibling's work did not survive the switch (tracked/untracked: $result)" ;;
  esac
}

# ── Case 1: nothing names an isolation ───────────────────────────────────────
#
# Two positional prompts. That is what a person types. Nothing here asks for a
# worktree, so the default answers. Before ADR 0027 the default was the shared
# tree. This case found no worker trees, and the sibling's edit was gone.

start_provider

head_ "two workers, no isolation named"

repo_default="$ROOT/default"
make_repo "$repo_default"
run_fleet "$repo_default" "$ROOT/default.log" \
  "answer in one short sentence" "answer in one short sentence"
two_trees_survive "$repo_default" "$ROOT/default.log" "switch-default"

# ── Case 2: a plan that names `isolated` ─────────────────────────────────────

head_ "two workers, isolated tasks"

repo_iso="$ROOT/isolated"
make_repo "$repo_iso"
write_plan "$ROOT/isolated.json" "isolated"
run_fleet "$repo_iso" "$ROOT/isolated.log" --plan "$ROOT/isolated.json"
two_trees_survive "$repo_iso" "$ROOT/isolated.log" "switch-isolated"

# ── Case 3: the control ──────────────────────────────────────────────────────

head_ "two workers, shared tree (the control)"

repo_shared="$ROOT/shared"
make_repo "$repo_shared"
write_plan "$ROOT/shared.json" "shared_tree"
run_fleet "$repo_shared" "$ROOT/shared.log" --plan "$ROOT/shared.json"

shared_trees="$(worker_trees "$repo_shared")"
if [ -z "$shared_trees" ]; then
  ok "the dispatch cut no tree — both workers ran in the checkout itself"
else
  bad "a shared-tree plan cut worker trees, which it must not: $shared_trees"
fi

result="$(experiment "$repo_shared" "$repo_shared" "switch-shared")"
case "$result" in
"lost kept")
  ok "one tree loses the tracked edit and keeps the untracked file"
  ;;
"kept kept")
  bad "the experiment cannot fail: one tree kept the edit through a forced switch"
  ;;
*)
  bad "the control ended in a state this suite does not model (tracked/untracked: $result)"
  ;;
esac

echo ""
echo "  $pass passed, $fail failed"
[ "$fail" -eq 0 ]
