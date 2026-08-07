"""Pre-agent git baseline for bare Terminal-Bench task workspaces.

Terminal-Bench task images are plain directories, and on a plain directory
Stella's diff probe is structurally blind (#973): every empty diff reads as
"cannot read the tree", candidate isolation is unavailable, the witness author
has nothing to snapshot, and the mutation audit cannot restore — most of the
verification ladder degrades on exactly this benchmark. One baseline commit
before the agent starts turns those channels on (issue #1211 item 4).

One workspace, one baseline, one disclosed state. The marker line the script
prints is the whole contract between the in-container shell and the host-side
parser, and the parsed report is published verbatim in trial metadata
(``stella_workspace_git_baseline``) and in the public ATIF trajectory — so
``state=preexisting`` has to keep meaning *the task owned this repository and
the adapter left it alone*. That is only true while exactly one thing may
create a repository here, which is why the script, its parser, and the step
that runs them live together in this module: a second baseline mechanism
would observe the first one's repository and publish a false provenance
claim about the workspace it just created.

Its own module rather than more length on the adapter package root, per the
repository's file-size ratchet rule: separable new code becomes a module.
"""

from __future__ import annotations

import shlex
import sys
from typing import Any

# Default ON; set the env to a falsy value to run the old bare-directory
# posture. A scored-behavior change either way — a claim run states which
# posture it used, and a run that turned it off discloses `state=disabled`
# rather than looking like a workspace nothing was ever attempted on.
GIT_BASELINE_ENV = "STELLA_TB_GIT_BASELINE"

# Skip the baseline when the workspace is larger than this many KiB: git
# objects roughly double a dataset-heavy image's disk usage, and task images
# carry disk limits the baseline must never be the thing that breaches.
GIT_BASELINE_SIZE_CAP_KB = 524_288  # 512 MiB

# The one line the in-container script and the host-side parser agree on.
GIT_BASELINE_MARKER = "stella-git-baseline:"
GIT_BASELINE_TIMEOUT_SEC = 180

# The identity and subject a reviewer finds on the one commit the adapter
# made. Both name the adapter, so a trajectory reporting `state=created` and
# the commit it refers to are recognizably the same event.
GIT_BASELINE_COMMIT_MESSAGE = "stella-harbor: task workspace baseline"
GIT_BASELINE_IDENT = "stella-harbor"
GIT_BASELINE_EMAIL = "stella-harbor@bench.invalid"

# The repo-wide ref pinning the baseline commit for Stella's witness
# machinery: `verify_done` resolves it in preference to HEAD, because the
# agent's own work is committed on top of the baseline during the trial and
# a flip measured against HEAD is then structurally impossible (#2067). The
# spelling is `WITNESS_BASELINE_TASK_REF` in `crates/stella-tools/src/verify.rs`.
GIT_BASELINE_REF = "refs/stella/task-baseline"


def git_baseline_script(workdir: str | None) -> str:
    """Build the POSIX-sh script that establishes the workspace git baseline.

    Always exits 0 and reports through one greppable marker line, because an
    evidence-channel setup step must never be able to fail a trial — a
    workspace it could not baseline runs exactly as every published number
    did (diff-blind), it just says so in metadata instead of silently.

    Branch order is load-bearing:

    - ``[ -e .git ]`` runs before ``rev-parse`` so a repository git refuses
      to read (dubious ownership) still reads as *preexisting* rather than
      falling through to ``git init`` — reinitializing and committing inside
      a task-owned repository would rewrite task state. ``fix-git``'s broken
      ``.git`` is the shape that makes this concrete.
    - ``rev-parse`` runs under ``safe.directory='*'`` for the same reason:
      a false "not a repository" is the one probe result that must not
      happen, and it covers the workspace-nested-inside-a-parent-repo case
      ``[ -e .git ]`` cannot see.
    - the size cap is tested only after both repository probes said no, so an
      oversized workspace that already has history is still reported as
      ``preexisting`` rather than as a skip that hides it.
    - ``.stella/`` is excluded via ``.git/info/exclude``, never a tracked
      ``.gitignore`` — the exclude file lives inside ``.git``, so the
      baseline adds zero entries to the tree a verifier might enumerate, and
      it is written before ``git add`` so Stella's own state can never enter
      the baseline or a later diff.
    - ``--allow-empty`` guarantees a baseline commit exists even for an
      empty task directory, so "repository with no HEAD" is not a state the
      diff probe can encounter.
    - ``update-ref`` pins the baseline commit at ``GIT_BASELINE_REF`` in the
      same chain, so a ``state=created`` workspace always carries the pin
      Stella's ``verify_done`` prefers over a HEAD the agent's own commits
      have advanced (#2067). ``state=preexisting`` deliberately writes no
      ref: a task-owned repository is left alone.

    The committer identity rides per-invocation ``-c`` flags, so no config
    file the task or the agent could observe is written, and the commit is
    made unsigned and without hooks: a task-supplied hook or a signing config
    must not be able to decide whether the baseline exists.
    """
    prologue = ""
    if workdir:
        prologue = (
            f"cd {shlex.quote(workdir)} || {{ "
            f"echo '{GIT_BASELINE_MARKER} state=error detail=workdir-unavailable'; "
            "exit 0; }; "
        )
    ident = (
        f"-c user.name={shlex.quote(GIT_BASELINE_IDENT)} "
        f"-c user.email={shlex.quote(GIT_BASELINE_EMAIL)}"
    )
    return (
        f"{prologue}"
        "if ! command -v git >/dev/null 2>&1; then "
        f"echo '{GIT_BASELINE_MARKER} state=unavailable detail=git-not-installed'; "
        "elif [ -e .git ] || git -c safe.directory='*' rev-parse "
        "--is-inside-work-tree >/dev/null 2>&1; then "
        f"echo '{GIT_BASELINE_MARKER} state=preexisting'; "
        "elif size_kb=$(du -sk . 2>/dev/null | cut -f1); "
        f'[ "${{size_kb:-0}}" -gt {GIT_BASELINE_SIZE_CAP_KB} ]; then '
        f'echo "{GIT_BASELINE_MARKER} state=skipped detail=workspace-over-size-cap '
        f'size_kb=${{size_kb:-0}} cap_kb={GIT_BASELINE_SIZE_CAP_KB}"; '
        "elif git init -q >/dev/null 2>&1 "
        "&& mkdir -p .git/info "
        "&& printf '%s\\n' '.stella/' >> .git/info/exclude "
        "&& git add -A >/dev/null 2>&1 "
        f"&& git {ident} commit -q --allow-empty --no-verify --no-gpg-sign "
        f"-m {shlex.quote(GIT_BASELINE_COMMIT_MESSAGE)} >/dev/null 2>&1 "
        f"&& git update-ref {GIT_BASELINE_REF} HEAD >/dev/null 2>&1; then "
        f'echo "{GIT_BASELINE_MARKER} state=created '
        'commit=$(git rev-parse HEAD 2>/dev/null)"; '
        "else "
        f"echo '{GIT_BASELINE_MARKER} state=failed detail=git-command-failed'; "
        "fi"
    )


def parse_git_baseline_report(stdout: str | None) -> dict[str, str]:
    """Parse the marker line into a report; absence is itself a finding.

    The last marker line wins (a retried step reports its final state), and
    only non-empty ``key=value`` tokens are kept, so a failed
    ``git rev-parse HEAD`` yields no ``commit`` key rather than an empty one.
    """
    if stdout:
        for line in reversed(stdout.splitlines()):
            stripped = line.strip()
            if not stripped.startswith(GIT_BASELINE_MARKER):
                continue
            report: dict[str, str] = {}
            for token in stripped[len(GIT_BASELINE_MARKER) :].split():
                key, sep, value = token.partition("=")
                if sep and key and value:
                    report[key] = value
            if report.get("state"):
                return report
            break
    return {"state": "unreported"}


async def ensure_git(agent: Any, environment: Any) -> None:
    """Install ``git`` into the task container if it is not already there.

    **Without it Stella runs as a bare worker loop.** The candidate machinery
    snapshots the working tree with ``git rev-parse`` before it generates
    anything; when the spawn fails the trial logs "candidate setup failed
    before any execution; running a bare worker turn on the working tree" and
    continues — degraded, but not obviously so. The witness and diff probes go
    blind for the same reason. A measured run recorded exactly that on every
    trial: the pipeline the contest is supposed to be measuring was never
    engaged. It is also the first branch ``git_baseline_script`` gives up on,
    which is what makes this module the place it belongs.

    Installing it is no more intrusive than what the comparison already allows
    the other side. Harbor's own Claude Code agent ``apt-get``s curl and a full
    Node toolchain into these same images as part of its setup, so a task image
    is plainly not treated as immutable — and a version-control binary changes
    what an agent can *observe*, not what the verifier checks.

    Best-effort by construction, for the same reason the baseline script is: an
    evidence-channel setup step must never be able to fail a trial. An image
    whose package manager is absent, offline, or read-only runs exactly as
    every published number did — diff-blind — and says so in the baseline
    metadata rather than dying here.

    A free function taking the agent, matching `run_git_baseline` below: it
    reads nothing off ``self`` but ``exec_as_root``, so binding it to the class
    bought nothing and cost the package root fifty lines.
    """
    try:
        await agent.exec_as_root(
            environment,
            command=(
                "if command -v git >/dev/null 2>&1; then exit 0; fi; "
                "if command -v apk >/dev/null 2>&1; then apk add --no-cache git; "
                "elif command -v apt-get >/dev/null 2>&1; then "
                "  apt-get update && apt-get install -y --no-install-recommends git; "
                "elif command -v dnf >/dev/null 2>&1; then dnf install -y git; "
                "elif command -v yum >/dev/null 2>&1; then yum install -y git; "
                "fi; "
                # Always succeed: the baseline probe reports the outcome.
                "command -v git >/dev/null 2>&1 || true"
            ),
            env={"DEBIAN_FRONTEND": "noninteractive"},
            timeout_sec=240,
        )
    except Exception:  # noqa: BLE001 - never fail a trial for this
        # Deliberately swallowed and deliberately silent. The outcome is
        # already reported through the git-baseline marker, which is the one
        # channel a reader checks; raising a second, louder signal here would
        # let a missing package manager end a trial that would otherwise have
        # run diff-blind exactly as every published number did. Nothing on the
        # agent is touched, because this must also hold for one built without
        # `__init__` — which is how the install tests construct one.
        pass


async def run_git_baseline(agent: Any, environment: Any) -> dict[str, str]:
    """Give a bare task workspace a git baseline, and report what happened.

    The single caller runs this once per task environment, before the
    code-graph step, so the ``.stella/`` exclusion is in place before
    ``stella init`` writes its DB into the workspace, and as the same
    in-container user the turn runs as — so the repository the diff probe
    later reads is owned by the user reading it.

    Returns the report the adapter discloses. Best-effort by construction:
    every outcome, including "could not even exec" and "the operator turned
    this off", comes back as a state rather than a raised error, because an
    evidence-channel setup step must never be able to fail a trial.
    """
    configured = agent._configured_value(GIT_BASELINE_ENV)
    if configured is not None and not _is_truthy(configured):
        return {"state": "disabled"}
    task_env_config = getattr(environment, "task_env_config", None)
    cwd = getattr(task_env_config, "workdir", None)
    try:
        result = await agent.exec_as_agent(
            environment,
            command=git_baseline_script(str(cwd) if cwd else None),
            timeout_sec=GIT_BASELINE_TIMEOUT_SEC,
        )
    except Exception as exc:  # noqa: BLE001 - a baseline aid, never fatal
        print(
            f"stella-adapter: workspace git baseline unavailable: {exc}",
            file=sys.stderr,
        )
        return {"state": "error", "detail": str(exc)}
    report = parse_git_baseline_report(getattr(result, "stdout", None))
    detail = report.get("detail")
    print(
        f"stella-adapter: workspace git baseline {report['state']}"
        + (f": {detail}" if detail else ""),
        file=sys.stderr,
    )
    return report


def _is_truthy(value: str | None) -> bool:
    """Mirror of the adapter root's `_is_truthy` (kept import-free both ways:
    the root imports this module, so this module must not import the root)."""
    if not value:
        return False
    return value.strip().lower() in ("1", "true", "yes", "on")
