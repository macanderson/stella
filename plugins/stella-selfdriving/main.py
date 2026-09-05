#!/usr/bin/env python3
"""`stella-selfdriving` — the delivery loop, as a driver program.

The standard library and nothing else, like every plugin here. That is
`doc:pipeline-as-plugins` §9 rule 3: a plugin that cannot be written without
an SDK means the protocol is too hard. This file is the whole program.

# What it is

A driver, not a wrapper. It never runs inside a turn. It starts them. The host
opens one session. This program says what a cycle should do, asks the host to
do it, and says what should happen next:

    host   {"point": "drive", "body": {"session": "cycle-7"}}
 -> plugin {"call": "backlog_next", "id": 1}
 -> host   {"result": 1, "ok": {"backlog": {"issues": [...]}}}
 -> plugin {"call": "backlog_claim", "id": 2}
 -> host   {"result": 2, "err": {"refusal": "unsupported", "detail": "..."}}
 -> plugin {"point": "drive", "body": {"next": {"halt": {"reason": "..."}}}}

One JSON message per line, both ways. `crates/stella-plugin/src/driver.rs` is
the contract. `docs/wire/wrapper.wire.json` holds its exact bytes.

# It asks, and never reaches

This program holds no forge token and no provider key. It has no worktree and
no budget. The host does each thing it needs, when asked.

The host checks each ask against the `[driver] calls` list in `plugin.toml`.
That is the list a person read before install. An ask that is not on it comes
back as `err` with `refusal: "undeclared"`. The right move then is to stop and
say so. Never to go get the thing some other way.

# What a cycle does now

The host serves `backlog_next`. Nothing else is built yet. So a cycle reads the
ranked queue. If there is work at the top, it asks to claim it.

The claim is turned down today. This program stops there. It will not work an
issue it cannot know is free. Two loops on one issue is what a claim is for,
and going ahead without one would trade a good refusal for a quiet race.

So a cycle ends in a `halt` that names what the host would not do. It becomes a
real loop once `work` and `deliver` are served. Nothing in this file changes
shape for that. Only the stage it reaches before it stops.

# `scripts/self-driving.sh` still exists

That shell script is the working driver. It holds the powers this package's
`[[capabilities]]` list names — `bash`, `write_file`, `process_spawn` — as
you, straight out. This program holds none of them. It asks. The shell script
stays until this one can do what it does, which is §10's own rule.
"""

import json
import sys

# The verbs a cycle asks for, in the order it needs them. Both are on
# `plugin.toml`'s `[driver] calls`. An ask that is not on that list would be
# turned down, and writing one here would be a bug, not a test of the gate.
BACKLOG_NEXT = "backlog_next"
BACKLOG_CLAIM = "backlog_claim"

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

    def ask(self, call):
        """Ask the host for `call`.

        Returns the `ok` table, or `None` when the host refused. A refusal is
        recorded and returned as an absence: it is an answer this program reads
        and degrades on, never an error that ends the session.
        """
        request_id = self._next_id
        self._next_id += 1
        self._write({"call": call, "id": request_id})
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

    An answer with no `backlog` member is a host that performed the call and
    reported nothing, which is a different fact from an empty queue — both read
    as "no work" here, and the caller says which it saw.
    """
    page = (ok or {}).get("backlog") or {}
    return page.get("issues") or []


def cycle(channel):
    """One cycle's decision, as the `next` it ends with."""
    served = channel.ask(BACKLOG_NEXT)
    if served is None:
        call, refusal, detail = channel.refusals[-1]
        return {
            "halt": {
                "reason": "%s was refused (%s): %s" % (call, refusal, detail)
            }
        }

    issues = queue_of(served)
    if not issues:
        return {"sleep": {"secs": IDLE_SECS}}

    top = issues[0]
    key = top.get("key", "?")
    title = top.get("title", "")
    sys.stderr.write("stella-selfdriving: next is %s %s\n" % (key, title))

    if channel.ask(BACKLOG_CLAIM) is None:
        call, refusal, detail = channel.refusals[-1]
        return {
            "halt": {
                "reason": (
                    "%s is ready, and %s was refused (%s): %s — this loop does not "
                    "work an issue it cannot claim, because two loops taking one "
                    "issue is what the claim prevents" % (key, call, refusal, detail)
                )
            }
        }

    return {
        "halt": {
            "reason": (
                "%s is claimed, and no host serves `work_start` yet — "
                "scripts/self-driving.sh is still the driver that works an issue" % key
            )
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
