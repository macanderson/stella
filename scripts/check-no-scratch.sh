#!/usr/bin/env bash
#
# Guard: no tracked file may be gitignored. See #448.
#
# Agent-session scratch kept reaching the remote — a bug-repro tree from a
# merged PR, "superpowers" plan/spec files describing already-shipped work, and
# per-session agent memory under .agent/. Adding .gitignore entries fixes the
# future only if nothing is *already* in the index: git ignores patterns for
# paths it is tracking, so a scratch directory committed once stays committed
# and silently accumulates, and `git add -f` re-adds it with no warning.
#
# The rule is general rather than a second copy of the
# scratch path list: **if a path is ignored, it must not be tracked.** That
# catches every future scratch directory the moment someone ignores it, with no
# pattern list here to drift out of sync with .gitignore.
#
# Note `git check-ignore` consults the index by default and reports a tracked
# file as "not ignored" — which is exactly the state this guard exists to
# find — so the check uses `git ls-files --cached --ignored` instead.
#
# This is the one guard #4952 did NOT widen, and the reason is here so the
# next reader does not take it for an oversight. Every other scanner
# there gained `--others --exclude-standard` so a brand-new file stops being
# invisible; here `--cached` is the subject. An untracked scratch file is the
# correct outcome, not a miss, and adding `--others` would fail the gate on
# every session's own working directory. Do not "fix" it to match the others.
# #4996 re-asked it and settled the same way; `scripts/test-no-scratch.sh`'s
# case F pins the answer so the next reader finds it as a test rather than as
# an argument.
#
# The consequence is that this guard's reach is exactly .gitignore's, so a
# scratch name nothing ignores is invisible here. That is not a hole to patch
# with a name list — the whole point of the rule above is that there is no
# pattern list here to drift — it is a reason to keep .gitignore covering the
# shapes sessions actually write. `.tmp-*` is there because `.tmp-msg.txt`
# matched neither `*.tmp` nor `tmp/` and rode a `git add -A` onto PR #4994.
#
# Uses portable POSIX tools so it runs on a bare CI runner.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if ! git rev-parse --git-dir >/dev/null 2>&1; then
  echo "check-no-scratch: not a git repository; skipping."
  exit 0
fi

offenders="$(git ls-files --cached --ignored --exclude-standard)"

if [ -z "$offenders" ]; then
  # The verdict is already decided; the write is best-effort. SIGPIPE is
  # ignored and the write's failure discarded, so a reader that closed the
  # pipe (`| head -1`, `| true`) cannot turn a green verdict into a failure
  # (#1815).
  trap '' PIPE
  echo "check-no-scratch: OK — no tracked file is gitignored." || true
  exit 0
fi

count="$(printf '%s\n' "$offenders" | wc -l | tr -d ' ')"

echo "check-no-scratch: FAIL — $count tracked file(s) match a .gitignore rule:" >&2
printf '%s\n' "$offenders" | sed 's/^/  /' >&2
cat >&2 <<'EOF'

These are tracked despite being ignored, so they will reach the remote.
Either:

  * they are scratch — untrack them (files stay on your disk):
        git rm -r --cached <path>
  * or they are real content and the ignore rule is too broad — narrow the
    rule in .gitignore, or add a negation (!path) for them.

A too-broad rule is the more dangerous case: it also means a NEW file added
under that path can never be committed without -f. That is how
website/public/icons/ ended up ignored (a bare "Icon?" pattern matched
"icons" on a case-insensitive filesystem).
EOF
exit 1
