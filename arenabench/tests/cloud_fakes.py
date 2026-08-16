# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""Recorded-call fakes and match fixtures the cloud witnesses share.

Not a test module: it holds the AWS fakes, the executor factory and the
match template that ``test_cloud.py`` (the submit contract and the pure
decisions) and ``test_cloud_commands.py`` (the ``_cmd_cloud_*`` CLI path)
both build on. The two were one file until it crossed the tree's 1500-line
ceiling; the fakes moved here rather than being duplicated or imported
test-module-to-test-module.

The fakes are small on purpose — moto would be a heavyweight dependency for
what is a handful of parameter-shape assertions.
"""

from __future__ import annotations

import argparse
import io
from types import SimpleNamespace

from arenabench.cloud import CloudExecutor
from arenabench.model import Contestant, Engine, MatchSpec

ACCOUNT = "123456789012"
BUCKET = f"arenabench-artifacts-{ACCOUNT}"
#: What ``infra/core.yaml``'s ``SutGitUrl`` defaults to, as the SUT build
#: project reports it back.
GIT_URL = "https://github.com/macanderson/stella.git"


# --------------------------------------------------------------------------
# recorded-call fakes — small on purpose; moto would be a heavyweight
# dependency for what is a handful of parameter-shape assertions
# --------------------------------------------------------------------------


class _NoSuchKeyError(Exception):
    pass


class FakeS3:
    def __init__(self, objects: dict[str, bytes] | None = None) -> None:
        self.objects: dict[str, bytes] = dict(objects or {})
        self.calls: list[tuple] = []
        # boto3 spells it NoSuchKey; the adapter catches it by that name.
        self.exceptions = SimpleNamespace(NoSuchKey=_NoSuchKeyError)

    # The keyword casing is the AWS SDK's, not PEP 8's: the adapter calls
    # with Bucket=/Key=/Body=, so the fakes must accept those spellings.
    def put_object(self, *, Bucket: str, Key: str, Body: bytes, **_: object) -> dict:  # noqa: N803
        self.calls.append(("put_object", Bucket, Key))
        self.objects[Key] = Body
        return {}

    def get_object(self, *, Bucket: str, Key: str) -> dict:  # noqa: N803
        self.calls.append(("get_object", Bucket, Key))
        if Key not in self.objects:
            raise _NoSuchKeyError(Key)
        return {"Body": io.BytesIO(self.objects[Key])}

    def list_objects_v2(self, **kwargs: object) -> dict:
        self.calls.append(("list_objects_v2", kwargs.get("Prefix")))
        prefix = str(kwargs.get("Prefix") or "")
        keys = sorted(k for k in self.objects if k.startswith(prefix))
        return {
            "Contents": [{"Key": k} for k in keys],
            "IsTruncated": False,
        }


class FakeBatch:
    """Records submissions; serves job states from a per-sweep script."""

    def __init__(self, state_script: list[dict[str, str]] | None = None) -> None:
        self.submitted: list[dict] = []
        self.describe_calls: list[list[str]] = []
        self.state_script = list(state_script or [])
        self._counter = 0

    def submit_job(self, **kwargs: object) -> dict:
        self.submitted.append(kwargs)
        self._counter += 1
        return {"jobId": f"job-{self._counter:03d}", "jobName": kwargs["jobName"]}

    def describe_jobs(self, *, jobs: list[str]) -> dict:
        self.describe_calls.append(list(jobs))
        states = self.state_script[0] if self.state_script else {}
        return {"jobs": [{"jobId": j, "status": states.get(j, "SUCCEEDED")} for j in jobs]}

    def advance(self) -> None:
        if len(self.state_script) > 1:
            self.state_script.pop(0)


class FakeSsm:
    def __init__(self, parameters: list[str]) -> None:
        self.parameters = parameters
        self.calls: list[dict] = []

    def get_parameters_by_path(self, **kwargs: object) -> dict:
        self.calls.append(dict(kwargs))
        # Two pages, to prove the paginator loop follows NextToken.
        first, rest = self.parameters[:1], self.parameters[1:]
        if "NextToken" not in kwargs:
            page = {"Parameters": [{"Name": n} for n in first]}
            if rest:
                page["NextToken"] = "page-2"
            return page
        return {"Parameters": [{"Name": n} for n in rest]}


class FakeCodeBuild:
    def __init__(self, statuses: list[str] | None = None) -> None:
        self.statuses = list(statuses or ["SUCCEEDED"])
        self.started: list[dict] = []
        self.project_calls: list[list[str]] = []
        self.polls = 0

    def start_build(self, **kwargs: object) -> dict:
        self.started.append(dict(kwargs))
        return {"build": {"id": "arenabench-sut-build:1234"}}

    def batch_get_builds(self, *, ids: list[str]) -> dict:
        status = self.statuses[min(self.polls, len(self.statuses) - 1)]
        self.polls += 1
        return {"builds": [{"id": ids[0], "buildStatus": status}]}

    def batch_get_projects(self, *, names: list[str]) -> dict:
        """The project's own ``GIT_URL`` — the remote a freshness check must
        consult, since it is the one the buildspec clones."""
        self.project_calls.append(list(names))
        return {
            "projects": [
                {
                    "name": names[0],
                    "environment": {
                        "environmentVariables": [
                            {"name": "GIT_URL", "value": GIT_URL},
                            {"name": "GIT_REF", "value": "main"},
                        ]
                    },
                }
            ]
        }


class FakeLsRemote:
    """``git ls-remote`` as a recorded call over a fixed remote ref table."""

    def __init__(self, refs: dict[str, str] | None = None) -> None:
        self.refs = dict(refs or {})
        self.calls: list[tuple[str, list[str]]] = []

    def __call__(self, url: str, patterns) -> str:
        patterns = list(patterns)
        self.calls.append((url, patterns))
        return "".join(
            f"{self.refs[p]}\t{p}\n" for p in patterns if p in self.refs
        )


def _refuse_ls_remote(url: str, patterns) -> str:
    """The seam a test uses to prove the remote is never consulted."""
    raise AssertionError(f"git ls-remote must not run: {url} {list(patterns)}")


class FakeSts:
    def get_caller_identity(self) -> dict:
        return {"Account": ACCOUNT}


class FakeDynamo:
    def __init__(self, pages: list[dict]) -> None:
        self.pages = list(pages)
        self.calls: list[dict] = []

    def query(self, **kwargs: object) -> dict:
        self.calls.append(dict(kwargs))
        return self.pages[min(len(self.calls) - 1, len(self.pages) - 1)]


def _executor(ls_remote=None, **clients: object) -> CloudExecutor:
    defaults: dict[str, object] = {"sts": FakeSts()}
    defaults.update(clients)
    return CloudExecutor(
        clients=defaults,
        ls_remote=ls_remote if ls_remote is not None else _refuse_ls_remote,
    )


# --------------------------------------------------------------------------
# spec helpers
# --------------------------------------------------------------------------


def _seat(seat_id: str, agent: str = "stella", api: str = "openrouter") -> Contestant:
    return Contestant(
        id=seat_id,
        name=seat_id,
        agent=agent,
        engine=Engine(api=api, model="z-ai/glm-5.2"),
        required_env=("OPENROUTER_API_KEY",),
    )


def _spec(
    tasks: tuple[str, ...] = ("task-a", "task-b"),
    seats: tuple[Contestant, ...] | None = None,
    attempts: int = 1,
) -> MatchSpec:
    return MatchSpec(
        id="m1",
        name="cloud match",
        dataset="terminal-bench-2.1",
        tasks=tasks,
        contestants=seats or (_seat("stella"), _seat("cc", agent="claude-code")),
        attempts=attempts,
    )


SUT = None  # filled per-test via resolve_sut or built directly


def _sut():
    from arenabench.cloud import SutBinary

    commit = "c" * 40
    return SutBinary(
        ref="main", commit=commit, uri=f"s3://{BUCKET}/binaries/main/{commit}/stella"
    )

TEMPLATE = """\
[match]
name = "cloud smoke"
dataset = "terminal-bench-2.1"
tasks = ["task-a"]

[[contestant]]
id = "stella"
agent = "stella"

  [contestant.engine]
  api = "openrouter"
  model = "z-ai/glm-5.2"

  [contestant.env]
  required = ["OPENROUTER_API_KEY"]
"""


def _run_args(template, **overrides) -> argparse.Namespace:
    defaults = dict(
        template=str(template),
        ref=None,
        run_id="r-test",
        burst=False,
        poll=0.0,
        vcpus=4,
        memory_mb=15360,
        no_wait=True,
        no_fetch=True,
        artifacts=False,
        out=None,
        region=None,
        bucket=None,
        # These tests are about submission, not about the banned-behavior
        # gate, so they take the explicit waiver — `_cmd_cloud_run` refuses to
        # start a run that is neither gated nor deliberately ungated, and that
        # refusal has its own witness in tests/test_gate.py.
        gate=None,
        no_gate=True,
        gate_allow=None,
    )
    defaults.update(overrides)
    return argparse.Namespace(**defaults)
