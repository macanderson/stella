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

    {"point": "before_turn", "body": {...BeforeTurnRequest}}
 -> {"point": "before_turn", "body": {"witness": [...]}}

    {"point": "after_turn", "body": {...AfterTurnRequest}}
 -> {"point": "after_turn", "body": {...AfterTurnResponse}}

Two points, and each says one thing. `before_turn` names the paths the flip
will be judged against — paths, never findings, because this plugin does not
vouch for its own witness and the host is the one that snapshots and compares
(#3499, #3587). `after_turn` reports the flip it observed. It contributes no
context and no role at `before_turn`: a verifier that steered the turn it is
about to judge would be grading its own work.

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
BEFORE_TURN = "before_turn"
AFTER_TURN = "after_turn"
POINTS = (BEFORE_TURN, AFTER_TURN)

AFTER_TURN_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "turn",
}
BEFORE_TURN_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "published",
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

    A process killed by a signal reports a negative `returncode` in Python
    (`-N` for signal `N`). The reported exit code is normalised to the shell
    `128+N` convention the rest of the codebase uses (see `CmdOutcome.exit_code`
    in `crates/stella-plugin/src/candidate_grant.rs`) so it is always
    non-negative: the wire measurement type is `u64`, and a negative value would
    fail to decode on the host, turning a genuine red/crashed test into an
    unparseable response.
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
    returncode = finished.returncode
    exit_code = returncode if returncode >= 0 else 128 - returncode
    return (returncode == 0, exit_code, elapsed_ms)


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


def under_root(root, candidate):
    """The candidate path, if it names an existing regular file inside `root`.

    Absolute paths and any path with a `..` segment are refused here rather
    than left for the host's fence to drop: a declaration is this plugin's own
    claim about its own witness, and one it already knows the host will refuse
    is noise in the wire log with no chance of becoming a watch.
    """
    if not candidate or os.path.isabs(candidate):
        return None
    parts = candidate.replace("\\", "/").split("/")
    if any(part == ".." for part in parts):
        return None
    return candidate if os.path.isfile(os.path.join(root, candidate)) else None


def cargo_witnesses(program, args):
    """`tests/<name>.rs` for every `--test <name>` in a cargo invocation.

    The motivating case for the whole declaration (#3587): `cargo test --test
    flip` names `flip`, and the artifact is `tests/flip.rs` **by cargo's
    convention and by nothing in the invocation**. The host is deliberately
    forbidden from deriving that — a host deriving a witness is a host guessing
    at one — so the plugin whose flip it is says it instead.

    A guess that is wrong costs nothing and cannot credit anything: the path
    will not exist, `under_root` drops it, the watch stays empty and the finding
    stays `NotChecked`, which is the same refusal-to-credit the invocation
    produced before this plugin said anything. Only a path that is really there
    can ever become a watch, and a watch only ever *withholds* credit.

    Not attempted for a workspace-scoped invocation whose target lives under a
    member directory (`cargo test -p stella-core --test loops`): the convention
    stops at the member's own root and this program has no way to find it.
    """
    if os.path.basename(program) not in {"cargo", "cargo.exe"}:
        return []
    if "test" not in args:
        return []
    named = []
    pending = False
    for arg in args:
        if pending:
            named.append(arg)
            pending = False
        elif arg == "--test":
            pending = True
        elif arg.startswith("--test="):
            named.append(arg[len("--test=") :])
    return ["tests/{}.rs".format(name) for name in named if name]


def witness_artifacts(body):
    """The paths this plugin will judge its flip against, for `before_turn`.

    Two sources, in the order a reader would look for them: the files the
    invocation itself names, and the files a runner's convention implies. The
    first is what the host already derives, and declaring it again is a no-op —
    the watch is add-only and keeps each artifact's first-observed identity — so
    this list is *what this plugin judges*, complete, rather than a diff against
    what the host happened to work out.

    Never a finding, only paths: this plugin does not vouch for its own witness
    and could not if it wanted to (#3499). The host snapshots each one before
    the turn and compares after it.
    """
    declared = []
    grant = body.get("candidate")
    if isinstance(grant, dict):
        deny_unknown(grant, GRANT_FIELDS, "the candidate grant")
        root = grant.get("root")
        if not isinstance(root, str) or not root:
            refuse("the candidate grant carried no root")

        plan = grant.get("test")
        if isinstance(plan, dict):
            deny_unknown(plan, TEST_FIELDS, "the test plan")
            program = plan.get("program")
            if not isinstance(program, str) or not program:
                refuse("the test plan carried no program")
            args = plan.get("args", [])
            if not isinstance(args, list) or not all(isinstance(a, str) for a in args):
                refuse("the test plan's args must be a list of strings")

            named = [arg for arg in args if arg and not arg.startswith("-")]
            for candidate in named + cargo_witnesses(program, args):
                resolved = under_root(root, candidate)
                if resolved is not None and resolved not in declared:
                    declared.append(resolved)

    if not declared:
        report(
            "this round names no artifact this plugin can point at, so the host "
            "has nothing to watch and the flip cannot be credited"
        )
    return declared


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
    """Decode `{"point": ..., "body": ...}` and return `(point, body)`."""
    envelope = read_json(stdin)
    if not isinstance(envelope, dict):
        refuse("stdin was not a single JSON object")
    if set(envelope) - {"point", "body"}:
        refuse("the request envelope carried a field outside {point, body}")

    point = envelope.get("point")
    if point not in POINTS:
        refuse(
            "this plugin answers only `{}`; it was asked `{}`".format(
                "` and `".join(POINTS), point
            )
        )
    body = envelope.get("body")
    if not isinstance(body, dict):
        refuse("the request carried no body object")
    deny_unknown(
        body,
        BEFORE_TURN_FIELDS if point == BEFORE_TURN else AFTER_TURN_FIELDS,
    )

    version = body.get("protocol_version")
    if version != PROTOCOL_VERSION:
        refuse(
            "this plugin speaks protocol version {}; the host wrote {}".format(
                PROTOCOL_VERSION, version
            )
        )
    return (point, body)


def answer(point, body):
    """The response body for one point.

    `before_turn` contributes the witness declaration and nothing else — no
    context, no role, no scope. A plugin that steered the turn it is about to
    judge would be grading its own work, which is the thing this whole
    extraction exists to prevent (#3511).
    """
    if point == BEFORE_TURN:
        return {
            "protocol_version": PROTOCOL_VERSION,
            "witness": witness_artifacts(body),
        }
    return {"protocol_version": PROTOCOL_VERSION, "evidence": assess(body)}


def main():
    try:
        point, body = read_request(sys.stdin)
        answered = answer(point, body)
    except Refusal as refusal:
        report(str(refusal))
        return 1
    write_json(sys.stdout, {"point": point, "body": answered})
    return 0


if __name__ == "__main__":
    sys.exit(main())
