#!/usr/bin/env python3
"""`stella-selfdriving` — the delivery loop, as a driver program.

The standard library and nothing else, like every plugin here. That is
`doc:pipeline-as-plugins` §9 rule 3: a plugin that cannot be written without
an SDK means the protocol is too hard. This file is the whole program.

# What it is

A driver, not a wrapper. It never runs inside a turn. It starts them. The host
opens a session, and opens another one each time this program answers `sleep`.
This program says what a cycle should do, asks the host to do it, and says what
should happen next:

    host   {"point": "drive", "body": {"session": "cycle-7"}}
 -> plugin {"call": "backlog_next", "id": 1}
 -> host   {"result": 1, "ok": {"backlog": {"issues": [...]}}}
 -> plugin {"call": "backlog_claim", "id": 2, "args": {...}}
 -> host   {"result": 2, "ok": {"claim": {"issue": "41", "held": true}}}
 -> plugin {"call": "work_start", "id": 3, "args": {...}}
 -> host   {"result": 3, "ok": {"work": {"state": "changed", ...}}}
 -> plugin {"call": "deliver_open", "id": 4}
 -> host   {"result": 4, "ok": {"pull_request": {"pr": "4102", ...}}}
 -> plugin {"point": "drive", "body": {"next": {"halt": {"reason": "..."}}}}

One JSON message per line, both ways. `crates/stella-plugin/src/driver.rs` is
the contract. `docs/wire/wrapper.wire.json` holds its exact bytes.

# It asks, and never reaches

This program holds no forge token and no provider key. It has no worktree and
no budget. The host does each thing it needs, when asked.

The host checks each ask against the `[driver] calls` list in `plugin.toml`.
That is the list a person read before install. An ask that is not on it comes
back as `err` with `refusal: "undeclared"`. The right move is to stop and say
so. Never to go get the thing some other way.

# What a cycle does now

The host serves `backlog_next`, `backlog_claim`, the three `work` verbs and the
four `deliver` verbs. So a cycle reads the ranked queue, claims the top of it,
asks Stella to work that one issue in a checkout of its own, opens the pull
request, reads the forge, and asks what to do next.

It will not work an issue it cannot know is free. Two loops on one issue is
what a claim is for, and going ahead without one would trade a good refusal for
a quiet race.

A cycle ends when the machine says anything but "merge". Waiting on CI,
pushing a fix and rebasing are all a later cycle's work. So this one says which
state it stopped in, and how long to wait.

# Who decides a merge

`deliver_next` answers over facts this program sends it. `deliver_merge` takes
no facts at all. That ask names a pull request and nothing else.

The host reads the forge itself. It runs the same machine over its own answer.
So a cycle that reported a build nobody saw gets a refusal. This program asks
for the merge when the decision it was given says to. The host is what makes
that safe, not this file.

# Arguments

An ask that is about a unit of work carries that unit:

    {"call": "work_start", "id": 3, "args": {"work_start": {"issue": "41"}}}

The table is named for the verb that reads it, and an ask carries its own
verb's table and no other. A verb that reads nothing sends no `args` key.

# `scripts/self-driving.sh` still exists

That shell script is the working driver. It holds the powers this package's
`[[capabilities]]` list names — `bash`, `write_file`, `process_spawn` — as
you, straight out. This program holds none of them. It asks. The shell script
stays until this one can do what it does, which is §10's own rule. What has
moved across so far is the queue read, the claim, the work, and the pull
request through to the merge; the sweep, the benchmark, the release install
and the daemon are still the shell script's.
"""

import json
import sys

# The verbs a cycle asks for, in the order it needs them. Every one is on
# `plugin.toml`'s `[driver] calls`. An ask that is not on that list comes back
# turned down. Writing one here would be a bug.
BACKLOG_NEXT = "backlog_next"
BACKLOG_CLAIM = "backlog_claim"
WORK_START = "work_start"
DELIVER_OPEN = "deliver_open"
DELIVER_OBSERVE = "deliver_observe"
DELIVER_NEXT = "deliver_next"
DELIVER_MERGE = "deliver_merge"

# How long to wait for the next cycle when there is nothing to do. Fifteen
# minutes is the shell driver's own idle pause. The host clamps it either way,
# so this is an ask and not a promise.
IDLE_SECS = 900


class Channel:
    """The session: one ask at a time, each answer read before the next."""

    def __init__(self, stdin, stdout):
        self._stdin = stdin
        self._stdout = stdout
        self._next_id = 1
        self.refusals = []

    def ask(self, call, args=None):
        """Ask the host for `call`, with `args` when the verb reads them.

        Returns the `ok` table, or `None` when the host refused. A refusal is
        recorded and returned as an absence: it is an answer this program reads
        and degrades on, never an error that ends the session.
        """
        request_id = self._next_id
        self._next_id += 1
        message = {"call": call, "id": request_id}
        if args is not None:
            message["args"] = args
        self._write(message)
        answer = self._read("an answer to %s" % call)
        if answer.get("result") != request_id:
            fail("the host answered %r with result %r" % (call, answer.get("result")))
        if "ok" in answer:
            return answer["ok"]
        refusal = answer.get("err") or {}
        self.refusals.append(
            (call, refusal.get("refusal", "unknown"), refusal.get("detail", ""))
        )
        return None

    def finish(self, nxt):
        """End the session by saying what should happen after it."""
        self._write({"point": "drive", "body": {"next": nxt}})

    def _write(self, message):
        self._stdout.write(json.dumps(message) + "\n")
        self._stdout.flush()

    def _read(self, what):
        line = self._stdin.readline()
        if not line:
            fail("the host closed the channel before sending %s" % what)
        try:
            return json.loads(line)
        except ValueError as error:
            fail("%s did not parse as JSON: %s" % (what, error))


def fail(reason):
    """Stop, loudly.

    Reserved for the host contradicting the contract — a truncated channel, an
    answer to an ask nobody made. A refused capability is an ordinary outcome
    and never comes here.
    """
    sys.stderr.write("stella-selfdriving: %s\n" % reason)
    raise SystemExit(1)


def read_session(stdin):
    """The drive request the host opens with."""
    line = stdin.readline()
    if not line:
        fail("no drive request arrived on stdin")
    try:
        request = json.loads(line)
    except ValueError as error:
        fail("the drive request did not parse as JSON: %s" % error)
    if request.get("point") != "drive":
        fail("expected a drive request, got point %r" % request.get("point"))
    body = request.get("body") or {}
    return body.get("session", "")


def queue_of(ok):
    """The issues a served `backlog_next` answered with.

    An answer with no `backlog` member is a host that did the call and reported
    nothing. That is a different fact from an empty queue, and a later cycle
    may want to tell them apart. Both mean "no work to take" here.
    """
    page = (ok or {}).get("backlog") or {}
    return page.get("issues") or []


def refused_halt(channel, said):
    """Stop, naming the ask the host would not perform.

    `said` is what this cycle was doing when it asked, so the reason a person
    reads says which stage the loop got to, not only which verb came back.
    """
    call, refusal, detail = channel.refusals[-1]
    return {"halt": {"reason": "%s: %s was refused (%s): %s" % (said, call, refusal, detail)}}


def member(ok, name):
    """One member of a served answer, or an empty table when it is absent.

    An answer with no member of that name is a host that did the call and
    reported nothing under it. Both read as "nothing to act on" here.
    """
    return (ok or {}).get(name) or {}


def deliver(channel, key, report):
    """Take a worked branch as far as this cycle's own decision allows.

    Returns the `next` the cycle ends with. Every stage stops on a refusal,
    naming the ask, because a loop that carried on past one would be guessing
    at a power it was not granted.
    """
    opened = channel.ask(DELIVER_OPEN)
    if opened is None:
        return refused_halt(
            channel,
            "%s is worked onto branch %s"
            % (key, report.get("branch", "?")),
        )
    pull_request = member(opened, "pull_request")
    number = pull_request.get("pr", "?")
    sys.stderr.write("stella-selfdriving: opened %s for %s\n" % (number, key))

    observed = channel.ask(DELIVER_OBSERVE, {"deliver_observe": {"pr": number}})
    if observed is None:
        return refused_halt(channel, "%s is open as %s" % (key, number))

    decided = channel.ask(
        DELIVER_NEXT, {"deliver_next": {"observation": member(observed, "observation")}}
    )
    if decided is None:
        return refused_halt(channel, "%s was read on %s" % (number, key))
    decision = member(decided, "decision")
    action = decision.get("action")
    state = decision.get("state", "?")

    if action != "merge":
        # Waiting, pushing a fix and rebasing are a later cycle's work. This
        # one sleeps. Holding a session open across CI helps nobody.
        sys.stderr.write(
            "stella-selfdriving: %s is %s, next action %s\n"
            % (number, state, action)
        )
        return {"sleep": {"secs": IDLE_SECS}}

    merged = channel.ask(DELIVER_MERGE, {"deliver_merge": {"pr": number}})
    if merged is None:
        return refused_halt(
            channel, "%s is %s and the machine said merge" % (number, state)
        )
    return {
        "halt": {
            "reason": "%s is merged for %s"
            % (member(merged, "merge").get("pr", number), key)
        }
    }


def cycle(channel):
    """One cycle's decision, as the `next` it ends with."""
    served = channel.ask(BACKLOG_NEXT)
    if served is None:
        return refused_halt(channel, "reading the queue")

    issues = queue_of(served)
    if not issues:
        return {"sleep": {"secs": IDLE_SECS}}

    top = issues[0]
    key = top.get("key", "?")
    title = top.get("title", "")
    sys.stderr.write("stella-selfdriving: next is %s %s\n" % (key, title))

    claimed = channel.ask(BACKLOG_CLAIM, {"backlog_claim": {"issue": key}})
    if claimed is None:
        return refused_halt(
            channel,
            "%s is ready, and this loop does not work an issue it cannot claim, "
            "because two loops taking one issue is what the claim prevents" % key,
        )

    claim = (claimed or {}).get("claim") or {}
    if not claim.get("held"):
        holder = claim.get("holder") or "another worker"
        sys.stderr.write("stella-selfdriving: %s is held by %s\n" % (key, holder))
        return {"sleep": {"secs": IDLE_SECS}}

    worked = channel.ask(WORK_START, {"work_start": {"issue": key}})
    if worked is None:
        return refused_halt(channel, "%s is claimed" % key)

    report = member(worked, "work")
    state = report.get("state")
    if state == "changed":
        return deliver(channel, key, report)
    if state == "no_change":
        return {
            "halt": {
                "reason": "%s needed no change: %s" % (key, report.get("detail", ""))
            }
        }
    return {
        "halt": {
            "reason": "%s did not finish: %s" % (key, report.get("detail", state))
        }
    }


def main():
    session = read_session(sys.stdin)
    channel = Channel(sys.stdin, sys.stdout)
    nxt = cycle(channel)
    for call, refusal, detail in channel.refusals:
        sys.stderr.write(
            "stella-selfdriving: session %s: %s refused (%s): %s\n"
            % (session, call, refusal, detail)
        )
    channel.finish(nxt)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
