#!/usr/bin/env python3
"""`stella-goal` — the goal-supervision reference plugin, as a plugin.

Standard library only, deliberately — the same rule `plugins/stella-plan` and
`plugins/stella-research` ship under (`doc:pipeline-as-plugins` §9 rule 3: "if
a plugin CANNOT be written without an SDK, the protocol is too complicated").
`main.py` is the whole program.

# The wire

`crates/stella-plugin/src/wire.rs` and `crates/stella-plugin/src/host_call.rs`
are the contract; these are its shapes as a plugin author meets them. The host
spawns `[runtime].argv` directly — no shell — writes one JSON request on
stdin, and reads the response from stdout. Unlike `stella-plan`/
`stella-research`, this plugin answers BOTH declared points, so every process
this program runs as answers exactly one of the two:

    {"point": "before_turn", "body": {...BeforeTurnRequest}}
 -> {"point": "before_turn", "body": {...BeforeTurnResponse}}

    host   {"point": "after_turn", "body": {...AfterTurnRequest}}
 -> plugin {"call": "child_turn", "id": 1, "args": {"role": "verifier", "instruction": "..."}}
 -> host   {"result": 1, "ok": {"role": "verifier", "seat": "...", "report": "...", "completed": true}}
 -> plugin {"point": "after_turn", "body": {"evidence": {...ObservedEvidence}}}   <- ends it

Every table denies unknown fields, the envelope included, so this program does
too, at every level — the same discipline `stella-plan/main.py` and
`stella-research/main.py` document at length and this file does not repeat.

There is no error variant in either point's response. A plugin that cannot
answer *fails* — non-zero exit, one line on stderr — and the host proceeds
without it: `before_turn` runs the turn with no contribution, `after_turn`
merges in `EvidenceSet::unobserved()`-equivalent evidence via the same
`ObservedEvidence::nothing()` a Rust host would build for a fault
(`crates/stella-runtime/src/wrapper/dispatch.rs::close_round`).

# What `judge`/`again` do with what this plugin reports

Nothing here decides done. `judge` and `again`
(`crates/stella-runtime/src/wrapper/verdict.rs`) are host-run, synchronous,
I/O-free functions over the `[requirements]`/`[oracle]` data `plugin.toml`
declares — the whole reason a goal-mode verifier's model call had to move
into `after_turn` rather than living in a `judge` this plugin could implement
(`doc:turn-loop-wrappers` §9.2, quoted in `plugin.toml`'s header). This
program's only job is to report the one number the declared oracle check
reads: `measurements = {"met": 1}` when the verifier said yes, `{"met": 0}`
when it said no, and no `"met"` key at all whenever nothing decided it either
way — see "Every way the ask can degrade" below.

# What the framing contributes, and why it is byte-mirrored

At the `execute` stage, `before_turn` contributes `goal_kickoff_text` from
`crates/stella-core/src/goal.rs`, copied byte-for-byte, for the same reason
`stella-plan/main.py` copies `PLANNER_INSTRUCTIONS`: `stella run --pipeline
goal-v1` runs no separate "goal kickoff" step the way `stella goal`'s own CLI
loop does (`stella_core::goal::goal_kickoff_text` is pushed once before that
loop even starts, in `crates/stella-cli/src/agent/goal/goal_wrapped.rs` and
its raw/classic siblings) — under this door the framing has to come from
somewhere, and it comes from here, every round, so a worker turn started
fresh sees the identical words either surface would have opened with.

# The verifier call: what it asks, and what it honestly cannot ask

`after_turn` asks for one bounded turn at the `verifier` role intent, with an
instruction built from `VERIFIER_SYSTEM_PROMPT`
(`crates/stella-core/src/goal.rs`), copied byte-for-byte for the reason
`stella-plan/main.py` gives for `PLANNER_INSTRUCTIONS`: that text is the
output contract this program's own `parse_verdict` depends on, and drifting it
here would make this plugin's parser and the model's actual instructions two
different programs.

**What `Engine::assess` reads that this plugin cannot.** The built-in verifier
(`crates/stella-core/src/goal.rs::Engine::assess`) renders the WHOLE recent
conversation, tail-biased, via `render_transcript_tail(messages, max_chars)` —
every user/assistant/tool message, not just the final answer. `AfterTurnRequest`
carries none of that: its `turn` field is a `TurnOutcome` with exactly four
facts — `completed`, `answer` (the final assistant text only), and `tools`/
`changed_files` (each `Option`, "this host does not report it" when absent,
`crates/stella-plugin/src/wire.rs`'s own doc comment on `TurnOutcome`). So the
verifier this plugin spends judges from a strictly narrower view than
`Engine::assess`'s real one — the final answer plus whichever of tool
names/changed files this door happens to report — and says so honestly in the
instruction it builds rather than pretending to have seen the transcript.
This is the same shape of narrowing `stella-plan/main.py` documents for its
own planner prompt (goal only, no `recall`/`research`/`repo_structure`/
`revision`) — see the README's "Known gaps" table.

# Every way the ask can degrade, and the one way it refuses

Mirrors `stella-plan/main.py`'s `HostCalls`/`child_turn` split exactly. The
host offers no channel (`unavailable`), the manifest declares no such role
intent (`undeclared`), the role resolves to the worker's own seat
(`forbidden`), the allowance is spent (`allowance-spent`), the host tried and
the turn failed (`failed`), or nobody answered — every one of these degrades
to empty evidence (no `"met"` measurement) and is reported on stderr, never
guessed at. Two further degradations are this program's own, past a
successful call: a completed child turn whose report does not parse as
`{"met": ..., ...}` (this plugin, like `stella-plan`, asks once per round and
cannot afford the built-in verifier's own JSON-repair retry within it: that is
`[loop] max_calls = 1`, the per-point gate, and it is a different number from
`max_child_turns`, which bounds the whole run), and a child turn the
host reports as **incomplete** — `crates/stella-core/src/goal.rs::Engine::assess`
treats an incomplete verifier sub-agent as no verdict at all
(`GoalAssessError::Incomplete`, never a fabricated answer), and this plugin
holds the same line: a partial judgement is not a judgement. The one thing
that **refuses** rather than degrades is an `ok` answer shaped differently
than `ChildTurnResult` — the host disagreeing about the contract, not an
ordinary outcome.

# How the verifier's own words reach the next round

`ObservedEvidence` carries `flip`, `measurements: BTreeMap<String, u64>` and
one advisory string, `detail` (#3840). This plugin sends
`GoalVerifierVerdict`'s `feedback` there, falling back to `reasoning` — the
byte-for-byte mirror of what `stella_core::goal::verifier_feedback_text` hands
the worker in the built-in loop.

`detail` is not evidence and cannot decide anything: it never reaches
`EvidenceSet`, which is the closed vocabulary `judge` is total over. The host
attaches it to the unmet clauses after the verdict is decided, and
`correction_text` (`crates/stella-runtime/src/wrapper/verdict.rs`) prints it
under the static `[requirements]` statement. So a held-open round is told what
the verifier actually found, and the verdict is still decided from the `met`
measurement alone.

# Every capability arrives in the request

`goal` is the only thing `before_turn` reads from the request body.
`candidate`, when present, is validated for shape (deny-unknown-fields) and
never read — this plugin opens no file. `published` signals are validated for
shape and ignored: this plugin's `[wrapper]` stage order names no condition,
so there is nothing for a published value to change. `after_turn` reads
`goal` and `turn`; `candidate` is validated and ignored for the same reason.
"""

import json
import sys

# The version every message on this wire carries (`wire::PROTOCOL_VERSION`).
PROTOCOL_VERSION = 1

# The two points this plugin answers, matching `[loop].points`.
BEFORE_TURN = "before_turn"
AFTER_TURN = "after_turn"

# The one stage this plugin frames the goal at. Every other declared stage's
# `before_turn` call gets an empty contribution, byte-identical to a host with
# no plugin installed.
EXECUTE_STAGE = "execute"

# The fields each request table declares. Anything else is a typo.
BEFORE_TURN_REQUEST_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "published",
}
AFTER_TURN_REQUEST_FIELDS = {
    "protocol_version",
    "wrapper",
    "stage",
    "round",
    "goal",
    "candidate",
    "turn",
}
CANDIDATE_GRANT_FIELDS = {"handle", "root", "test"}
PUBLISHED_SIGNAL_FIELDS = {"signal", "value"}
TURN_OUTCOME_FIELDS = {"completed", "answer", "tools", "changed_files"}

# `StageName`, closed — mirrored from `stella-plan/main.py` verbatim. A stage
# name this plugin does not recognise is indistinguishable from "some stage
# that is not execute", which would silently under-frame a turn this plugin
# was installed to supervise.
STAGE_NAMES = (
    "triage",
    "recall",
    "research",
    "plan",
    "scope",
    "execute",
    "witness",
    "verify",
    "verdict",
    "reflect",
    "contextwrite",
    "complete",
)

# ── The host-call channel (`doc:wrapper-socket` §6b) ─────────────────────────
CALL_CHILD_TURN = "child_turn"
CALL_ANSWER_FIELDS = {"result", "ok", "err"}
CHILD_TURN_OK_FIELDS = {"role", "seat", "report", "completed"}
CHILD_TURN_ANSWER = "the child turn answer"

# The `[roles.<name>]` key this plugin declares and the only role intent it
# ever names. No shipped host binds a seat for it today — see main.py's
# module doc and plugin.toml's header.
ROLE_INTENT = "verifier"

# Byte-mirrored from `crates/stella-core/src/goal.rs::VERIFIER_SYSTEM_PROMPT`.
# This is the output contract `parse_verdict` below depends on — a model told
# anything else is a model this program cannot honestly parse.
VERIFIER_SYSTEM_PROMPT = (
    "You are an impartial verifier assessing whether a coding agent "
    "has fully met a stated goal. Judge from EVIDENCE, never from claims: use whatever "
    "read-only tools you are offered to verify the work directly whenever the transcript "
    "alone is not conclusive — read the changed files, check the tests exist, inspect CI. "
    "Claimed success without supporting evidence is NOT met. The strongest completion "
    "evidence is a witness test observed to fail on the previous code and pass on the new "
    "code; a merely green test suite is weak evidence, since it cannot distinguish real "
    "work from vacuous tests or unwired code. If you need something only the worker can "
    "provide (a trace, a screenshot, a system log, an explanation), set met:false and put "
    "the request in feedback — the worker acts on it next round. When decided, end your "
    "reply with ONLY a JSON object, no prose after it:\n"
    '{"met": true|false, "reasoning": "why, in one or two sentences", '
    '"feedback": "if not met: the single most useful next action or evidence request"}'
)

# Byte-mirrored from `crates/stella-core/src/goal.rs::goal_kickoff_text`.
def goal_kickoff_text(goal):
    return (
        "GOAL: {}\n\nWork toward this goal. An independent verifier will assess the "
        "result after each working round from the transcript evidence; keep your work "
        "verifiable (run tests, show outputs)."
    ).format(goal)


class Refusal(Exception):
    """The plugin cannot answer. Exits non-zero; the host proceeds without it."""


def refuse(reason):
    raise Refusal(reason)


def deny_unknown(table, allowed, subject="the request"):
    """Refuse a table carrying a key the contract does not declare.

    Identical shape to `stella-plan/main.py::deny_unknown`.
    """
    unknown = sorted(set(table) - allowed)
    if unknown:
        refuse(
            "{} denies unknown fields; got {}".format(subject, ", ".join(unknown))
        )
    return table


def read_json(stream):
    """The next JSON document on `stream`, or `None` when there is not one.

    Identical implementation to `stella-plan/main.py::read_json` — see that
    file for why both hosts this program meets are legitimate. Kept local so
    this program has no import beyond the standard library and no dependency
    on its siblings.
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
    """One JSON document, on one line, flushed — the flush is load-bearing,
    see `stella-plan/main.py::write_json`."""
    stream.write(json.dumps(document) + "\n")
    stream.flush()


def report(reason):
    """Say on stderr what this plugin degraded to, and why — never silent."""
    sys.stderr.write("stella-goal: {}\n".format(reason))


def validate_candidate(body):
    candidate = body.get("candidate")
    if candidate is not None:
        if not isinstance(candidate, dict):
            refuse("candidate must be a CandidateGrant object")
        deny_unknown(candidate, CANDIDATE_GRANT_FIELDS)
        # Never read further: this plugin opens no file under any root the
        # host grants it. Validated for shape and nothing else.


def read_envelope(stdin):
    """Decode `{"point": ..., "body": ...}` and return `(point, body)`."""
    envelope = read_json(stdin)
    if not isinstance(envelope, dict):
        refuse("stdin was not a single JSON object")
    if set(envelope) - {"point", "body"}:
        refuse("the request envelope carried a field outside {point, body}")

    point = envelope.get("point")
    if point not in (BEFORE_TURN, AFTER_TURN):
        refuse(
            "this plugin answers before_turn or after_turn only; the host "
            "asked for {}".format(json.dumps(point))
        )

    body = envelope.get("body")
    if not isinstance(body, dict):
        refuse("the request envelope carried no object body")
    return point, body


def check_protocol_version(body):
    version = body.get("protocol_version")
    # `isinstance(True, int)` is True in Python, so a JSON `true` would
    # otherwise compare equal to version 1. It does not, and this says so.
    if isinstance(version, bool) or version != PROTOCOL_VERSION:
        refuse(
            "this plugin speaks wrapper protocol_version {}; the host asked "
            "for {}".format(PROTOCOL_VERSION, json.dumps(version))
        )


def read_before_turn_request(body):
    deny_unknown(body, BEFORE_TURN_REQUEST_FIELDS)
    check_protocol_version(body)

    stage = body.get("stage")
    if stage not in STAGE_NAMES:
        refuse(
            "StageName is a closed set {{{}}}; got {}".format(
                ", ".join(STAGE_NAMES), json.dumps(stage)
            )
        )

    validate_candidate(body)

    for published in body.get("published", []) or []:
        if not isinstance(published, dict):
            refuse("published must be an array of PublishedSignal objects")
        deny_unknown(published, PUBLISHED_SIGNAL_FIELDS)

    goal = body.get("goal")
    if not isinstance(goal, str):
        refuse("the request carried no goal")
    return body


def read_turn_outcome(turn):
    if not isinstance(turn, dict):
        refuse("the request carried no turn")
    deny_unknown(turn, TURN_OUTCOME_FIELDS, "the turn outcome")
    completed = turn.get("completed")
    if not isinstance(completed, bool):
        refuse("the turn outcome carried no completed")
    answer = turn.get("answer", "")
    if not isinstance(answer, str):
        refuse("the turn outcome's answer must be a string")
    tools = turn.get("tools")
    if tools is not None:
        if not isinstance(tools, list) or not all(isinstance(t, str) for t in tools):
            refuse("the turn outcome's tools must be an array of strings, or absent")
    changed_files = turn.get("changed_files")
    if changed_files is not None:
        if not isinstance(changed_files, list) or not all(
            isinstance(f, str) for f in changed_files
        ):
            refuse(
                "the turn outcome's changed_files must be an array of strings, or absent"
            )
    return {
        "completed": completed,
        "answer": answer,
        "tools": tools,
        "changed_files": changed_files,
    }


def read_after_turn_request(body):
    deny_unknown(body, AFTER_TURN_REQUEST_FIELDS)
    check_protocol_version(body)

    stage = body.get("stage")
    if stage is not None and stage not in STAGE_NAMES:
        refuse(
            "StageName is a closed set {{{}}}; got {}".format(
                ", ".join(STAGE_NAMES), json.dumps(stage)
            )
        )

    validate_candidate(body)

    goal = body.get("goal")
    if not isinstance(goal, str):
        refuse("the request carried no goal")

    turn = read_turn_outcome(body.get("turn"))
    return goal, turn


class HostCalls:
    """This plugin's end of the host-call conversation (§6b).

    Identical shape to `stella-plan/main.py::HostCalls` — see that file's
    docstring for the outcome-vs-shape distinction this class enforces.
    """

    def __init__(self, stdin, stdout):
        self.stdin = stdin
        self.stdout = stdout
        self.asked = 0

    def ask(self, call, args):
        """Make one host call and return its `ok` payload, or `None`.

        `None` covers every way a call can fail to produce one: refused for
        any of the closed reasons, answered for a different call, or not
        answered at all. The *reason* is only ever reported, on stderr.
        """
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
        deny_unknown(answer, CALL_ANSWER_FIELDS, CHILD_TURN_ANSWER)
        ok = answer.get("ok")
        failure = answer.get("err")
        if (ok is None) == (failure is None):
            refuse("a host-call answer carries either ok or err, never both")
        if failure is not None:
            report(
                "the host did not serve the {} call: {}".format(call, refusal(failure))
            )
            return None
        if not isinstance(ok, dict):
            refuse("the child turn answer's ok must be an object")
        return ok


def refusal(failure):
    """A failed call's `err`, as the one line this plugin logs about it.

    Identical to `stella-plan/main.py::refusal`.
    """
    if isinstance(failure, dict):
        code = failure.get("refusal")
        detail = failure.get("detail")
        if isinstance(code, str) and isinstance(detail, str):
            return "{}: {}".format(code, detail)
    return json.dumps(failure, sort_keys=True)


def verifier_instruction(goal, turn):
    """The `child_turn` instruction this plugin sends — the narrowed verifier
    prompt described in this module's docstring.

    `ChildTurnArgs` carries one `instruction: String`, not a separate
    system/user split — so the fixed instructions and the per-call payload
    are concatenated here, mirroring `stella-plan/main.py::planner_instruction`.
    """
    lines = ["## Goal", goal.strip(), "", "## The working turn that just ran"]
    lines.append("Completed: {}".format("true" if turn["completed"] else "false"))
    if turn["answer"].strip():
        lines.append("")
        lines.append("### Final answer")
        lines.append(turn["answer"].strip())
    if turn["tools"] is not None:
        lines.append("")
        lines.append("### Tools dispatched")
        lines.append(", ".join(turn["tools"]) if turn["tools"] else "(none)")
    if turn["changed_files"] is not None:
        lines.append("")
        lines.append("### Files changed")
        lines.append(", ".join(turn["changed_files"]) if turn["changed_files"] else "(none)")
    lines.append("")
    lines.append(
        "(This is the working turn's final answer and, where this host reports them, "
        "the tools it dispatched and the files it changed — not the full transcript. "
        "Judge from what is here; do not assume a fuller record exists.)"
    )
    payload = "\n".join(lines)
    return VERIFIER_SYSTEM_PROMPT + "\n\n" + payload


def child_turn(host_calls, goal, turn):
    """Ask for one verifier-role child turn; return its result or `None`.

    `None` is every degradation this module's docstring names, already
    reported by `HostCalls.ask`. A malformed `ok` payload is not a
    degradation, and refuses instead — mirrors
    `stella-plan/main.py::child_turn` exactly.
    """
    ok = host_calls.ask(
        CALL_CHILD_TURN,
        {"role": ROLE_INTENT, "instruction": verifier_instruction(goal, turn)},
    )
    if ok is None:
        return None
    deny_unknown(ok, CHILD_TURN_OK_FIELDS, CHILD_TURN_ANSWER)
    role = ok.get("role")
    seat = ok.get("seat")
    report_text = ok.get("report")
    completed = ok.get("completed")
    if not isinstance(role, str):
        refuse("the child turn answer carried no role")
    if not isinstance(seat, str):
        refuse("the child turn answer carried no seat")
    if not isinstance(report_text, str):
        refuse("the child turn answer carried no report")
    if not isinstance(completed, bool):
        refuse("the child turn answer's completed must be a boolean")
    return {"role": role, "seat": seat, "report": report_text, "completed": completed}


# ── Verdict parsing, ported from crates/stella-core/src/goal.rs ─────────────
#
# `parse_verdict` is mirrored deliberately, the way `stella-plan/main.py`
# mirrors `plan.rs::parse_plan`: the built-in verifier's own
# `VERIFIER_SYSTEM_PROMPT` promises the model a trailing JSON object, and a
# plugin that read the answer differently would be a second, silently
# diverging definition of what counts as a verdict. `goal.rs`'s own
# `parse_verdict_tolerates_prose_and_fences` / `_survives_braces_in_the_
# verifiers_prose` / `_handles_braces_inside_the_verdicts_own_strings` tests
# are the closest thing to a shared golden corpus, and this port's vectors
# exercise the same shapes those tests do.


def parse_verdict(text):
    """Parse `{"met": bool, "reasoning": str, "feedback": str}` from a
    verifier's report. Scans every `{` before the LAST `}`, rightmost first,
    and returns the first span that decodes as a well-typed verdict — the
    same last-`{`-before-final-`}` rule `goal.rs::parse_verdict` uses, ported
    byte-for-byte rather than re-derived, because the old first-`{`..last-`}`
    span this replaced silently turned `met: true` into "not met" whenever
    the verifier's own prose quoted a brace before its JSON answer.
    """
    end = text.rfind("}")
    if end == -1:
        return None
    starts = [i for i in range(end) if text[i] == "{"]
    for start in reversed(starts):
        candidate = text[start : end + 1]
        try:
            parsed = json.loads(candidate)
        except ValueError:
            continue
        verdict = _as_verdict(parsed)
        if verdict is not None:
            return verdict
    return None


def _as_verdict(parsed):
    """`parsed` as a `GoalVerifierVerdict`, or `None` if it does not match:
    `met` is a required boolean, `reasoning`/`feedback` default to `""` and
    must be strings when present — mirrors serde's `#[serde(default)]` on a
    struct with no `deny_unknown_fields` (extra keys are tolerated)."""
    if not isinstance(parsed, dict):
        return None
    met = parsed.get("met")
    if not isinstance(met, bool):
        return None
    reasoning = parsed.get("reasoning", "")
    feedback = parsed.get("feedback", "")
    if not isinstance(reasoning, str) or not isinstance(feedback, str):
        return None
    return {"met": met, "reasoning": reasoning, "feedback": feedback}


def evidence_nothing():
    """What this plugin reports when it observed nothing at all — mirrors
    `ObservedEvidence::nothing()`: no flip was ever attempted (this oracle's
    evidence is not a transition either way, so this is always the honest
    answer — see `plugin.toml`'s `flip = "not-applicable"`), and no
    measurement, which is what makes `judge` abstain
    (`UndecidedReason::MeasurementMissing`) rather than credit or blame an
    assessment that never happened."""
    return {"flip": "not-attempted", "measurements": {}}


def evidence_met(verdict):
    """The verdict as evidence: `met` as a 0/1 measurement, and the verifier's
    own words as the advisory `detail` (#3840).

    `detail` is never read by `judge` — the host attaches it to the unmet
    clauses *after* deciding, and the only thing that reads it is the
    correction a held-open round renders. That is what makes it safe to carry
    free text at all, and it is the byte-for-byte mirror of what
    `stella_core::goal::verifier_feedback_text` hands the worker: `feedback`
    when the verifier wrote one, falling back to `reasoning`."""
    evidence = {
        "flip": "not-attempted",
        "measurements": {"met": 1 if verdict["met"] else 0},
    }
    words = verdict["feedback"] or verdict["reasoning"]
    if words:
        evidence["detail"] = "Verifier feedback: {}".format(words)
    return evidence


def before_turn_response(body):
    """The one contribution this plugin makes at `before_turn`: the goal
    framing, at `execute` only. Every other stage answers empty — absent
    `context` is the host's own `BeforeTurnResponse::empty()` shape, so an
    empty contribution is the same bytes a host with no plugin installed
    would see."""
    read_before_turn_request(body)
    response = {"protocol_version": PROTOCOL_VERSION}
    if body["stage"] == EXECUTE_STAGE:
        response["context"] = [
            {"label": "goal:framing", "text": goal_kickoff_text(body["goal"])}
        ]
    return response


def after_turn_response(body, host_calls):
    """This plugin's one piece of real work: spend the verifier child turn
    and report what it decided, as evidence — never a verdict."""
    goal, turn = read_after_turn_request(body)

    result = child_turn(host_calls, goal, turn)
    if result is None:
        return {"protocol_version": PROTOCOL_VERSION, "evidence": evidence_nothing()}

    if not result["completed"]:
        report(
            "the verifier turn did not reach a final answer — its budget or "
            "step cap was reached; a partial judgement is not a judgement, "
            "contributing no evidence"
        )
        return {"protocol_version": PROTOCOL_VERSION, "evidence": evidence_nothing()}

    verdict = parse_verdict(result["report"])
    if verdict is None:
        report(
            "the verifier turn's report contained no parseable verdict; "
            "contributing no evidence"
        )
        return {"protocol_version": PROTOCOL_VERSION, "evidence": evidence_nothing()}

    return {
        "protocol_version": PROTOCOL_VERSION,
        "evidence": evidence_met(verdict),
    }


def main():
    try:
        point, body = read_envelope(sys.stdin)
        if point == BEFORE_TURN:
            response_body = before_turn_response(body)
        else:
            response_body = after_turn_response(body, HostCalls(sys.stdin, sys.stdout))
    except Refusal as refusal_reason:
        sys.stderr.write("stella-goal: {}\n".format(refusal_reason))
        return 1

    write_json(sys.stdout, {"point": point, "body": response_body})
    return 0


if __name__ == "__main__":
    sys.exit(main())
