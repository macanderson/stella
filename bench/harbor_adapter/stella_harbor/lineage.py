"""Carrying what one trial learned into the next — and marking the result forever.

Every Harbor trial runs in a container that is deleted at teardown, so Stella
starts every task blank. It mines reflections, promotes skills, publishes
context records and authors tools during a run, and all of it dies with the
container. Running the same task ten times therefore measures the same agent
ten times: the flat line is an artifact of the harness, not a fact about the
agent, and "does Stella get better at a task it has seen before" has never been
askable here.

**Lineage** makes it askable. A lineage is an identified chain of trials on one
task: generation N's learned artifacts are harvested out of the container
before teardown, stored, and seeded back into generation N+1's container before
its agent takes a turn. Two operations, never one — harvest and seed are
separate functions with separate call sites, per the repository's
single-purpose rule (AGENTS.md invariant #9); there is no ``lineage(mode=…)``.

What travels is *learned state*, deliberately not telemetry:
``.stella/memories/``, ``.stella/skills/``, ``.stella/tools/``,
``.stella/rules/`` (with ``governance.toml`` and ``promotions.jsonl``) at the
workspace tier, and ``~/.stella/{skills,tools,agents}`` at the user tier.
:mod:`stella_harbor.telemetry_export` continues to carry ``.stella/private/``
separately and unchanged: that is *measurement of* the run, this is *state
carried into* the run, and mixing the two archives would make it impossible to
say which of the two a later trial was contaminated by.

# Why the contamination marker is the point of this module

A seeded trial has seen its task before. That is the entire experiment — and it
is also a loaded gun aimed at every benchmark number this project publishes. A
seeded 5/5 and a clean 5/5 are not the same claim, and nothing about the
resulting ``result.json``, the solve rate, or the trajectory looks any
different. The failure mode is not that someone lies; it is that six weeks
later a seeded run sits in the archive next to fifty clean ones, gets swept
into an average by a script that had no way to know, and the project quotes a
number it cannot defend. This has already happened to this benchmark with a
*milder* confound — the Claude Code arm silently spanning eight product
versions inside one archive (see :mod:`arenabench.provenance`) — and lineage is
strictly worse, because the contamination is deliberate and per-task.

So the marker is not a nicety attached to the feature; the feature is only
allowed to exist because the marker is structural:

* **Default off, and off costs nothing.** With no lineage id configured
  :func:`seed_lineage` and :func:`harvest_lineage` return before touching the
  container, the network, or the filesystem. A run without a lineage id is
  byte-identical to one from before this module existed, which is what keeps
  every existing measurement valid.
* **The id and the generation are recorded in provenance**, which is the
  record that already exists to stop two incomparable result sets being mixed,
  and it rides into the match's ``comparability_key``. A seeded match therefore
  cannot share a key with a clean one — and ``arenabench.series`` groups by
  that key, so no chart can draw one line through both.
* **The manifest is written per trial**, at seed time and again at harvest
  time, so a trial killed mid-run still discloses that it was seeded rather
  than reading as a clean loss.
* **A requested seed that cannot be honoured refuses the trial.** This is the
  one place in this file that is not best-effort, and it is the opposite of
  :mod:`stella_harbor.telemetry_export`'s contract on purpose. A trial that
  silently ran clean while its operator believed it was seeded produces a
  *clean* number wearing a *seeded* label — mislabelled in the direction that
  flatters nobody but confuses everybody. Failing the trial is recoverable; a
  mislabelled number in the archive is not. (The genuinely-normal case, "this
  lineage has no prior generation yet", is not a failure and is recorded as
  ``first-generation``.)
* **Harvest is best-effort**, because by the time it runs the trial has already
  been graded and nothing it does can change the number. A harvest that fails
  costs the *next* generation, never this one's verdict.

If you are reading this because you want to drop the marker to make two runs
comparable: they are not comparable. That is what the marker is telling you.

# Scope: per task, not per run

A lineage is keyed ``(lineage_id, task_id)``. Trial N+1 of ``fix-git`` seeds
from trial N of ``fix-git`` and from nothing else. Per-run scope was considered
and rejected on three grounds. The measurement question is per task ("is the
agent better at *this* task the second time"), and cross-task carryover would
make contamination unattributable — a task the agent had never seen would
inherit another task's memories and its "clean" cell would silently stop being
clean. Solve rate is computed per task, so per-task scope keeps exactly one
cell of the matrix contaminated per generation. And mechanically, trials of
different tasks in one match run concurrently, so a per-run store would need
write serialisation across containers for a merge nobody asked for.

# Conflict policy: the seed is a floor, the trial is the truth

The seed lands before the agent's first turn, so anything the trial writes
afterwards overwrites it in the ordinary filesystem sense, and the harvest
snapshots the container's final state. A memory the agent rewrote is carried
forward as rewritten; a skill it deleted is simply absent from the next
generation. There is no merge, no three-way resolution, and no
"seeded version wins" branch — the trial that just ran is always the more
recent authority about its own task, and a merge algorithm here would be a
second, unmeasured intervention sitting between the agent and its own state.

# Storage

The same S3 artifacts bucket ArenaBench already uses, under::

    lineage/<lineage-id>/<task-slug>/<generation:06d>/artifacts.tar.gz
    lineage/<lineage-id>/<task-slug>/<generation:06d>/manifest.json

Generations are dense, zero-padded and immutable: ``latest`` is derived by
listing the prefix rather than kept in a pointer object, because a pointer is a
second cell that can disagree with the objects it names, and a lineage's whole
value is that its history is inspectable. Publication is conditional
(``IfNoneMatch``), so two concurrent attempts on one task cannot collide on a
generation number — the loser takes the next one instead of overwriting the
winner.

Its own module rather than more length on the adapter package root, per the
repository's file-size ratchet rule: separable new code becomes a module.
"""

from __future__ import annotations

import json
import re
import shlex
import sys
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from .telemetry_export import await_maybe

__all__ = [
    "LINEAGE_BUCKET_ENV",
    "LINEAGE_DIR",
    "LINEAGE_ENV",
    "LINEAGE_MANIFEST_NAME",
    "LINEAGE_PREFIX",
    "LineageError",
    "LineageSelector",
    "LineageStore",
    "discover_task_id",
    "disclosed_lineage",
    "harvest_lineage",
    "harvest_script",
    "parse_lineage_selector",
    "parse_lineage_report",
    "seed_lineage",
    "seed_script",
    "task_slug",
]

#: The opt-in. ``"<id>"`` seeds the newest generation of that lineage;
#: ``"<id>@<generation>"`` replays one exact generation. Unset or blank is the
#: default and means no lineage at all — see the module docstring on why that
#: path must stay free of every side effect.
LINEAGE_ENV = "STELLA_LINEAGE"

#: Overrides the artifacts bucket. Unset, the bucket is derived from the
#: caller's AWS account exactly as ``arenabench.cloud`` derives it, so an
#: operator who already has ArenaBench working needs to configure nothing.
LINEAGE_BUCKET_ENV = "STELLA_LINEAGE_BUCKET"

#: Root key prefix inside the artifacts bucket.
LINEAGE_PREFIX = "lineage"

#: Host-side directory (under the trial's ``agent/`` logs) holding this
#: trial's lineage manifest. Deliberately beside — never inside — the
#: ``telemetry/`` directory :mod:`stella_harbor.telemetry_export` writes.
LINEAGE_DIR = "lineage"
LINEAGE_MANIFEST_NAME = "manifest.json"

#: Workspace-tier learned state: durable lessons, promoted skills, authored
#: tools, and published context records. Directories, copied whole.
WORKSPACE_DIRS = ("memories", "skills", "tools", "rules")
#: Workspace-tier files that sit beside the records rather than inside a
#: directory, depending on the release that wrote them.
WORKSPACE_FILES = ("governance.toml", "promotions.jsonl")
#: User-tier learned state under ``~/.stella``.
USER_DIRS = ("skills", "tools", "agents")

#: The one line the in-container scripts and the host-side parser agree on,
#: matching :mod:`stella_harbor.git_baseline`'s marker discipline.
LINEAGE_MARKER = "stella-lineage:"

#: Container-side scratch paths. Under ``/tmp`` so nothing lands in the task
#: workspace and no graded diff can ever contain them.
REMOTE_ARCHIVE = "/tmp/.stella-lineage.tar.gz"
_REMOTE_SEED_STAGE = "/tmp/.stella-lineage-seed"
_REMOTE_HARVEST_STAGE = "/tmp/.stella-lineage-harvest"

_SEED_TIMEOUT_SEC = 300
_HARVEST_TIMEOUT_SEC = 300

#: Bounded retries when two concurrent trials race for a generation number.
_PUBLISH_ATTEMPTS = 8

_DEFAULT_WORKDIR = "/app"

_LINEAGE_ID_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,63}")
_GENERATION_RE = re.compile(r"[0-9]{6}")
_SLUG_UNSAFE_RE = re.compile(r"[^A-Za-z0-9._-]+")

TRIAL_CONFIG_FILENAME = "config.json"


class LineageError(RuntimeError):
    """A requested lineage could not be honoured.

    Raised, never swallowed. See the module docstring: the failure this guards
    against is a trial that ran clean while its record says it was seeded.
    """


@dataclass(frozen=True)
class LineageSelector:
    """What the operator asked for: a lineage, and optionally one generation."""

    lineage_id: str
    #: ``None`` means "the newest generation there is", which is the normal
    #: mode; an integer replays one exact generation of the chain.
    generation: int | None = None

    def label(self) -> str:
        if self.generation is None:
            return self.lineage_id
        return f"{self.lineage_id}@{self.generation}"


def parse_lineage_selector(raw: Any) -> LineageSelector | None:
    """Parse ``STELLA_LINEAGE``, or ``None`` when no lineage was requested.

    ``None`` is the default and the only path with no side effects at all. A
    *malformed* value is an error rather than a shrug, for the reason the whole
    module exists: a typo that silently degrades to "no lineage" produces a
    clean run an operator believes is a seeded one.
    """
    if raw is None:
        return None
    text = str(raw).strip()
    if not text:
        return None
    lineage_id, sep, generation = text.partition("@")
    if _LINEAGE_ID_RE.fullmatch(lineage_id) is None:
        raise LineageError(
            f"{LINEAGE_ENV}={text!r} is not a usable lineage id: expected "
            "1-64 characters of [A-Za-z0-9._-] starting alphanumeric, "
            "optionally followed by @<generation>"
        )
    if not sep:
        return LineageSelector(lineage_id)
    if not generation.isdigit():
        raise LineageError(
            f"{LINEAGE_ENV}={text!r} pins a generation that is not a "
            "non-negative integer"
        )
    return LineageSelector(lineage_id, int(generation))


def task_slug(task_id: str) -> str:
    """A task identifier as one S3 path segment.

    Terminal-Bench names tasks ``org/name``; the slash becomes ``-`` rather
    than a nested prefix so that listing a lineage's tasks is one flat listing
    and a generation address is always exactly three segments deep.
    """
    slug = _SLUG_UNSAFE_RE.sub("-", task_id.strip()).strip("-.")
    return slug[:120] or "unknown-task"


def discover_task_id(logs_dir: Any) -> str | None:
    """Which task this trial is running, read off Harbor's own trial config.

    ``logs_dir`` is what Harbor hands the agent (``TrialPaths.agent_dir``) and
    the trial's ``config.json`` is its immediate parent — the same single
    location :func:`stella_harbor.turn_budget.discover_trial_deadline` reads,
    and for the same reason: walking further up would eventually find some
    other trial's configuration and key this trial's harvest to another task.

    Three spellings are accepted because Harbor has used all three across the
    releases this adapter has run against: the package name under
    ``task.task.name``, a flat ``task.name``, and finally the directory name of
    ``task.path`` for a task resolved from a local export. ``None`` when none
    of them is available — a lineage that cannot be keyed is refused by the
    caller, never guessed at.
    """
    if not logs_dir:
        return None
    try:
        raw = (Path(logs_dir).parent / TRIAL_CONFIG_FILENAME).read_text(
            encoding="utf-8"
        )
        trial_config = json.loads(raw)
    except (OSError, ValueError):
        return None
    task = trial_config.get("task") if isinstance(trial_config, dict) else None
    if not isinstance(task, dict):
        return None
    package = task.get("task")
    if isinstance(package, dict) and str(package.get("name") or "").strip():
        return str(package["name"]).strip()
    if str(task.get("name") or "").strip():
        return str(task["name"]).strip()
    path = str(task.get("path") or "").strip()
    return Path(path).name or None if path else None


def parse_lineage_report(stdout: str | None) -> dict[str, str]:
    """Parse the marker line an in-container script printed.

    The last marker wins and only non-empty ``key=value`` tokens are kept,
    mirroring :func:`stella_harbor.git_baseline.parse_git_baseline_report` —
    the two scripts speak the same shape deliberately, so a reader learns one
    convention rather than two.
    """
    if stdout:
        for line in reversed(stdout.splitlines()):
            stripped = line.strip()
            if not stripped.startswith(LINEAGE_MARKER):
                continue
            report: dict[str, str] = {}
            for token in stripped[len(LINEAGE_MARKER) :].split():
                key, sep, value = token.partition("=")
                if sep and key and value:
                    report[key] = value
            if report.get("state"):
                return report
            break
    return {"state": "unreported"}


def harvest_script(workdir: str | None) -> str:
    """POSIX sh that stages this trial's learned state into one tarball.

    Stages into ``/tmp`` and copies rather than tarring the live trees in
    place, so the archive's member paths are the tier layout
    (``workspace/…``, ``user/…``) rather than whatever absolute paths this
    image happens to use — which is what lets :func:`seed_script` restore into
    a container with a different workdir or a different home.

    Always exits 0 and reports through one marker line. The trial has already
    been graded by the time this runs; a harvest that fails costs the next
    generation, and must never be able to touch this one's verdict.
    """
    wd = shlex.quote(workdir or _DEFAULT_WORKDIR)
    stage = shlex.quote(_REMOTE_HARVEST_STAGE)
    out = shlex.quote(REMOTE_ARCHIVE)
    return (
        f"WD={wd}; STAGE={stage}; OUT={out}; ST=$WD/.stella; "
        'UH="${HOME:-/root}/.stella"; '
        'rm -rf "$STAGE" "$OUT" >/dev/null 2>&1; '
        'mkdir -p "$STAGE/workspace" "$STAGE/user" >/dev/null 2>&1; '
        f"for d in {' '.join(WORKSPACE_DIRS)}; do "
        'if [ -d "$ST/$d" ]; then cp -a "$ST/$d" "$STAGE/workspace/$d" '
        ">/dev/null 2>&1 || true; fi; done; "
        f"for f in {' '.join(WORKSPACE_FILES)}; do "
        'if [ -f "$ST/$f" ]; then cp -a "$ST/$f" "$STAGE/workspace/$f" '
        ">/dev/null 2>&1 || true; fi; done; "
        f"for d in {' '.join(USER_DIRS)}; do "
        'if [ -d "$UH/$d" ]; then cp -a "$UH/$d" "$STAGE/user/$d" '
        ">/dev/null 2>&1 || true; fi; done; "
        'N=$(find "$STAGE" -type f 2>/dev/null | wc -l | tr -d " "); '
        'if [ "${N:-0}" -eq 0 ]; then '
        f"echo '{LINEAGE_MARKER} state=empty'; "
        'elif tar -czf "$OUT" -C "$STAGE" . >/dev/null 2>&1; then '
        f'echo "{LINEAGE_MARKER} state=harvested files=$N"; '
        "else "
        f"echo '{LINEAGE_MARKER} state=failed detail=tar-failed'; "
        "fi"
    )


def seed_script(workdir: str | None) -> str:
    """POSIX sh that materialises an uploaded archive into both tiers.

    Order is required. The ``.stella/`` exclusion is written into the
    repository's ``info/exclude`` **before** anything is unpacked into the
    workspace, so seeded artifacts can never enter a graded diff even for a
    task that shipped its own git repository (where
    :mod:`stella_harbor.git_baseline` deliberately leaves the repo alone and
    writes no exclusion of its own). The git directory is resolved with
    ``rev-parse`` rather than assumed to be ``$WD/.git``, because a task whose
    workspace is a linked worktree has a ``.git`` *file*, and appending to a
    file that is not a directory would silently corrupt its gitdir pointer.

    Unlike the harvest, this reports failure states the caller raises on: a
    seed that did not happen must not be recorded as one that did.
    """
    wd = shlex.quote(workdir or _DEFAULT_WORKDIR)
    stage = shlex.quote(_REMOTE_SEED_STAGE)
    archive = shlex.quote(REMOTE_ARCHIVE)
    return (
        f"WD={wd}; STAGE={stage}; ARCHIVE={archive}; "
        'UH="${HOME:-/root}/.stella"; '
        'rm -rf "$STAGE" >/dev/null 2>&1; '
        'if ! mkdir -p "$STAGE" >/dev/null 2>&1; then '
        f"echo '{LINEAGE_MARKER} state=failed detail=stage-unavailable'; exit 0; fi; "
        'if ! tar -xzf "$ARCHIVE" -C "$STAGE" >/dev/null 2>&1; then '
        f"echo '{LINEAGE_MARKER} state=failed detail=untar-failed'; exit 0; fi; "
        'GD=$(cd "$WD" 2>/dev/null && git -c safe.directory="*" rev-parse '
        "--git-dir 2>/dev/null); "
        'if [ -n "$GD" ]; then case "$GD" in /*) ;; *) GD="$WD/$GD";; esac; '
        'mkdir -p "$GD/info" >/dev/null 2>&1 && '
        '{ grep -qxF ".stella/" "$GD/info/exclude" 2>/dev/null || '
        'printf "%s\\n" ".stella/" >> "$GD/info/exclude" 2>/dev/null; } || true; fi; '
        'if ! mkdir -p "$WD/.stella" "$UH" >/dev/null 2>&1; then '
        f"echo '{LINEAGE_MARKER} state=failed detail=target-unavailable'; exit 0; fi; "
        'if [ -d "$STAGE/workspace" ]; then '
        'cp -a "$STAGE/workspace/." "$WD/.stella/" >/dev/null 2>&1 || true; fi; '
        'if [ -d "$STAGE/user" ]; then '
        'cp -a "$STAGE/user/." "$UH/" >/dev/null 2>&1 || true; fi; '
        'W=$(find "$WD/.stella" -type f 2>/dev/null | wc -l | tr -d " "); '
        'U=$(find "$UH" -type f 2>/dev/null | wc -l | tr -d " "); '
        f'echo "{LINEAGE_MARKER} state=seeded workspace_files=${{W:-0}} '
        'user_files=${U:-0}"'
    )


def _boto3() -> Any:
    """Import boto3 on first use, with an actionable message when it is absent.

    Lazy for the reason :func:`arenabench.cloud._boto3` is lazy: the adapter's
    only hard dependency is Harbor, and a bench box that never uses lineage
    must not be made to install an AWS SDK to run a trial.
    """
    try:
        import boto3  # noqa: PLC0415 — deliberately deferred, see docstring
    except ImportError as exc:  # pragma: no cover — environment-dependent
        raise LineageError(
            "lineage needs boto3 to reach the artifacts bucket; install the "
            "adapter's `lineage` extra (`pip install stella-harbor[lineage]`)"
        ) from exc
    return boto3


class LineageStore:
    """Generation-numbered lineage archives in the S3 artifacts bucket."""

    def __init__(self, bucket: str = "", clients: dict[str, Any] | None = None):
        self._bucket = bucket
        self._clients = dict(clients or {})

    @classmethod
    def from_lookup(cls, lookup: Any) -> LineageStore:
        """Build a store from the adapter's configured-value resolver."""
        return cls(bucket=str(lookup(LINEAGE_BUCKET_ENV) or "").strip())

    def _client(self, service: str) -> Any:
        if service not in self._clients:
            self._clients[service] = _boto3().client(service)
        return self._clients[service]

    @property
    def bucket(self) -> str:
        """The artifacts bucket, derived from the account id unless pinned.

        The same derivation :meth:`arenabench.cloud.Cloud.bucket` uses, so an
        operator with ArenaBench's cloud path working configures nothing extra
        and the two never disagree about where artifacts live.
        """
        if not self._bucket:
            identity = self._client("sts").get_caller_identity()
            self._bucket = f"arenabench-artifacts-{identity['Account']}"
        return self._bucket

    def _prefix(self, lineage_id: str, slug: str) -> str:
        return f"{LINEAGE_PREFIX}/{lineage_id}/{slug}/"

    def generations(self, lineage_id: str, slug: str) -> list[int]:
        """Every generation of one lineage's task, ascending.

        Derived by listing rather than read from a pointer object: a pointer is
        a second cell that can disagree with the objects it names, and the
        whole value of a numbered chain is that its history is inspectable.
        """
        prefix = self._prefix(lineage_id, slug)
        found: list[int] = []
        token: str | None = None
        while True:
            request: dict[str, Any] = {
                "Bucket": self.bucket,
                "Prefix": prefix,
                "Delimiter": "/",
            }
            if token:
                request["ContinuationToken"] = token
            page = self._client("s3").list_objects_v2(**request)
            for entry in page.get("CommonPrefixes") or []:
                leaf = str(entry.get("Prefix") or "").removeprefix(prefix).strip("/")
                if _GENERATION_RE.fullmatch(leaf):
                    found.append(int(leaf))
            token = (
                page.get("NextContinuationToken") if page.get("IsTruncated") else None
            )
            if not token:
                break
        return sorted(found)

    def archive_key(self, lineage_id: str, slug: str, generation: int) -> str:
        """The immutable address of one generation's artifacts."""
        return f"{self._prefix(lineage_id, slug)}{generation:06d}/artifacts.tar.gz"

    def manifest_key(self, lineage_id: str, slug: str, generation: int) -> str:
        return f"{self._prefix(lineage_id, slug)}{generation:06d}/manifest.json"

    def fetch(
        self, lineage_id: str, slug: str, generation: int | None
    ) -> tuple[int, bytes] | None:
        """One generation's archive, or ``None`` when the chain has none.

        ``generation`` ``None`` takes the newest. ``None`` back means the
        lineage genuinely has no generation for this task yet, which is the
        ordinary first-run case — distinct from a pinned generation that does
        not exist, which the caller treats as a misconfiguration.
        """
        if generation is None:
            available = self.generations(lineage_id, slug)
            if not available:
                return None
            generation = available[-1]
        key = self.archive_key(lineage_id, slug, generation)
        try:
            body = self._client("s3").get_object(Bucket=self.bucket, Key=key)["Body"]
        except Exception:  # noqa: BLE001 — botocore's NoSuchKey is client-built
            return None
        return generation, body.read()

    def publish(
        self, lineage_id: str, slug: str, archive: bytes, manifest: dict[str, Any]
    ) -> int:
        """Append one generation to a lineage, and return its number.

        Written conditionally (``IfNoneMatch``), so two attempts of the same
        task racing to append cannot land on one generation number: the loser's
        put is refused and it takes the next number instead of overwriting the
        winner's artifacts. Bounded, because an unbounded retry against a
        genuinely broken bucket would hang a teardown.
        """
        s3 = self._client("s3")
        bucket = self.bucket
        generation = (self.generations(lineage_id, slug) or [-1])[-1] + 1
        for _ in range(_PUBLISH_ATTEMPTS):
            key = self.archive_key(lineage_id, slug, generation)
            try:
                s3.put_object(
                    Bucket=bucket, Key=key, Body=archive, IfNoneMatch="*"
                )
            except Exception as exc:  # noqa: BLE001 — see _is_precondition_failure
                if not _is_precondition_failure(exc):
                    raise
                generation += 1
                continue
            s3.put_object(
                Bucket=bucket,
                Key=self.manifest_key(lineage_id, slug, generation),
                Body=json.dumps({**manifest, "generation": generation}, indent=2)
                .encode("utf-8"),
            )
            return generation
        raise LineageError(
            f"could not append a generation to lineage {lineage_id}/{slug} after "
            f"{_PUBLISH_ATTEMPTS} attempts — every candidate number was taken"
        )


def _is_precondition_failure(exc: Exception) -> bool:
    """Whether an S3 error is "that key already exists".

    Matched on the response code rather than an exception class because
    botocore synthesises its error classes at runtime, so importing the type
    would make this module import boto3 eagerly — the exact thing
    :func:`_boto3` exists to avoid.
    """
    response = getattr(exc, "response", None)
    if not isinstance(response, dict):
        return False
    error = response.get("Error")
    code = str((error or {}).get("Code") or "")
    return code in ("PreconditionFailed", "412")


def disclosed_lineage(report: dict[str, Any] | None) -> dict[str, Any] | None:
    """The trial-metadata form of a lineage report, or ``None`` for no lineage.

    ``{"state": "off"}`` is a real report — it says the operators asked for
    nothing — but it must not become a metadata key, because a run with no
    lineage has to be byte-identical to one from before this feature existed.
    Every other state is disclosed verbatim: a trial that was seeded, that
    started a chain, or that failed to harvest all say so in the same place.
    """
    if not report or report.get("state") == "off":
        return None
    return report


def _workdir_of(environment: Any) -> str:
    return (
        getattr(getattr(environment, "task_env_config", None), "workdir", None)
        or _DEFAULT_WORKDIR
    )


def _write_manifest(agent: Any, report: dict[str, Any]) -> None:
    """Persist this trial's lineage disclosure beside its other agent logs.

    Written at seed time and rewritten at harvest time, so a trial killed
    between the two still says it was seeded rather than reading as a clean
    loss. Best-effort *as a write*: the authoritative match-level marker is the
    lineage id recorded in provenance at launch, which does not depend on any
    trial artifact surviving.
    """
    logs_dir = getattr(agent, "logs_dir", None)
    if logs_dir is None:
        return
    path = Path(logs_dir) / LINEAGE_DIR / LINEAGE_MANIFEST_NAME
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps({"schema": 1, **report}, indent=2) + "\n", encoding="utf-8"
        )
    except OSError as exc:  # noqa: BLE001 — disclosure, not the trial's verdict
        print(f"stella-adapter: could not write lineage manifest: {exc}",
              file=sys.stderr)


async def seed_lineage(agent: Any, environment: Any) -> dict[str, Any]:
    """Materialise a prior generation's learned state into this container.

    Runs before the agent's first turn. Returns the disclosure report, which
    the caller keeps for trial metadata; ``{"state": "off"}`` when no lineage
    was requested, reached without touching the container, the network, or the
    filesystem — see the module docstring on why that path must stay empty.

    Raises :class:`LineageError` when a lineage *was* requested and could not
    be honoured. This is deliberately not best-effort: a trial that ran clean
    under a seeded label is the one outcome worse than a failed trial.
    """
    selector = parse_lineage_selector(agent._configured_value(LINEAGE_ENV))
    if selector is None:
        return {"state": "off"}

    task_id = discover_task_id(getattr(agent, "logs_dir", None))
    if not task_id:
        raise LineageError(
            f"{LINEAGE_ENV}={selector.label()!r} was requested but this trial's "
            "task could not be identified from Harbor's config.json, so the "
            "generation could not be addressed. A lineage is keyed "
            "(lineage_id, task_id) and is never guessed at."
        )
    slug = task_slug(task_id)
    report: dict[str, Any] = {
        "lineage": selector.lineage_id,
        "task_id": task_id,
        "task_slug": slug,
        "requested_generation": selector.generation,
        "seeded_generation": None,
        "harvested_generation": None,
        "recorded_at": time.time(),
    }

    store = LineageStore.from_lookup(agent._configured_value)
    found = store.fetch(selector.lineage_id, slug, selector.generation)
    if found is None:
        if selector.generation is not None:
            raise LineageError(
                f"lineage {selector.label()} pins a generation that does not "
                f"exist for task {task_id}"
            )
        report["state"] = "first-generation"
        report["bucket"] = store.bucket
        _write_manifest(agent, report)
        print(
            f"stella-adapter: lineage {selector.lineage_id} has no prior "
            f"generation for {task_id}; this trial starts it",
            file=sys.stderr,
        )
        return report

    generation, archive = found
    with tempfile.NamedTemporaryFile(suffix=".tar.gz") as handle:
        handle.write(archive)
        handle.flush()
        await await_maybe(environment.upload_file, handle.name, REMOTE_ARCHIVE)
    result = await agent.exec_as_agent(
        environment,
        command=seed_script(_workdir_of(environment)),
        timeout_sec=_SEED_TIMEOUT_SEC,
    )
    parsed = parse_lineage_report(getattr(result, "stdout", None))
    if parsed.get("state") != "seeded":
        raise LineageError(
            f"lineage {selector.lineage_id} generation {generation} could not be "
            f"materialised into this container: {parsed}"
        )
    report["state"] = "seeded"
    report["seeded_generation"] = generation
    report["bucket"] = store.bucket
    report["workspace_files"] = int(parsed.get("workspace_files") or 0)
    report["user_files"] = int(parsed.get("user_files") or 0)
    _write_manifest(agent, report)
    print(
        f"stella-adapter: lineage {selector.lineage_id} seeded generation "
        f"{generation} for {task_id} — THIS TRIAL IS CONTAMINATED and its "
        "result is not comparable with a clean run",
        file=sys.stderr,
    )
    return report


async def harvest_lineage(agent: Any, environment: Any) -> dict[str, Any]:
    """Append this trial's learned state to its lineage as a new generation.

    Runs after the trial has been graded, which is what makes it best-effort:
    nothing it does can change this trial's verdict, and a harvest that fails
    costs only the next generation. ``{"state": "off"}`` with no side effects
    when no lineage was requested.
    """
    report = dict(getattr(agent, "_lineage", None) or {})
    try:
        selector = parse_lineage_selector(agent._configured_value(LINEAGE_ENV))
    except LineageError:
        # Already refused the trial at seed time; nothing to add here.
        return report or {"state": "off"}
    if selector is None:
        return {"state": "off"}
    if not report.get("task_slug"):
        return report or {"state": "off"}

    try:
        result = await agent.exec_as_agent(
            environment,
            command=harvest_script(_workdir_of(environment)),
            timeout_sec=_HARVEST_TIMEOUT_SEC,
        )
        parsed = parse_lineage_report(getattr(result, "stdout", None))
        if parsed.get("state") != "harvested":
            report["harvest_state"] = parsed.get("state", "unreported")
            _write_manifest(agent, report)
            return report
        with tempfile.TemporaryDirectory() as scratch:
            local = Path(scratch) / "artifacts.tar.gz"
            await await_maybe(environment.download_file, REMOTE_ARCHIVE, local)
            archive = local.read_bytes()
        store = LineageStore.from_lookup(agent._configured_value)
        generation = store.publish(
            selector.lineage_id,
            str(report["task_slug"]),
            archive,
            {
                "lineage": selector.lineage_id,
                "task_id": report.get("task_id", ""),
                "parent_generation": report.get("seeded_generation"),
                "files": int(parsed.get("files") or 0),
                "bytes": len(archive),
                "harvested_at": time.time(),
            },
        )
        report["harvest_state"] = "published"
        report["harvested_generation"] = generation
    except Exception as exc:  # noqa: BLE001 — never fails an already-graded trial
        report["harvest_state"] = "error"
        report["harvest_error"] = str(exc)
        print(f"stella-adapter: lineage harvest failed: {exc}", file=sys.stderr)
    _write_manifest(agent, report)
    return report
