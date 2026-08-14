# SPDX-License-Identifier: Apache-2.0
# Copyright (c) 2026 the ArenaBench authors
"""``arenabench`` command line.

The web UI is the product for exploring; the CLI is how a match becomes
repeatable. ``arenabench run match.toml`` executes a committed template with no
browser involved, which is what CI needs — the same match, the same way, every
time, with credentials coming from the environment rather than the file.
"""

from __future__ import annotations

import argparse
import json
import logging
import random
import subprocess
import sys
import textwrap
from pathlib import Path
from typing import TYPE_CHECKING

from . import gate, provenance
from .agents import AGENTS
from .recorder import IMAGE_TAG, build_image, docker_available, image_present
from .registry import DEFAULT_REGISTRY, export_root, sample_tasks
from .replay import ReplayError, replay_flip
from .server import default_workspace, serve
from .telemetry import EVENTS_NAME, MetricsReader, TrialMetrics, aggregate

if TYPE_CHECKING:  # the harness reader is imported lazily, by command
    from .harness import SeatBehaviour

__all__ = ["main"]


def _configure_logging(verbose: bool) -> None:
    logging.basicConfig(
        level=logging.DEBUG if verbose else logging.INFO,
        format="%(asctime)s %(levelname)-7s %(name)s  %(message)s",
        datefmt="%H:%M:%S",
    )


def _watch_line(match_id: str, watch_port: int | None) -> str:
    """Where to watch a started match, as the operator-facing ``watch`` line.

    A pure function of the two facts that were each wrong when this was
    inlined in :func:`_cmd_run`, and that nothing executed until an operator
    ran a real match:

    * **which id** — the CLI read ``match.id``, which :class:`~.runner.Match`
      does not define (it holds the spec rather than copying its identity).
      The ``AttributeError`` landed *after* ``runner.start``, so every
      ``arenabench run`` launched its trials and then died on the line telling
      the operator where to watch them: harbor kept running unsupervised, no
      ``--results`` snapshot was written, and the arena reported the match
      ``finished`` while its container was still up.
    * **which URL** — the client is one static export whose only routes are
      ``/``, ``/_not-found`` and ``/transcript``, and it selects a match from
      the ``match`` query parameter. A ``/matches/<id>`` path falls through to
      static file serving and 404s.

    ``watch_port`` is already resolved by the caller so this stays free of
    I/O — a string an operator pastes into a browser is worth being able to
    assert on directly.
    """
    if watch_port is None:
        return f"arenabench serve   (then open match {match_id})"
    return f"http://127.0.0.1:{watch_port}/?match={match_id}"


def _cmd_run(args: argparse.Namespace) -> int:
    """Run a committed ``arenabench.toml`` headlessly — the CI entry point.

    Credentials are read from the process environment against each seat's
    declared ``required`` list, never from the file — through
    :func:`~.credentials.apply_ambient_credentials`, the same function the
    server calls, so "which ambient names may a seat receive" has exactly one
    implementation (#2654) — then from the saved credential set at
    :func:`~.credentials.credentials_path`, and finally, for
    a seat that declared the Claude subscription token, from this machine's own
    Claude Code login (:func:`~.claude_oauth.apply_local_claude_login`). So a
    local, repeat ``arenabench run`` does not need the same ``export`` lines
    pasted before every invocation. A seat's own environment always wins over
    every fill layer. A candidate still missing from all of them aborts before
    any container starts, because an unauthenticated arm scores zero and a zero
    is indistinguishable from a real result on a scoreboard.

    The layers are the server's, in the server's order, on purpose: a template
    that launches from the UI and the same template run here must resolve the
    same credentials, or "reproduces this match exactly" is false in the one
    place nobody checks.
    """
    import os
    import time

    from .agents import dead_seat_reason
    from .config import MatchTemplateError, load_match, required_env
    from .credentials import (
        credentials_path,
        load_credentials,
        resolve_launch_credentials,
    )
    from .monitor import MatchWatcher
    from .runner import MatchRunner
    from .server import ArenaServer, find_running_arena

    try:
        spec = load_match(Path(args.template).expanduser())
    except MatchTemplateError as exc:
        for problem in exc.problems:
            print(f"error: {problem}", file=sys.stderr)
        return 2

    needed = required_env(spec)
    saved = load_credentials()
    # Every fill layer, through the one implementation the server also calls.
    # This path used to inline its own ambient fill over `required_env`'s
    # *derived* candidate set, which for an undeclared seat includes the
    # agent's `alt_credential_env` — so a `CLAUDE_CODE_OAUTH_TOKEN` exported in
    # the operator's shell silently credentialled an undeclared `claude-code`
    # seat here and nothing through the UI (#2654). Two arms you believed were
    # on different credentials, both on one plan, with only the entry point to
    # tell them apart. The *refusal* below still reads the derived set: #1827
    # keeps those two decisions separate on purpose.
    spec, oauth_notes = resolve_launch_credentials(spec)
    if saved:
        filled = sum(
            1
            for contestant in spec.contestants
            for name in needed.get(contestant.id, [])
            if name in saved and not os.environ.get(name)
        )
        if filled:
            print(f"credentials: {filled} value(s) filled in from {credentials_path()}")
    for note in oauth_notes:
        print(f"credentials: {note}")

    missing: list[str] = []
    for contestant in spec.contestants:
        candidates = needed.get(contestant.id, [])
        # The candidate names are alternatives, not a conjunction: a seat
        # carrying any one of them is credentialled (a Claude subscription
        # token authenticates a seat that has no ANTHROPIC_API_KEY at all).
        if not candidates or contestant.env or args.allow_missing_env:
            continue
        if contestant.required_env:
            missing.append(f"{contestant.name}: any of {', '.join(candidates)}")
        else:
            # An undeclared seat is refused with the fix that will actually
            # work for it. Since the ambient fill became declaration-only
            # (#2654), exporting one of these names no longer reaches this
            # seat — naming them as if it would is the misdirection that turns
            # a five-second fix into an afternoon.
            names = ", ".join(candidates)
            missing.append(
                f"{contestant.name}: declares no [contestant.env] required, so "
                "an exported variable cannot reach it — add "
                f"[contestant.env] required = [...] naming any of {names}, or "
                f"save the value at {credentials_path()}"
            )
    if missing:
        print("error: required credentials are not set:", file=sys.stderr)
        for line in missing:
            print(f"  {line}", file=sys.stderr)
        print("  (use --allow-missing-env to run anyway)", file=sys.stderr)
        return 2

    # A seat can hold a perfectly valid credential its CLI has no way to read
    # — measured as eleven trials dying at turn one, each scoring a 0.0
    # indistinguishable from a real loss. No `--allow-missing-env` escape
    # here: that flag says "run uncredentialled on purpose", while this is a
    # seat that believes it *is* credentialled and is wrong.
    dead = [
        f"{contestant.name}: {reason}"
        for contestant in spec.contestants
        if (reason := dead_seat_reason(contestant))
    ]
    if dead:
        print("error: seat(s) cannot authenticate as configured:", file=sys.stderr)
        for line in dead:
            print(f"  {line}", file=sys.stderr)
        return 2

    # One minimal request on the subscription route before any container
    # exists: an exhausted five-hour limit turned 4 of 6 head-to-head trials
    # into quota measurements, and the only preflight was a hand-rolled
    # script one layer too late (#3216). Fail closed, like the SUT check
    # below; the refusal names the reset time when the provider does.
    from .claude_oauth import seat_quota_refusals

    throttled = seat_quota_refusals(spec)
    if throttled:
        print("error: subscription seat(s) cannot serve right now:", file=sys.stderr)
        for name, reason in throttled.items():
            print(f"  {name}: {reason}", file=sys.stderr)
        return 2

    # The same SUT preflight the server runs in `create_match`, which this
    # path never consulted: #2098's reproduction entered exactly here, a
    # template whose default `main` had moved since the build, and the match
    # launched anyway. Refusing before any container starts is the contract.
    from .sut import sut_problem_for

    sut_problem = sut_problem_for(spec)
    if sut_problem:
        print("error: the match cannot run the Stella it declares:", file=sys.stderr)
        print(f"  {sut_problem}", file=sys.stderr)
        return 2

    workspace = Path(args.workspace).expanduser()
    arena = ArenaServer(workspace)
    runner: MatchRunner = arena.runner

    print(f"match     : {spec.name}")
    print(f"dataset   : {spec.dataset}")
    print(f"tasks     : {len(spec.tasks) or 'all'}")
    for contestant in spec.contestants:
        print(f"  {contestant.name:24} {contestant.agent:14} {contestant.engine.label}")

    match = runner.create(spec)
    runner.start(match)

    # Name the arena already serving this workspace, if there is one. `run` is
    # headless by contract and must not start a server, but it knows the match
    # id and the operator's next question is always "where do I watch it" —
    # answered previously by starting another arena and forgetting the port.
    #
    # The id is `spec.id`, not `match.id`: `Match` holds the spec rather than
    # copying its identity, and the server reads it the same way (its match
    # list emits `match.spec.id`).
    watch_port = find_running_arena(workspace, "127.0.0.1")
    print(f"watch     : {_watch_line(match.spec.id, watch_port)}")
    # The built-in supervisor: the same rules `arenabench watch` runs from
    # another process, reported inline so a CI log shows a dead arm at the
    # minute it died rather than in the postmortem (#1480).
    watcher = MatchWatcher(match.workspace)
    while match.status == "running":
        time.sleep(args.poll)
        for detection in watcher.scan():
            print(f"  !! {detection.to_line()}")
        if args.progress:
            snapshot = match.snapshot()
            parts = [
                f"{c['name']}={c['totals']['passed']}/{c['totals']['judged']}"
                for c in snapshot["contestants"]
            ]
            print(f"  [{snapshot['elapsed'] / 60:5.1f}m] " + "  ".join(parts))
    for detection in watcher.scan():
        print(f"  !! {detection.to_line()}")

    snapshot = match.snapshot()
    print(f"\nfinished in {snapshot['elapsed'] / 60:.1f}m")
    # Printed above the numbers, not below them, because it changes what every
    # number underneath means. A seeded arm had already seen these tasks, so
    # its solve rate is a measurement of self-improvement and not of the agent
    # — see the lineage section of `arenabench.provenance`.
    if match.provenance is not None and match.provenance.contaminated:
        print(
            f"  !! SEEDED RUN ({match.provenance.lineage_label}) — these agents "
            "had seen these tasks before. Not comparable with a clean run; "
            "never average the two."
        )
    worst = 0
    for entry in snapshot["contestants"]:
        totals = entry["totals"]
        # The comparable figure first, and the agent's own number labelled as
        # its own: the two are computed from different tables, so printing them
        # in one unlabelled column would be subtracting one from the other.
        priced = totals["priced_cost"]
        cost = f"${priced:.2f}" if priced is not None else "unpriced"
        print(
            f"  {entry['name']:24} {totals['solve_rate']:5.1f}%  "
            f"({totals['passed']}/{totals['judged']})  {cost:>9}  "
            f"(self-reported ${totals['total_cost']:.2f})"
        )
        # A blank cost column is a finding, not a formatting detail: it went
        # unremarked across every kimi match because "unpriced" named nothing
        # an operator could go fix (#2108). Naming the slug turns it into a
        # one-line edit to `pricing.PRICES`.
        for slug in totals.get("unpriced_models") or ():
            print(f"    !! no pricing entry for {slug} — priced cost unavailable")
        # The other way a total goes short: trials nothing measured. Their
        # tokens and cost read 0 in every figure on the line above, which is
        # the absence of a measurement and not a measurement of zero (#2132).
        # Printed with the denominator so the line says what it is a total of.
        unmeasured = totals.get("unmeasured_trials") or 0
        if unmeasured:
            print(
                f"    !! {unmeasured}/{totals['trials']} trials published no usage "
                "— the token and cost totals above are over the rest"
            )
        if totals["judged"] == 0:
            worst = 1
    if args.results:
        Path(args.results).expanduser().write_text(
            json.dumps(snapshot, indent=2), encoding="utf-8"
        )
        print(f"  results -> {args.results}")
    if watcher.invalidating:
        # Same contract as `arenabench watch`: exit 3 means these numbers
        # must not be published, which outranks "an arm judged nothing".
        print("match invalidated — see the !! lines above", file=sys.stderr)
        return 3
    return worst


def _cmd_contest(args: argparse.Namespace) -> int:
    """``arenabench contest`` — build a head-to-head match, optionally submit it.

    Writes the ``arenabench.toml`` *first*, always, even when submitting: the
    file is the record of what was measured, and a run whose configuration
    exists only inside the process that launched it cannot be reproduced or
    audited afterwards.
    """
    from .config import dump_match
    from .presets import ContestError, contest_comparability_note, contest_spec

    try:
        spec = contest_spec(
            args.versus_model,
            num_tasks=args.num_tasks,
            concurrency=args.concurrency,
            attempts=args.attempts,
            record_video=not args.no_video,
            name=args.name,
        )
    except ContestError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    destination = Path(args.output or f"contest-{args.num_tasks}task.toml")
    destination.write_text(dump_match(spec), encoding="utf-8")
    print(f"match     : {destination}")
    print(f"tasks     : {len(spec.tasks)}  attempts: {spec.attempts}")
    note = contest_comparability_note(args.num_tasks)
    if note:
        print(f"WARNING   : {note}")

    if not args.submit:
        print("\nnot submitted. Run it locally with:")
        print(f"  arenabench run {destination}")
        print("…or on AWS Batch (one job per trial) with:")
        print(f"  arenabench cloud run {destination} --ref main")
        return 0

    # Named before the run starts, not after: the watcher is worth opening
    # while trials are queueing, and a run id printed only on completion is a
    # run nobody watched.
    if args.run_id:
        print(f"\nwatch the scoreboard while it runs:")
        print(f"  arenabench cloud watch {args.run_id}")
    else:
        print("\nwatch the scoreboard with `arenabench cloud watch <run-id>` "
              "(the id is printed below)")

    # Hand off rather than re-implement: `cloud run` owns SUT resolution,
    # per-trial submission, the refusal of seats with no SSM credentials, and
    # result download. A second submission path here would be a second thing
    # to keep correct.
    from .cloud import _cmd_cloud_run

    cloud_args = argparse.Namespace(
        template=str(destination),
        ref=args.ref,
        run_id=args.run_id,
        burst=args.burst,
        poll=args.poll,
        vcpus=args.vcpus,
        memory_mb=args.memory_mb,
        no_wait=args.no_wait,
        no_fetch=False,
        artifacts=False,
        out=None,
        region=args.region,
        bucket=args.bucket,
        # The banned-behavior gate's three `cloud run` flags. Built by hand
        # here because this namespace is, so an attribute added to the `run`
        # parser and not added here is an AttributeError at submit time —
        # after the contest TOML has already been written.
        gate=args.gate,
        no_gate=args.no_gate,
        gate_allow=args.gate_allow,
    )
    return _cmd_cloud_run(cloud_args)


def _cmd_template(args: argparse.Namespace) -> int:
    """Print a starter ``arenabench.toml`` to stdout or a file."""
    from .config import dump_match
    from .model import MatchSpec

    spec = MatchSpec.from_json(
        {
            "name": args.name,
            "dataset": args.dataset,
            "tasks": [],
            "attempts": 1,
            "concurrency": 1,
            "record_video": False,
            "contestants": [
                {
                    "id": "stella",
                    "name": "Stella",
                    "agent": "stella",
                    "engine": {
                        "api": "openrouter",
                        "model": "z-ai/glm-5.2",
                        "effort": "medium",
                        "roles": {
                            "judge": {"model": "openai/gpt-5.5", "effort": "xhigh"},
                            "triage": {"model": "z-ai/glm-4.7-flash", "effort": "low"},
                        },
                    },
                },
                {
                    "id": "claude-code",
                    "name": "Claude Code",
                    "agent": "claude-code",
                    "engine": {
                        "api": "openrouter",
                        "model": "z-ai/glm-5.2",
                        "effort": "medium",
                        "base_url": "https://openrouter.ai/api/v1",
                    },
                },
            ],
        }
    )
    text = dump_match(spec)
    if args.output:
        Path(args.output).expanduser().write_text(text, encoding="utf-8")
        print(f"wrote {args.output}")
    else:
        print(text, end="")
    return 0


def _cmd_serve(args: argparse.Namespace) -> int:
    try:
        serve(
            workspace=Path(args.workspace).expanduser(),
            host=args.host,
            port=args.port,
            open_browser=not args.no_browser,
            allow_remote=args.allow_remote,
            reuse=not args.no_reuse,
        )
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    return 0


def _cmd_datasets(args: argparse.Namespace) -> int:
    for dataset in DEFAULT_REGISTRY.datasets.values():
        tasks = DEFAULT_REGISTRY.tasks(dataset.key)
        state = f"{len(tasks)} tasks" if tasks else "not fetched"
        print(f"{dataset.key:22} {state:>14}   {dataset.title}")
        print(f"{'':22} {'':>14}   {dataset.harbor_id}")
        if not tasks:
            print(f"{'':22} {'':>14}   fetch: harbor download {dataset.harbor_id}")
    return 0


def _cmd_harness(args: argparse.Namespace) -> int:
    """What each arm's harness disclosed, and how it spent its turn.

    The half of a head-to-head that a solve rate cannot express. Two agents at
    5/8 that reached it with 22 shell calls and with 7 reads plus an edit did
    not do the same thing, and neither did two arms whose tool rosters, memory
    files or permission modes differed while a match description said they were
    "the same configuration". This prints the evidence rather than the claim.

    One block per arm, in one layout, from each arm's own stream — Stella's
    event journal and Claude Code's stream-json tee are two spellings of one
    vocabulary (:class:`arenabench.harness.HarnessDialect`), so the two are
    read side by side rather than in two commands (#2519). Where the
    vocabularies genuinely differ the block says so: Stella's ``steps`` counts
    one model call per pipeline role and Claude Code's counts one assistant
    turn, and a table that puts those in one column is the sort of plausible
    number this repository treats as worse than a crash.

    Reads only artifacts, like everything else here, so it works the same on a
    live match and on an archive — and running it cannot perturb what it reads.
    """
    from .harness import HarnessReading, StreamReader, summarize

    match_dir = Path(args.workspace).expanduser() / "matches" / args.match
    if not match_dir.is_dir():
        print(f"no such match: {match_dir}")
        return 1

    reader = StreamReader()
    #: (job, arm) → that arm's reading of every trial in the job. Keyed by arm
    #: as well as by job because "which arm is this" is a fact about the files,
    #: not about the directory's name.
    seats: dict[tuple[str, str], list[HarnessReading]] = {}
    for job_dir in sorted((match_dir / "jobs").glob("*")):
        if not job_dir.is_dir():
            continue
        for trial_dir in sorted(job_dir.glob("*")):
            for reading in reader.read_trial(trial_dir):
                seats.setdefault((job_dir.name, reading.dialect.arm), []).append(reading)

    if not seats:
        print(
            f"{args.match}: no arm published a harness stream.\n"
            "An arm is read from its own append-only journal — Stella's "
            "`agent/stella-events.jsonl` or a stream-json CLI's stdout tee. A "
            "match with neither has only its ATIF trajectories; see "
            "`arenabench series`."
        )
        return 0

    for (name, arm), readings in sorted(seats.items()):
        seat = summarize(readings)
        if seat is None:  # pragma: no cover - `seats` only holds non-empty lists
            continue
        totals, unpublished = seat.totals, seat.dialect.unpublished
        print(f"\n=== {name}  ({seat.trials} trial{'s' if seat.trials != 1 else ''} · {arm})")
        _print_harness_profile(seat)
        calls = totals.get("tool_calls") or {}
        elapsed = totals.get("tool_ms") or {}
        print(
            f"  turns          {totals.get('steps', 0)} model steps, "
            f"{totals.get('tools', 0)} tool calls, "
            f"{totals.get('tool_errors', 0)} of them errors"
        )
        # The vocabulary note, printed for every arm rather than only for the
        # odd one out: a reader comparing two blocks needs both definitions in
        # front of them, and only one of them being stated invites the reading
        # that the unstated one is the default.
        _print_harness_note(f"a step here is {seat.dialect.steps_are}")
        if calls:
            print("  tool mix       " + ", ".join(
                f"{tool}x{n}"
                + (f" ({elapsed[tool] / 1000:.0f}s)" if tool in elapsed else "")
                for tool, n in sorted(calls.items(), key=lambda kv: -kv[1])
            ))
        _print_harness_spend(seat)
        clock = []
        if "duration_ms" not in unpublished:
            clock.append(f"{totals.get('duration_ms', 0) / 1000:.0f}s wall")
        if "api_ms" not in unpublished:
            clock.append(f"{totals.get('api_ms', 0) / 1000:.0f}s in provider calls")
        clock.append(f"{totals.get('tool_wall_ms', 0) / 1000:.0f}s in tools")
        # Deliberately not subtracted into an "overhead" figure: both harnesses
        # overlap tools with provider calls — Harbor forces background tasks on
        # for Claude Code, and Stella speculates read-only calls — so the
        # difference goes negative on real trials.
        print("  clock          " + ", ".join(clock))
        if unpublished:
            # Named, not omitted. A column this arm's stream never carries is
            # a gap in the apparatus, and a reader who cannot see the list
            # reads the arm as having scored zero on every one of them.
            _print_harness_note(", ".join(sorted(unpublished)), label="not published")
            _print_harness_note("(absent from this arm's stream — not zeros)")
    print(
        "\nCost is the agent's own figure and is never compared across seats — "
        "each vendor prices from its own table. `arenabench series` recomputes "
        "both arms at one rate."
    )
    print(
        "Read each block's step note before putting two arms' steps in one "
        "column: they are different units."
    )
    return 0


#: Where a harness block's values start, in columns. The label column is
#: 15 wide after a two-space indent, which is what every row here has always
#: used; a wrapped continuation lines up under the value rather than under the
#: label, so a long note reads as one field instead of as several.
_HARNESS_LABEL = 15
_HARNESS_INDENT = " " * (2 + _HARNESS_LABEL)


def _print_harness_note(text: str, label: str = "") -> None:
    """One field of a harness block, wrapped into the value column."""
    lines = textwrap.wrap(text, width=96 - len(_HARNESS_INDENT)) or [""]
    print(("  " + label.ljust(_HARNESS_LABEL) if label else _HARNESS_INDENT) + lines[0])
    for line in lines[1:]:
        print(_HARNESS_INDENT + line)


def _print_harness_profile(seat: SeatBehaviour) -> None:
    """The boot disclosure half of one arm's block.

    An arm whose stream carries no self-description gets one line saying so
    rather than a column of `?`: the absence is a property of that harness, and
    the SUT commit in `provenance.sut_seats` is what identifies it instead.
    """
    if not seat.dialect.boot_disclosure:
        _print_harness_note(
            "not disclosed on this arm's stream — the SUT commit identifies it "
            "(`arenabench provenance`)",
            label="boot",
        )
        models = seat.totals.get("models") or []
        _print_harness_note(", ".join(models) if models else "?", label="models")
        return
    profile = seat.profile
    print(f"  product        {profile.version or '?'}")
    print(f"  model          {profile.model or '?'}")
    print(f"  permission     {profile.permission_mode or '?'}")
    print(f"  credential     {profile.api_key_source or '?'}")
    print(f"  tools offered  {len(profile.tools)}  ({', '.join(profile.tools)})")
    if profile.mcp_servers:
        print(f"  mcp servers    {', '.join(profile.mcp_servers)}")
    if profile.skills:
        print(f"  skills         {len(profile.skills)}")
    if profile.subagents:
        print(f"  subagents      {', '.join(profile.subagents)}")


def _print_harness_spend(seat: SeatBehaviour) -> None:
    """The token half of one arm's block, or an honest statement of its absence.

    Three outcomes, never two. A seat routed through `ANTHROPIC_BASE_URL` to a
    non-Anthropic gateway streams `{"input_tokens": 0, "output_tokens": 0}` on
    every message while its own session transcript carries the real figures, so
    the reader falls back to the transcript and says which source it used
    (#2520). Only when *neither* source has published anything is the column
    unknown — and unknown is printed as unknown, because a `0` beside a nonzero
    self-reported cost reads as an efficiency claim.
    """
    from .harness import USAGE_STREAM, USAGE_TRANSCRIPT

    totals, unpublished = seat.totals, seat.dialect.unpublished
    reasoning = (
        [] if "thinking_tokens" in unpublished
        else [f"{totals.get('thinking_tokens', 0)} reasoning (est.)"]
    )
    cost = f"${totals.get('total_cost', 0.0):.4f} self-reported"
    measured = {USAGE_STREAM, USAGE_TRANSCRIPT} & set(seat.usage_sources)
    if not measured:
        _print_harness_note(
            "stream reported no usage (gateway route) and no session transcript "
            "carried any either",
            label="spend",
        )
        _print_harness_note(", ".join(["unknown, not zero", *reasoning, cost]))
        _print_harness_note(
            "token counts for this arm arrive with the ATIF trajectory at "
            "teardown — see `arenabench series`"
        )
        return
    _print_harness_note(
        ", ".join(
            [
                f"{totals.get('tokens_out', 0)} out",
                f"{totals.get('prompt_tokens', 0)} in "
                f"({totals.get('cache_read', 0)} from cache)",
                *reasoning,
                cost,
            ]
        ),
        label="spend",
    )
    if USAGE_TRANSCRIPT in seat.usage_sources:
        _print_harness_note(
            "read from the session transcript: this seat's streamed counters "
            "are zeroed by its gateway route"
        )


def _cmd_provenance(args: argparse.Namespace) -> int:
    """Show — and with ``--migrate``, backfill — what each match was measured with.

    The listing groups by comparability key rather than by date, because the
    question this answers is never "what ran when": it is "which of these
    numbers may I put in the same table". Matches sharing a key may be
    compared; matches with different keys are different experiments that
    happen to live in the same directory.
    """
    root = Path(args.workspace).expanduser() / "matches"
    if not root.is_dir():
        print(f"no matches under {root}")
        return 0

    rows: list[tuple[str, str, str]] = []
    backfilled = 0
    blended: list[tuple[str, str, list[str]]] = []
    violated: list[tuple[str, str, list[str]]] = []
    for match_dir in sorted(p for p in root.iterdir() if p.is_dir()):
        record = provenance.read_match(match_dir)
        state = "recorded"
        if record is None:
            rebuilt = provenance.backfill_match(match_dir)
            if rebuilt is None:
                rows.append((match_dir.name, "—", "no jobs on disk"))
                continue
            if args.migrate:
                provenance.write_match(match_dir, rebuilt)
                backfilled += 1
                state = "backfilled"
            else:
                state = "would backfill"
            record = rebuilt
        elif record.source == provenance.SOURCE_BACKFILLED:
            state = "backfilled"
        elif args.migrate:
            # A recorded match still learns its opponent's version late: the
            # trials had not run when the record was written.
            stamped = provenance.stamp_agent_versions(match_dir)
            if stamped is not None:
                record = stamped
        for seat_id in record.mixed_version_seats:
            blended.append(
                (
                    match_dir.name,
                    seat_id,
                    [str(v) for v in record.agent_seats[seat_id].get("versions") or []],
                )
            )
        for seat_id, ran in record.violated_pins.items():
            violated.append((match_dir.name, seat_id, ran))
        rows.append((match_dir.name, record.comparability_key, state))

    width = max((len(key) for _, key, _ in rows), default=20)
    for name, key, state in rows:
        print(f"{name:14} {key:<{width}}  {state}")

    # The headline: how many distinct experiments are sitting in here. One key
    # means everything is comparable; more than one means a table built from
    # all of it would be mixing apparatus, which is the thing worth saying out
    # loud rather than leaving to be noticed.
    keys = {key for _, key, _ in rows if key != "—"}
    print()
    if len(keys) <= 1:
        print(f"{len(rows)} matches, one comparability key — safe to compare.")
    else:
        print(f"{len(rows)} matches across {len(keys)} comparability keys:")
        for key in sorted(keys):
            count = sum(1 for _, k, _ in rows if k == key)
            print(f"  {count:>3}  {key}")
        print("Numbers from different keys are different experiments. Label, never sum.")

    # Louder than the key listing, because a blended arm is not a different
    # experiment sitting tidily beside the others — it is one number that is
    # secretly two, inside a single match nobody has any reason to distrust.
    if blended:
        print(f"\n{len(blended)} arm(s) ran more than one product version:")
        for match_name, seat_id, versions in blended:
            print(f"  {match_name:14} {seat_id:<28} {' + '.join(versions)}")
        print(
            "Such an arm's numbers are a blend of those releases, not a "
            "measurement of any one of them. Pin the seat's `agent_version` "
            "to make the next run comparable."
        )
    if violated:
        print(f"\n{len(violated)} pinned arm(s) ran something else:")
        for match_name, seat_id, ran in violated:
            print(f"  {match_name:14} {seat_id:<28} actually ran {', '.join(ran)}")
        print("The installer did not honour the pin; the key names what ran.")

    if args.migrate and backfilled:
        print(f"\nbackfilled {backfilled} match(es).")
    elif not args.migrate and any(state == "would backfill" for _, _, state in rows):
        print("\nre-run with --migrate to write the reconstructed records.")
    return 0


def _cmd_tasks(args: argparse.Namespace) -> int:
    tasks = DEFAULT_REGISTRY.tasks(args.dataset)
    if not tasks:
        dataset = DEFAULT_REGISTRY.get(args.dataset)
        target = dataset.harbor_id if dataset else args.dataset
        print(f"no tasks on disk; fetch with: harbor download {target}", file=sys.stderr)
        return 1
    total = len(tasks)
    seed = args.seed
    if args.random:
        # An unseeded draw still gets a seed, and prints it. Otherwise the one
        # thing you need to run this slice again is the one thing you no
        # longer have.
        if seed is None:
            seed = random.randrange(1, 2**31)
        tasks = sample_tasks(
            tasks,
            args.random,
            seed,
            exclude_heavy=args.exclude_heavy,
            max_memory_mb=args.max_memory_mb,
        )

    for task in tasks:
        heavy = " [heavy]" if task.heavy else ""
        print(f"{task.name:38} {task.difficulty:8} {task.category}{heavy}")

    if args.random:
        # Report the size of the pool actually drawn from, not the dataset. A
        # filtered draw is a sample of a smaller population, and "10 of 89"
        # would quietly claim otherwise.
        drawn_from = len(
            sample_tasks(
                DEFAULT_REGISTRY.tasks(args.dataset),
                total,
                seed,
                exclude_heavy=args.exclude_heavy,
                max_memory_mb=args.max_memory_mb,
            )
        )
        narrowed = "" if drawn_from == total else f" (of {total} in the dataset)"
        print(
            f"\n{len(tasks)} drawn from a pool of {drawn_from}{narrowed} — "
            f"reproduce with --random {args.random} --seed {seed}"
            + (" --exclude-heavy" if args.exclude_heavy else "")
            + (f" --max-memory-mb {args.max_memory_mb}" if args.max_memory_mb else ""),
            file=sys.stderr,
        )
    else:
        print(f"\n{total} tasks", file=sys.stderr)
    return 0


def _cmd_export(args: argparse.Namespace) -> int:
    """Materialise a dataset for offline running.

    Worth its own verb because it is the difference between a match that
    finishes and one that dies at task three. Harbor resolves a registry ref
    per task, at run time; an export resolves the digest once, here.
    """
    dataset = DEFAULT_REGISTRY.get(args.dataset)
    if dataset is None:
        print(f"unknown dataset: {args.dataset}", file=sys.stderr)
        return 1
    dest = Path(args.output).expanduser() if args.output else export_root() / dataset.key
    print(f"exporting {dataset.harbor_id}\n       to {dest}")
    try:
        DEFAULT_REGISTRY.fetch(dataset.key, dest=dest)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        print(f"export failed: {exc}", file=sys.stderr)
        return 1
    run_path = DEFAULT_REGISTRY.local_run_path(dataset.key)
    if run_path is None:
        print("export finished but no runnable task tree appeared", file=sys.stderr)
        return 1
    print(f"ready: {run_path}")
    return 0


def _cmd_agents(args: argparse.Namespace) -> int:
    for spec in AGENTS.values():
        kind = "built-in" if spec.harbor_agent and not spec.import_path else "arenabench"
        knobs = ",".join(sorted(spec.honours))
        print(f"{spec.slug:18} {kind:11} honours: {knobs}")
    return 0


def _task_dir_for(trial_dir: Path) -> Path | None:
    """Where the task this trial ran lives, read back from its own result.

    Harbor records the absolute path it resolved the task from, so a trial
    carries the location of its own verifier and the operator does not have to
    reconstruct which export a months-old run used.
    """
    try:
        raw = json.loads((trial_dir / "result.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    path = ((raw.get("task_id") or {}) if isinstance(raw, dict) else {}).get("path")
    return Path(str(path)) if path else None


#: Grade -> the one-word reading, for the terminal's narrow column.
_GRADE_GLYPH: dict[str, str] = {
    "flip": "flip",
    "oracle": "oracle",
    "refuted": "refuted",
    "unproven": "unproven",
    "waived": "waived",
    "unwarranted": "not due",
    "model": "model",
    "heuristic": "heuristic",
    "none": "-",
}


def _cmd_proof(args: argparse.Namespace) -> int:
    """Print the proof rail of every trial under a path.

    The terminal twin of the web UI's proof column, and the surface that
    actually gets used on the benchmark rig, which has no browser. Accepts a
    match directory, a job directory or a single trial: everything containing
    a Stella event stream underneath it is read.
    """
    root = Path(args.path).expanduser().resolve()
    if not root.is_dir():
        print(f"no such directory: {root}", file=sys.stderr)
        return 1

    reader = MetricsReader()
    trials: list[tuple[str, str, TrialMetrics]] = []
    for events in sorted(root.rglob(EVENTS_NAME)):
        trial_dir = events.parent.parent
        metrics = reader.read(trial_dir, trial_dir.name)
        if metrics.proof is not None:
            # A match directory holds one job per seat, and a seat is what a
            # reader is comparing. Without it the trials of two seats
            # interleave under task names that look like duplicates.
            trials.append((trial_dir.parent.name, trial_dir.name, metrics))

    if not trials:
        print(f"no Stella event stream under {root}", file=sys.stderr)
        return 1

    # Only shown when the path spans more than one seat: for a single job it
    # is the same string on every row.
    seats = {job for job, _, _ in trials}
    trials.sort(key=lambda row: (row[0], row[1]))
    width = min(46, max(len(name) for _, name, _ in trials))
    seat_width = min(28, max(len(job) for job in seats)) if len(seats) > 1 else 0
    seat_head = f"{'seat':<{seat_width}}  " if seat_width else ""
    print(
        f"{seat_head}{'trial':<{width}}  {'grade':<10} {'witness':<8} "
        f"{'oracle':<10} wrong  rung"
    )
    for job, name, metrics in trials:
        proof = metrics.proof
        assert proof is not None  # filtered above
        # `+` passed, `-` failed, in the order observed. "never ran" gets its
        # own token: a lone `-` would otherwise read as one failing run, and
        # an oracle that never ran is the absence of the measurement rather
        # than a failing one.
        strip = "".join("+" if run.passed else "-" for run in proof.oracle_runs)
        strip = strip or "(none)"
        seat_cell = f"{job[:seat_width]:<{seat_width}}  " if seat_width else ""
        print(
            f"{seat_cell}{name[:width]:<{width}}  "
            f"{_GRADE_GLYPH.get(proof.grade, proof.grade):<10} "
            f"{'yes' if proof.witness_authored else 'no':<8} {strip[:10]:<10} "
            f"{proof.failed_attempts:<5}  {proof.rung or '-'}"
        )

    totals = aggregate([m for _, _, m in trials])
    print(
        f"\n{totals['rail_trials']} trial(s) with a rail · "
        f"{totals['proven']} proven · {totals['witnessed']} witnessed · "
        f"{totals['wrong_guesses']} wrong guess(es) · "
        f"{totals['refuted']} refuted · "
        f"{totals['claimed_without_proof']} claimed without proof"
    )

    for job, name, metrics in trials:
        proof = metrics.proof
        assert proof is not None
        reasons = [
            *(("no witness", text) for text in proof.witness_unavailable),
            *(("unverifiable", text) for text in proof.verification_unavailable),
            *(("degraded", text) for text in proof.verdict_degraded),
        ]
        # The roster is only worth printing where the rail did something: a
        # model-verdict trial's roster is the same two rows on every task.
        pivotal = proof.witness_authored or bool(reasons) or proof.oracle_runs
        if not (args.full or pivotal):
            continue
        print(f"\n── {job + ' · ' if seat_width else ''}{name} ── {proof.grade}")
        if proof.witness_authored:
            print(f"   witness   {proof.witness_path}  ({proof.witness_command})")
        if proof.tracked_command:
            print(f"   command   {proof.tracked_command}")
        for role in proof.roles:
            print(f"   {role.role:<16} {role.model}  ×{role.calls}")
        for label, text in reasons:
            print(f"   {label}: {text}")
        if args.full and proof.verdict_summary:
            print(f"   verdict: {proof.verdict_summary}")
    return 0


def _cmd_flip(args: argparse.Namespace) -> int:
    trial_dir = Path(args.trial_dir).expanduser().resolve()
    if not trial_dir.is_dir():
        print(f"no such trial directory: {trial_dir}", file=sys.stderr)
        return 1
    task_dir = Path(args.task_dir).expanduser() if args.task_dir else _task_dir_for(trial_dir)
    if task_dir is None or not task_dir.is_dir():
        print(
            "could not resolve the task directory; pass --task-dir",
            file=sys.stderr,
        )
        return 1

    try:
        result = replay_flip(
            trial_dir,
            task_dir,
            verifier_timeout=args.verifier_timeout,
            confirm_window=args.confirm_window,
        )
    except ReplayError as exc:
        print(str(exc), file=sys.stderr)
        return 1

    if result.flip_index is None:
        print(
            f"no snapshot passed ({result.snapshots} captured, "
            f"{result.probes} probed, {result.unknown} unknown)"
        )
        # "No snapshot passed" reads as "the agent never solved it", so it must
        # never be printed alone when capture is the thing that failed — that
        # is how a solved trial gets recorded as a failure (#2196).
        capture = result.capture
        if capture is not None and capture.broke:
            print(
                f"warning: capture broke during this trial — "
                f"{capture.snapshots} of {capture.attempts} attempt(s) landed, "
                f"and the last one failed: {capture.reason}\n"
                "         this trial was not fully measured; do not read the "
                "absence of a flip as the agent never solving it",
                file=sys.stderr,
            )
        return 0
    print(
        f"flip at snapshot {result.flip_index}/{result.snapshots - 1} "
        f"(t+{result.flip_elapsed:.0f}s)"
    )
    print(
        f"kept running {result.wasted_elapsed:.0f}s after it was already passing "
        f"— {result.probes} verifier runs, {result.unknown} unknown"
    )
    return 0


def _cmd_watch(args: argparse.Namespace) -> int:
    """Tail a match's artifacts and emit one line per detection.

    The supervising process is the customer: CI greps text lines and acts on
    the exit code; a subscriber (the self-driving loop, a ``Monitor``) reads
    ``--format jsonl`` — agent monitor protocol events, one per line — and
    reacts while the match is still running. Read-only with respect to the
    run, like everything else in the arena: a watched contestant's number
    cannot change because someone was watching.

    Exit codes: 0 = nothing invalidating (warnings and notices may still have
    printed), 2 = usage error, 3 = at least one critical detection — the
    match's numbers must not be published.
    """
    import time

    from .monitor import DEFAULT_RULES, MatchWatcher, Thresholds, stream_event

    # A match id resolves inside the workspace; a path is taken as-is, which
    # is what lets the same command run against an archive or a fixture tree.
    target = Path(args.match).expanduser()
    match_dir = (
        target
        if (target / "jobs").is_dir()
        else Path(args.workspace).expanduser() / "matches" / args.match
    )
    if not (match_dir / "jobs").is_dir():
        print(f"no such match: {args.match} (looked at {match_dir})", file=sys.stderr)
        return 2

    rules = (
        tuple(part.strip() for part in args.rules.split(",") if part.strip())
        if args.rules
        else DEFAULT_RULES
    )
    thresholds = Thresholds(
        min_steps=args.min_steps,
        min_output_tokens=args.min_output_tokens,
        stall_minutes=args.stall_minutes,
    )
    try:
        watcher = MatchWatcher(match_dir, rules=rules, thresholds=thresholds)
    except ValueError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    jsonl = args.format == "jsonl"
    if jsonl:
        print(
            json.dumps(
                stream_event(
                    "watching",
                    source="arenabench",
                    run=watcher.run,
                    rules=list(rules),
                )
            ),
            flush=True,
        )

    try:
        while True:
            for detection in watcher.scan():
                line = (
                    json.dumps(
                        detection.to_event(source="arenabench", run=watcher.run)
                    )
                    if jsonl
                    else detection.to_line()
                )
                print(line, flush=True)
            if not args.follow:
                break
            time.sleep(args.poll)
    except KeyboardInterrupt:
        # A follow subscription ends when its supervisor says so; ^C is the
        # normal way to say it, not a crash — so the summary and the exit
        # code still happen.
        pass

    if jsonl:
        print(watcher.summary_json(), flush=True)
    return 3 if watcher.invalidating else 0


def _cmd_build_recorder(args: argparse.Namespace) -> int:
    if not docker_available():
        print("docker is not available", file=sys.stderr)
        return 1
    if image_present() and not args.force:
        print(f"{IMAGE_TAG} already built (use --force to rebuild)")
        return 0
    ok = build_image()
    print(f"{'built' if ok else 'FAILED to build'} {IMAGE_TAG}")
    return 0 if ok else 1


def main(argv: list[str] | None = None) -> int:
    # The panel size is in `contest`'s help text and is its default, so it is
    # read while the parser is built rather than when a command runs.
    from .presets import PANEL_TASKS

    parser = argparse.ArgumentParser(
        prog="arenabench",
        description="Run coding-agent benchmarks as a live, side-by-side contest.",
    )
    parser.add_argument("-v", "--verbose", action="store_true")
    subparsers = parser.add_subparsers(dest="command", required=True)

    run_parser = subparsers.add_parser(
        "run", help="run a committed arenabench.toml (no browser; for CI)"
    )
    run_parser.add_argument("template", help="path to an arenabench.toml")
    run_parser.add_argument("--workspace", default=str(default_workspace()))
    run_parser.add_argument("--results", help="write the final snapshot JSON here")
    run_parser.add_argument("--poll", type=float, default=15.0)
    run_parser.add_argument(
        "--progress", action="store_true", help="print a line per poll"
    )
    run_parser.add_argument(
        "--allow-missing-env",
        action="store_true",
        help="launch even when a seat's declared credentials are absent",
    )
    run_parser.set_defaults(func=_cmd_run)

    contest_parser = subparsers.add_parser(
        "contest",
        help="build (and optionally submit) a Stella-vs-Claude-Code contest",
    )
    contest_parser.add_argument(
        "--versus-model",
        required=True,
        help="the worker BOTH arms run, e.g. anthropic/claude-sonnet-5 or "
             "claude-sonnet-5 (either spelling)",
    )
    contest_parser.add_argument(
        "--num-tasks", type=int, default=len(PANEL_TASKS),
        help=f"how many of the {len(PANEL_TASKS)}-task panel to run "
             f"(default: all {len(PANEL_TASKS)}; fewer is not comparable to "
             "the full-panel history)",
    )
    contest_parser.add_argument(
        "--concurrency", type=int, default=1,
        help="task slots per runner. One slot is TWO contestant containers "
             "racing, so a laptop-class Docker allocation serialises honestly "
             "at 1. Irrelevant to --submit, which fans out one job per trial.",
    )
    contest_parser.add_argument("--attempts", type=int, default=1)
    contest_parser.add_argument("--name", help="match name")
    contest_parser.add_argument("-o", "--output", help="where to write the toml")
    contest_parser.add_argument(
        "--no-video", action="store_true", help="do not record trial video"
    )
    contest_parser.add_argument(
        "--submit", action="store_true",
        help="submit to AWS Batch via `cloud run` after writing the toml",
    )
    contest_parser.add_argument("--ref", help="git ref of the Stella SUT (--submit)")
    contest_parser.add_argument("--run-id", help="run identifier (--submit)")
    contest_parser.add_argument(
        "--burst", action="store_true",
        help="submit to the spot queue — NOT for measured comparisons, since "
             "a spot interruption is indistinguishable from a loss",
    )
    contest_parser.add_argument("--poll", type=float, default=15.0)
    contest_parser.add_argument("--vcpus", type=int, default=4)
    contest_parser.add_argument("--memory-mb", type=int, default=15360)
    contest_parser.add_argument("--no-wait", action="store_true")
    # The same three flags `cloud run` declares, from the same definition:
    # `contest --submit` hands off by building that command's namespace by
    # hand, so a flag on one parser and not the other breaks the handoff.
    gate.register_arguments(contest_parser)
    contest_parser.add_argument("--region")
    contest_parser.add_argument("--bucket")
    contest_parser.set_defaults(func=_cmd_contest)

    template_parser = subparsers.add_parser(
        "template", help="print a starter arenabench.toml"
    )
    template_parser.add_argument("-o", "--output", help="write here instead of stdout")
    template_parser.add_argument("--name", default="my match")
    template_parser.add_argument("--dataset", default="terminal-bench-2.1")
    template_parser.set_defaults(func=_cmd_template)

    serve_parser = subparsers.add_parser("serve", help="run the arena web UI")
    serve_parser.add_argument("--host", default="127.0.0.1")
    serve_parser.add_argument("--port", type=int, default=8900)
    serve_parser.add_argument("--workspace", default=str(default_workspace()))
    serve_parser.add_argument(
        "--no-browser", action="store_true", help="do not open a browser"
    )
    serve_parser.add_argument(
        "--allow-remote",
        action="store_true",
        help="permit a non-loopback --host; anyone who can reach it can spend "
             "your provider credits",
    )
    serve_parser.add_argument(
        "--no-reuse",
        action="store_true",
        help="start a new arena even if one already serves this workspace "
             "(default: reuse it and print its URL)",
    )
    serve_parser.set_defaults(func=_cmd_serve)

    datasets_parser = subparsers.add_parser("datasets", help="list registered datasets")
    datasets_parser.set_defaults(func=_cmd_datasets)

    harness_parser = subparsers.add_parser(
        "harness",
        help="what each arm's harness disclosed, and how it spent its turn",
    )
    harness_parser.add_argument("match", help="match id under <workspace>/matches")
    harness_parser.add_argument("--workspace", default=str(default_workspace()))
    harness_parser.set_defaults(func=_cmd_harness)

    provenance_parser = subparsers.add_parser(
        "provenance",
        help="what each match was measured with, grouped by comparability key",
    )
    provenance_parser.add_argument("--workspace", default=str(default_workspace()))
    provenance_parser.add_argument(
        "--migrate",
        action="store_true",
        help="write reconstructed records for matches that recorded none. The "
             "Harbor version is not recoverable and is never invented — a "
             "backfilled record says 'unknown, older than X' and is marked as "
             "reconstructed rather than measured.",
    )
    provenance_parser.set_defaults(func=_cmd_provenance)

    tasks_parser = subparsers.add_parser("tasks", help="list a dataset's tasks")
    tasks_parser.add_argument("dataset", default="terminal-bench-2.1", nargs="?")
    tasks_parser.add_argument(
        "--random",
        type=int,
        metavar="N",
        help="draw N tasks at random instead of listing all of them",
    )
    tasks_parser.add_argument(
        "--seed",
        type=int,
        help="seed for --random; one is chosen and printed if you omit it",
    )
    tasks_parser.add_argument(
        "--exclude-heavy",
        action="store_true",
        help="draw only from tasks that do not force concurrency to 1",
    )
    tasks_parser.add_argument(
        "--max-memory-mb",
        type=int,
        default=0,
        metavar="MB",
        help="draw only from tasks asking for at most MB of memory; with N "
             "contestants racing, the ceiling that matters is your Docker "
             "allocation divided by N",
    )
    tasks_parser.set_defaults(func=_cmd_tasks)

    export_parser = subparsers.add_parser(
        "export", help="materialise a dataset for offline running"
    )
    export_parser.add_argument("dataset", default="terminal-bench-2.1", nargs="?")
    export_parser.add_argument(
        "-o", "--output", help="where to write it (default: ~/.arenabench/datasets)"
    )
    export_parser.set_defaults(func=_cmd_export)

    agents_parser = subparsers.add_parser("agents", help="list available agents")
    agents_parser.set_defaults(func=_cmd_agents)

    recorder_parser = subparsers.add_parser(
        "build-recorder", help="build the MP4 screen-recorder image"
    )
    recorder_parser.add_argument("--force", action="store_true")
    recorder_parser.set_defaults(func=_cmd_build_recorder)

    flip_parser = subparsers.add_parser(
        "flip",
        help="find when a trial started passing, and what it did afterwards",
    )
    proof_parser = subparsers.add_parser(
        "proof",
        help="what proved each trial: the witness, the oracle flip, and who ran",
    )
    proof_parser.add_argument(
        "path",
        help="a match, job or trial directory — every event stream underneath is read",
    )
    proof_parser.add_argument(
        "--full",
        action="store_true",
        help="print the roster and verdict summary for every trial, not only "
             "the ones where the rail did something",
    )
    proof_parser.set_defaults(func=_cmd_proof)
    flip_parser.add_argument("trial_dir", help="a trial directory containing arena/snapshots")
    flip_parser.add_argument(
        "--task-dir",
        help="the task's dataset directory (default: resolved from result.json)",
    )
    flip_parser.add_argument(
        "--verifier-timeout",
        type=float,
        default=1800.0,
        help="seconds allowed for one verifier run (default: 1800)",
    )
    flip_parser.add_argument(
        "--confirm-window",
        type=int,
        default=3,
        help="snapshots to re-probe before the bisected answer (default: 3)",
    )
    flip_parser.set_defaults(func=_cmd_flip)

    watch_parser = subparsers.add_parser(
        "watch",
        help="tail a match's artifacts and report failure signatures live",
    )
    watch_parser.add_argument(
        "match", help="a match id in the workspace, or a path to a match directory"
    )
    watch_parser.add_argument("--workspace", default=str(default_workspace()))
    watch_parser.add_argument(
        "--rules",
        help="comma-separated rule list (default: zero-token,premature-complete,"
             "late-verdict,stall; also available: usage-incomplete)",
    )
    watch_parser.add_argument(
        "--format",
        choices=("text", "jsonl"),
        default="text",
        help="text = one human-readable line per detection; jsonl = agent "
             "monitor protocol events for a subscribing process",
    )
    watch_parser.add_argument(
        "--follow",
        action="store_true",
        help="keep watching and emit new detections as they appear (^C to stop)",
    )
    watch_parser.add_argument(
        "--poll", type=float, default=15.0, help="seconds between scans with --follow"
    )
    watch_parser.add_argument(
        "--min-steps",
        type=int,
        default=5,
        help="premature-complete floor: steps a completed-and-failed trial "
             "should have taken (default: 5)",
    )
    watch_parser.add_argument(
        "--min-output-tokens",
        type=int,
        default=300,
        help="premature-complete floor: output tokens likewise (default: 300)",
    )
    watch_parser.add_argument(
        "--stall-minutes",
        type=float,
        default=10.0,
        help="minutes of silence before a running trial counts as stalled "
             "(default: 10)",
    )
    watch_parser.set_defaults(func=_cmd_watch)

    # The AWS Batch executor registers its own verb tree; everything cloud
    # lives in .cloud (boto3 stays a lazy, optional import there).
    from .assemble import register_cli as _register_assemble_cli
    from .cloud import register_cli as _register_cloud_cli

    _register_cloud_cli(subparsers)
    # Assembly is a local fold over a fetched directory and knows nothing about
    # AWS, so it is its own verb rather than a `cloud` one: it must stay
    # runnable on a run fetched months ago, and on a machine with no
    # credentials at all.
    _register_assemble_cli(subparsers)

    args = parser.parse_args(argv)
    _configure_logging(args.verbose)
    return int(args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
