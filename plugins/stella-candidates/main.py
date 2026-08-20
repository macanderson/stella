#!/usr/bin/env python3
"""`stella-candidates` — best-of-N, as a plugin.

Standard library only, deliberately — the rule every first-party plugin here
ships under (`doc:pipeline-as-plugins` §9 rule 3). `main.py` is the whole
program.

# What it does

The capability landed with #3844 and nothing consumed it: `candidate_fanout`,
`run_test` and `adopt_candidate` sat on the wire with no plugin asking for any
of them. This is the consumer, and it is the only thing in the tree that
exercises the `again?` point.

One point, `after_turn`, and one conversation inside it:

    host   {"point":"after_turn","body":{...}}
 -> plugin {"call":"candidate_fanout","id":1,"args":{"role":"builder",...}}
 -> host   {"result":1,"ok":{"requested":4,"candidates":[...]}}
 -> plugin {"call":"run_test","id":2,"args":{"candidate":"..."}}      (per candidate)
 -> host   {"result":2,"err":{"refusal":"unsupported",...}}          (today)
 -> plugin {"call":"adopt_candidate","id":N,"args":{"candidate":"..."}}
 -> host   {"result":N,"ok":{"adopted":"...","discarded":[...]}}
 -> plugin {"point":"after_turn","body":{"evidence":{...}}}          <- ends it

# Why everything is at `after_turn`

`doc:plugin-completion-plan` §4.2 puts the fan-out at `before_turn`. That is
not implementable: a plugin is a fresh process per point
(`SubprocessWrapper::exchange` spawns per call), so a `before_turn` fan-out
exits before `after_turn` runs and has nothing left to report — and
`ObservedEvidence` is an `after_turn` value. Nothing carries across the
boundary. A plugin that fanned out at `before_turn` would have to fan out again
to report, buying N writing worker turns twice.

So the host's own turn is attempt 0, and this plugin buys alternatives at
`after_turn`. Same best-of-N, one process, one fewer turn.

# How it decides, and what it will not do

The manifest declares the rule as data. This program computes the numbers the
rule reads and never a verdict — `judge` is the host's own function.

**Scoring is mechanical and stays that way.** The winner is the candidate that
finished, passed its test if a test signal exists, and changed the fewest
lines. There is no model call anywhere in this file and no arm that asks for
one. A model-scored ranking is not expressible in the oracle grammar
(`doc:plugin-completion-plan` §6.1: "a verdict over an aggregate the oracle
computes, not a quantifier the host evaluates") and smuggling one in through
the oracle process is the failure mode §6 exists to refuse.

# The test signal, and asking for it anyway

`run_test` is answered `HostCallRefusal::Unsupported` by every host today
(#3580) — the arm is unconditional in `wrapper/host_call.rs`. This plugin asks
for it regardless, and degrades to the mechanical signals the fan-out already
carries when the answer is a refusal.

That is deliberate: `run_test` is the *designed* way to ask, and reading the
candidate grant's `TestPlan` and running it here — the way
`verify-{rs,py,ts}` run theirs — would route around #3580 permanently instead
of inheriting its fix. Ask, degrade, disclose. `test-signal-available` reports
which of the two happened, so a reader can tell a run that ranked on tests from
one that ranked on diff size alone.
"""

import json
import sys

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
CALL_ANSWER_FIELDS = {"result", "ok", "err"}

# `host_call::CandidateFanoutResult` / `FanoutCandidate`, field for field.
FANOUT_OK_FIELDS = {"requested", "candidates"}
CANDIDATE_FIELDS = {
    "candidate",
    "root",
    "report",
    "completed",
    "files_changed",
    "lines_changed",
}
ADOPT_OK_FIELDS = {"adopted", "discarded"}

CALL_FANOUT = "candidate_fanout"
CALL_RUN_TEST = "run_test"
CALL_ADOPT = "adopt_candidate"

# The `[roles.<name>]` key this plugin declares. Must resolve to the WORKER's
# seat — a fan-out candidate is the work, not evidence about it.
ROLE_INTENT = "builder"

# The ask. The host clamps it and reports what it actually ran.
REQUESTED_WIDTH = 4


class Refusal(Exception):
    """The plugin cannot answer. Exits non-zero; the host reads the silence."""


def refuse(reason):
    raise Refusal(reason)


def deny_unknown(table, allowed, subject="the request"):
    unknown = sorted(set(table) - allowed)
    if unknown:
        refuse("{} denies unknown fields; got {}".format(subject, ", ".join(unknown)))
    return table


def read_json(stream):
    """The next JSON document on `stream`, or `None` when there is not one."""
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
    """One JSON document, on one line, flushed. The flush is load-bearing."""
    stream.write(json.dumps(document) + "\n")
    stream.flush()


def report(reason):
    """Say on stderr what this plugin did or degraded to — never silent."""
    sys.stderr.write("stella-candidates: {}\n".format(reason))


def refusal_text(failure):
    """A failed call's `err`, as the one line this plugin logs about it."""
    if isinstance(failure, dict):
        code = failure.get("refusal")
        detail = failure.get("detail")
        if isinstance(code, str) and isinstance(detail, str):
            return "{}: {}".format(code, detail)
    return json.dumps(failure, sort_keys=True)


class HostCalls:
    """This plugin's end of the host-call conversation (§6b).

    `ask` returns the `ok` payload or `None`. `None` covers every way a call can
    fail to produce one — refused for any of the closed reasons, answered for a
    different call, or not answered at all — and does not branch on which,
    because every one of them leaves this plugin with the same job: degrade, and
    say so on stderr.

    `refused` records the last refusal code, because this plugin has one
    degradation it must report as a *number* rather than only as prose: whether
    a test signal existed at all.
    """

    def __init__(self, stdin, stdout):
        self.stdin = stdin
        self.stdout = stdout
        self.asked = 0
        self.last_refusal = None

    def ask(self, call, args):
        self.asked += 1
        call_id = self.asked
        write_json(self.stdout, {"call": call, "id": call_id, "args": args})

        answer = read_json(self.stdin)
        if not isinstance(answer, dict):
            report("the host did not answer the {} call".format(call))
            return None
        answered = answer.get("result")
        if isinstance(answered, bool) or answered != call_id:
            report(
                "the host answered call {} while this plugin asked {}".format(
                    json.dumps(answered), call_id
                )
            )
            return None
        deny_unknown(answer, CALL_ANSWER_FIELDS, "a host-call answer")
        ok = answer.get("ok")
        failure = answer.get("err")
        if (ok is None) == (failure is None):
            refuse("a host-call answer carries either ok or err, never both")
        if failure is not None:
            if isinstance(failure, dict):
                self.last_refusal = failure.get("refusal")
            report("the host did not serve {}: {}".format(call, refusal_text(failure)))
            return None
        if not isinstance(ok, dict):
            refuse("a host-call answer's ok must be an object")
        return ok


def fan_out(host_calls, goal, width):
    """Ask for `width` isolated writing turns. Returns `(requested, candidates)`.

    `(0, [])` on every degradation — the host serves no fan-out plane, the
    manifest was not granted the call, or the answer was unreadable. A plugin
    that cannot fan out has nothing to choose between, which is a report of
    nothing rather than a failure.
    """
    ok = host_calls.ask(
        CALL_FANOUT,
        {"role": ROLE_INTENT, "instruction": candidate_instruction(goal), "width": width},
    )
    if ok is None:
        return (0, [])
    deny_unknown(ok, FANOUT_OK_FIELDS, "the fan-out answer")
    requested = ok.get("requested")
    candidates = ok.get("candidates", [])
    if not isinstance(requested, int) or isinstance(requested, bool):
        refuse("the fan-out answer carried no requested width")
    if not isinstance(candidates, list):
        refuse("the fan-out answer's candidates must be a list")

    scored = []
    for entry in candidates:
        if not isinstance(entry, dict):
            refuse("a fan-out candidate must be an object")
        deny_unknown(entry, CANDIDATE_FIELDS, "a fan-out candidate")
        handle = entry.get("candidate")
        completed = entry.get("completed")
        lines = entry.get("lines_changed")
        files = entry.get("files_changed")
        if not isinstance(handle, str) or not handle:
            refuse("a fan-out candidate carried no handle")
        if not isinstance(completed, bool):
            refuse("a fan-out candidate's completed must be a boolean")
        if not isinstance(lines, int) or isinstance(lines, bool):
            refuse("a fan-out candidate carried no lines_changed")
        if not isinstance(files, int) or isinstance(files, bool):
            refuse("a fan-out candidate carried no files_changed")
        scored.append(
            {
                "handle": handle,
                "completed": completed,
                "lines_changed": lines,
                "files_changed": files,
                # Filled in below, when a test signal exists at all.
                "passed": None,
            }
        )
    return (requested, scored)


def candidate_instruction(goal):
    """What every candidate turn is asked to do.

    The goal, and a bounded restatement of the rule they are about to be judged
    by. Telling them the rule is not gaming it: the rule is in the manifest a
    human read before installing, and a worker that knows "smallest correct
    change wins" writes a better candidate than one guessing at the criterion.
    """
    return (
        "{}\n\n"
        "You are one of several independent attempts at this task, each in its "
        "own isolated worktree. Exactly one attempt will be kept and the rest "
        "discarded. The attempt that is kept is the one that finishes, passes "
        "the project's tests, and changes the fewest lines to do it. Make the "
        "smallest correct change you can; do not rewrite what you do not have "
        "to."
    ).format(goal)


def score_tests(host_calls, candidates):
    """Ask the host to run each candidate's tests. Returns whether it could.

    Today every host answers `unsupported` (#3580), so this returns `False` and
    every candidate keeps `passed = None`. Asking anyway is the point: when
    #3580 lands, this plugin ranks on tests with no change to it.
    """
    served = False
    for candidate in candidates:
        ok = host_calls.ask(CALL_RUN_TEST, {"candidate": candidate["handle"]})
        if ok is None:
            continue
        served = True
        # The host has no `run_test` answer shape yet (`HostCallOk` carries no
        # RunTest variant), so this reads the two keys any such answer must
        # plausibly carry and treats anything else as no signal. It is written
        # to be replaced by a typed read the day the shape exists, not to guess
        # cleverly in the meantime.
        passed = ok.get("passed")
        if isinstance(passed, bool):
            candidate["passed"] = passed
    if not served:
        report(
            "no host served run_test (#3580), so candidates are ranked on what "
            "the fan-out itself reported: completion, then diff size"
        )
    return served


def choose(candidates):
    """The winner, or `None` when there is nothing to choose.

    Mechanical, in this order:

    1. A candidate that did not finish is never chosen. `completed = false` is
       an ordinary outcome — its carve ran out, its step cap hit — and its
       workspace is still readable, but adopting an unfinished attempt would
       land a half-change on the real tree.
    2. Among those, a candidate whose tests passed beats one whose tests failed.
       With no test signal every candidate is equal here, and the tiebreak below
       does all the work.
    3. Among those, the smallest `lines_changed` wins.
    4. Ties break on the handle, so the choice is deterministic rather than
       dependent on the order the host happened to mint them in.
    """
    finished = [c for c in candidates if c["completed"]]
    if not finished:
        return None
    if any(c["passed"] is False for c in finished):
        passing = [c for c in finished if c["passed"] is not False]
        if passing:
            finished = passing
    return min(finished, key=lambda c: (c["lines_changed"], c["handle"]))


def adopt(host_calls, winner):
    """Land the winner. Returns whether the host says it did."""
    ok = host_calls.ask(CALL_ADOPT, {"candidate": winner["handle"]})
    if ok is None:
        return False
    deny_unknown(ok, ADOPT_OK_FIELDS, "the adopt answer")
    adopted = ok.get("adopted")
    if not isinstance(adopted, str) or not adopted:
        refuse("the adopt answer named no adopted candidate")
    discarded = ok.get("discarded", [])
    if not isinstance(discarded, list):
        refuse("the adopt answer's discarded must be a list")
    report(
        "adopted {} and discarded {} sibling(s)".format(adopted, len(discarded))
    )
    return True


def evidence(measurements):
    """One `ObservedEvidence`. This plugin's evidence is never a flip."""
    return {"flip": "not-applicable", "measurements": measurements}


def assess(body, host_calls):
    """The whole of `after_turn`."""
    goal = body.get("goal")
    if not isinstance(goal, str) or not goal:
        refuse("the request carried no goal")

    requested, candidates = fan_out(host_calls, goal, REQUESTED_WIDTH)
    if not candidates:
        report(
            "no candidate workspaces were built, so there is nothing to choose "
            "between and nothing was adopted"
        )
        # Deliberately NOT `candidate-adopted: 0` alone — the check reads that
        # name and a 0 is an honest claim here: the plugin ran, asked, and
        # adopted nothing. `candidates-scored: 0` is what says why.
        return evidence(
            {
                "candidates-scored": 0,
                "candidate-adopted": 0,
                "winner-lines-changed": 0,
                "test-signal-available": 0,
            }
        )

    served = score_tests(host_calls, candidates)
    winner = choose(candidates)
    if winner is None:
        report(
            "{} candidate(s) were built and none finished, so none was adopted".format(
                len(candidates)
            )
        )
        return evidence(
            {
                "candidates-scored": len(candidates),
                "candidate-adopted": 0,
                "winner-lines-changed": 0,
                "test-signal-available": 1 if served else 0,
            }
        )

    landed = adopt(host_calls, winner)
    if requested > len(candidates):
        report(
            "asked for {} candidates and the host ran {}".format(
                requested, len(candidates)
            )
        )
    return evidence(
        {
            "candidates-scored": len(candidates),
            "candidate-adopted": 1 if landed else 0,
            "winner-lines-changed": winner["lines_changed"],
            "test-signal-available": 1 if served else 0,
        }
    )


def read_request(stdin):
    """Decode `{"point": ..., "body": ...}` and return the `after_turn` body."""
    envelope = read_json(stdin)
    if not isinstance(envelope, dict):
        refuse("stdin was not a single JSON object")
    if set(envelope) - {"point", "body"}:
        refuse("the request envelope carried a field outside {point, body}")

    point = envelope.get("point")
    if point != POINT:
        refuse("this plugin answers only `{}`; it was asked `{}`".format(POINT, point))
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
    host_calls = HostCalls(sys.stdin, sys.stdout)
    try:
        body = read_request(sys.stdin)
        observed = assess(body, host_calls)
    except Refusal as refused:
        report(str(refused))
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
