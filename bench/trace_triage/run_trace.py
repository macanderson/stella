"""Read one ArenaBench run off disk as trials, events, rewards and exceptions.

This is the ground-truth layer of `make triage-bench-traces`. Everything a
detector concludes has to come from here, because `stella-events.jsonl` is the
only artifact that is not a projection: `result.json`'s verdict cell is
either/or and hides an exception when a task passes, the UI reads the same
projections, and a `reward.txt` of `0` says nothing about *why*.

Layout, as written by the cloud runner and mirrored from
`s3://arenabench-artifacts-578673726240/runs/<run_id>/`:

    <run_root>/
      <trial_uuid>/
        results.json                      <- harbor stats, incl. exception_stats
        ws/matches/<match_id>/
          spec.json                       <- the seated contestants + role models
          provenance.json                 <- dataset digest, SUT commit
          jobs/<job_name>/
            <task_id>/                    <- e.g. code-from-image__UTUWFRc
              agent/stella-events.jsonl   <- ground truth
              exception.txt               <- present only when the trial threw
              verifier/reward.txt         <- "0" or "1"

One `<trial_uuid>` holds exactly one graded task in every run recorded so far,
but nothing in the layout promises that, so a trial dir yielding several task
directories yields several `Trial`s rather than one arbitrary winner.

Two rules this module exists to enforce, both of which the repository has been
burned by:

* **Guard every `json.loads`.** A run still streaming has a half-written last
  line, and a `JSONDecodeError` there would take out the whole triage of a run
  whose other 39 trials are complete. Malformed lines are counted, never fatal,
  and the count is reported so a reader can tell "no such event" from "the tail
  was unreadable".
* **`proof` discriminates on `step.kind`, not the top-level `type`.** Every
  `proof` event carries `"type": "proof"`; what it *proves* lives in
  `step.kind` (`oracle`, `warrant`, `witness_unavailable`, ...). Reading the
  top-level tag gives one undifferentiated bucket.
"""

from __future__ import annotations

import json
from collections.abc import Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

S3_BUCKET = "arenabench-artifacts-578673726240"

# Event tags this module indexes eagerly. Everything else stays in `events` and
# is still reachable — the index is a convenience, never a filter.
_INDEXED = (
    "step_usage",
    "step_manifest",
    "proof",
    "tool_start",
    "tool_result",
    "file_change",
    "verdict",
    "error",
    "reasoning",
    "stage",
    "complete",
    "loop_detected",
)


@dataclass(frozen=True)
class ToolCall:
    """A `tool_start` joined to its `tool_result` by `call_id`.

    The join is what makes a tool observation citable: `tool_result` carries
    only the id, so a result quoted without its call says nothing about what
    was asked. `result` is `None` for a call whose result never landed (the
    process died mid-tool), which is itself a finding rather than a gap.
    """

    call_id: str
    name: str
    input: dict[str, Any]
    result: dict[str, Any] | None
    start_index: int
    result_index: int | None

    @property
    def error(self) -> dict[str, Any] | None:
        """The error envelope, if the result carried one."""
        if not self.result:
            return None
        output = self.result.get("output")
        if not isinstance(output, dict):
            return None
        err = output.get("error")
        return err if isinstance(err, dict) else None

    @property
    def error_message(self) -> str:
        err = self.error
        if not err:
            return ""
        message = err.get("message")
        return message if isinstance(message, str) else ""

    @property
    def error_class(self) -> str | None:
        """The executor's `ErrorClass` token, or `None` when the wire has none.

        Serialized by `crates/stella-protocol/src/tool.rs` only when the error
        was classified, so an old trace and an unclassified error look the
        same here: both `None`. A detector reading this must treat `None` as
        "no claim", never as a class of its own.
        """
        err = self.error
        if not err:
            return None
        cls = err.get("class")
        return cls if isinstance(cls, str) else None

    @property
    def ok_content(self) -> str:
        if not self.result:
            return ""
        output = self.result.get("output")
        if not isinstance(output, dict):
            return ""
        ok = output.get("ok")
        if not isinstance(ok, dict):
            return ""
        content = ok.get("content")
        return content if isinstance(content, str) else ""

    def signature(self) -> str:
        """Byte-stable `(tool, args)` identity, for spotting a repeat."""
        return json.dumps([self.name, self.input], sort_keys=True)

    def output_signature(self) -> str:
        return json.dumps(self.result.get("output") if self.result else None, sort_keys=True)


@dataclass
class Trial:
    """One graded task attempt, with its events already read and indexed."""

    run_id: str
    trial_uuid: str
    task_id: str
    root: Path
    run_root: Path
    events: list[dict[str, Any]] = field(default_factory=list)
    malformed_lines: int = 0
    by_type: dict[str, list[dict[str, Any]]] = field(default_factory=dict)
    tool_calls: list[ToolCall] = field(default_factory=list)
    reward: float | None = None
    exception_text: str = ""
    exception_types: tuple[str, ...] = ()

    # ---------------------------------------------------------------- views

    @property
    def task_name(self) -> str:
        """The dataset task, with the per-trial suffix stripped.

        `code-from-image__UTUWFRc` is one *attempt* at `code-from-image`; the
        suffix is fresh every run, so anything that must be stable across runs
        (a fingerprint, a rate keyed by task) uses this and not `task_id`.
        """
        return self.task_id.split("__", 1)[0]

    def of_type(self, tag: str) -> list[dict[str, Any]]:
        return self.by_type.get(tag, [])

    def proofs(self, kind: str | None = None) -> list[dict[str, Any]]:
        """`proof` steps, discriminated on `step.kind` — never the top tag."""
        steps = []
        for event in self.of_type("proof"):
            step = event.get("step")
            if not isinstance(step, dict):
                continue
            if kind is None or step.get("kind") == kind:
                steps.append(step)
        return steps

    def role_census(self) -> dict[tuple[str, str], int]:
        """`(role, model)` counted off `step_usage` — completed calls only.

        `step_manifest` records a call the engine *intended*; `step_usage`
        records one that returned. A census off the manifest counts calls that
        never completed as if they had run.
        """
        census: dict[tuple[str, str], int] = {}
        for event in self.of_type("step_usage"):
            key = (str(event.get("role") or "?"), str(event.get("model") or "?"))
            census[key] = census.get(key, 0) + 1
        return census

    # ------------------------------------------------------------- citation

    def s3_key(self, relative: str = "agent/stella-events.jsonl") -> str:
        """The fetchable S3 key for an artifact of this trial.

        A finding without one of these is a claim a reader cannot check, which
        is the failure mode this whole tool exists to prevent.
        """
        prefix = self.root.relative_to(self.run_root).as_posix()
        return f"s3://{S3_BUCKET}/runs/{self.run_id}/{prefix}/{relative}"

    @property
    def events_path(self) -> Path:
        return self.root / "agent" / "stella-events.jsonl"


@dataclass
class Run:
    """A whole run: its trials, and the seat configuration it was launched with."""

    run_id: str
    root: Path
    trials: list[Trial] = field(default_factory=list)
    declared_roles: dict[str, str] = field(default_factory=dict)
    declared_worker_model: str = ""
    sut_ref: str = ""
    match_name: str = ""
    bare_loop: bool = False
    """Whether the seat ran the bare turn loop instead of the staged pipeline.

    Load-bearing, not decoration. A bare-loop arm has no verify stage, so it
    emits no `verdict` and authors no witness — and a pipeline detector run
    against it reports 20/20 of an absence that is the arm working as designed.
    That exact false positive was live here until a bare run was pointed at it.
    """

    @property
    def graded_trials(self) -> list[Trial]:
        return [t for t in self.trials if t.reward is not None]

    def s3_prefix(self) -> str:
        return f"s3://{S3_BUCKET}/runs/{self.run_id}/"


# --------------------------------------------------------------------------
# Reading
# --------------------------------------------------------------------------


def iter_events(path: Path) -> Iterator[tuple[int, dict[str, Any] | None]]:
    """Yield `(line_number, event_or_None)`; `None` marks an unreadable line.

    Line numbers are 1-based so a finding can cite `stella-events.jsonl:4217`
    and a reader can `sed -n 4217p` it.
    """
    with path.open(encoding="utf-8", errors="replace") as handle:
        for lineno, line in enumerate(handle, start=1):
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except (ValueError, UnicodeDecodeError):
                yield lineno, None
                continue
            yield lineno, event if isinstance(event, dict) else None


def _read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8", errors="replace"))
    except (OSError, ValueError):
        return {}
    return value if isinstance(value, dict) else {}


def _read_reward(task_dir: Path) -> float | None:
    path = task_dir / "verifier" / "reward.txt"
    try:
        return float(path.read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return None


def _exception_types(results: dict[str, Any], task_id: str) -> tuple[str, ...]:
    """Exception class names harbor recorded for `task_id`.

    Harbor buckets these per eval under `stats.evals.<eval>.exception_stats`,
    as `{"AgentTimeoutError": ["code-from-image__UTUWFRc"]}`. Reading the class
    name from here rather than regexing `exception.txt` keeps the detector on
    the runner's own classification instead of on traceback prose.

    Note the file: this is harbor's **job-level** `result.json`
    (`ws/matches/*/jobs/*/result.json`), not the trial-root `results.json`,
    which is arenabench's and carries the match/dataset/provenance instead.
    The two are one character apart in the name and hold different documents;
    reading the wrong one reports no exceptions at all on a run where ten
    trials threw.
    """
    found: list[str] = []
    stats = results.get("stats")
    evals = (stats or {}).get("evals") if isinstance(stats, dict) else None
    if not isinstance(evals, dict):
        return ()
    for entry in evals.values():
        if not isinstance(entry, dict):
            continue
        stats = entry.get("exception_stats")
        if not isinstance(stats, dict):
            continue
        for name, tasks in stats.items():
            if isinstance(tasks, list) and task_id in tasks:
                found.append(str(name))
    return tuple(sorted(set(found)))


def load_trial(run_id: str, run_root: Path, trial_uuid: str, task_dir: Path) -> Trial:
    trial = Trial(
        run_id=run_id,
        trial_uuid=trial_uuid,
        task_id=task_dir.name,
        root=task_dir,
        run_root=run_root,
        reward=_read_reward(task_dir),
        # The job directory is the task directory's parent.
        exception_types=_exception_types(
            _read_json(task_dir.parent / "result.json"), task_dir.name
        ),
    )
    exception_path = task_dir / "exception.txt"
    if exception_path.exists():
        trial.exception_text = exception_path.read_text(encoding="utf-8", errors="replace")

    events_path = trial.events_path
    if events_path.exists():
        starts: dict[str, tuple[dict[str, Any], int]] = {}
        pending: dict[str, ToolCall] = {}
        for lineno, event in iter_events(events_path):
            if event is None:
                trial.malformed_lines += 1
                continue
            trial.events.append(event)
            tag = event.get("type")
            if tag in _INDEXED:
                trial.by_type.setdefault(str(tag), []).append(event)
            if tag == "tool_start":
                call = event.get("call")
                if isinstance(call, dict) and call.get("call_id"):
                    starts[str(call["call_id"])] = (call, lineno)
            elif tag == "tool_result":
                call_id = str(event.get("call_id") or "")
                call, start_line = starts.pop(call_id, ({}, -1))
                trial.tool_calls.append(
                    ToolCall(
                        call_id=call_id,
                        name=str(call.get("name") or "?"),
                        input=call.get("input") if isinstance(call.get("input"), dict) else {},
                        result=event,
                        start_index=start_line,
                        result_index=lineno,
                    )
                )
        # A `tool_start` with no `tool_result` is a tool the process died
        # inside. It is evidence, so it is kept rather than dropped.
        for call_id, (call, start_line) in starts.items():
            pending[call_id] = ToolCall(
                call_id=call_id,
                name=str(call.get("name") or "?"),
                input=call.get("input") if isinstance(call.get("input"), dict) else {},
                result=None,
                start_index=start_line,
                result_index=None,
            )
        trial.tool_calls.extend(pending.values())
    return trial


def _declared_seat(results: dict[str, Any]) -> tuple[str, dict[str, str], str, str, bool]:
    """Pull the launched configuration out of a trial's `results.json`.

    This is the run's copy of what the match file asked for — the thing a
    `(role, model)` census is checked against. Reading it from the artifact
    rather than from a `match.toml` on someone's laptop is deliberate: the
    artifact is what the run actually carried.
    """
    match = results.get("match")
    if not isinstance(match, dict):
        return "", {}, "", "", False
    contestants = match.get("contestants")
    if not isinstance(contestants, list) or not contestants:
        return "", {}, "", str(match.get("name") or ""), False
    engine = contestants[0].get("engine") if isinstance(contestants[0], dict) else None
    if not isinstance(engine, dict):
        return "", {}, "", str(match.get("name") or ""), False
    roles: dict[str, str] = {}
    declared = engine.get("roles")
    if isinstance(declared, dict):
        for role, spec in declared.items():
            if isinstance(spec, dict) and spec.get("model"):
                roles[str(role)] = str(spec["model"])
    return (
        str(engine.get("qualified_model") or engine.get("model") or ""),
        roles,
        str(match.get("sut_ref") or ""),
        str(match.get("name") or ""),
        bool(engine.get("bare_loop")),
    )


def load_run(root: Path, run_id: str | None = None) -> Run:
    """Load every trial under a mirrored run directory.

    `root` is the directory holding the per-trial UUID directories — i.e. a
    local mirror of `runs/<run_id>/`. `run_id` defaults to the directory name,
    which is what `aws s3 sync` produces.
    """
    root = root.resolve()
    run = Run(run_id=run_id or root.name, root=root)
    for trial_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        results = _read_json(trial_dir / "results.json")
        if not run.declared_worker_model:
            worker, roles, sut_ref, name, bare = _declared_seat(results)
            if worker:
                run.declared_worker_model = worker
                run.declared_roles = roles
                run.sut_ref = sut_ref
                run.match_name = name
                run.bare_loop = bare
        for task_dir in sorted(trial_dir.glob("ws/matches/*/jobs/*/*")):
            if not task_dir.is_dir():
                continue
            if not (task_dir / "agent").exists() and not (task_dir / "verifier").exists():
                continue
            run.trials.append(load_trial(run.run_id, root, trial_dir.name, task_dir))
    return run
