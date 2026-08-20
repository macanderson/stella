#!/usr/bin/env python3
"""`stella-witness` — the flip oracle, in the open, as a plugin.

Standard library only, deliberately — the same rule `plugins/stella-plan` and
`plugins/stella-research` ship under (`doc:pipeline-as-plugins` §9 rule 3: "if
a plugin CANNOT be written without an SDK, the protocol is too complicated").
`main.py` is the whole program.

# The wire

`crates/stella-plugin/src/wire.rs` is the contract. The host spawns
`[runtime].argv` directly — no shell — writes one JSON request on stdin and
reads the response from stdout:

    {"point": "after_turn", "body": {...AfterTurnRequest}}
 -> {"point": "after_turn", "body": {...AfterTurnResponse}}

No host call happens in between. This plugin declares no `[loop] calls` and
asks the host for nothing: everything it needs — the invocation, its arguments
and the red half of the flip — arrives in the grant.

Every table denies unknown fields, the envelope included, so this program does
too, at every level.

There is no error variant in `AfterTurnResponse`. A plugin that cannot answer
*fails* — non-zero exit, one line on stderr — and the host reads the silence
as `EvidenceSet::unobserved`, which makes `judge` abstain rather than blame a
worker for evidence nobody collected.

# What it reports, and what it refuses to report

`ObservedEvidence` — a flip and a set of numbers. Never a verdict: `judge` is
the host's own synchronous function over the rule `plugin.toml` declares AS
DATA, and a plugin that graded its own work would be the thing this whole
extraction exists to prevent (#3511, `doc:wrapper-socket` §7).

The one number it will not report is a score. There is no model call anywhere
in this file, and there is no arm that asks for one: `ladder_decision`'s
terminality — no arm escalates to a model — is the property being carried over
from the deleted built-in path, and it is carried by there being nothing here
that could.

# The nucleus this carries

`FlipOracle`, `FlipState` and `normalize_command` are ported from
`crates/stella-pipeline/src/verify.rs` as it stood at the commit before
`a6d3db4f6` — the last commit before the built-in staged pipeline was deleted
(#3865). The port is behavioural, not textual: the transition table below is
the same table, and `flip_requires_a_prior_failing_observation` is the same
property.

**The invariant the whole design rests on: `Flipped` is reachable only by
passing through `Failing` for the same normalized command.** A pass with no
prior failure proves nothing and does not even lock the command.

See README.md for what v1 deliberately does not carry.
"""

import json
import os
import subprocess
import sys
import time

PROTOCOL_VERSION = 1
POINT = "after_turn"

REQUEST_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "turn",
}
GRANT_FIELDS = {"handle", "root", "test"}
TEST_FIELDS = {"program", "args", "baseline"}

# `TestBaseline`, as the wire spells it.
BASELINE_NOT_RUN = "not-run"
BASELINE_PASSED = "passed"
BASELINE_FAILED = "failed"
BASELINE_UNOBSERVED = "unobserved"

# `FlipObservation`, as the wire spells it.
FLIP_NOT_ATTEMPTED = "not-attempted"
FLIP_ACHIEVED = "achieved"
FLIP_NOT_ACHIEVED = "not-achieved"
FLIP_UNSATISFIABLE = "unsatisfiable"
FLIP_UNOBSERVABLE = "unobservable"

# `FlipState`, ported.
STATE_NONE = "none"
STATE_FAILING = "failing"
STATE_UNSTABLE = "unstable"
STATE_FLIPPED = "flipped"

# `ObserveOutcome`, ported. Returned so the transition table is directly
# assertable, exactly as the Rust enum existed for.
ADVANCED = "advanced"
NO_EVIDENCE = "no-evidence"
IGNORED = "ignored"


class Refusal(Exception):
    """The plugin cannot answer. Exits non-zero; the host reads the silence."""


def refuse(reason):
    raise Refusal(reason)


def deny_unknown(table, allowed, subject="the request"):
    """Refuse a table carrying a key the contract does not declare."""
    unknown = sorted(set(table) - allowed)
    if unknown:
        refuse("{} denies unknown fields; got {}".format(subject, ", ".join(unknown)))
    return table


def read_json(stream):
    """The next JSON document on `stream`, or `None` when there is not one.

    Line-oriented *and* whole-document tolerant, matching the sibling plugins:
    both shapes are legitimate hosts, and a plugin that only handled one would
    hang against the other.
    """
    decoder = json.JSONDecoder()
    buffered = ""
    while True:
        text = buffered.strip()
        if text:
            try:
                document, end = decoder.raw_decode(text)
            except ValueError:
                pass  # Not yet complete, or never will be. EOF decides which.
            else:
                if not text[end:].strip():
                    return document
        line = stream.readline()
        if not line:
            return None
        buffered += line


def write_json(stream, document):
    """One JSON document, on one line, flushed. The flush is load-bearing: the
    host is reading a pipe and will wait forever for a buffered answer."""
    stream.write(json.dumps(document) + "\n")
    stream.flush()


def report(reason):
    """Say on stderr what this plugin degraded to, and why — never silent."""
    sys.stderr.write("stella-witness: {}\n".format(reason))


# ── The flip oracle, ported from crates/stella-pipeline/src/verify.rs ───────


def normalize_command(command):
    """Trim, and collapse every run of whitespace to a single space.

    Makes `"cargo   test  -p x"` and `"cargo test -p x"` the same tracked
    command while leaving token order — which can be semantically load-bearing
    — untouched. A pass on `cargo test -p a` must never be credited to a
    failure of `cargo test -p b`, so reordering is deliberately NOT normalized
    away.
    """
    return " ".join(command.split())


class FlipOracle:
    """The deterministic flip oracle (L-E11).

    Construct empty; feed it `(command, passed)` observations. It locks onto
    the first command it sees *fail* and thereafter only reasons about that one
    normalized command.

    Transition table, ported unchanged:

    | state    | observation                | next                          |
    |----------|----------------------------|-------------------------------|
    | none     | pass (any cmd)             | none (NO_EVIDENCE)            |
    | none     | fail (cmd C)               | failing, tracked=C            |
    | any      | different cmd than tracked | unchanged (IGNORED)           |
    | failing  | fail (tracked)             | failing                       |
    | failing  | pass (tracked)             | flipped                       |
    | flipped  | pass (tracked)             | flipped                       |
    | flipped  | fail (tracked)             | failing (honest regression)   |
    | unstable | pass (tracked)             | flipped                       |
    | unstable | fail (tracked)             | unstable                      |

    The honest `flipped -> failing` regression edge keeps the oracle truthful
    if a "fixed" test starts failing again; it never violates the core
    invariant, because reaching `flipped` still required a prior `failing` of
    the same command. `unstable` is entered only through `confirm`, never
    through an observation.
    """

    def __init__(self):
        self.tracked = None
        self.state = STATE_NONE

    def observe(self, command, passed):
        """Observe one run. Returns what the observation did."""
        norm = normalize_command(command)
        if self.tracked is None:
            if passed:
                # A pass with no prior failure proves nothing — do not even
                # lock the command (L-E11). This line is the invariant.
                return NO_EVIDENCE
            self.tracked = norm
            self.state = STATE_FAILING
            return ADVANCED

        if self.tracked != norm:
            return IGNORED

        if passed:
            self.state = STATE_NONE if self.state == STATE_NONE else STATE_FLIPPED
        elif self.state == STATE_UNSTABLE:
            # The confirmation already failed once; another failure adds
            # nothing, and the pass that WAS observed stays on record.
            self.state = STATE_UNSTABLE
        else:
            self.state = STATE_FAILING
        return ADVANCED

    def confirm(self, passed):
        """Record the confirmation re-run of a flip (#859).

        A deterministic pass is credited only when the tracked command also
        passed a second time on the same tree, so a flaky test that failed for
        an unrelated reason on the baseline cannot buy credit with one lucky
        pass. A failed confirmation demotes `flipped` to `unstable` — "the pass
        could not be reproduced", which is a different fact from "the test
        never passed".
        """
        if self.state == STATE_FLIPPED and not passed:
            self.state = STATE_UNSTABLE

    def is_flipped(self):
        return self.state == STATE_FLIPPED

    def is_unstable(self):
        return self.state == STATE_UNSTABLE

    def outcome(self):
        """The oracle's finding, as the wire's `FlipObservation` (#2556).

        `unobservable` is *the oracle never locked onto a command* — the same
        condition as `tracked is None` — and is deliberately distinct from
        `not-achieved`, which means a command was tracked and did not flip.
        """
        if self.is_flipped():
            return FLIP_ACHIEVED
        if self.tracked is None:
            return FLIP_UNOBSERVABLE
        return FLIP_NOT_ACHIEVED


# ── Running the invocation the grant names ─────────────────────────────────


def run_test(program, args, cwd):
    """Run one test invocation in `cwd`. Returns `(passed, exit_code, ms)`.

    `passed` is `exit_code == 0` and nothing cleverer: the runner's own exit
    status is the contract every test runner in the closed vocabulary already
    honours, and parsing output to second-guess it is the fingerprint work v1
    does not carry (see README).

    A program that cannot be started at all is not a failing test — it is an
    unobservable one. It comes back with `exit_code = None`, which the caller
    turns into `unobservable` rather than feeding the oracle a `False` it
    would read as red.
    """
    started = time.monotonic()
    try:
        finished = subprocess.run(  # noqa: S603 - the grant names the program
            [program] + list(args),
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
    except OSError as error:
        report("the invocation could not be started: {}".format(error))
        return (False, None, int((time.monotonic() - started) * 1000))
    elapsed_ms = int((time.monotonic() - started) * 1000)
    return (finished.returncode == 0, finished.returncode, elapsed_ms)


def evidence(flip, measurements):
    """One `ObservedEvidence`, as the wire spells it."""
    return {"flip": flip, "measurements": measurements}


def unobservable(reason):
    """What this plugin reports when it saw nothing at all.

    `unobservable` and an empty measurement set, never a zero: a name absent
    from `measurements` is *missing*, and a name present with a 0 is a claim.
    Reporting `test-command-exit-code: 0` for a test that never ran would
    credit a check the plugin has no evidence for.
    """
    report(reason)
    return evidence(FLIP_UNOBSERVABLE, {})


def assess(body):
    """The whole of `after_turn`: observe the flip, report what was seen."""
    grant = body.get("candidate")
    if not isinstance(grant, dict):
        return unobservable(
            "this host granted no candidate workspace, so there is no tree to "
            "run the invocation in and nothing to observe"
        )
    deny_unknown(grant, GRANT_FIELDS, "the candidate grant")

    root = grant.get("root")
    if not isinstance(root, str) or not root:
        refuse("the candidate grant carried no root")

    plan = grant.get("test")
    if not isinstance(plan, dict):
        return unobservable(
            "the candidate grant names no test invocation, so there is no "
            "command whose red-to-green transition could be observed"
        )
    deny_unknown(plan, TEST_FIELDS, "the test plan")

    program = plan.get("program")
    if not isinstance(program, str) or not program:
        refuse("the test plan carried no program")
    args = plan.get("args", [])
    if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
        refuse("the test plan's args must be a list of strings")
    baseline = plan.get("baseline", BASELINE_NOT_RUN)
    if baseline not in {
        BASELINE_NOT_RUN,
        BASELINE_PASSED,
        BASELINE_FAILED,
        BASELINE_UNOBSERVED,
    }:
        refuse("the test plan carried an unknown baseline: {}".format(baseline))

    if not os.path.isdir(root):
        return unobservable(
            "the granted root does not exist or is not a directory: {}".format(root)
        )

    command = " ".join([program] + list(args))
    oracle = FlipOracle()

    # The red half, as the host pinned it before the turn. Only `failed` is
    # red: `not-run` and `unobserved` say nothing about assertions either way,
    # and feeding either to the oracle as a failure would manufacture the very
    # baseline a flip is supposed to have earned.
    if baseline == BASELINE_FAILED:
        oracle.observe(command, False)

    # The green half, now.
    passed, exit_code, elapsed_ms = run_test(program, args, root)
    if exit_code is None:
        return unobservable(
            "the invocation could not be run in the granted root, so this turn "
            "has no observation either way"
        )
    oracle.observe(command, passed)

    # #859: a pass is credited only if it reproduces on the same tree.
    if oracle.is_flipped():
        confirmed, _, _ = run_test(program, args, root)
        oracle.confirm(confirmed)
        if not confirmed:
            report(
                "the flip's confirmation re-run did not pass — reporting it as "
                "unstable rather than crediting a pass that did not reproduce"
            )

    measurements = {
        "test-command-exit-code": exit_code,
        "test-duration-ms": elapsed_ms,
        "flip-unstable": 1 if oracle.is_unstable() else 0,
    }
    return evidence(oracle.outcome(), measurements)


def read_request(stdin):
    """Decode `{"point": ..., "body": ...}` and return the `after_turn` body."""
    envelope = read_json(stdin)
    if not isinstance(envelope, dict):
        refuse("stdin was not a single JSON object")
    if set(envelope) - {"point", "body"}:
        refuse("the request envelope carried a field outside {point, body}")

    point = envelope.get("point")
    if point != POINT:
        refuse(
            "this plugin answers only `{}`; it was asked `{}`".format(POINT, point)
        )
    body = envelope.get("body")
    if not isinstance(body, dict):
        refuse("the request carried no body object")
    deny_unknown(body, REQUEST_FIELDS)

    version = body.get("protocol_version")
    if version != PROTOCOL_VERSION:
        refuse(
            "this plugin speaks protocol version {}; the host wrote {}".format(
                PROTOCOL_VERSION, version
            )
        )
    return body


def main():
    try:
        body = read_request(sys.stdin)
        observed = assess(body)
    except Refusal as refusal:
        report(str(refusal))
        return 1
    write_json(
        sys.stdout,
        {
            "point": POINT,
            "body": {"protocol_version": PROTOCOL_VERSION, "evidence": observed},
        },
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
